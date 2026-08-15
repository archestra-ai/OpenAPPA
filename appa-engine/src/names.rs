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

name_newtype!(
    AuthorityName
);
name_newtype!(
    SanitizerName
);

impl SanitizerName {
    /// The reserved builtin sanitizer of the quarantine exit. The engine
    /// itself applies it — deriving the return unchanged and claiming its trust transition from
    /// engine-held facts — so a deployment registers the name but never binds an implementation.
    pub const ATTEST_SCHEMA: &'static str = "attest-schema";

    pub fn is_attest_schema(&self) -> bool {
        self.as_str() == Self::ATTEST_SCHEMA
    }
}

name_newtype!(
    CastName
);
name_newtype!(
    DynamicResolverName
);
name_newtype!(
    MarkName
);
name_newtype!(
    TagName
);
name_newtype!(
    SurfaceName
);
