//! Authorities: who may loosen the policy, and how loosening is recorded.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::contract::{ToolRequest, Violation};
use crate::label::{Grant, Label};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthorityName(String);

impl AuthorityName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthorityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ruling {
    Approve {
        reason: String,
    },
    Deny {
        reason: String,
    },
}

/// Anything that can adjudicate an escalation: a human in the loop, a judge
/// model, a dual-LLM check, a regex, a webhook...
pub trait Authority {
    fn rule(
        &self,
        needed: &Grant,
        request: &ToolRequest,
        context: &Label,
        violations: &[Violation],
    ) -> Option<(AuthorityName, Ruling)>;
}

impl<A: Authority, B: Authority> Authority for (A, B) {
    fn rule(
        &self,
        needed: &Grant,
        request: &ToolRequest,
        context: &Label,
        violations: &[Violation],
    ) -> Option<(AuthorityName, Ruling)> {
        self.0
            .rule(needed, request, context, violations)
            .or_else(|| self.1.rule(needed, request, context, violations))
    }
}
