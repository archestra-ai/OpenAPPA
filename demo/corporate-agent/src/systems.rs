//! The mock corporate systems, as plain folders on disk.
//!
//! Each [`System`] is a subdirectory under a data root holding `.md`/`.txt`
//! files. The three verbs — [`search`], [`read`], [`create`] — plus the
//! [`send_email`] sink are the whole behaviour; the MCP server in
//! [`crate::server`] is a thin wrapper that exposes them per system. Keeping
//! the semantics here (once) means the 13 tool methods stay trivial delegators.
//!
//! All file names that reach the filesystem come from the model (untrusted),
//! so every entry point runs them through [`validate_file_name`] before
//! touching disk.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One mock internal system, backed by a subdirectory of the data root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Hr,
    Finance,
    TaskTracker,
    PublicForum,
    Email,
}

impl System {
    /// The subdirectory name under the data root.
    pub fn dir_name(self) -> &'static str {
        match self {
            System::Hr => "hr",
            System::Finance => "finance",
            System::TaskTracker => "task_tracker",
            System::PublicForum => "public_forum",
            System::Email => "email",
        }
    }

    fn dir(self, root: &Path) -> PathBuf {
        root.join(self.dir_name())
    }
}

impl fmt::Display for System {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

/// A single search hit: the file it matched and the first matching line.
pub struct Hit {
    pub file: String,
    pub snippet: String,
}

/// A file name supplied by the model was unsafe.
#[derive(Debug, thiserror::Error)]
#[error("invalid file name {name:?}: {reason}")]
pub struct NameError {
    name: String,
    reason: &'static str,
}

/// Reading a named file failed.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("no file named {name:?} in the {system} system; available: {available}")]
    NotFound {
        system: System,
        name: String,
        available: String,
    },
    #[error("reading {name:?} from {system}: {source}")]
    Io {
        system: System,
        name: String,
        #[source]
        source: io::Error,
    },
}

/// Creating a named file failed.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("a file named {name:?} already exists in the {system} system")]
    Exists { system: System, name: String },
    #[error("writing {name:?} to {system}: {source}")]
    Io {
        system: System,
        name: String,
        #[source]
        source: io::Error,
    },
}

/// Reject anything that could escape the system's directory or hide as a
/// dotfile. Model-supplied input — validated at this single choke point.
pub fn validate_file_name(name: &str) -> Result<(), NameError> {
    let err = |reason| {
        Err(NameError {
            name: name.to_string(),
            reason,
        })
    };
    if name.trim().is_empty() {
        return err("empty");
    }
    if name.contains('/') || name.contains('\\') {
        return err("contains a path separator");
    }
    if name.contains("..") {
        return err("contains '..'");
    }
    if name.starts_with('.') {
        return err("starts with '.'");
    }
    if Path::new(name).is_absolute() {
        return err("is an absolute path");
    }
    Ok(())
}

/// Read every `.md`/`.txt` file in `dir`, sorted by name, as `(name, body)`.
fn list_files(dir: &Path) -> io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A system whose folder does not exist yet reads as empty rather than erroring.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let is_text = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "md" || e == "txt");
        if !is_text || !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let body = fs::read_to_string(&path).unwrap_or_default();
        out.push((name, body));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn available_names(dir: &Path) -> String {
    match list_files(dir) {
        Ok(files) if !files.is_empty() => files.into_iter().map(|(n, _)| n).collect::<Vec<_>>().join(", "),
        _ => "(none)".to_string(),
    }
}

/// Case-insensitive substring search over file names and contents. Returns a
/// hit per matching file, its snippet being the first matching line (trimmed).
pub fn search(root: &Path, system: System, query: &str) -> io::Result<Vec<Hit>> {
    let needle = query.trim().to_lowercase();
    let dir = system.dir(root);
    let mut hits = Vec::new();
    for (name, body) in list_files(&dir)? {
        if needle.is_empty() {
            // An empty query lists everything (first line as the snippet).
            hits.push(Hit {
                snippet: first_line(&body),
                file: name,
            });
            continue;
        }
        if name.to_lowercase().contains(&needle) {
            hits.push(Hit {
                snippet: first_line(&body),
                file: name,
            });
            continue;
        }
        if let Some(line) = body.lines().find(|l| l.to_lowercase().contains(&needle)) {
            hits.push(Hit {
                file: name,
                snippet: truncate(line.trim(), 200),
            });
        }
    }
    Ok(hits)
}

/// Full contents of a named file. Not-found lists the available files so the
/// model can correct itself.
pub fn read(root: &Path, system: System, file: &str) -> Result<String, ReadError> {
    validate_file_name(file)?;
    let dir = system.dir(root);
    match fs::read_to_string(dir.join(file)) {
        Ok(body) => Ok(body),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ReadError::NotFound {
            system,
            name: file.to_string(),
            available: available_names(&dir),
        }),
        Err(source) => Err(ReadError::Io {
            system,
            name: file.to_string(),
            source,
        }),
    }
}

/// Write a new file. Refuses to overwrite an existing one.
pub fn create(root: &Path, system: System, file: &str, content: &str) -> Result<(), CreateError> {
    validate_file_name(file)?;
    let dir = system.dir(root);
    fs::create_dir_all(&dir).map_err(|source| CreateError::Io {
        system,
        name: file.to_string(),
        source,
    })?;
    let path = dir.join(file);
    if path.exists() {
        return Err(CreateError::Exists {
            system,
            name: file.to_string(),
        });
    }
    fs::write(&path, content).map_err(|source| CreateError::Io {
        system,
        name: file.to_string(),
        source,
    })
}

/// The mocked outbound email sink: writes the message as a file into the
/// `email/` folder and returns the saved file name. There is no `read`/`search`
/// counterpart — the folder is purely an observable side-effect the injection
/// demo inspects.
pub fn send_email(root: &Path, to: &str, subject: &str, body: &str) -> io::Result<String> {
    let dir = System::Email.dir(root);
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file = format!("{stamp}-{}.md", slug(subject));
    let contents = format!("To: {to}\nSubject: {subject}\n\n{body}\n");
    fs::write(dir.join(&file), contents)?;
    Ok(file)
}

fn first_line(body: &str) -> String {
    truncate(body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim(), 200)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A filesystem-safe slug for an email subject (used only for the file name).
fn slug(subject: &str) -> String {
    let mut s: String = subject
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    let s: String = s.chars().take(40).collect();
    if s.is_empty() { "message".to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_dotfiles() {
        assert!(validate_file_name("../secrets.md").is_err());
        assert!(validate_file_name("a/b.md").is_err());
        assert!(validate_file_name("a\\b.md").is_err());
        assert!(validate_file_name(".hidden").is_err());
        assert!(validate_file_name("   ").is_err());
        assert!(validate_file_name("ok.md").is_ok());
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("Q2 Report!!"), "q2-report");
        assert_eq!(slug("   "), "message");
    }
}
