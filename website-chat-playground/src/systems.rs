//! The playground's world: a CRM, a GitHub issue tracker, outbound email,
//! finance, and meeting recordings — folders of records on disk, markdown for
//! CRM notes and JSON for the rest, one file per record either way.
//!
//! Each system is a directory under the session's data root and contributes a
//! fixed, named set of tools — the names are the product's, not a naming
//! convention, so routing is an explicit match rather than a prefix rule.
//! Reads are deliberately `list_*`, returning the whole book: the world is a
//! dozen records, and keyword search at that size only teaches a model to
//! guess keywords and give up. The systems are chosen so the interesting
//! flows are obvious: customer records, invoices and recordings are
//! confidential, issues are written by anyone, filing an issue or sending an
//! email publishes, and moving money wants a human.
//!
//! This is the playground's own world. The bench's corp systems stay frozen
//! for measurement; nothing here is shared with them.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum System {
    Crm,
    Github,
    Email,
    Finance,
    Meetings,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown system {0:?}")]
pub struct UnknownSystem(pub String);

impl System {
    pub const ALL: [System; 5] = [
        System::Crm,
        System::Github,
        System::Email,
        System::Finance,
        System::Meetings,
    ];

    pub fn id(self) -> &'static str {
        match self {
            System::Crm => "crm",
            System::Github => "github",
            System::Email => "email",
            System::Finance => "finance",
            System::Meetings => "meetings",
        }
    }

    /// The tools this system contributes, in the order the UI lists them.
    /// Meetings is retrieval-only by design: recordings are made elsewhere.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            System::Crm => &["list_customers", "create_customer_data"],
            System::Github => &["list_issues", "create_issue"],
            System::Email => &["send_email"],
            System::Finance => &["list_invoices", "make_transfer"],
            System::Meetings => &["list_recordings"],
        }
    }

    pub fn parse(name: &str) -> Result<System, UnknownSystem> {
        System::ALL
            .into_iter()
            .find(|system| system.id() == name)
            .ok_or_else(|| UnknownSystem(name.to_string()))
    }

    fn dir(self, root: &Path) -> PathBuf {
        root.join(self.id())
    }

    fn ext(self) -> &'static str {
        match self {
            System::Crm => "md",
            System::Github | System::Email | System::Finance | System::Meetings => "json",
        }
    }
}

impl fmt::Display for System {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    List,
    Create,
}

/// Which system and verb a tool name denotes. Explicit, because the names are
/// the ones a real CRM and issue tracker would use, not a generated pattern.
pub fn route(tool: &str) -> Option<(System, Verb)> {
    match tool {
        "list_customers" => Some((System::Crm, Verb::List)),
        "create_customer_data" => Some((System::Crm, Verb::Create)),
        "list_issues" => Some((System::Github, Verb::List)),
        "create_issue" => Some((System::Github, Verb::Create)),
        "send_email" => Some((System::Email, Verb::Create)),
        "list_invoices" => Some((System::Finance, Verb::List)),
        "make_transfer" => Some((System::Finance, Verb::Create)),
        "list_recordings" => Some((System::Meetings, Verb::List)),
        _ => None,
    }
}

// ---- file-level semantics ---------------------------------------------------

/// A record name that would escape its directory. Every name reaching the
/// filesystem comes from the model, so it is validated before use.
#[derive(Debug, thiserror::Error)]
#[error("invalid name {name:?}: {reason}")]
pub struct NameError {
    pub name: String,
    pub reason: &'static str,
}

pub fn validate_name(name: &str) -> Result<(), NameError> {
    let bad = |reason| {
        Err(NameError {
            name: name.to_string(),
            reason,
        })
    };
    if name.is_empty() {
        return bad("empty");
    }
    if name.starts_with('.') {
        return bad("starts with a dot");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return bad("path separators and .. are not allowed");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return bad("only letters, digits, dashes and underscores");
    }
    Ok(())
}

