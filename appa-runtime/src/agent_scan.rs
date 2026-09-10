//! The subagent definitions in reach that declare `maxTurns`.
//!
//! Claude Code ends such a subagent at its turn cap with no SubagentStop, so
//! the runtime's return check never runs and the parent receives the
//! subagent's partial output unchecked. A prompt is refused while one exists.
//! The project and user agent directories and the installed plugins' agent
//! directories hold the definitions a session can start; agents passed on the
//! command line (`--agents`, `--plugin-dir`) are not scanned.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a definition is read for the declaration. `maxTurns` is
/// frontmatter, at the top; the rest of a file under a project directory is
/// whatever size the project made it, and the hook must not follow it there.
const DEFINITION_HEAD_BYTES: u64 = 64 * 1024;

/// The refusal a prompt gets while a definition in reach declares `maxTurns`,
/// naming each file, or `None` when the session can be protected.
pub(crate) fn refusal() -> Option<String> {
    let project = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let claude = crate::init::paths::claude_config_dir().ok().flatten();
    let declaring = declaring_max_turns(project.as_deref(), claude.as_deref());
    if declaring.is_empty() {
        return None;
    }
    let mut message = String::from(
        "[appa] this session cannot be protected while a subagent definition declares maxTurns: \
         Claude Code ends that subagent without the return check and hands the parent its \
         partial output unchecked. Remove maxTurns from:",
    );
    for path in declaring {
        message.push_str("\n  ");
        message.push_str(&path.display().to_string());
    }
    Some(message)
}

/// Every definition under the project, user, and installed plugin agent
/// directories that declares `maxTurns`, in path order.
pub(crate) fn declaring_max_turns(project: Option<&Path>, claude: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(project) = project {
        found.extend(definitions_in(&project.join(".claude/agents")));
    }
    if let Some(claude) = claude {
        found.extend(definitions_in(&claude.join("agents")));
        found.extend(plugin_definitions_under(&claude.join("plugins/cache")));
    }
    found.retain(|path| declares_max_turns(path));
    found.sort();
    found
}

/// The `.md` files directly inside one agents directory.
fn definitions_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|extension| extension == "md"))
        .collect()
}

/// Every `agents/*.md` at any depth under the plugin cache. A symlinked
/// directory is not entered: the cache holds unpacked plugins, and a link back
/// up the tree would otherwise keep the hook walking.
fn plugin_definitions_under(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![cache.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            } else if directory.file_name().is_some_and(|name| name == "agents")
                && path.extension().is_some_and(|extension| extension == "md")
            {
                found.push(path);
            }
        }
    }
    found
}

fn declares_max_turns(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(DEFINITION_HEAD_BYTES).read_to_end(&mut head).is_err() {
        return false;
    }
    String::from_utf8_lossy(&head).lines().any(declares)
}

/// The line carries the key as YAML reads it: `maxTurns` first on the line,
/// indented or not, then its colon with or without a space before it.
fn declares(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("maxTurns")
        .is_some_and(|rest| rest.trim_start().starts_with(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directory is created");
        fs::write(path, text).expect("the definition is written");
    }

    #[test]
    fn definitions_declaring_max_turns_are_found_in_every_directory_a_session_can_start_them_from() {
        let root = tempfile::tempdir().expect("temporary directory");
        let project = root.path().join("project");
        let claude = root.path().join("claude");
        write(
            &project.join(".claude/agents/capped.md"),
            "---\nname: capped\nmaxTurns: 3\n---\n",
        );
        write(&project.join(".claude/agents/free.md"), "---\nname: free\n---\n");
        write(&project.join(".claude/agents/notes.txt"), "maxTurns: 3\n");
        write(&claude.join("agents/user-capped.md"), "maxTurns: 1\n");
        write(
            &claude.join("plugins/cache/market/tool/1.0/agents/plugin-capped.md"),
            "description: x\nmaxTurns: 2\n",
        );
        write(
            &claude.join("plugins/cache/market/tool/1.0/skills/not-an-agent.md"),
            "maxTurns: 2\n",
        );
        write(&claude.join("agents/mentions.md"), "text about maxTurns: in prose\n");
        write(
            &claude.join("agents/indented.md"),
            "---\n  name: indented\n  maxTurns: 4\n---\n",
        );
        write(
            &claude.join("agents/spaced.md"),
            "---\nname: spaced\nmaxTurns : 4\n---\n",
        );

        assert_eq!(
            declaring_max_turns(Some(&project), Some(&claude)),
            vec![
                claude.join("agents/indented.md"),
                claude.join("agents/spaced.md"),
                claude.join("agents/user-capped.md"),
                claude.join("plugins/cache/market/tool/1.0/agents/plugin-capped.md"),
                project.join(".claude/agents/capped.md"),
            ]
        );
        assert!(declaring_max_turns(None, None).is_empty());
        assert!(declaring_max_turns(Some(&root.path().join("absent")), Some(&root.path().join("absent"))).is_empty());
    }

    /// A link in the plugin cache that points back up the tree is not entered,
    /// so the walk ends; the definitions reachable without it are still found.
    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_in_the_plugin_cache_does_not_keep_the_walk_going() {
        let root = tempfile::tempdir().expect("temporary directory");
        let claude = root.path().join("claude");
        let plugin = claude.join("plugins/cache/market/tool/1.0");
        write(&plugin.join("agents/capped.md"), "maxTurns: 2\n");
        std::os::unix::fs::symlink(&claude, plugin.join("back")).expect("the cycle is linked");
        assert_eq!(
            declaring_max_turns(None, Some(&claude)),
            vec![plugin.join("agents/capped.md")]
        );
    }

    /// Only the head of a definition is read: a declaration in the frontmatter
    /// of a large file is found, and the size of what follows it is the
    /// project's business, not the hook's.
    #[test]
    fn a_definition_is_read_only_as_far_as_its_frontmatter_reaches() {
        let root = tempfile::tempdir().expect("temporary directory");
        let project = root.path().join("project");
        let prose = "a line of prose that declares nothing\n".repeat(20_000);
        write(
            &project.join(".claude/agents/large.md"),
            &format!("---\nname: large\nmaxTurns: 3\n---\n{prose}"),
        );
        write(
            &project.join(".claude/agents/buried.md"),
            &format!("---\nname: buried\n---\n{prose}maxTurns: 3\n"),
        );
        assert_eq!(
            declaring_max_turns(Some(&project), None),
            vec![project.join(".claude/agents/large.md")]
        );
    }
}
