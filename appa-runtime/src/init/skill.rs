//! The appa-guide skill, written to the user's skills directory from the bytes
//! compiled into this binary.
//!
//! Claude Code loads only SKILL.md when a slash command starts, and reading a
//! reference beside it would itself be a gated `Read` call, so the Claude Code
//! reference is inlined after the router. The policy-review guide the skill
//! consults for syntax is written beside it, so the skill reads the guide of
//! the version it runs with. The canonical package stays decomposed for hosts
//! such as kagent that load their own reference through their native file tool.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Compensation, InitError, Undo, file_before, write_state};

pub(super) const TEXT: &str = concat!(
    include_str!("../../../integrations/appa-guide/SKILL.md"),
    "\n\n",
    include_str!("../../../integrations/appa-guide/references/claude-code.md"),
);

const CONTRACTS: &str = include_str!("../../../website/content/docs/contracts.md");

/// Every version an install wrote opens with this frontmatter; a file under the
/// skill's name that does not is someone else's.
const OWNED_PREFIX: &str = "---\nname: appa-guide\n";

pub(super) fn path(claude_dir: &Path) -> PathBuf {
    claude_dir.join("skills/appa-guide/SKILL.md")
}

fn contracts_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join("skills/appa-guide/references/contracts.md")
}

enum Present {
    Absent,
    Current,
    Earlier,
}

/// The skill file decides ownership of the whole skill directory.
fn current(claude_dir: &Path) -> Result<Present, InitError> {
    let path = path(claude_dir);
    match file_before(&path)? {
        None => Ok(Present::Absent),
        Some(bytes) if bytes == TEXT.as_bytes() => Ok(Present::Current),
        Some(bytes) if bytes.starts_with(OWNED_PREFIX.as_bytes()) => Ok(Present::Earlier),
        Some(_) => Err(InitError::SkillConflict { path }),
    }
}

/// The skill file is absent or an install's, or the profile is refused before
/// anything is written to it.
pub(super) fn verify(claude_dir: &Path) -> Result<(), InitError> {
    current(claude_dir).map(drop)
}

pub(super) fn install(claude_dir: &Path, compensation: &mut Compensation) -> Result<(), InitError> {
    match current(claude_dir)? {
        Present::Current => {}
        Present::Absent | Present::Earlier => write(&path(claude_dir), TEXT, compensation)?,
    }
    let contracts = contracts_path(claude_dir);
    if file_before(&contracts)?.as_deref() != Some(CONTRACTS.as_bytes()) {
        write(&contracts, CONTRACTS, compensation)?;
    }
    Ok(())
}

fn write(path: &Path, text: &str, compensation: &mut Compensation) -> Result<(), InitError> {
    compensation.record(Undo::File {
        path: path.to_path_buf(),
        before: file_before(path)?,
    });
    write_state(path, text.as_bytes())
}

/// Remove an install's skill files, and the directories they alone filled.
pub(super) fn remove(claude_dir: &Path) -> Result<(), InitError> {
    match current(claude_dir)? {
        Present::Absent => return Ok(()),
        Present::Current | Present::Earlier => {}
    }
    let contracts = contracts_path(claude_dir);
    let skill = path(claude_dir);
    for path in [&contracts, &skill] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(InitError::WriteFile {
                    path: path.clone(),
                    source,
                });
            }
        }
    }
    for directory in [contracts.parent(), skill.parent()].into_iter().flatten() {
        if let Err(error) = fs::remove_dir(directory) {
            tracing::debug!(path = %directory.display(), %error, "leaving the skill directory");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiled_skill_is_the_router_with_the_claude_code_reference_inlined() {
        assert!(TEXT.starts_with(OWNED_PREFIX));
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("integrations/appa-guide");
        let router = fs::read_to_string(root.join("SKILL.md")).unwrap();
        let reference = fs::read_to_string(root.join("references/claude-code.md")).unwrap();
        assert_eq!(TEXT, format!("{router}\n\n{reference}"));
    }

    #[test]
    fn only_an_installs_skill_files_are_replaced_or_removed() {
        let root = tempfile::tempdir().unwrap();
        let claude_dir = root.path().join("claude");
        let path = path(&claude_dir);
        let contracts = contracts_path(&claude_dir);

        let mut compensation = Compensation::default();
        install(&claude_dir, &mut compensation).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEXT);
        assert_eq!(fs::read_to_string(&contracts).unwrap(), CONTRACTS);
        assert_eq!(compensation.done.len(), 2);
        install(&claude_dir, &mut compensation).unwrap();
        assert_eq!(compensation.done.len(), 2, "current files are not rewritten");

        let earlier = format!("{OWNED_PREFIX}description: an earlier install\n---\n");
        fs::write(&path, &earlier).unwrap();
        fs::remove_file(&contracts).unwrap();
        install(&claude_dir, &mut compensation).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEXT);
        assert_eq!(fs::read_to_string(&contracts).unwrap(), CONTRACTS);
        assert_eq!(compensation.done.len(), 4);

        fs::write(&path, "---\nname: my-guide\n---\n").unwrap();
        assert!(matches!(verify(&claude_dir), Err(InitError::SkillConflict { .. })));
        assert!(install(&claude_dir, &mut Compensation::default()).is_err());
        assert!(remove(&claude_dir).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "---\nname: my-guide\n---\n");
        assert_eq!(fs::read_to_string(&contracts).unwrap(), CONTRACTS);

        fs::write(&path, &earlier).unwrap();
        remove(&claude_dir).unwrap();
        assert!(!path.exists());
        assert!(!contracts.exists());
        assert!(
            !path.parent().unwrap().exists(),
            "the emptied directories go with the files"
        );
        remove(&claude_dir).unwrap();
        verify(&claude_dir).unwrap();

        // An earlier install that wrote no guide is removed all the same.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &earlier).unwrap();
        remove(&claude_dir).unwrap();
        assert!(!path.parent().unwrap().exists());
    }
}
