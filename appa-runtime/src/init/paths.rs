//! Where an installation puts its files, and how those locations are named.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::InitError;

pub(super) struct DeploymentPaths {
    pub(super) install_dir: PathBuf,
    pub(super) config_dir: PathBuf,
    pub(super) data_dir: PathBuf,
    pub(super) claude_dir: PathBuf,
}

/// A directory override made absolute, without touching the filesystem.
///
/// Overrides are rendered into a deployment's hooks and hashed into its
/// identity, and a hook runs from whatever working directory Claude was
/// launched in. A relative override would therefore resolve somewhere else
/// entirely at hook time, so it is made absolute here, once, before any of that.
/// This is lexical: no `canonicalize`, no case folding, consistent with
/// refusing rather than normalizing elsewhere.
fn absolute_directory(path: PathBuf) -> Result<PathBuf, InitError> {
    std::path::absolute(&path).map_err(|source| InitError::AbsolutePath { path, source })
}

/// The platform config file used by installed deployments and `appa describe`.
pub fn installed_config_path() -> PathBuf {
    match installed_config_dir() {
        Ok(Some(directory)) => directory.join("appa.toml"),
        Ok(None) => PathBuf::from("appa.toml"),
        Err(error) => {
            tracing::warn!(%error, "falling back to the working directory for the config path");
            PathBuf::from("appa.toml")
        }
    }
}

pub(super) fn deployment_paths() -> Result<DeploymentPaths, InitError> {
    let home = user_home();
    let config_dir = installed_config_dir()?.ok_or(InitError::MissingHome)?;
    let data_dir = installed_data_dir()?.ok_or(InitError::MissingHome)?;
    let install_dir = if let Some(path) = env::var_os("APPA_INSTALL_DIR") {
        absolute_directory(PathBuf::from(path))?
    } else if cfg!(windows) {
        data_dir.join("bin")
    } else {
        home.as_ref().ok_or(InitError::MissingHome)?.join(".local/bin")
    };
    let claude_dir = match env::var_os("CLAUDE_CONFIG_DIR") {
        Some(path) => absolute_directory(PathBuf::from(path))?,
        None => home.ok_or(InitError::MissingHome)?.join(".claude"),
    };
    Ok(DeploymentPaths {
        install_dir,
        config_dir,
        data_dir,
        claude_dir,
    })
}

pub(super) fn user_home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).or({
        #[cfg(windows)]
        {
            env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(windows))]
        {
            None
        }
    })
}

fn installed_config_dir() -> Result<Option<PathBuf>, InitError> {
    if let Some(path) = env::var_os("APPA_CONFIG_DIR") {
        return absolute_directory(PathBuf::from(path)).map(Some);
    }
    #[cfg(target_os = "macos")]
    return Ok(env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa")));
    #[cfg(target_os = "windows")]
    return Ok(env::var_os("APPDATA").map(PathBuf::from).map(|path| path.join("appa")));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Ok(env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config/appa"))
        }))
}

fn installed_data_dir() -> Result<Option<PathBuf>, InitError> {
    if let Some(path) = env::var_os("APPA_DATA_DIR") {
        return absolute_directory(PathBuf::from(path)).map(Some);
    }
    #[cfg(target_os = "macos")]
    return Ok(env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa")));
    #[cfg(target_os = "windows")]
    return Ok(env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("appa")));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Ok(env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share/appa"))
        }))
}

pub(super) fn appa_filename() -> &'static str {
    if cfg!(windows) { "appa.exe" } else { "appa" }
}

/// The comparison operand on Windows: the fully resolved path, folded for the
/// case-insensitive filesystem.
///
/// Both sides are canonicalized, rather than trusting `Get-Process.Path` to
/// return one particular form. `fs::canonicalize` yields the Win32 final path,
/// whose extended-length prefix is stripped so the two sides can be compared as
/// written. This is equality of resolved final paths, not file-ID identity, so
/// two hard links to one file at different paths compare unequal: such a pair is
/// reported as two deployments rather than one.
#[cfg(windows)]
pub(super) fn windows_identity(path: &Path) -> Option<String> {
    let canonical = fs::canonicalize(path).ok()?;
    let text = canonical.to_str()?;
    Some(text.strip_prefix(r"\\?\").unwrap_or(text).to_lowercase())
}

/// Whether two paths name the same existing file, resolving symlinks. On Unix
/// `(dev, ino)` identity also names hard links to one file as that file.
pub(super) fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}
