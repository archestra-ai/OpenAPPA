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

/// A person's ruling the harness obtained itself for the offer a
/// control call quotes. A harness whose review channel is its own —
/// where the runtime's elicitation reaches no person — shows the
/// [`Review`] text and returns the answer here; the runtime spends it
/// as the human authority's answer for that one execution, exactly as
/// an elicitation's Accept or Decline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruling {
    Approve,
    Deny,
}

/// An offer whose plan consults a human authority, with the review as
/// the person reads it — the consult artifact alone: the authority and
/// its hint, the exact tool, the canonical arguments, and what the
/// ruling covers. Nothing the model said is in it, and the person's
/// answer never passes back through the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub offer: String,
    pub text: String,
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
        /// A ruling the harness already obtained for the offer this
        /// control call quotes; `None` on every ordinary call.
        ruling: Option<Ruling>,
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
        /// The remedies the block offers, in the order the feedback lists
        /// them, for a harness that routes one itself rather than through
        /// the model's control call.
        offers: Vec<OfferedRemedy>,
        /// The offers whose plans consult a human authority, for a
        /// harness that reviews through its own channel.
        review: Vec<Review>,
    },
    Block {
        reason: String,
    },
    ReplaceOutput {
        output: String,
    },
    /// What crosses to the parent is `value`, not the child's message as
    /// it spelled it: a shaped return crosses in canonical form, and a
    /// sanitized one as the sanitizer's derivation. A harness that
    /// delivers the child's own words has the child return `value`
    /// verbatim — the next stop that carries it crosses and answers
    /// `Ack`; a harness that speaks for the child returns `value` on its
    /// behalf the same way. A return that crosses as spoken answers
    /// `Ack`, so this variant never merely reports a crossing.
    ChildReturn {
        value: String,
    },
    /// The event is acknowledged, and `text` goes to the actor it names
    /// as context the harness hands that actor: at a child's start, the
    /// return contract the child works under.
    Context {
        text: String,
    },
    Refuse {
        detail: String,
    },
}

/// One remedy a block offers: the id `execute_remedy_plan` takes and,
/// where the plan declares a child's return, how that return crosses.
/// `None` for a plan that declares no return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedRemedy {
    pub id: String,
    pub returns: Option<OfferedReturn>,
}

/// How a return-declaring plan crosses the child's return: as the child
/// spoke it, or through the registered sanitizer named (`attest-schema`
/// attests a schema the declaration supplies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferedReturn {
    AsSpoken,
    Sanitized { sanitizer: String },
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
    /// The children of the actor's family a call's arguments name by
    /// the harness's own on-disk spellings of a child's transcript or
    /// output file. The runtime refuses the call when one names a child
    /// the family opened: a child's words reach its parent through the
    /// checked return only. A recognizer of the default spellings, not a
    /// guarantee that no other path reaches the file.
    pub names_children: fn(&Actor, &ProposedCall) -> Vec<TrajectoryId>,
}
