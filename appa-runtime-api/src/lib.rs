//! appa-runtime-api — the vocabulary the runtime and its harness
//! adapters share.

/// Identity of one trajectory (root or child). The adapter derives it
/// from the harness's own ids with a harness prefix; there is no
/// translation table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrajectoryId(pub String);

/// A model-directed tool call at the harness's execution boundary. The
/// arguments are the JSON spelling the harness would execute. The engine
/// canonicalizes them; the runtime and adapter do not parse or rewrite them.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProposedCall {
    pub tool: String,
    pub arguments: Box<serde_json::value::RawValue>,
}

/// Equality is over the bytes as spelled, because that is the only
/// equality this type can answer: deciding that two spellings are the
/// same call needs the registered contract and the engine's
/// canonicalization, neither of which this crate has. Parsing here to
/// compare would reintroduce exactly what `arguments` exists to
/// prevent — `serde_json` would make `{"a":1,"a":2}` equal to the
/// admissible `{"a":2}`, though the engine refuses the first.
/// Callers that need same-call identity compare the
/// engine's canonical bytes; the runtime does
/// (`api::session::classify_report`).
impl PartialEq for ProposedCall {
    fn eq(&self, other: &Self) -> bool {
        self.tool == other.tool && self.arguments.get() == other.arguments.get()
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpawnBinding(pub String);

/// How a starting child names the spawn that prepared its fork.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SpawnRef {
    Binding(SpawnBinding),
    InFlight,
}

/// One hook event in the runtime's vocabulary. The adapter maps each
/// of its harness's hooks onto exactly one variant — or onto no event
/// at all for hooks the deployment does not gate.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    SessionStart {
        root: TrajectoryId,
    },
    Prompt {
        actor: Actor,
        text: String,
    },
    /// The actor finished a turn. Nothing it released is still running,
    /// so a dispatch still open names a call the harness never ran.
    TurnEnd {
        actor: Actor,
    },
    ToolCall {
        actor: Actor,
        call: ProposedCall,
        spawn: bool,
    },
    ToolResult {
        actor: Actor,
        call: ProposedCall,
        outcome: ToolOutcome,
    },
    ChildStart {
        root: TrajectoryId,
        child: TrajectoryId,
        spawn: SpawnRef,
    },
    ChildEnd {
        /// The root whose log this child's records land in. No
        /// parent: which branch this child was forked from is the log's own
        /// record, and a caller's claim about it would be a second answer.
        root: TrajectoryId,
        child: TrajectoryId,
        value: Option<String>,
    },
    SpawnResult {
        actor: Actor,
        call: ProposedCall,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    },
}

/// The runtime's answer to one hook event. The adapter renders each
/// variant into its harness's wire; it interprets nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    Ack,
    AllowCall {
        spawn: Option<SpawnBinding>,
    },
    PassControl,
    /// The call is blocked; `feedback` tells the model why — a requirement
    /// the trajectory's label does not meet, or the narrowing the call would
    /// cause.
    DenyCall {
        feedback: String,
    },
    Block {
        reason: String,
    },
    ReplaceOutput {
        output: String,
    },
    /// The child's return crosses as `value` and not as the child
    /// spelled it — a `return_sanitizer` produced it. A
    /// return that crosses unchanged answers `Ack`, so this variant
    /// names a substitution and never merely a crossing. A harness with
    /// no way to substitute the return where the parent receives it
    /// cannot enforce it.
    ChildReturn {
        value: String,
    },
    Refuse {
        detail: String,
    },
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
/// wire bytes into at most one event; render one decision, for the event
/// it answers, into the harness's wire JSON. Plain `fn` pointers — no
/// trait, no captured state — because an adapter that could hold state
/// or reach the runtime would breach the boundary this crate declares.
#[derive(Clone, Copy)]
pub struct Codec {
    pub parse: fn(&[u8]) -> Result<Option<HookEvent>, ParseRefusal>,
    pub render: fn(&HookEvent, &HookDecision) -> serde_json::Value,
}
