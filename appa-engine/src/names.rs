//! Name newtypes for the registry's four kinds and the routing vocabulary. Names are routing
//! identity, never algebraic — they do not fold and never enter a check.

use serde::{Deserialize, Serialize};

macro_rules! name_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                $name(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

name_newtype!(AuthorityName);
name_newtype!(SanitizerName);

impl SanitizerName {
    /// The reserved builtin sanitizer of the quarantine exit. The engine
    /// itself applies it — deriving the return unchanged and claiming its trust transition from
    /// engine-held facts — so a deployment registers the name but never binds an implementation.
    pub const ATTEST_SCHEMA: &'static str = "attest-schema";

    pub fn is_attest_schema(&self) -> bool {
        self.as_str() == Self::ATTEST_SCHEMA
    }
}

name_newtype!(AnnotatorName);
name_newtype!(IdentityImplementationName);
name_newtype!(GroupName);

impl std::fmt::Display for GroupName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}

/// One audience entry as policy and tool arguments spell it: the reserved word `public` is
/// the Public audience itself, `self` and `internal` are the built-in chain audiences, an
/// `@`-marked spelling is a group reference — a configured named audience or a
/// source-qualified selector — and any other string is one literal reader ID. `@` with no
/// name after it, and a malformed selector form, read as nothing. The one grammar: a
/// declared audience list and an `includes($arg)` placeholder's actual both read through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudienceArgument {
    Public,
    Chain(crate::label::ChainAudience),
    Group(crate::label::GroupRef),
    Reader(crate::label::ReaderId),
}

impl AudienceArgument {
    pub fn parse(value: &str) -> Option<AudienceArgument> {
        if value == "public" {
            return Some(AudienceArgument::Public);
        }
        if let Some(chain) = crate::label::ChainAudience::parse(value) {
            return Some(AudienceArgument::Chain(chain));
        }
        match value.strip_prefix('@') {
            Some(reference) => crate::label::GroupRef::parse(reference).map(AudienceArgument::Group),
            None => {
                let reader = crate::label::ReaderId::new(value);
                reader.is_literal().then_some(AudienceArgument::Reader(reader))
            }
        }
    }
}

name_newtype!(MarkName);
name_newtype!(TagName);
name_newtype!(SurfaceName);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_argument_spells_an_audience_a_group_or_one_reader() {
        assert_eq!(AudienceArgument::parse("public"), Some(AudienceArgument::Public));
        assert_eq!(
            AudienceArgument::parse("self"),
            Some(AudienceArgument::Chain(crate::label::ChainAudience::Self_))
        );
        assert_eq!(
            AudienceArgument::parse("internal"),
            Some(AudienceArgument::Chain(crate::label::ChainAudience::Internal))
        );
        assert_eq!(
            AudienceArgument::parse("@auditors"),
            Some(AudienceArgument::Group(crate::label::GroupRef::Named(GroupName::new(
                "auditors"
            ))))
        );
        assert_eq!(
            AudienceArgument::parse("@google-workspace:group/finance@corp.com"),
            Some(AudienceArgument::Group(crate::label::GroupRef::Source {
                provider: "google-workspace".into(),
                selector: "group/finance@corp.com".into()
            }))
        );
        assert_eq!(
            AudienceArgument::parse("ap@corp.example"),
            Some(AudienceArgument::Reader(crate::label::ReaderId::new("ap@corp.example")))
        );
        assert_eq!(
            AudienceArgument::parse("Public"),
            Some(AudienceArgument::Reader(crate::label::ReaderId::new("Public"))),
            "the reserved words are exact"
        );
        assert_eq!(AudienceArgument::parse("@"), None);
        assert_eq!(AudienceArgument::parse("@slack:"), None);
        assert_eq!(AudienceArgument::parse(""), None, "an empty id names no reader");
        assert_eq!(GroupName::new("auditors").to_string(), "@auditors");
    }
}
