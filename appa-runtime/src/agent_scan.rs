//! The subagent definitions in reach that declare `maxTurns`.
//!
//! Claude Code ends such a subagent at its turn cap with no SubagentStop, so
//! the runtime's return check never runs and the parent receives the
//! subagent's partial output unchecked. A prompt is refused while one exists.
//! The project and user agent directories and the installed plugins' agent
//! directories hold the definitions a session can start; agents passed on the
//! command line (`--agents`, `--plugin-dir`) are not scanned.

use std::cmp::Ordering;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a definition is read for the declaration. `maxTurns` is
/// frontmatter, at the top; the rest of a file under a project directory is
/// whatever size the project made it, and the hook must not follow it there.
const DEFINITION_HEAD_BYTES: u64 = 64 * 1024;

/// Why a definition stops the session from being protected. A frontmatter this
/// scan cannot read through counts as one that declares `maxTurns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Finding {
    MaxTurns,
    Unclosed,
    Unparseable,
}

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
        "[appa] this session cannot be protected while a subagent definition may declare maxTurns: \
         Claude Code ends that subagent without the return check and hands the parent its \
         partial output unchecked. Fix each definition:",
    );
    for (path, finding) in declaring {
        let remedy = match finding {
            Finding::MaxTurns => "declares maxTurns; remove it",
            Finding::Unclosed => "frontmatter is not closed by a `---` line within the first 64 KiB; shorten it",
            Finding::Unparseable => {
                "frontmatter has a line other than a plain `key: value`, an indented continuation, or a comment; \
                 rewrite it in block style"
            }
        };
        message.push_str(&format!("\n  {}: {remedy}", path.display()));
    }
    Some(message)
}

