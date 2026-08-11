//! appa-runtime-api — the vocabulary the runtime and its harness
//! adapters share.

/// Identity of one trajectory (root or child). The adapter derives it
/// from the harness's own ids with a harness prefix; there is no
/// translation table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrajectoryId(pub String);

/// A tool call as the model proposed it. The engine canonicalizes;
/// the runtime and the adapter pass it through unchanged.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProposedCall {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// What a dispatched tool produced, in the runtime's typing. The
/// adapter owns the mapping from its harness's wire to this type.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Success { body: OutcomeBody },
    Failure { message: String },
    Indeterminate,
}

/// The tool output body, where available. `Unavailable` records a
/// success whose body the runtime refused to carry (for example, over
/// the byte cap — the cap is the runtime's to apply, so the adapter
/// carries the body it saw).
#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeBody {
    Available(String),
    Unavailable,
}

/// Which trajectory an event belongs to: the root, and the child when
/// the harness attributes the event to one.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub root: TrajectoryId,
    pub child: Option<TrajectoryId>,
}

/// One hook event in the runtime's vocabulary. The adapter maps each
/// of its harness's hooks onto exactly one variant — or onto no event
/// at all for hooks the deployment does not gate.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    SessionStart { root: TrajectoryId },
    Prompt { actor: Actor, text: String },
    ToolCall { actor: Actor, call: ProposedCall },
    ToolResult {
        actor: Actor,
        call: ProposedCall,
        outcome: ToolOutcome,
    },
    ChildStart { parent: TrajectoryId, child: TrajectoryId },
    ChildEnd {
        parent: TrajectoryId,
        child: TrajectoryId,
        value: Option<String>,
    },
}

/// The runtime's answer to one hook event. The adapter renders each
/// variant into its harness's wire; it interprets nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    Ack,
    AllowCall,
    PassControl,
    DenyCall { feedback: String },
    Block { reason: String },
    Refuse { detail: String },
}

/// A refusal at the parse stage, before any event exists. `Unreadable`
/// is not-even-JSON; `Malformed` parsed but misses a field its hook
/// requires. Both block the action: hooks fail closed.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseRefusal {
    Unreadable { detail: String },
    Malformed { detail: String },
}

/// One harness adapter's whole surface: two plain functions. Parse the
/// wire bytes into at most one event; render one decision into the
/// harness's wire JSON. Plain `fn` pointers — no trait, no captured
/// state — because an adapter that could hold state or reach the
/// runtime would breach the boundary this crate declares.
#[derive(Clone, Copy)]
pub struct Codec {
    pub parse: fn(&[u8]) -> Result<Option<HookEvent>, ParseRefusal>,
    pub render: fn(&HookDecision) -> serde_json::Value,
}
