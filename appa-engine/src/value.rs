//! Values, provenance, and the identities that bind a ruling to the exact call it ruled on.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contract::{DynamicAudienceBinding, PinnedDynamicResolution};
use crate::label::Label;
use crate::params::CanonicalArguments;

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
    /// Digest the canonical rendered call: domain-separated over the tool name and the
    /// argument object's RFC 8785 canonical bytes, so equal argument objects
    /// bind the same call regardless of source key order or whitespace.
    pub(crate) fn of_call(tool: &ToolName, arguments: &CanonicalArguments) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(tool.0.as_bytes());
        hasher.update([0u8]);
        hasher.update(arguments.canonical_bytes());
        CanonicalDigest(hasher.finalize().into())
    }

    /// Digest one proposal batch's policy-content payload: domain-separated over each call's
    /// ordered rendered digest and the dynamic resolutions pinned to it, so a repeat carrying the
    /// same content binds the same payload and anything else is an identity conflict.
    pub(crate) fn of_batch<'a>(
        calls: impl IntoIterator<Item = &'a ResolvedCall>,
        spawn: Option<crate::transition::SpawnMark>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"appa.proposal-batch.v1");
        // The mark is content: the same calls with and without a spawn are two different acts.
        match spawn {
            Some(mark) => {
                hasher.update([1u8]);
                hasher.update(mark.index().to_be_bytes());
            }
            None => hasher.update([0u8]),
        }
        for call in calls {
            hasher.update([0u8]);
            hasher.update(call.digest().0);
            for resolution in &call.dynamic_resolutions {
                hasher.update(canonical_resolution(resolution));
            }
        }
        CanonicalDigest(hasher.finalize().into())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn canonical_resolution(resolution: &PinnedDynamicResolution) -> Vec<u8> {
    serde_json_canonicalizer::to_vec(resolution).expect("a pinned resolution canonicalizes")
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

/// Domain-separated hashing over **length-prefixed** fields.
///
/// Bare concatenation over variable-length names is ambiguous: `("ab", "c")` and `("a", "bc")`
/// would hash alike, so a trajectory and a batch name could be chosen to collide with another
/// pair. Every field carries its own length, which makes the encoding injective.
struct Framed(Sha256);

impl Framed {
    fn tagged(domain: &'static [u8]) -> Framed {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        Framed(hasher)
    }

    fn field(mut self, bytes: &[u8]) -> Framed {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
        self
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// One act's fresh 256 bits of runtime entropy. The engine mixes it into every
/// identity it derives for that act and keeps none of it: explicit entropy is input data, never
/// engine state. Runtime supplies it and persists the result; it never allocates or
/// binds an individual offer identity itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferNonce([u8; 32]);

impl OfferNonce {
    pub const fn new(bytes: [u8; 32]) -> Self {
        OfferNonce(bytes)
    }
}

/// One surfaced block's identity, derived by the engine from the act's nonce and what
/// the block is about. Fresh per surfaced block: the same call blocked again under a new act gets
/// a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockId([u8; 32]);

impl BlockId {
    /// The block one proposal's refusal surfaces. Bound to the deciding act and the exact position
    /// within it, so two identical siblings surface two blocks.
    pub(crate) fn of_proposal(
        nonce: &OfferNonce,
        trajectory: &TrajectoryId,
        batch: &crate::transition::ProposalBatchId,
        position: u32,
        call: &CanonicalDigest,
    ) -> Self {
        BlockId(
            Framed::tagged(b"appa.block.v1")
                .field(&nonce.0)
                .field(trajectory.0.as_bytes())
                .field(batch.as_str().as_bytes())
                .field(&position.to_be_bytes())
                .field(&call.0)
                .finish(),
        )
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One executable offer's identity. Unguessable without the act's nonce, so
/// possession of one proves the offer reached model context before the acceptance naming it was
/// authored — which is what makes an acceptance informed without any turn or round counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferId([u8; 32]);

impl OfferId {
    /// One plan of one block: its deterministic position in the derived order, and the canonical
    /// digest of the plan itself, so two blocks never share an identity and one block's plans
    /// never collide with each other.
    pub(crate) fn of_plan(block: &BlockId, position: u32, plan: &[u8]) -> Self {
        OfferId(
            Framed::tagged(b"appa.offer.v1")
                .field(&block.0)
                .field(&position.to_be_bytes())
                .field(plan)
                .finish(),
        )
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

/// Identifies one prepared fork. Derived from the dispatch whose release prepared it,
/// never minted by a runtime: one release prepares one fork, and a repeat of the same spawn call
/// is a new dispatch and so a new fork. The child's own identity is not part of it — the host does
/// not know that yet when the spawn is released.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ForkId(DispatchId);

impl ForkId {
    pub fn of(dispatch: &DispatchId) -> Self {
        ForkId(dispatch.clone())
    }

    pub fn dispatch(&self) -> &DispatchId {
        &self.0
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
    ProviderRun {
        tool: ToolName,
        batch: crate::transition::ProposalBatchId,
        position: u32,
        effects: crate::fact::EffectSet,
    },
}

/// A value's body — opaque to the engine, which checks labels, never content. Content robustness
/// is the registered sanitizer's/authority's concern, not the engine's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueBody(std::sync::Arc<str>);

impl ValueBody {
    pub fn new(body: impl Into<String>) -> Self {
        ValueBody(body.into().into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ValueBody {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ValueBody {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(ValueBody::new(String::deserialize(deserializer)?))
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedCall {
    tool: ToolName,
    arguments: CanonicalArguments,
    dynamic_resolutions: Vec<PinnedDynamicResolution>,
}

impl<'de> Deserialize<'de> for ResolvedCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireCall {
            tool: ToolName,
            arguments: CanonicalArguments,
            dynamic_resolutions: Vec<PinnedDynamicResolution>,
        }

        let wire = WireCall::deserialize(deserializer)?;
        let pinned = wire.dynamic_resolutions.clone();
        let canonical = ResolvedCall::new(wire.tool, wire.arguments).with_dynamic_resolutions(wire.dynamic_resolutions);
        if canonical.dynamic_resolutions != pinned {
            return Err(serde::de::Error::custom(
                "pinned dynamic answers are not in their canonical order",
            ));
        }
        Ok(canonical)
    }
}

impl ResolvedCall {
    pub(crate) fn new(tool: ToolName, arguments: CanonicalArguments) -> Self {
        ResolvedCall {
            tool,
            arguments,
            dynamic_resolutions: Vec::new(),
        }
    }

    pub fn tool(&self) -> &ToolName {
        &self.tool
    }

    pub fn arguments(&self) -> &serde_json::Value {
        self.arguments.value()
    }

    pub fn canonical_arguments(&self) -> &CanonicalArguments {
        &self.arguments
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
        self.dynamic_resolutions.sort_by_cached_key(canonical_resolution);
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
    use crate::params::ToolParameters;
    use serde_json::json;

    fn args(value: serde_json::Value) -> CanonicalArguments {
        CanonicalArguments::from_value(&value, &ToolParameters::open()).expect("test arguments are dialect-valid")
    }

    fn call(tool: &str, value: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args(value))
    }

    #[test]
    fn digest_is_deterministic_and_key_order_independent() {
        let a = call("transfer", json!({ "to": "alice", "amount": 1 }));
        let b = call("transfer", json!({ "amount": 1, "to": "alice" }));
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn digest_separates_distinct_calls() {
        let base = call("transfer", json!({ "to": "a" }));
        let other_arg = call("transfer", json!({ "to": "b" }));
        let other_tool = call("refund", json!({ "to": "a" }));
        assert_ne!(base.digest(), other_arg.digest());
        assert_ne!(base.digest(), other_tool.digest());
    }

    #[test]
    fn dynamic_resolution_does_not_enter_the_call_digest() {
        let binding = DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "recipient".into(),
        };
        let base = call("send", json!({ "recipient": "room" }));
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
    fn pinned_answers_are_a_set_whatever_order_they_arrive_in() {
        let answer = |resolver: &str, reader: &str| {
            PinnedDynamicResolution::from_answer(
                DynamicAudienceBinding {
                    resolver: crate::names::DynamicResolverName::new(resolver),
                    argument: "recipient".into(),
                },
                Some(crate::label::Audience::restricted([crate::label::ReaderId::new(
                    reader,
                )])),
            )
        };
        let base = call("send", json!({ "recipient": "room" }));
        let one = base
            .clone()
            .with_dynamic_resolutions(vec![answer("directory", "alice"), answer("acl", "bob")]);
        let other = base.with_dynamic_resolutions(vec![answer("acl", "bob"), answer("directory", "alice")]);
        assert_eq!(one, other);
        assert_eq!(
            CanonicalDigest::of_batch([&one], None),
            CanonicalDigest::of_batch([&other], None)
        );
    }

    #[test]
    fn persisted_pinned_answers_refuse_a_non_canonical_spelling() {
        let answer = |resolver: &str| {
            PinnedDynamicResolution::from_answer(
                DynamicAudienceBinding {
                    resolver: crate::names::DynamicResolverName::new(resolver),
                    argument: "recipient".into(),
                },
                Some(crate::label::Audience::restricted([crate::label::ReaderId::new(
                    "alice",
                )])),
            )
        };
        let canonical = call("send", json!({ "recipient": "room" }))
            .with_dynamic_resolutions(vec![answer("acl"), answer("directory")]);
        let wire = serde_json::to_value(&canonical).expect("a call serializes");
        assert_eq!(
            serde_json::from_value::<ResolvedCall>(wire.clone()).expect("the canonical form round-trips"),
            canonical
        );

        for spelling in [
            vec![answer("directory"), answer("acl")],
            vec![answer("acl"), answer("acl"), answer("directory")],
        ] {
            let mut tampered = wire.clone();
            tampered["dynamic_resolutions"] = serde_json::to_value(&spelling).expect("answers serialize");
            assert!(serde_json::from_value::<ResolvedCall>(tampered).is_err());
        }
    }

    #[test]
    fn dispatch_id_distinguishes_occurrences() {
        let call = call("send", json!({}));
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
