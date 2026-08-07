//! Values, provenance, and the identities that bind a ruling to the exact call it ruled on.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contract::{DynamicAudienceBinding, PinnedDynamicResolution};
use crate::label::Label;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(name: impl Into<String>) -> Self {
        ToolName(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The model-assigned identifier of a proposed tool call (OpenAI's `tool_calls[].id`). Opaque to the
/// algebra: it exists only to pair an assistant turn's proposed call with the model-visible response
/// the transcript later shows for it (CC2/RP1) — never a routing or security identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(id: impl Into<String>) -> Self {
        ToolCallId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TrajectoryId(String);

impl TrajectoryId {
    pub fn new(id: impl Into<String>) -> Self {
        TrajectoryId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A stable id for an admitted value: its position in the log's value sequence, assigned
/// deterministically by the projection at append order (see the event-log slice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ValueId(u64);

impl ValueId {
    pub const fn new(index: u64) -> Self {
        ValueId(index)
    }

    pub const fn index(self) -> u64 {
        self.0
    }
}

/// A collision-resistant digest of the canonical rendered call (tool + resolved arguments). Two
/// calls with the same digest are the same call; a ruling is scoped to one digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CanonicalDigest([u8; 32]);

impl CanonicalDigest {
    fn of_call(tool: &ToolName, arguments: &serde_json::Value) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(tool.0.as_bytes());
        hasher.update([0u8]);
        let bytes = serde_json::to_vec(arguments).expect("a serde_json::Value re-serializes");
        hasher.update(&bytes);
        CanonicalDigest(hasher.finalize().into())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A digest of a raw tool result. Binds a cast resolution or a child-return derivation to the bytes it
/// derived from, so a later differing result cannot silently reuse an old derivative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RawResultDigest([u8; 32]);

impl RawResultDigest {
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        RawResultDigest(hasher.finalize().into())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Identifies one dispatch of one call within a trajectory. The occurrence counter distinguishes a
/// repeated identical call — a second `transfer(A, $1)` is a new dispatch, not a re-issue.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DispatchId {
    trajectory: TrajectoryId,
    digest: CanonicalDigest,
    occurrence: u32,
}

impl DispatchId {
    pub fn new(trajectory: TrajectoryId, digest: CanonicalDigest, occurrence: u32) -> Self {
        DispatchId {
            trajectory,
            digest,
            occurrence,
        }
    }

    pub fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    pub fn digest(&self) -> &CanonicalDigest {
        &self.digest
    }

    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }
}

/// Identifies one value a child branch returned through `submit_result`. The occurrence
/// distinguishes repeated returns from the same child; a merge consumes exactly one, once.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChildReturnId {
    child: TrajectoryId,
    occurrence: u32,
}

impl ChildReturnId {
    pub fn new(child: TrajectoryId, occurrence: u32) -> Self {
        ChildReturnId { child, occurrence }
    }

    pub fn child(&self) -> &TrajectoryId {
        &self.child
    }

    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }
}

/// How a value entered the trajectory — recorded for audit and branch attribution. The label's
/// numeric fold does not depend on this; provenance answers *where from*, the label *what it is*.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    UserInput,
    ToolResult { dispatch: DispatchId },
    ChildReturn { child: TrajectoryId, id: ChildReturnId },
}

/// A value's body — opaque to the engine, which checks labels, never content. Content robustness
/// is the registered sanitizer's/authority's concern, not the engine's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueBody(String);

impl ValueBody {
    pub fn new(body: impl Into<String>) -> Self {
        ValueBody(body.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledValue {
    pub body: ValueBody,
    pub label: Label,
}

impl LabeledValue {
    pub fn new(body: ValueBody, label: Label) -> Self {
        LabeledValue { body, label }
    }
}

/// A proposed tool call the runtime has resolved: the tool, its concrete argument tree, and the prior
/// values the arguments reference (coarse in v1 — see the plan's dependency-discovery note). The
/// [`CanonicalDigest`] is **derived from the tool and arguments on demand**, never a stored field —
/// so a value round-tripped through `serde` cannot carry a digest inconsistent with its arguments
/// (which would let an approved call's digest ride on a different call's arguments).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCall {
    tool: ToolName,
    arguments: serde_json::Value,
    arg_refs: Vec<ValueId>,
    #[serde(skip, default)]
    dynamic_resolutions: Vec<PinnedDynamicResolution>,
}

impl ResolvedCall {
    pub fn new(tool: ToolName, arguments: serde_json::Value, arg_refs: Vec<ValueId>) -> Self {
        ResolvedCall {
            tool,
            arguments,
            arg_refs,
            dynamic_resolutions: Vec::new(),
        }
    }

    pub fn tool(&self) -> &ToolName {
        &self.tool
    }

    pub fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }

    pub fn arg_refs(&self) -> &[ValueId] {
        &self.arg_refs
    }

    pub fn with_dynamic_resolutions(mut self, resolutions: Vec<PinnedDynamicResolution>) -> Self {
        self.dynamic_resolutions.clear();
        for resolution in resolutions {
            match self
                .dynamic_resolutions
                .iter()
                .position(|existing| existing.binding() == resolution.binding())
            {
                Some(index) => self.dynamic_resolutions[index] = resolution,
                None => self.dynamic_resolutions.push(resolution),
            }
        }
        self
    }

    pub fn dynamic_resolutions(&self) -> &[PinnedDynamicResolution] {
        &self.dynamic_resolutions
    }

    pub fn dynamic_resolution(&self, binding: &DynamicAudienceBinding) -> Option<&crate::label::Audience> {
        self.dynamic_resolutions
            .iter()
            .find(|resolution| resolution.binding() == binding)
            .and_then(PinnedDynamicResolution::audience)
    }

    pub fn digest(&self) -> CanonicalDigest {
        CanonicalDigest::of_call(&self.tool, &self.arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_is_deterministic_and_key_order_independent() {
        let a = ResolvedCall::new(ToolName::new("transfer"), json!({ "to": "alice", "amount": 1 }), vec![]);
        let b = ResolvedCall::new(
            ToolName::new("transfer"),
            json!({ "amount": 1, "to": "alice" }),
            vec![ValueId::new(7)],
        );
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn digest_separates_distinct_calls() {
        let base = ResolvedCall::new(ToolName::new("transfer"), json!({ "to": "a" }), vec![]);
        let other_arg = ResolvedCall::new(ToolName::new("transfer"), json!({ "to": "b" }), vec![]);
        let other_tool = ResolvedCall::new(ToolName::new("refund"), json!({ "to": "a" }), vec![]);
        assert_ne!(base.digest(), other_arg.digest());
        assert_ne!(base.digest(), other_tool.digest());
    }

    #[test]
    fn dynamic_resolution_does_not_enter_the_call_digest() {
        let binding = DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "recipient".into(),
        };
        let base = ResolvedCall::new(ToolName::new("send"), json!({ "recipient": "room" }), vec![]);
        let resolved = base
            .clone()
            .with_dynamic_resolutions(vec![PinnedDynamicResolution::from_answer(
                binding,
                Some(crate::label::Audience::restricted([crate::label::ReaderId::new(
                    "alice",
                )])),
            )]);
        assert_eq!(base.digest(), resolved.digest());
    }

    #[test]
    fn dispatch_id_distinguishes_occurrences() {
        let call = ResolvedCall::new(ToolName::new("send"), json!({}), vec![]);
        let traj = TrajectoryId::new("t1");
        let first = DispatchId::new(traj.clone(), call.digest(), 0);
        let second = DispatchId::new(traj, call.digest(), 1);
        assert_ne!(first, second);
        assert_eq!(first.digest(), second.digest());
    }

    #[test]
    fn raw_result_digest_binds_bytes() {
        assert_eq!(RawResultDigest::of(b"hello"), RawResultDigest::of(b"hello"));
        assert_ne!(RawResultDigest::of(b"hello"), RawResultDigest::of(b"world"));
    }
}
