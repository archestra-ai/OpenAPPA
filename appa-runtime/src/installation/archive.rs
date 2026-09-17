//! Archives an install reads bytes through: the bounded fetch of a release
//! asset or source archive, and the bounded extraction of either.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use appa_package::tree::{MAX_ENTRIES, MAX_UNCOMPRESSED_BYTES};
use thiserror::Error;

/// The one place a debug-only seam reads the environment.
///
/// Test seams for the endpoint, the runtime starter and acquisition URLs all use
/// this function. `[profile.release]` pins `debug-assertions = false`, so a
/// shipped binary reads no environment here at all. The release workflow proves
/// that on the packaged artifact by feeding it a malformed `APPA_ENDPOINT` and
/// requiring it to be ignored; that single probe stands for these seams only for
/// as long as this is the only gate.
pub(crate) fn debug_override(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        env::var(name).ok()
    } else {
        None
    }
}

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("cannot write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("the archive at {path} is unusable: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("cannot fetch {url}: {reason}")]
    Fetch { url: String, reason: String },
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

const RELEASE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/releases/download";
const SOURCE_ARCHIVE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/archive";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;

/// The release download base. `APPA_RELEASE_BASE_URL` overrides it in debug
/// builds only, and the install reads it once at the boundary rather than
/// leaving the fetch to consult the environment underneath its caller.
pub(crate) fn release_base_url() -> String {
    debug_override("APPA_RELEASE_BASE_URL").unwrap_or_else(|| RELEASE_BASE_URL.to_owned())
}

/// The immutable GitHub source-archive base. Like the release test seam, the
/// override exists only in debug builds and cannot redirect a shipped binary.
pub(crate) fn source_archive_base_url() -> String {
    debug_override("APPA_SOURCE_ARCHIVE_BASE_URL").unwrap_or_else(|| SOURCE_ARCHIVE_BASE_URL.to_owned())
}

/// The one directory an extracted source archive holds: the repository root.
pub(crate) fn single_directory(container: &Path) -> Result<PathBuf, ArchiveError> {
    let mut entries = fs::read_dir(container).map_err(|source| ArchiveError::Read {
        path: container.to_path_buf(),
        source,
    })?;
    let first = entries
        .next()
        .transpose()
        .map_err(|source| ArchiveError::Read {
            path: container.to_path_buf(),
            source,
        })?
        .ok_or_else(|| ArchiveError::Malformed {
            path: container.to_path_buf(),
            reason: "it contains no repository root".to_owned(),
        })?;
    if entries.next().is_some() || !first.path().is_dir() {
        return Err(ArchiveError::Malformed {
            path: container.to_path_buf(),
            reason: "it does not contain exactly one repository root".to_owned(),
        });
    }
    Ok(first.path())
}

