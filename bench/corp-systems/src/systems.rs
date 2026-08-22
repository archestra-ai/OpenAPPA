//! The mock corporate systems, as plain folders on disk.
//!
//! Each [`System`] is a subdirectory under a data root holding `.md`/`.txt`
//! files. The three verbs — [`search`], [`read`], [`create`] — plus the
//! [`send_email`] sink, [`execute_wire`] action, and
//! [`share_legal_packet`] composite are the whole
//! behaviour; the MCP server in [`crate::server`] is a thin wrapper that exposes
//! them per system. Keeping the semantics here (once) means the 17 tool methods
//! stay trivial delegators.
//!
//! All file names that reach the filesystem come from the model (untrusted),
//! so every entry point runs them through [`validate_file_name`] before
//! touching disk.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One mock internal system, backed by a subdirectory of the data root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum System {
    Hr,
    Finance,
    TaskTracker,
    PublicForum,
    Vendor,
    Email,
    Wire,
}

/// A `--systems` enable list could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum SystemListError {
    #[error("unknown system {0:?}; valid systems: hr, finance, task_tracker, public_forum, vendor, email, wire")]
    Unknown(String),
    #[error("empty system list: enable at least one of hr, finance, task_tracker, public_forum, vendor, email, wire")]
    Empty,
}

impl System {
    pub const ALL: [System; 7] = [
        System::Hr,
        System::Finance,
        System::TaskTracker,
        System::PublicForum,
        System::Vendor,
        System::Email,
        System::Wire,
    ];

    /// The subdirectory name under the data root.
    pub fn dir_name(self) -> &'static str {
        match self {
            System::Hr => "hr",
            System::Finance => "finance",
            System::TaskTracker => "task_tracker",
            System::PublicForum => "public_forum",
            System::Vendor => "vendor",
            System::Email => "email",
            System::Wire => "wire",
        }
    }

    /// Parse one system name (the `dir_name` vocabulary).
    pub fn parse(name: &str) -> Result<System, SystemListError> {
        System::ALL
            .into_iter()
            .find(|s| s.dir_name() == name)
            .ok_or_else(|| SystemListError::Unknown(name.to_string()))
    }

    /// Parse a comma-separated enable list, e.g. `"hr, public_forum,email"`.
    /// Whitespace around tokens is trimmed and duplicates are idempotent; an
    /// empty token or a blank list is an error — a server with no systems is a
    /// misconfiguration, not a degenerate success.
    pub fn parse_list(list: &str) -> Result<BTreeSet<System>, SystemListError> {
        if list.trim().is_empty() {
            return Err(SystemListError::Empty);
        }
        let mut enabled = BTreeSet::new();
        for token in list.split(',') {
            enabled.insert(System::parse(token.trim())?);
        }
        Ok(enabled)
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

/// Receipt for one outbound email created by a server-side composite.
#[derive(Debug, PartialEq, Eq)]
pub struct EmailReceipt {
    pub recipient: String,
    pub subject: String,
    pub archive_file: String,
}

impl fmt::Display for EmailReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "email sent to {} (subject: {:?}); archived as {}",
            self.recipient, self.subject, self.archive_file
        )
    }
}

/// Result of reading and sharing one finance legal packet.
#[derive(Debug, PartialEq, Eq)]
pub struct SharedLegalPacket {
    pub receipt: EmailReceipt,
    pub packet_contents: String,
}

/// Tool-controlled evidence that an actual transfer action was dispatched.
/// Generic file tools cannot write this sink.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WireReceipt {
    pub request_file: String,
    pub amount_usd: u64,
    pub beneficiary_account: String,
    pub status: String,
}

impl fmt::Display for SharedLegalPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{}", self.receipt, self.packet_contents)
    }
}

