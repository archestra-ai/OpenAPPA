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

name_newtype!(CastName);
name_newtype!(AnnotatorName);
name_newtype!(MembershipResolverName);
name_newtype!(GroupName);

impl std::fmt::Display for GroupName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}

/// How an `includes($arg)` placeholder reads its actual string argument: the
/// reserved word `public` is the Public audience itself, an `@`-marked name is a group for the
/// membership resolver, and any other string is one literal reader ID. `@` with no name after
/// it is malformed and reads as nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AudienceArgument {
    Public,
    Group(GroupName),
    Reader(crate::label::ReaderId),
}

impl AudienceArgument {
    pub(crate) fn parse(value: &str) -> Option<AudienceArgument> {
        match value {
            "public" => Some(AudienceArgument::Public),
            _ => match value.strip_prefix('@') {
                Some("") => None,
                Some(group) => Some(AudienceArgument::Group(GroupName::new(group))),
                None => Some(AudienceArgument::Reader(crate::label::ReaderId::new(value))),
            },
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
    fn a_placeholder_argument_spells_public_a_group_or_one_reader() {
        assert_eq!(AudienceArgument::parse("public"), Some(AudienceArgument::Public));
        assert_eq!(
            AudienceArgument::parse("@auditors"),
            Some(AudienceArgument::Group(GroupName::new("auditors")))
        );
        assert_eq!(
            AudienceArgument::parse("ap@corp.example"),
            Some(AudienceArgument::Reader(crate::label::ReaderId::new("ap@corp.example")))
        );
        assert_eq!(
            AudienceArgument::parse("Public"),
            Some(AudienceArgument::Reader(crate::label::ReaderId::new("Public"))),
            "the reserved word is exact"
        );
        assert_eq!(AudienceArgument::parse("@"), None);
        assert_eq!(GroupName::new("auditors").to_string(), "@auditors");
    }
}
