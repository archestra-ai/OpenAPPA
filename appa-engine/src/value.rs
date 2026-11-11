//! Values, provenance, and the identities that bind a ruling to the exact call it ruled on.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contract::PinnedAnnotation;
use crate::label::Label;
use crate::params::CanonicalArguments;

/// A peer message the runtime holds, pinned on the call that reads it: the held id, the body's
/// digest, and the label the message is held at. The trusted runtime supplies it from its own
/// store, never from model arguments. The engine knows no other host-pinned content: what an
/// external store holds reaches a call through its Annotator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldMessage {
    pub id: String,
    pub digest: String,
    pub label: Label,
}

/// The held message a record pins. A log written before a held message had a record of its
/// own pins it under `file_basis` as a file read whose version is the held id. That is the same
/// message, and any other file basis is refused as undecodable.
pub(crate) fn recorded_held<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<HeldMessage>, D::Error> {
    #[derive(Deserialize)]
    struct FileRead {
        version: String,
        digest: String,
        label: Label,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Recorded {
        Held(HeldMessage),
        FileRead {
            #[serde(rename = "Read")]
            read: FileRead,
        },
    }
    Ok(
        Option::<Recorded>::deserialize(deserializer)?.map(|recorded| match recorded {
            Recorded::Held(held) => held,
            Recorded::FileRead { read } => HeldMessage {
                id: read.version,
                digest: read.digest,
                label: read.label,
            },
        }),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(name: impl Into<String>) -> Self {
        ToolName(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Internal normalization of a host-native MCP rule, not a callable name.
    /// Only the server component varies; this is not a general glob language.
    pub fn is_name_selector(&self) -> bool {
        self.0.strip_prefix("mcp/*/").is_some_and(valid_tool_segment)
    }

    pub(crate) fn matches_name(&self, actual: &ToolName) -> bool {
        if self == actual {
            return true;
        }
        let Some(leaf) = self.0.strip_prefix("mcp/*/").filter(|_| self.is_name_selector()) else {
            return false;
        };
        let Some((server, tool)) = actual.0.strip_prefix("mcp/").and_then(|rest| rest.split_once('/')) else {
            return false;
        };
        valid_tool_segment(server) && !server.contains("__") && tool == leaf
    }
}

fn valid_tool_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub use appa_runtime_api::TrajectoryId;

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
pub struct CanonicalDigest(#[serde(with = "crate::hex32")] [u8; 32]);

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
    /// ordered rendered digest and the annotation evidence pinned to it, so a repeat carrying the
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
            if let Some(pinned) = &call.annotation {
                hasher.update([3u8]);
                hasher.update(canonical_json(pinned));
            }
            if let Some(held) = &call.held {
                hasher.update([4u8]);
                hasher.update(canonical_json(held));
            }
        }
        CanonicalDigest(hasher.finalize().into())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn canonical_json<T: Serialize>(pinned: &T) -> Vec<u8> {
    serde_json_canonicalizer::to_vec(pinned).expect("a pinned answer canonicalizes")
}

/// A digest of a raw tool result. Binds a child-return derivation to the bytes it
/// derived from, so a later differing result cannot silently reuse an old derivative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RawResultDigest(#[serde(with = "crate::hex32")] [u8; 32]);

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
pub struct BlockId(#[serde(with = "crate::hex32")] [u8; 32]);

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

    /// The stage one confined candidate surfaces. Bound to the dispatch and to the
    /// generation the candidate stands at, so each successive stage of the same confinement is a
    /// stage of its own even under a repeated nonce.
    pub(crate) fn of_candidate(
        nonce: &OfferNonce,
        dispatch: &DispatchId,
        generation: crate::basis::SubjectGeneration,
    ) -> Self {
        BlockId(
            Framed::tagged(b"appa.block.v1")
                .field(&nonce.0)
                .field(dispatch.trajectory().0.as_bytes())
                .field(dispatch.digest().0.as_slice())
                .field(&generation.value().to_be_bytes())
                .finish(),
        )
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One executable offer's identity. Derived from the act's nonce and the
/// plan's content, so nobody allocates it and a replayed act reproduces the identical menu
/// rather than a second identity for one plan. It is the engine's canonical identity: which
/// trajectory may execute an offer is the runtime's question, answered from the harness channel
/// and never from this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferId(#[serde(with = "crate::hex32")] [u8; 32]);

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

    /// The offer's lowercase-hex wire form. Runtime surfaces this string to the model and routes
    /// `execute_remedy_plan` by it; the id itself stays engine-derived.
    pub fn to_hex(&self) -> String {
        crate::hex32::encode(&self.0)
    }

    /// Parse an offer id the model named back into its 32 bytes. Untrusted input: a wrong length or
    /// a non-hex character is refused, never guessed. A well-formed id this family never
    /// opened is not this parser's concern — the engine refuses it as an unknown offer.
    pub fn from_hex(text: &str) -> Result<OfferId, OfferIdParseError> {
        crate::hex32::decode(text).map(OfferId).ok_or(OfferIdParseError)
    }
}

/// A named offer id that is not 64 lowercase-hex characters. The runtime maps it to
/// unknown-offer feedback; it is never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("an offer id is 64 lowercase-hex characters")]
pub struct OfferIdParseError;

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
/// is a new dispatch and so a new fork. A single spawn's fork does not name its child — the host
/// does not know that yet when the spawn is released. A fan-out spawn's member fork does: it is
/// prepared when that child starts, one per child.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ForkId {
    Spawn(DispatchId),
    Member { spawn: DispatchId, child: TrajectoryId },
}

impl ForkId {
    pub fn of(dispatch: &DispatchId) -> Self {
        ForkId::Spawn(dispatch.clone())
    }

    pub fn member(spawn: &DispatchId, child: &TrajectoryId) -> Self {
        ForkId::Member {
            spawn: spawn.clone(),
            child: child.clone(),
        }
    }

    /// The spawn dispatch that released this fork, or the fan-out spawn a member belongs to.
    pub fn dispatch(&self) -> &DispatchId {
        match self {
            ForkId::Spawn(dispatch) | ForkId::Member { spawn: dispatch, .. } => dispatch,
        }
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
/// These are the admitted values, and only these: a user turn or other principal
/// context is outside engine policy and admits nothing, so it has no provenance here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    ToolResult {
        dispatch: DispatchId,
    },
    ChildReturn {
        child: TrajectoryId,
        id: ChildReturnId,
    },
    ProviderRun {
        tool: ToolName,
        batch: crate::transition::ProposalBatchId,
        position: u32,
        effects: crate::fact::EffectSet,
        #[serde(default, skip_serializing_if = "crate::audience::AudienceEvidence::is_empty")]
        evidence: crate::audience::AudienceEvidence,
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

/// The ordinal of a tool declaration among declarations for the same harness tool, as one
/// registry selects it. Never stored: a recorded call is read under its judging registry, which
/// selects afresh by tool and arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ToolDeclarationId(u32);

impl ToolDeclarationId {
    pub(crate) fn new(ordinal: usize) -> Option<Self> {
        ordinal.try_into().ok().map(ToolDeclarationId)
    }

    #[cfg(test)]
    pub(crate) fn ordinal(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedCall {
    tool: ToolName,
    arguments: CanonicalArguments,
    annotation: Option<PinnedAnnotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    held: Option<HeldMessage>,
}

impl<'de> Deserialize<'de> for ResolvedCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireCall {
            tool: ToolName,
            arguments: CanonicalArguments,
            annotation: Option<PinnedAnnotation>,
            #[serde(default, alias = "file_basis", deserialize_with = "recorded_held")]
            held: Option<HeldMessage>,
        }

        let wire = WireCall::deserialize(deserializer)?;
        Ok(ResolvedCall::new(wire.tool, wire.arguments)
            .with_annotation(wire.annotation)
            .with_held(wire.held))
    }
}

impl ResolvedCall {
    pub(crate) fn new(tool: ToolName, arguments: CanonicalArguments) -> Self {
        ResolvedCall {
            tool,
            arguments,
            annotation: None,
            held: None,
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

    /// Attach the complete annotation pinned to this call. `None` is the static case: the
    /// declaration is the annotation, and the engine reads it from the registry. Whether a pinned
    /// annotation is admissible on this call is [`crate::check::validate_annotation`]'s to decide.
    pub fn with_annotation(mut self, annotation: Option<PinnedAnnotation>) -> Self {
        self.annotation = annotation;
        self
    }

    pub fn annotation(&self) -> Option<&PinnedAnnotation> {
        self.annotation.as_ref()
    }

    /// The held peer message this call reads, where the runtime pinned one.
    pub fn held(&self) -> Option<&HeldMessage> {
        self.held.as_ref()
    }

    pub(crate) fn with_held(mut self, held: Option<HeldMessage>) -> Self {
        self.held = held;
        self
    }

    /// Label of the value returned by this call.
    pub fn output_label(&self, contract: &crate::contract::ToolAnnotation) -> Label {
        self.output_label_from(&contract.output_label())
    }

    /// [`ResolvedCall::output_label`] from a declared output label already resolved: the one an
    /// outcome reads, which meets the terms that released the call with the judging policy's.
    /// A read of a held message returns that message, so its result carries the message's label.
    pub(crate) fn output_label_from(&self, declared: &Label) -> Label {
        match &self.held {
            None => declared.clone(),
            Some(held) => held.label.combine(declared),
        }
    }

    /// The canonical digest of this exact rendered call, recomputed from the tool and arguments.
    /// The pinned answers are **not** part of it: a repeat is the same rendered call whatever an
    /// Annotator said, which is why anything holding a call by identity alone must compare the call
    /// itself where the answers decide a check.
    pub fn digest(&self) -> CanonicalDigest {
        CanonicalDigest::of_call(&self.tool, &self.arguments)
    }

    /// Is this the call another proposal renders, up to the held message the host pins? The
    /// callee, its canonical arguments and its annotation are the call; the held message is
    /// evidence the host pins for one proposal, and a record of a call carries none. A staged
    /// derivation is matched this way so a held read can take one at all.
    pub(crate) fn renders(&self, other: &ResolvedCall) -> bool {
        self.tool == other.tool && self.arguments == other.arguments && self.annotation == other.annotation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{AnnotationEvidence, Delta, PinnedAnnotation, ProducedAnnotation, Requires};
    use crate::label::{Audience, ReaderId, Trust};
    use crate::params::ToolParameters;
    use serde_json::json;

    fn args(value: serde_json::Value) -> CanonicalArguments {
        CanonicalArguments::from_value(&value, &ToolParameters::open()).expect("test arguments are dialect-valid")
    }

    fn call(tool: &str, value: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args(value))
    }

    fn pinned(bound_to: &ResolvedCall, annotator: &str) -> PinnedAnnotation {
        PinnedAnnotation::new(
            crate::names::AnnotatorName::new(annotator),
            bound_to.digest(),
            ProducedAnnotation {
                evidence: None,
                delta: Delta {
                    trust: Some(crate::label::Trust::new(0)),
                    audience: None,
                },
                forwards: Delta::NONE,
                emits: Default::default(),
                requires: Requires::default(),
            },
        )
    }

    #[test]
    fn a_held_read_returns_the_held_label_met_with_the_delta() {
        let held = HeldMessage {
            id: "h1".into(),
            digest: "d1".into(),
            label: Label::new(Trust::new(0), Audience::restricted([ReaderId::new("insider")])),
        };
        let read = call("read_peer_message", json!({ "id": "h1" })).with_held(Some(held.clone()));
        let declared = Label::new(Trust::new(1), Audience::public());
        assert_eq!(read.output_label_from(&declared), held.label.combine(&declared));
        assert_eq!(
            call("read_peer_message", json!({})).output_label_from(&declared),
            declared
        );
        assert_ne!(
            CanonicalDigest::of_batch([&read], None),
            CanonicalDigest::of_batch([&call("read_peer_message", json!({ "id": "h1" }))], None),
            "the held message is part of the batch identity"
        );
    }

    #[test]
    fn a_pinned_annotation_is_part_of_the_batch_identity() {
        let bare = call("Bash", json!({ "command": "ls" }));
        let annotated = |annotator: &str| bare.clone().with_annotation(Some(pinned(&bare, annotator)));
        let unpinned = CanonicalDigest::of_batch([&bare], None);
        assert_ne!(unpinned, CanonicalDigest::of_batch([&annotated("a")], None));
        assert_ne!(
            CanonicalDigest::of_batch([&annotated("a")], None),
            CanonicalDigest::of_batch([&annotated("b")], None)
        );
        assert_eq!(
            CanonicalDigest::of_batch([&annotated("a")], None),
            CanonicalDigest::of_batch([&annotated("a")], None)
        );
        // The same read judged over two versions of a file is two batches.
        let read = call("Read", json!({ "file_path": "/w/notes.md" }));
        let over = |version: &str| {
            let produced = ProducedAnnotation {
                evidence: Some(AnnotationEvidence::new(version).expect("short evidence")),
                ..pinned(&read, "file-tracker").produced().clone()
            };
            let pin = PinnedAnnotation::new(
                crate::names::AnnotatorName::new("file-tracker"),
                read.digest(),
                produced,
            );
            read.clone().with_annotation(Some(pin))
        };
        assert_ne!(
            CanonicalDigest::of_batch([&over("sha256:aa")], None),
            CanonicalDigest::of_batch([&over("sha256:bb")], None)
        );
        assert_eq!(over("sha256:aa").digest(), over("sha256:bb").digest());
    }

    #[test]
    fn an_offer_id_round_trips_through_its_hex_wire_form() {
        let id = OfferId::of_plan(&BlockId([7u8; 32]), 3, b"plan-bytes");
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(OfferId::from_hex(&hex), Ok(id));
    }

    #[test]
    fn a_malformed_offer_id_is_refused_not_guessed() {
        assert_eq!(OfferId::from_hex("abc"), Err(OfferIdParseError));
        assert_eq!(OfferId::from_hex(&"g".repeat(64)), Err(OfferIdParseError));
        assert_eq!(OfferId::from_hex(&"AB".repeat(32)), Err(OfferIdParseError));
    }

    #[test]
    fn every_identity_takes_its_hex_form_on_the_record() {
        let block = BlockId([7u8; 32]);
        let offer = OfferId::of_plan(&block, 3, b"plan-bytes");
        let raw = RawResultDigest::of(b"result");
        let canonical = call("transfer", json!({ "to": "alice" })).digest();

        assert_eq!(json!(offer), json!(offer.to_hex()));
        assert_eq!(json!(block), json!(crate::hex32::encode(block.bytes())));
        assert_eq!(json!(raw), json!(crate::hex32::encode(raw.bytes())));
        assert_eq!(json!(canonical), json!(crate::hex32::encode(canonical.bytes())));

        assert_eq!(
            serde_json::from_value::<OfferId>(json!(offer)).expect("an offer id reads back"),
            offer
        );
        assert_eq!(
            serde_json::from_value::<BlockId>(json!(block)).expect("a block id reads back"),
            block
        );
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
    fn annotation_evidence_is_not_rendered_call_identity() {
        let base = call("lookup", json!({ "id": 7, "deep": true }));
        let resolved = base.clone().with_annotation(Some(pinned(&base, "classifier")));
        assert_eq!(base.digest(), resolved.digest());
    }

    #[test]
    fn a_pinned_annotation_round_trips_through_the_calls_wire_form() {
        let unpinned = call("Bash", json!({ "command": "ls" }));
        let annotated = unpinned.clone().with_annotation(Some(pinned(&unpinned, "classifier")));
        let wire = serde_json::to_value(&annotated).expect("a call serializes");
        assert_eq!(
            serde_json::from_value::<ResolvedCall>(wire).expect("the wire form round-trips"),
            annotated
        );
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
