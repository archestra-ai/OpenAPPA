//! The plugin's own configuration: where its store lives, the labels paths start with, the
//! tools it serves, and the sandbox SafeBash runs in. Initial path classification lives here
//! and nowhere else: the policy routes file tools to the plugin and never names a path.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::label::Label;

/// The configuration a deployment runs without a file of its own.
const DEFAULT: &str = include_str!("../file-tracker.toml");

#[derive(Debug)]
pub struct Config {
    pub store: PathBuf,
    pub paths: Vec<PathRule>,
    pub tools: Vec<ToolRole>,
    pub sandbox: Sandbox,
    /// How long an annotation waits for a reserved file to settle before it refuses.
    pub settle_wait: std::time::Duration,
    /// How long an annotated call that no record ever mentions keeps its files reserved.
    pub reservation_timeout: std::time::Duration,
}

/// A path pattern and what it says about every file it matches. `*` matches any run of
/// characters, `/` included. A pattern with a `/` is matched against the absolute, resolved
/// path; one without is matched against the file's name, so `*secrets*` names
/// `db-secrets.yaml` and not every file in a repository called `k8s-secrets`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRule {
    pub pattern: String,
    /// The label such a file has before any recorded write, and keeps under every later one.
    #[serde(default)]
    pub trust: Option<String>,
    #[serde(default)]
    pub audience: Option<Vec<String>>,
    /// The trust a call that writes such a file requires.
    #[serde(default)]
    pub write_trust: Option<String>,
    /// The attention marks a call that writes such a file requires.
    #[serde(default)]
    pub write_attention: Vec<String>,
}

impl PathRule {
    pub fn matches(&self, path: &Path) -> bool {
        let text = match self.pattern.contains('/') {
            true => path.to_str(),
            false => path.file_name().and_then(|name| name.to_str()),
        };
        text.is_some_and(|text| wildcard(&self.pattern, text))
    }

    pub fn label(&self) -> Label {
        Label {
            trust: self.trust.clone(),
            audience: self.audience.clone().into_iter().collect(),
        }
    }
}

/// What one tool does to files, and which argument names the file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRole {
    pub name: String,
    pub role: Role,
    /// The argument holding the file's path; `run` reads `input_files`, `output_files` and
    /// `network_hosts` instead.
    #[serde(default)]
    pub path: Option<String>,
}

/// How a call relates to the files it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Returns the file's content: the result carries the file's label.
    Read,
    /// Replaces the file: what it held before is gone, so its label is dropped.
    Write,
    /// Changes the file in place: the predecessor's content stays, and its label with it.
    Edit,
    /// Runs a command over declared input files, output files and network hosts.
    Run,
}

#[derive(Clone, Debug)]
pub struct Sandbox {
    /// The sandbox runtime and its arguments; SafeBash appends `--settings <file> -c <command>`.
    pub command: Vec<String>,
    /// Paths a command may read besides its inputs and the system: toolchains under the home
    /// directory or in another hidden top-level directory, such as `/opt/homebrew`. Each is
    /// assumed to hold no credentials: a path rule hides no file under it.
    pub readable: Vec<PathBuf>,
    /// Seconds a command may run.
    pub timeout_secs: u64,
}

/// A `[sandbox]` table as written: a deployment's `readable` adds to the built-in one, and
/// each setting it names replaces the built-in one.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSandbox {
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(default)]
    readable: Vec<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    store: Option<PathBuf>,
    #[serde(default)]
    settle_wait_ms: Option<u64>,
    #[serde(default)]
    reservation_timeout_secs: Option<u64>,
    #[serde(default)]
    path: Vec<PathRule>,
    #[serde(default)]
    tool: Vec<ToolRole>,
    #[serde(default)]
    sandbox: RawSandbox,
}

impl Config {
    /// The file `--config` names, else `appa/file-tracker.toml` in the user's config directory
    /// when it exists, over the built-in defaults. A file's path rules, tools and readable
    /// paths are added to the built-in ones — a path's label meets every rule that matches
    /// it — and its other settings replace the built-in ones. A readable path may start with
    /// `~/`, the home directory; one that does not exist is dropped.
    pub fn load(explicit: Option<&Path>) -> Result<Config, String> {
        let defaults: RawConfig = toml::from_str(DEFAULT).expect("the built-in configuration parses");
        let file = explicit.map(Path::to_path_buf).or_else(|| {
            config_home()
                .map(|home| home.join("appa/file-tracker.toml"))
                .filter(|path| path.exists())
        });
        let (raw, base) = match &file {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
                let raw: RawConfig = toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
                (raw, path.parent().map(Path::to_path_buf))
            }
            None => (defaults_shell(), None),
        };
        let resolve = |path: PathBuf| match &base {
            Some(base) if path.is_relative() => base.join(path),
            _ => path,
        };
        let store = match raw.store.or(defaults.store) {
            Some(store) => resolve(store),
            None => state_home()
                .ok_or("no store is configured and neither XDG_STATE_HOME nor HOME is set")?
                .join("appa/file-tracker.json"),
        };
        let mut paths = raw.path;
        paths.extend(defaults.path);
        let mut tools = raw.tool;
        tools.extend(defaults.tool);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let readable = raw
            .sandbox
            .readable
            .iter()
            .chain(&defaults.sandbox.readable)
            .filter_map(|written| match written.strip_prefix("~/") {
                Some(rest) => home.as_ref().map(|home| home.join(rest)),
                None => Some(resolve(PathBuf::from(written))),
            })
            .filter(|path| path.exists())
            .collect();
        Ok(Config {
            store,
            paths,
            tools,
            sandbox: Sandbox {
                command: raw
                    .sandbox
                    .command
                    .or(defaults.sandbox.command)
                    .expect("the built-in configuration names a sandbox command"),
                readable,
                timeout_secs: raw
                    .sandbox
                    .timeout_secs
                    .or(defaults.sandbox.timeout_secs)
                    .expect("the built-in configuration names a sandbox timeout"),
            },
            settle_wait: std::time::Duration::from_millis(
                raw.settle_wait_ms.or(defaults.settle_wait_ms).unwrap_or(3000),
            ),
            reservation_timeout: std::time::Duration::from_secs(
                raw.reservation_timeout_secs
                    .or(defaults.reservation_timeout_secs)
                    .unwrap_or(300),
            ),
        })
    }

    pub fn role(&self, tool: &str) -> Option<&ToolRole> {
        self.tools.iter().find(|role| role.name == tool)
    }
}