/// A legal packet could not be read or emailed.
#[derive(Debug, thiserror::Error)]
pub enum ShareLegalPacketError {
    #[error("reading legal packet: {0}")]
    Read(#[from] ReadError),
    #[error("sending legal packet: {0}")]
    Send(#[source] io::Error),
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
    let mut out = match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(out) => out,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            return Err(CreateError::Exists {
                system,
                name: file.to_string(),
            });
        }
        Err(source) => {
            return Err(CreateError::Io {
                system,
                name: file.to_string(),
                source,
            });
        }
    };
    out.write_all(content.as_bytes()).map_err(|source| CreateError::Io {
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
    // Second-resolution stamps collide for rapid same-subject sends, and an
    // overwrite would silently swallow an email — corrupting anything that
    // scores the sink by its files. A process-wide sequence plus `create_new`
    // (retried on a cross-process collision) makes every send its own file.
    static EMAIL_SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = System::Email.dir(root);
    fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let contents = format!("To: {to}\nSubject: {subject}\n\n{body}");
    loop {
        let seq = EMAIL_SEQ.fetch_add(1, Ordering::Relaxed);
        let file = format!("{stamp}-{seq:03}-{}.md", slug(subject));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&file))
        {
            Ok(mut out) => {
                out.write_all(contents.as_bytes())?;
                return Ok(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Execute one wire transfer and persist a structured, tool-controlled
/// receipt. The dedicated `wire/` sink is deliberately outside the generic
/// corporate file systems, so prose such as "pending" cannot impersonate an
/// executed transfer.
pub fn execute_wire(root: &Path, request_file: &str, amount_usd: u64, beneficiary_account: &str) -> io::Result<String> {
    validate_file_name(request_file).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if amount_usd == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "wire amount must be positive",
        ));
    }
    if beneficiary_account.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "beneficiary account must not be empty",
        ));
    }

    let receipt = WireReceipt {
        request_file: request_file.to_string(),
        amount_usd,
        beneficiary_account: beneficiary_account.to_string(),
        status: "executed".to_string(),
    };
    let mut body = serde_json::to_vec_pretty(&receipt).map_err(io::Error::other)?;
    body.push(b'\n');

    let dir = root.join("wire");
    fs::create_dir_all(&dir)?;
    let file = format!("{request_file}.json");
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(&file))?;
    out.write_all(&body)?;
    Ok(file)
}

/// Read a finance file and email its exact contents as one server-side action.
/// The read completes before the email sink is touched.
pub fn share_legal_packet(
    corpus_root: &Path,
    sink_root: &Path,
    file: &str,
    to: &str,
) -> Result<SharedLegalPacket, ShareLegalPacketError> {
    let packet_contents = read(corpus_root, System::Finance, file)?;
    let subject = format!("Legal packet: {file}");
    let archive_file = send_email(sink_root, to, &subject, &packet_contents).map_err(ShareLegalPacketError::Send)?;
    Ok(SharedLegalPacket {
        receipt: EmailReceipt {
            recipient: to.to_string(),
            subject,
            archive_file,
        },
        packet_contents,
    })
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

    #[test]
    fn vendor_uses_generic_file_operations() {
        let root = scratch("vendor");
        let content = "# Acme Cloud\n\nStatus: approved\n";

        create(&root, System::Vendor, "acme.md", content).unwrap();

        assert_eq!(read(&root, System::Vendor, "acme.md").unwrap(), content);
        let hits = search(&root, System::Vendor, "approved").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "acme.md");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shares_exact_legal_packet_after_reading_it() {
        let root = scratch("share-success");
        let packet = "# Legal packet\n\nCounterparty: Acme\n";
        fs::create_dir_all(root.join("corpus/finance")).unwrap();
        fs::write(root.join("corpus/finance/acme.md"), packet).unwrap();

        let shared =
            share_legal_packet(&root.join("corpus"), &root.join("sink"), "acme.md", "legal@example.com").unwrap();

        assert_eq!(shared.packet_contents, packet);
        assert_eq!(shared.receipt.recipient, "legal@example.com");
        assert_eq!(shared.receipt.subject, "Legal packet: acme.md");
        let archived = fs::read_to_string(root.join("sink/email").join(shared.receipt.archive_file)).unwrap();
        assert_eq!(
            archived,
            format!("To: legal@example.com\nSubject: Legal packet: acme.md\n\n{packet}")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_legal_packet_sends_no_email() {
        let root = scratch("share-missing");

        let error = share_legal_packet(
            &root.join("corpus"),
            &root.join("sink"),
            "missing.md",
            "legal@example.com",
        )
        .unwrap_err();

        assert!(matches!(error, ShareLegalPacketError::Read(ReadError::NotFound { .. })));
        assert!(!root.join("sink/email").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corp-systems-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
