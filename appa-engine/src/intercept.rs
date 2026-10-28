//! Interceptors: checks the policy binds to a tool call that answer `yes`, `no` or `ask` about
//! the exact call. The runtime runs them; the engine holds their answers to the call as pinned
//! evidence, as it holds an Annotator's. A `no` or an `ask` is a requirement gap that only a
//! component bound to the tool by `unblocks` can lift: an authority lifts either, an input
//! sanitizer lifts a `no` by rewriting the call, which every Interceptor then judges afresh.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::authority::Scope;
use crate::names::{InterceptorName, TagName};
use crate::value::CanonicalDigest;

/// One Interceptor's answer about one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// No objection. It never lifts a gap another check found.
    Yes,
    /// The call is blocked until a bound authority approves it or a bound input sanitizer
    /// rewrites it into a call the Interceptor answers `yes` for.
    No,
    /// The call is blocked until a bound authority approves it.
    Ask,
}

/// An Interceptor's answer with its stated reason. The reason is what feedback and unblockers
/// read; it is not part of the gap the answer opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptorAnswer {
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Every answer the call's Interceptors gave, pinned to the canonical digest of the call they
/// judged. A rewrite is a new call and is judged afresh. The parts live behind one box, as a
/// pinned annotation's do: the pin rides every call record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Interceptions(Box<InterceptionParts>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InterceptionParts {
    call: CanonicalDigest,
    answers: BTreeMap<InterceptorName, InterceptorAnswer>,
}

impl Interceptions {
    pub fn new(call: CanonicalDigest, answers: BTreeMap<InterceptorName, InterceptorAnswer>) -> Self {
        Interceptions(Box::new(InterceptionParts { call, answers }))
    }

    pub fn call(&self) -> &CanonicalDigest {
        &self.0.call
    }

    pub fn answers(&self) -> &BTreeMap<InterceptorName, InterceptorAnswer> {
        &self.0.answers
    }

    /// The answers that block the call: every `no` and `ask`, in interceptor name order.
    pub fn blocking(&self) -> impl Iterator<Item = (&InterceptorName, Verdict)> {
        self.0
            .answers
            .iter()
            .filter(|(_, answer)| answer.verdict != Verdict::Yes)
            .map(|(name, answer)| (name, answer.verdict))
    }

    /// The reason `interceptor` gave, when it gave one.
    pub fn reason(&self, interceptor: &InterceptorName) -> Option<&str> {
        self.0.answers.get(interceptor)?.reason.as_deref()
    }
}

/// A tool-call Interceptor as the engine knows it: its name and the tools it judges. The
/// implementation and what it reads live in the runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptorDeclaration {
    pub name: InterceptorName,
    /// The tools the Interceptor judges, by tag. Unscoped = every tool.
    pub scope: Scope,
}

impl InterceptorDeclaration {
    /// The tag the policy compiler gives every tool an Interceptor's `tools` names, and that
    /// Interceptor's scope: `intercept:<interceptor>`. The prefix is reserved; policy tags
    /// never carry it.
    pub const TAG_PREFIX: &'static str = "intercept:";

    pub fn judges(&self, tags: &[TagName]) -> bool {
        self.scope.covers(tags)
    }
}

/// Why a call's interceptions do not fit the policy.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InterceptionRefusal {
    /// The call carries no interceptions, or not every Interceptor that judges it answered.
    #[error("the call lacks the answers of {}", names(.0))]
    Needed(Vec<InterceptorName>),
    #[error("the interceptions do not fit the call: {0}")]
    Foreign(String),
}

fn names(names: &[InterceptorName]) -> String {
    names.iter().map(InterceptorName::as_str).collect::<Vec<_>>().join(", ")
}