/// Stream the artifact to `destination`, enforcing the size cap as it goes.
///
/// The install is the synchronous CLI path and never runs under an existing
/// reactor, so this owns a current-thread runtime for the duration of the fetch.
pub(crate) fn download_bounded(url: &str, destination: &Path, max_bytes: u64) -> Result<(), ArchiveError> {
    crate::tls::install_crypto_provider();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| ArchiveError::Fetch {
            url: url.to_owned(),
            reason: format!("cannot start a runtime for the download: {source}"),
        })?;

    runtime.block_on(async {
        let failed = |reason: String| ArchiveError::Fetch {
            url: url.to_owned(),
            reason,
        };
        let https_only = url.starts_with("https://");
        let client = reqwest::Client::builder()
            .user_agent(concat!("appa/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many artifact redirects");
                }
                if https_only && attempt.url().scheme() != "https" {
                    return attempt.error("an authenticated artifact request cannot redirect to plaintext HTTP");
                }
                attempt.follow()
            }))
            .build()
            .map_err(|error| failed(error.to_string()))?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| failed(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(failed(format!("the release responded {status}")));
        }
        if let Some(length) = response.content_length()
            && length > max_bytes
        {
            return Err(failed(format!(
                "it declares {length} bytes, more than the {max_bytes} accepted"
            )));
        }

        let mut file = fs::File::create(destination).map_err(|source| ArchiveError::Write {
            path: destination.to_path_buf(),
            source,
        })?;
        let mut written = 0u64;
        let mut stream = response;
        while let Some(chunk) = stream.chunk().await.map_err(|error| failed(error.to_string()))? {
            written = written.saturating_add(chunk.len() as u64);
            if written > max_bytes {
                return Err(failed(format!("it exceeds the {max_bytes} bytes accepted")));
            }
            std::io::Write::write_all(&mut file, &chunk).map_err(|source| ArchiveError::Write {
                path: destination.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Unpack a verified archive. Absolute paths, `..` components and anything that
/// is not a regular file or directory are refused; entry count and total
/// uncompressed bytes are capped. Modes come from the install, not from the
/// archive.
pub(crate) fn extract_archive(archive: &Path, destination: &Path) -> Result<(), ArchiveError> {
    extract_bounded(archive, destination, MAX_UNCOMPRESSED_BYTES, MAX_ENTRIES)
}

pub(crate) fn extract_bundle_archive(archive: &Path, destination: &Path) -> Result<(), ArchiveError> {
    // A complete marketplace may already use its entry allowance. The bundle
    // adds a separately bounded custom-file snapshot, descriptor, selection,
    // config, artifact files and directories.
    extract_bounded(archive, destination, 512 * 1024 * 1024, MAX_ENTRIES * 2 + 32)
}

fn extract_bounded(archive: &Path, destination: &Path, max_bytes: u64, max_entries: usize) -> Result<(), ArchiveError> {
    let file = fs::File::open(archive).map_err(|source| ArchiveError::Read {
        path: archive.to_path_buf(),
        source,
    })?;
    // Include headers/padding in a second bound, including PAX records the tar
    // reader consumes internally before yielding a regular entry.
    let decoded = std::io::Read::take(
        flate2::read::GzDecoder::new(file),
        max_bytes + (max_entries as u64 * 1024),
    );
    let mut tar = tar::Archive::new(decoded);
    let malformed = |reason: String| ArchiveError::Malformed {
        path: archive.to_path_buf(),
        reason,
    };
    let entries = tar
        .entries()
        .map_err(|error| malformed(format!("it is not a tar archive: {error}")))?;

    let mut count = 0usize;
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let mut entry = entry.map_err(|error| malformed(format!("unreadable entry: {error}")))?;
        count += 1;
        if count > max_entries {
            return Err(malformed(format!("it holds more than {max_entries} entries")));
        }
        total = total.saturating_add(entry.size());
        if total > max_bytes {
            return Err(malformed(format!("it unpacks to more than {max_bytes} bytes")));
        }

        let kind = entry.header().entry_type();
        // GitHub source archives may contain POSIX PAX metadata records. They
        // describe following entries and do not materialize in the tree; the
        // following entry's resolved path is still checked below.
        if matches!(kind, tar::EntryType::XHeader | tar::EntryType::XGlobalHeader) {
            continue;
        }

        let path = entry
            .path()
            .map_err(|error| malformed(format!("unreadable entry path: {error}")))?
            .into_owned();
        let relative = match safe_relative(&path) {
            EntryPath::Relative(relative) => relative,
            // A `./` root entry carries no content of its own.
            EntryPath::ArchiveRoot if kind == tar::EntryType::Directory => continue,
            EntryPath::ArchiveRoot => return Err(malformed("only a directory may name the archive root".into())),
            EntryPath::Escaping => {
                return Err(malformed(format!("{} escapes the archive root", path.display())));
            }
        };
        let target = destination.join(&relative);
        if !seen.insert(relative) {
            return Err(malformed(format!("{} occurs more than once", path.display())));
        }
        let write = |source: std::io::Error| ArchiveError::Write {
            path: target.clone(),
            source,
        };

        match kind {
            tar::EntryType::Directory => fs::create_dir_all(&target).map_err(write)?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|source| ArchiveError::Write {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(write)?;
                std::io::copy(&mut entry, &mut out).map_err(|source| ArchiveError::Write {
                    path: target.clone(),
                    source,
                })?;
            }
            other => {
                return Err(malformed(format!(
                    "{} is a {other:?} entry; only regular files and directories are accepted",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// What an archive entry's path denotes.
#[derive(Debug, PartialEq, Eq)]
enum EntryPath {
    Relative(PathBuf),
    /// `.` or `./`: the archive root itself, which carries no content.
    ArchiveRoot,
    /// Absolute, or climbing out with `..`.
    Escaping,
}

fn safe_relative(path: &Path) -> EntryPath {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            _ => return EntryPath::Escaping,
        }
    }
    if relative.as_os_str().is_empty() {
        EntryPath::ArchiveRoot
    } else {
        EntryPath::Relative(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_bundles_have_room_for_metadata_beyond_the_package_entry_limit() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("bundle.tar.gz");
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        ));
        for index in 0..=MAX_ENTRIES {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("entry-{index}"), std::io::empty())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        let packages = root.path().join("packages");
        let bundle = root.path().join("bundle");
        fs::create_dir(&packages).unwrap();
        fs::create_dir(&bundle).unwrap();
        assert!(extract_archive(&archive, &packages).is_err());
        extract_bundle_archive(&archive, &bundle).unwrap();
        assert_eq!(fs::read_dir(bundle).unwrap().count(), MAX_ENTRIES + 1);
    }

    #[test]
    fn extraction_refuses_duplicate_paths_without_overwriting_the_first_file() {
        for second in ["file", "./file"] {
            let source = tempfile::tempdir().unwrap();
            let destination = tempfile::tempdir().unwrap();
            let archive = source.path().join("bundle.tar.gz");
            let packed = fs::File::create(&archive).unwrap();
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(packed, flate2::Compression::fast()));
            for (name, bytes) in [("file", b"first".as_slice()), (second, b"second".as_slice())] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, bytes).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();

            assert!(matches!(
                extract_archive(&archive, destination.path()),
                Err(ArchiveError::Malformed { .. })
            ));
            assert_eq!(fs::read(destination.path().join("file")).unwrap(), b"first");
        }
    }

    #[test]
    fn extraction_refuses_traversal_and_specials() {
        for escaping in ["../escape", "/absolute", "plugin/../../escape"] {
            assert_eq!(safe_relative(Path::new(escaping)), EntryPath::Escaping);
        }
        assert_eq!(safe_relative(Path::new(".")), EntryPath::ArchiveRoot);
        assert_eq!(
            safe_relative(Path::new("./plugin/hooks.json")),
            EntryPath::Relative(PathBuf::from("plugin/hooks.json"))
        );
    }
}
