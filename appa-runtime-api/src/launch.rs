//! What a protected launcher reports about one session start. A host may change its session
//! id while one process keeps running the same conversation; the launch is the identity that
//! outlives that change, so the runtime can keep one family for it.

use crate::TrajectoryId;

const TOKEN_MAX_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTokenError {
    Empty,
    Length { chars: usize },
    Character,
}

impl std::fmt::Display for LaunchTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("a launch token is empty"),
            Self::Length { chars } => write!(f, "a launch token of {chars} characters; at most {TOKEN_MAX_CHARS}"),
            Self::Character => f.write_str("a launch token is ASCII letters, digits, and '-' only"),
        }
    }
}

impl std::error::Error for LaunchTokenError {}

/// The identity one protected launcher minted for the host process it started. The process
/// environment carries it, and a session cannot change its own environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LaunchToken(String);

impl LaunchToken {
    pub fn parse(text: &str) -> Result<Self, LaunchTokenError> {
        match text.chars().count() {
            0 => Err(LaunchTokenError::Empty),
            chars if chars > TOKEN_MAX_CHARS => Err(LaunchTokenError::Length { chars }),
            _ if !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => Err(LaunchTokenError::Character),
            _ => Ok(Self(text.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LaunchToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for LaunchToken {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for LaunchToken {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        LaunchToken::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Why the host started this session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    /// A new conversation.
    Startup,
    /// A conversation the host already had, under its own id.
    Resume,
    /// The conversation was cleared: the host context starts empty under a new id.
    Clear,
    /// The conversation was compacted under the same id.
    Compact,
    /// A copy of another conversation under a new id.
    Fork,
}

/// One session start inside a protected launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchStart {
    pub launch: LaunchToken,
    /// The session a fork copied, where the launcher knows it.
    pub forked_from: Option<TrajectoryId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_token_is_a_bounded_ascii_word() {
        assert!(LaunchToken::parse("0b6c1f5e-8f0a-4a57-9d6e-2f7c3e1a9b10").is_ok());
        assert_eq!(LaunchToken::parse(""), Err(LaunchTokenError::Empty));
        assert_eq!(LaunchToken::parse("a b"), Err(LaunchTokenError::Character));
        assert_eq!(LaunchToken::parse("a:b"), Err(LaunchTokenError::Character));
        assert_eq!(
            LaunchToken::parse(&"a".repeat(65)),
            Err(LaunchTokenError::Length { chars: 65 })
        );
    }
}
