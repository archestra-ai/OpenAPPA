//! The validated spellings both manifests are written in.
//!
//! Every one of these is built through a constructor that refuses the
//! malformed spelling, so a parsed manifest cannot hold one.

use std::fmt;
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    #[error("`{0}` is not a name: a name matches ^[a-z0-9][a-z0-9-]*$")]
    Malformed(String),
}

/// A marketplace, package or plugin name: lowercase ASCII, digits and dashes,
/// starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    pub fn parse(text: &str) -> Result<Self, NameError> {
        match lower_kebab(text) {
            true => Ok(Self(text.to_owned())),
            false => Err(NameError::Malformed(text.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The credential namespace this package owns. Dashes become underscores
    /// because an environment variable carries no dash.
    pub fn credential_prefix(&self) -> CredentialPrefix {
        CredentialPrefix(format!(
            "{PROVIDER_CREDENTIAL_PREFIX}{}",
            self.0.to_uppercase().replace('-', "_")
        ))
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The namespace a deployment names its provider credentials in.
const PROVIDER_CREDENTIAL_PREFIX: &str = "APPA_PROVIDER_";

/// The environment variables one package's helpers may read.
///
/// A deployment hands a helper its credential through the process environment,
/// so the variable a helper names is the only thing between its own credential
/// and every other package's. The prefix is derived from the package name and
/// never declared, so a package cannot name itself into another's credentials.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CredentialPrefix(String);

impl CredentialPrefix {
    /// Whether this package owns `variable`. The prefix is a whole name, not a
    /// character run: `APPA_PROVIDER_SLACK` owns `APPA_PROVIDER_SLACK_TOKEN`
    /// and does not own `APPA_PROVIDER_SLACKBOT_TOKEN`.
    pub fn owns(&self, variable: &str) -> bool {
        variable == self.0 || variable.strip_prefix(&self.0).is_some_and(|rest| rest.starts_with('_'))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The grammar shared by every name in these manifests.
pub(crate) fn lower_kebab(text: &str) -> bool {
    let mut characters = text.chars();
    let starts = matches!(characters.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit());
    starts
        && characters.all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// The middle segment of a canonical tool id: `mcp/<namespace>/<tool>`. Its
/// alphabet is the harness's, not this marketplace's — an MCP server key such
/// as `claude_ai_Slack` is a namespace and can never be a `PackageName` — so
/// the two grammars are separate on purpose. This one must accept exactly what
/// `appa_runtime_api::CanonicalTool` accepts between the family and the tool;
/// `appa-runtime` holds the test that keeps them agreeing, because only it
/// depends on both crates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Namespace(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NamespaceError {
    #[error("`{0}` is not a namespace: a namespace is ASCII alphanumerics, `_`, `.` and `-`, and never empty")]
    Malformed(String),
    #[error("`{0}` is not a namespace: a namespace cannot contain `__`")]
    DoubleUnderscore(String),
}

impl Namespace {
    pub fn parse(text: &str) -> Result<Self, NamespaceError> {
        let spellable = !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !spellable {
            return Err(NamespaceError::Malformed(text.to_owned()));
        }
        match text.contains("__") {
            true => Err(NamespaceError::DoubleUnderscore(text.to_owned())),
            false => Ok(Self(text.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RelativePathError {
    #[error("a path is empty")]
    Empty,
    #[error("`{0}` is not a relative path")]
    NotRelative(String),
    #[error("`{0}` leaves the directory it is written in")]
    Traversal(String),
    #[error("`{0}` has an empty or unportable segment")]
    Segment(String),
}

/// A path contained by the directory it is written in: relative, `/`-separated,
/// possibly nested, with no `..`, no empty segment and no absolute prefix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(String);

impl RelativePath {
    pub fn parse(text: &str) -> Result<Self, RelativePathError> {
        if text.is_empty() {
            return Err(RelativePathError::Empty);
        }
        if text.starts_with('/') || Path::new(text).is_absolute() {
            return Err(RelativePathError::NotRelative(text.to_owned()));
        }
        for segment in text.split('/') {
            match segment {
                ".." => return Err(RelativePathError::Traversal(text.to_owned())),
                "" | "." => return Err(RelativePathError::Segment(text.to_owned())),
                // A backslash is a separator on one of the platforms this runs
                // on, so it is never part of a portable segment.
                segment if segment.contains('\\') => return Err(RelativePathError::Segment(text.to_owned())),
                _ => {}
            }
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The harnesses this workspace ships adapters and batteries for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Host {
    ClaudeCode,
    Kagent,
}

impl Host {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "claude-code" => Some(Self::ClaudeCode),
            "kagent" => Some(Self::Kagent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Kagent => "kagent",
        }
    }
}

impl fmt::Display for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a marketplace package is. The kind is the table a package is listed
/// under, never a free string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackageKind {
    Adapter,
    Battery,
}

impl PackageKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "adapter" => Some(Self::Adapter),
            "battery" => Some(Self::Battery),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adapter => "adapter",
            Self::Battery => "battery",
        }
    }
}

impl fmt::Display for PackageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_take_lowercase_dashed_spellings() {
        assert_eq!(PackageName::parse("claude-code").unwrap().as_str(), "claude-code");
        assert_eq!(PackageName::parse("0").unwrap().as_str(), "0");
    }

    #[test]
    fn names_refuse_other_spellings() {
        for spelling in ["", "-github", "GitHub", "git hub", "git_hub", "gith/ub"] {
            assert!(matches!(PackageName::parse(spelling), Err(NameError::Malformed(_))));
        }
    }

    #[test]
    fn relative_paths_may_be_nested() {
        assert_eq!(
            RelativePath::parse("adapters/claude-code").unwrap().as_str(),
            "adapters/claude-code"
        );
    }

    #[test]
    fn relative_paths_refuse_escape_and_empty_segments() {
        assert_eq!(RelativePath::parse(""), Err(RelativePathError::Empty));
        assert!(matches!(
            RelativePath::parse("/etc/passwd"),
            Err(RelativePathError::NotRelative(_))
        ));
        assert!(matches!(
            RelativePath::parse("../outside"),
            Err(RelativePathError::Traversal(_))
        ));
        assert!(matches!(
            RelativePath::parse("plugin/../../outside"),
            Err(RelativePathError::Traversal(_))
        ));
        assert!(matches!(
            RelativePath::parse("plugin//hooks"),
            Err(RelativePathError::Segment(_))
        ));
        assert!(matches!(
            RelativePath::parse("./plugin"),
            Err(RelativePathError::Segment(_))
        ));
    }

    #[test]
    fn a_namespace_takes_the_harness_alphabet_and_a_package_name_does_not() {
        for spelling in ["github", "claude_ai_Slack", "claude-code", "svc.team-a"] {
            assert_eq!(
                Namespace::parse(spelling)
                    .expect("the harness spells namespaces this way")
                    .as_str(),
                spelling
            );
        }
        assert!(
            PackageName::parse("claude_ai_Slack").is_err(),
            "a package name is lower kebab"
        );
        assert!(matches!(
            Namespace::parse("mcp__github"),
            Err(NamespaceError::DoubleUnderscore(_))
        ));
        assert!(matches!(Namespace::parse(""), Err(NamespaceError::Malformed(_))));
        assert!(matches!(
            Namespace::parse("with space"),
            Err(NamespaceError::Malformed(_))
        ));
    }

    #[test]
    fn hosts_and_kinds_are_closed() {
        assert_eq!(Host::parse("kagent"), Some(Host::Kagent));
        assert_eq!(Host::parse("codex"), None);
        assert_eq!(PackageKind::parse("battery"), Some(PackageKind::Battery));
        assert_eq!(PackageKind::parse("skill"), None);
    }
}
