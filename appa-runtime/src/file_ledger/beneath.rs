//! Workspace-relative paths resolved one directory at a time, never through a symlink. Each
//! component is opened relative to the directory before it, so a parent directory swapped for
//! a link after validation cannot redirect a read, a write or a digest outside the workspace.

use std::ffi::OsString;
use std::fs::File;
use std::io;
#[cfg(feature = "daemon")]
use std::io::Read;
use std::path::{Component, Path};

pub(crate) use imp::Entry;

enum Parents {
    Existing,
    #[cfg(feature = "daemon")]
    Create,
}

/// The file at `relative`, or `None` when it or one of its parent directories does not exist.
/// A symlink anywhere below the workspace is an error.
pub(crate) fn open(workspace: &Path, relative: &str) -> io::Result<Option<File>> {
    match Entry::locate(workspace, relative)? {
        Some(entry) => entry.open(),
        None => Ok(None),
    }
}

impl Entry {
    /// The entry, or `None` when one of its parent directories does not exist.
    pub(crate) fn locate(workspace: &Path, relative: &str) -> io::Result<Option<Self>> {
        walk(workspace, relative, Parents::Existing)
    }

    /// The entry, creating its missing parent directories.
    #[cfg(feature = "daemon")]
    pub(crate) fn create(workspace: &Path, relative: &str) -> io::Result<Self> {
        walk(workspace, relative, Parents::Create)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

fn components(relative: &str) -> io::Result<(Vec<&std::ffi::OsStr>, OsString)> {
    let mut names = Path::new(relative)
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a normalized relative path",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let name = names
        .pop()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty path"))?;
    Ok((names, name.to_owned()))
}

#[cfg(unix)]
mod imp {
    use super::*;
    #[cfg(feature = "daemon")]
    use rustix::fs::AtFlags;
    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    #[cfg(feature = "daemon")]
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The last component of a workspace-relative path, held through its opened parent directory.
    pub(crate) struct Entry {
        parent: std::os::fd::OwnedFd,
        name: OsString,
    }

    const DIRECTORY: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    pub(super) fn walk(workspace: &Path, relative: &str, parents: Parents) -> io::Result<Option<Entry>> {
        let (directories, name) = components(relative)?;
        let mut parent = rustix::fs::openat(rustix::fs::CWD, workspace, DIRECTORY, Mode::empty())?;
        for directory in directories {
            parent = match (
                rustix::fs::openat(&parent, directory, DIRECTORY, Mode::empty()),
                &parents,
            ) {
                (Ok(next), _) => next,
                (Err(Errno::NOENT), Parents::Existing) => return Ok(None),
                #[cfg(feature = "daemon")]
                (Err(Errno::NOENT), Parents::Create) => {
                    match rustix::fs::mkdirat(&parent, directory, Mode::from_raw_mode(0o777)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    rustix::fs::openat(&parent, directory, DIRECTORY, Mode::empty())?
                }
                (Err(error), _) => return Err(error.into()),
            };
        }
        Ok(Some(Entry { parent, name }))
    }

    impl Entry {
        /// The file at this entry, or `None` when it does not exist. A symlink is an error.
        pub(crate) fn open(&self) -> io::Result<Option<File>> {
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            match rustix::fs::openat(&self.parent, &self.name, flags, Mode::empty()) {
                Ok(file) => Ok(Some(File::from(file))),
                Err(Errno::NOENT) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }

        /// Replace this entry with `content`: staged beside it, synced, then renamed over it.
        #[cfg(feature = "daemon")]
        pub(crate) fn publish(&self, content: &mut impl Read) -> io::Result<()> {
            let (staged, mut file) = self.stage()?;
            let written = io::copy(content, &mut file)
                .and_then(|_| file.sync_all())
                .and_then(|()| {
                    rustix::fs::renameat(&self.parent, &staged, &self.parent, &self.name).map_err(io::Error::from)
                });
            written.or_else(|error| {
                rustix::fs::unlinkat(&self.parent, &staged, AtFlags::empty())?;
                Err(error)
            })
        }

        /// Move this entry to `destination`, replacing what is there.
        #[cfg(feature = "daemon")]
        pub(crate) fn rename_to(&self, destination: &Entry) -> io::Result<()> {
            rustix::fs::renameat(&self.parent, &self.name, &destination.parent, &destination.name)
                .map_err(io::Error::from)
        }

        #[cfg(feature = "daemon")]
        fn stage(&self) -> io::Result<(OsString, File)> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            loop {
                let mut staged = OsString::from(".appa-staged-");
                staged.push(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match rustix::fs::openat(&self.parent, &staged, flags, Mode::from_raw_mode(0o600)) {
                    Ok(file) => return Ok((staged, File::from(file))),
                    Err(Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub(crate) struct Entry(std::convert::Infallible);

    pub(super) fn walk(_: &Path, relative: &str, _: Parents) -> io::Result<Option<Entry>> {
        components(relative)?;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    impl Entry {
        pub(crate) fn open(&self) -> io::Result<Option<File>> {
            match self.0 {}
        }

        #[cfg(feature = "daemon")]
        pub(crate) fn publish(&self, _: &mut impl Read) -> io::Result<()> {
            match self.0 {}
        }

        #[cfg(feature = "daemon")]
        pub(crate) fn rename_to(&self, _: &Entry) -> io::Result<()> {
            match self.0 {}
        }
    }
}

use imp::walk;