/// Every definition under the project, user, and installed plugin agent
/// directories that may declare `maxTurns`, in path order.
pub(crate) fn declaring_max_turns(project: Option<&Path>, claude: Option<&Path>) -> Vec<(PathBuf, Finding)> {
    let mut found = Vec::new();
    if let Some(project) = project {
        found.extend(definitions_in(&project.join(".claude/agents")));
    }
    if let Some(claude) = claude {
        found.extend(definitions_in(&claude.join("agents")));
        found.extend(plugin_definitions_under(&claude.join("plugins/cache")));
    }
    let mut declaring: Vec<_> = found
        .into_iter()
        .filter_map(|path| finding(&path).map(|finding| (path, finding)))
        .collect();
    declaring.sort();
    declaring
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

fn finding(path: &Path) -> Option<Finding> {
    let file = fs::File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(DEFINITION_HEAD_BYTES).read_to_end(&mut head).ok()?;
    frontmatter_finding(&String::from_utf8_lossy(&head))
}

/// Claude Code reads a definition's settings from the YAML frontmatter that
/// opens it; a file that does not open with `---` defines no agent. Inside the
/// frontmatter every line at the mapping's indentation must be a plain
/// `key: value`, so a flow mapping or an escaped key cannot carry `maxTurns`
/// past the scan.
fn frontmatter_finding(head: &str) -> Option<Finding> {
    let head = head.strip_prefix('\u{feff}').unwrap_or(head);
    let mut lines = head.lines();
    match lines.next().map(str::trim_end) {
        Some("---") => {}
        _ if head.trim_start().starts_with("---") => return Some(Finding::Unparseable),
        _ => return None,
    }
    let mut top = None;
    for line in lines {
        if line.trim_end() == "---" {
            return None;
        }
        let content = line.trim_start_matches(' ');
        if content.trim().is_empty() || content.starts_with('#') {
            continue;
        }
        let indent = line.len() - content.len();
        match indent.cmp(top.get_or_insert(indent)) {
            Ordering::Greater => {}
            Ordering::Less => return Some(Finding::Unparseable),
            Ordering::Equal => match key(content) {
                Some("maxTurns") => return Some(Finding::MaxTurns),
                Some(_) => {}
                None => return Some(Finding::Unparseable),
            },
        }
    }
    Some(Finding::Unclosed)
}

/// The key a block mapping line opens with: plain or in one layer of quotes
/// without escapes, then its colon, ending the line or followed by a space.
fn key(content: &str) -> Option<&str> {
    let (key, rest) = match content.as_bytes().first()? {
        quote @ (b'"' | b'\'') => content[1..].split_once(char::from(*quote))?,
        _ => content.split_at(content.find([':', ' ', '\t']).unwrap_or(content.len())),
    };
    let value = rest.trim_start_matches([' ', '\t']).strip_prefix(':')?;
    let plain = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    (plain && (value.is_empty() || value.starts_with([' ', '\t']))).then_some(key)
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
        write(&claude.join("agents/user-capped.md"), "---\nmaxTurns: 1\n---\n");
        write(
            &claude.join("plugins/cache/market/tool/1.0/agents/plugin-capped.md"),
            "---\ndescription: x\nmaxTurns: 2\n---\n",
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
        write(
            &claude.join("agents/quoted.md"),
            "---\nname: quoted\n\"maxTurns\": 4\n---\n",
        );
        write(
            &claude.join("agents/unclosed.md"),
            "---\nname: unclosed\n\"maxTurns: 4\n---\n",
        );

        write(&claude.join("agents/no-frontmatter.md"), "maxTurns: 1\n");
        assert_eq!(
            declaring_max_turns(Some(&project), Some(&claude)),
            vec![
                (claude.join("agents/indented.md"), Finding::MaxTurns),
                (claude.join("agents/quoted.md"), Finding::MaxTurns),
                (claude.join("agents/spaced.md"), Finding::MaxTurns),
                (claude.join("agents/unclosed.md"), Finding::Unparseable),
                (claude.join("agents/user-capped.md"), Finding::MaxTurns),
                (
                    claude.join("plugins/cache/market/tool/1.0/agents/plugin-capped.md"),
                    Finding::MaxTurns
                ),
                (project.join(".claude/agents/capped.md"), Finding::MaxTurns),
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
        write(&plugin.join("agents/capped.md"), "---\nmaxTurns: 2\n---\n");
        std::os::unix::fs::symlink(&claude, plugin.join("back")).expect("the cycle is linked");
        assert_eq!(
            declaring_max_turns(None, Some(&claude)),
            vec![(plugin.join("agents/capped.md"), Finding::MaxTurns)]
        );
    }

    /// Only the head of a definition is read: a declaration in the frontmatter
    /// of a large file is found, the body is not scanned, and a frontmatter
    /// that does not close within the head counts as declaring.
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
        let padding = "# padding\n".repeat(8_000);
        write(
            &project.join(".claude/agents/padded.md"),
            &format!("---\nname: padded\n{padding}maxTurns: 3\n---\n"),
        );
        assert_eq!(
            declaring_max_turns(Some(&project), None),
            vec![
                (project.join(".claude/agents/large.md"), Finding::MaxTurns),
                (project.join(".claude/agents/padded.md"), Finding::Unclosed),
            ]
        );
    }

    #[test]
    fn only_frontmatter_lines_the_scan_can_classify_pass() {
        let cases = [
            ("---\nname: plain\ndescription: x\n---\nmaxTurns: 3\n", None),
            ("---\nname: x\ntools:\n  - Read\n# note\n\n---\n", None),
            ("---\nname: x\ndescription: |\n  {maxTurns: 1}\n---\n", None),
            ("---\n{maxTurns: 1}\n---\n", Some(Finding::Unparseable)),
            ("---\nname: x\n\"max\\u0054urns\": 1\n---\n", Some(Finding::Unparseable)),
            ("---\n  name: x\nmaxTurns: 1\n---\n", Some(Finding::Unparseable)),
            ("\n---\nmaxTurns: 1\n---\n", Some(Finding::Unparseable)),
            ("---\nname: x\n", Some(Finding::Unclosed)),
        ];
        for (definition, expected) in cases {
            assert_eq!(frontmatter_finding(definition), expected, "{definition:?}");
        }
    }
}
