//! What a host pins on a trajectory family when it first opens it: which remedies the runtime
//! applies at a block on its own, and the host's name for the authority line it composed.

/// The remedies the runtime applies at a block without the agent executing the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoApply {
    /// A plan whose every step is a stock `approve` ruling.
    #[default]
    RulingsOnly,
    /// A plan whose every step is a stock `approve` ruling or the acceptance of a narrowing.
    RulingsAndAccept,
}

impl AutoApply {
    fn is_rulings_only(&self) -> bool {
        *self == AutoApply::RulingsOnly
    }
}

/// Who took a remedy plan: the agent executing it, or the runtime under the family's
/// [`AutoApply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Initiator {
    Agent,
    AutoApply,
}

const LINE_VARIANT_MAX_BYTES: usize = 64;

/// The host's opaque name for the authority line it composed for a family, such as `observe`
/// or `enforce`: 1 to 64 ASCII letters, digits, `-`, `_` or `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct LineVariant(String);

/// Why a spelling is no [`LineVariant`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineVariantError {
    Empty,
    TooLong { bytes: usize },
    Character,
}

impl std::fmt::Display for LineVariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("a line variant is empty"),
            Self::TooLong { bytes } => {
                write!(f, "a line variant of {bytes} bytes; at most {LINE_VARIANT_MAX_BYTES}")
            }
            Self::Character => f.write_str("a line variant holds only ASCII letters, digits, '-', '_' and '.'"),
        }
    }
}

impl std::error::Error for LineVariantError {}

impl LineVariant {
    pub fn parse(text: &str) -> Result<Self, LineVariantError> {
        match text.len() {
            0 => Err(LineVariantError::Empty),
            bytes if bytes > LINE_VARIANT_MAX_BYTES => Err(LineVariantError::TooLong { bytes }),
            _ if !text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')) =>
            {
                Err(LineVariantError::Character)
            }
            _ => Ok(Self(text.to_string())),
        }
    }
}

impl TryFrom<String> for LineVariant {
    type Error = LineVariantError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

/// The settings a family's first opening pins. Every trajectory in the family reads them, and
/// no later event changes them. The default is what a family opened without them keeps.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionSettings {
    #[serde(default, skip_serializing_if = "AutoApply::is_rulings_only")]
    pub auto_apply: AutoApply,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_variant: Option<LineVariant>,
}

impl SessionSettings {
    pub fn is_default(&self) -> bool {
        *self == SessionSettings::default()
    }

    /// Whether an event naming `named` continues a family these settings pinned: every event
    /// but one naming a line variant other than the one pinned. A family pinned without one,
    /// as every family opened before line variants was, continues under any, and keeps the
    /// settings it pinned.
    pub fn continued_by(&self, named: Option<&LineVariant>) -> bool {
        match (&self.line_variant, named) {
            (Some(pinned), Some(named)) => pinned == named,
            (None, _) | (_, None) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_variant_is_a_short_token() {
        assert!(LineVariant::parse("observe").is_ok());
        assert!(LineVariant::parse("enforce-v2.1_a").is_ok());
        assert_eq!(LineVariant::parse(""), Err(LineVariantError::Empty));
        assert_eq!(
            LineVariant::parse(&"a".repeat(65)),
            Err(LineVariantError::TooLong { bytes: 65 })
        );
        for spelling in ["obs erve", "observe\n", "наблюдать", "a/b"] {
            assert_eq!(
                LineVariant::parse(spelling),
                Err(LineVariantError::Character),
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn default_settings_serialize_to_nothing_and_read_back_from_nothing() {
        assert_eq!(
            serde_json::to_value(SessionSettings::default()).unwrap(),
            serde_json::json!({})
        );
        let read: SessionSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(read.is_default());
        let pinned = SessionSettings {
            auto_apply: AutoApply::RulingsAndAccept,
            line_variant: Some(LineVariant::parse("observe").unwrap()),
        };
        let spelled = serde_json::to_value(&pinned).unwrap();
        assert_eq!(
            spelled,
            serde_json::json!({"auto_apply": "rulings_and_accept", "line_variant": "observe"})
        );
        assert_eq!(serde_json::from_value::<SessionSettings>(spelled).unwrap(), pinned);
        assert!(serde_json::from_value::<SessionSettings>(serde_json::json!({"line_variant": ""})).is_err());
    }

    #[test]
    fn a_family_is_refused_only_a_line_variant_other_than_the_one_it_pinned() {
        let observe = LineVariant::parse("observe").unwrap();
        let enforce = LineVariant::parse("enforce").unwrap();
        let pinned = SessionSettings {
            auto_apply: AutoApply::RulingsAndAccept,
            line_variant: Some(observe.clone()),
        };
        assert!(pinned.continued_by(None));
        assert!(pinned.continued_by(Some(&observe)));
        assert!(!pinned.continued_by(Some(&enforce)));
        assert!(SessionSettings::default().continued_by(Some(&observe)));
    }
}