fn defaults_shell() -> RawConfig {
    RawConfig {
        store: None,
        settle_wait_ms: None,
        reservation_timeout_secs: None,
        path: Vec::new(),
        tool: Vec::new(),
        sandbox: RawSandbox::default(),
    }
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

fn state_home() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
}

/// `*` matches any run of characters; every other character matches itself. Each literal
/// between stars is taken at its first occurrence, which is the match if any is: the time is
/// linear in the text, whatever the number of stars.
fn wildcard(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().expect("split yields a first part");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let middle: Vec<&str> = parts.collect();
    let Some((last, middle)) = middle.split_last() else {
        return rest.is_empty();
    };
    for literal in middle {
        match rest.find(literal) {
            Some(at) => rest = &rest[at + literal.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_spans_directories_and_the_rest_matches_literally() {
        assert!(wildcard("*/.env*", "/w/app/.env.local"));
        assert!(!wildcard("*/.env*", "/w/app/env"));
        assert!(wildcard("*.pem", "/w/key.pem"));
        assert!(!wildcard("*.pem", "/w/key.pem.txt"));
        assert!(wildcard("/etc/ssh/*", "/etc/ssh/sshd_config"));
    }

    /// The linear matcher agrees with the definition — try every split — on every pattern
    /// and text of up to five characters over `a`, `b` and `*`.
    #[test]
    fn the_wildcard_agrees_with_trying_every_split() {
        fn by_definition(pattern: &str, text: &str) -> bool {
            match pattern.split_once('*') {
                None => pattern == text,
                Some((head, rest)) => text
                    .strip_prefix(head)
                    .is_some_and(|text| (0..=text.len()).any(|at| by_definition(rest, &text[at..]))),
            }
        }
        let words = |alphabet: &[char]| -> Vec<String> {
            let mut all = vec![String::new()];
            for length in 1..=5 {
                let mut next: Vec<String> = all.iter().filter(|word| word.len() == length - 1).cloned().collect();
                next = next
                    .iter()
                    .flat_map(|word| alphabet.iter().map(move |letter| format!("{word}{letter}")))
                    .collect();
                all.extend(next);
            }
            all
        };
        let texts = words(&['a', 'b']);
        for pattern in words(&['a', 'b', '*']) {
            for text in &texts {
                assert_eq!(
                    wildcard(&pattern, text),
                    by_definition(&pattern, text),
                    "{pattern:?} on {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_pattern_without_a_slash_names_the_file_and_one_with_a_slash_its_path() {
        let rule = |pattern: &str| PathRule {
            pattern: pattern.to_string(),
            trust: None,
            audience: None,
            write_trust: None,
            write_attention: Vec::new(),
        };
        assert!(rule("*secrets*").matches(Path::new("/w/app/db-secrets.yaml")));
        assert!(!rule("*secrets*").matches(Path::new("/w/k8s-secrets/README.md")));
        assert!(rule("*/.ssh/*").matches(Path::new("/home/me/.ssh/id_ed25519")));
        assert!(!rule("*/.ssh/*").matches(Path::new("/home/me/ssh/id_ed25519")));
    }

    #[test]
    fn the_built_in_configuration_classifies_credentials_and_serves_both_harnesses() {
        let config = Config::load(Some(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/file-tracker.toml"
        ))))
        .unwrap();
        let env = Path::new("/w/app/.env");
        assert!(
            config
                .paths
                .iter()
                .any(|rule| rule.matches(env) && rule.audience.as_deref() == Some(&["self".to_string()][..]))
        );
        for tool in [
            "host/claude-code/Read",
            "host/claude-code/Write",
            "host/claude-code/Edit",
            "mcp/file-tracker/SafeBash",
            "host/claude-code/Bash",
            "host/opencode/read",
            "host/opencode/appa_safebash",
        ] {
            assert!(config.role(tool).is_some(), "{tool} has a role");
        }
    }
}
