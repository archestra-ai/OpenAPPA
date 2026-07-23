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
    /// An authority's name — the routing/audit identity of one home of judgment.
    AuthorityName
);
name_newtype!(
    /// A registered sanitizer's name.
    SanitizerName
);
name_newtype!(
    /// A registered cast's name.
    CastName
);
name_newtype!(
    /// An attention mark — the shared currency a tool demands and an authority attends.
    MarkName
);
name_newtype!(
    /// A routing tag: an authority's scope names the tags it has jurisdiction over.
    TagName
);