/// Every record in a system, `(name, body)`, name-sorted. A missing directory
/// is an empty system, not an error.
pub fn list(root: &Path, system: System) -> std::io::Result<Vec<(String, String)>> {
    let dir = system.dir(root);
    let mut out = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(system.ext()) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        out.push((stem.to_string(), fs::read_to_string(&path)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("a record named {name:?} already exists in {system}")]
    Exists { system: System, name: String },
    #[error("writing {name:?} to {system}: {source}")]
    Io {
        system: System,
        name: String,
        source: std::io::Error,
    },
}

pub fn create(root: &Path, system: System, name: &str, body: &str) -> Result<(), CreateError> {
    validate_name(name)?;
    let dir = system.dir(root);
    fs::create_dir_all(&dir).map_err(|source| CreateError::Io {
        system,
        name: name.to_string(),
        source,
    })?;
    let path = dir.join(format!("{name}.{}", system.ext()));
    if path.exists() {
        return Err(CreateError::Exists {
            system,
            name: name.to_string(),
        });
    }
    fs::write(&path, body).map_err(|source| CreateError::Io {
        system,
        name: name.to_string(),
        source,
    })
}

/// The next free `<prefix>-N` number in a system, at least `floor + 1` — so
/// the writing tools (`create_issue`, `send_email`, `make_transfer`) name
/// their own records.
pub fn next_number(root: &Path, system: System, prefix: &str, floor: u32) -> std::io::Result<u32> {
    let lead = format!("{prefix}-");
    let highest = list(root, system)?
        .into_iter()
        .filter_map(|(name, _)| name.strip_prefix(&lead).and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(floor);
    Ok(highest + 1)
}

// ---- tool argument shapes ---------------------------------------------------

#[derive(Deserialize)]
pub struct CreateCustomerArgs {
    pub customer: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct CreateIssueArgs {
    pub title: String,
    pub body: String,
}

#[derive(Deserialize)]
pub struct SendEmailArgs {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Deserialize)]
pub struct TransferArgs {
    pub amount_usd: f64,
    pub to_account: String,
    #[serde(default)]
    pub memo: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("appa-demo-systems-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("crm")).unwrap();
        fs::create_dir_all(dir.join("github")).unwrap();
        dir
    }

    #[test]
    fn every_tool_routes_and_every_system_owns_its_tools() {
        for system in System::ALL {
            for tool in system.tools() {
                assert_eq!(route(tool).map(|(s, _)| s), Some(system), "{tool}");
            }
        }
        assert!(route("read_hr").is_none(), "the bench's names are not ours");
    }

    #[test]
    fn names_from_the_model_cannot_escape_the_directory() {
        for bad in ["../secrets", "a/b", ".hidden", "", "with space"] {
            assert!(validate_name(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(validate_name("acme-corp").is_ok());
    }

    #[test]
    fn list_returns_every_record_sorted_and_skips_foreign_extensions() {
        let root = scratch("list");
        fs::write(root.join("crm/globex.md"), "# Globex\n").unwrap();
        fs::write(root.join("crm/acme-corp.md"), "# Acme\n").unwrap();
        fs::write(root.join("crm/notes.json"), "{}").unwrap(); // wrong format for crm
        let records = list(&root, System::Crm).unwrap();
        assert_eq!(
            records,
            vec![
                ("acme-corp".to_string(), "# Acme\n".to_string()),
                ("globex".to_string(), "# Globex\n".to_string()),
            ]
        );
        assert!(
            list(&root, System::Meetings).unwrap().is_empty(),
            "missing dir is empty"
        );
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let root = scratch("create");
        create(&root, System::Github, "issue-101", "first").unwrap();
        assert!(matches!(
            create(&root, System::Github, "issue-101", "second"),
            Err(CreateError::Exists { .. })
        ));
        assert_eq!(fs::read_to_string(root.join("github/issue-101.json")).unwrap(), "first");
    }

    #[test]
    fn record_numbers_continue_from_the_highest() {
        let root = scratch("numbers");
        assert_eq!(next_number(&root, System::Github, "issue", 100).unwrap(), 101);
        create(&root, System::Github, "issue-101", "x").unwrap();
        create(&root, System::Github, "issue-107", "x").unwrap();
        assert_eq!(next_number(&root, System::Github, "issue", 100).unwrap(), 108);
        assert_eq!(next_number(&root, System::Finance, "transfer", 0).unwrap(), 1);
    }
}
