//! appa-runtime-api — the vocabulary the runtime and its harness
//! adapters share, and the canonical hook wire ([`wire`]) that carries
//! it between a host's adapter and the runtime.

mod wire;

pub use wire::{
    Accepted, Adapter, AsSpoken, DecisionName, Derived, DeriveFn, EventName, OutcomeStatus, PROTOCOL, WireDecision,
    WireEvent, WireOffer, WireOutcome, WireReturn, WireReview, WireRuling,
};

/// The hosts this runtime can serve. The one place harness names appear
/// as a closed set: each variant fixes a trajectory prefix, a raw tool
/// domain, and a spawn coverage rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterName {
    ClaudeCode,
    Kagent,
}

impl AdapterName {
    pub const ALL: [AdapterName; 2] = [AdapterName::ClaudeCode, AdapterName::Kagent];

    pub fn as_str(self) -> &'static str {
        match self {
            AdapterName::ClaudeCode => "claude-code",
            AdapterName::Kagent => "kagent",
        }
    }

    /// The prefix every trajectory id of this adapter carries.
    pub fn prefix(self) -> &'static str {
        match self {
            AdapterName::ClaudeCode => "cc",
            AdapterName::Kagent => "kagent",
        }
    }

    pub fn root(self, host_id: &str) -> TrajectoryId {
        TrajectoryId(format!("{}:{host_id}", self.prefix()))
    }
}

impl std::fmt::Display for AdapterName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AdapterName {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        AdapterName::ALL
            .into_iter()
            .find(|name| name.as_str() == text)
            .ok_or_else(|| format!("{text} is not an adapter; one of: claude-code, kagent"))
    }
}

/// A tool's canonical identity: `<family>/<namespace>/<tool>` for the
/// `mcp`, `host`, and `agent` families, and `appa/execute_remedy_plan`
/// as the whole `appa` family. Policies the runtime loads name tools this
/// way; an adapter maps its host's raw spelling onto it bijectively over
/// the host's raw domain. The runtime keys every fact on this identity;
/// the raw spelling stays with the host for dispatch and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalTool(String);

/// The runtime's own control tool, the one member of the `appa` family.
pub const CONTROL_TOOL: &str = "appa/execute_remedy_plan";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalToolError {
    Family { name: String },
    Shape { name: String },
    Segment { name: String, segment: String },
    Namespace { name: String },
    Control { name: String },
}

impl std::fmt::Display for CanonicalToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Family { name } => write!(f, "{name}: a canonical tool starts with mcp/, host/, agent/, or appa/"),
            Self::Shape { name } => write!(f, "{name}: a canonical tool is <family>/<namespace>/<tool>"),
            Self::Segment { name, segment } => {
                write!(f, "{name}: segment {segment:?} is not [A-Za-z0-9_.-]+")
            }
            Self::Namespace { name } => write!(f, "{name}: a namespace segment cannot contain __"),
            Self::Control { name } => write!(f, "{name}: the appa family has one member, {CONTROL_TOOL}"),
        }
    }
}

impl std::error::Error for CanonicalToolError {}

fn is_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

impl CanonicalTool {
    pub fn parse(name: &str) -> Result<Self, CanonicalToolError> {
        let refuse_shape = || CanonicalToolError::Shape { name: name.to_string() };
        let (family, rest) = name.split_once('/').ok_or_else(|| CanonicalToolError::Family {
            name: name.to_string(),
        })?;
        match family {
            "appa" => {
                if name == CONTROL_TOOL {
                    Ok(Self(name.to_string()))
                } else {
                    Err(CanonicalToolError::Control { name: name.to_string() })
                }
            }
            "mcp" | "host" | "agent" => {
                let (namespace, tool) = rest.split_once('/').ok_or_else(refuse_shape)?;
                for segment in [namespace, tool] {
                    if !is_segment(segment) {
                        return Err(CanonicalToolError::Segment {
                            name: name.to_string(),
                            segment: segment.to_string(),
                        });
                    }
                }
                if namespace.contains("__") {
                    return Err(CanonicalToolError::Namespace { name: name.to_string() });
                }
                Ok(Self(name.to_string()))
            }
            _ => Err(CanonicalToolError::Family {
                name: name.to_string(),
            }),
        }
    }

    /// Build one from its parts, refusing what [`CanonicalTool::parse`] refuses.
    pub fn of(family: &str, namespace: &str, tool: &str) -> Result<Self, CanonicalToolError> {
        Self::parse(&format!("{family}/{namespace}/{tool}"))
    }

    pub fn control() -> Self {
        Self(CONTROL_TOOL.to_string())
    }

    pub fn is_control(&self) -> bool {
        self.0 == CONTROL_TOOL
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for CanonicalTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod canonical_tests {
    use super::*;

    #[test]
    fn the_grammar_is_per_family() {
        for name in [
            "mcp/github/create_issue",
            "host/claude-code/Bash",
            "agent/kagent/log-analyst",
            "mcp/a.b-c_d/T.o-o_l",
            CONTROL_TOOL,
        ] {
            assert_eq!(CanonicalTool::parse(name).map(|tool| tool.into_string()), Ok(name.to_string()));
        }
        assert!(CanonicalTool::parse(CONTROL_TOOL).expect("control").is_control());
        assert!(!CanonicalTool::parse("mcp/appa/execute_remedy_plan").expect("mcp").is_control());
        for name in [
            "",
            "Bash",
            "mcp__github__x",
            "mcp/github",
            "mcp/github/",
            "mcp//x",
            "mcp/github/x/y",
            "mcp/a__b/x",
            "mcp/a b/x",
            "mcp/a(b)/x",
            "host/claude-code/Bash(command:ls)",
            "appa/other",
            "appa/execute_remedy_plan/x",
            "tool/x/y",
            "*",
        ] {
            assert!(CanonicalTool::parse(name).is_err(), "{name:?} must not parse");
        }
    }

    #[test]
    fn serde_and_names_round_trip() {
        let tool: CanonicalTool = serde_json::from_str(r#""mcp/github/x""#).expect("parses");
        assert_eq!(serde_json::to_string(&tool).expect("serializes"), r#""mcp/github/x""#);
        assert!(serde_json::from_str::<CanonicalTool>(r#""github""#).is_err());
        for name in AdapterName::ALL {
            assert_eq!(name.as_str().parse::<AdapterName>(), Ok(name));
            assert_eq!(serde_json::to_string(&name).expect("serializes"), format!("{:?}", name.as_str()));
        }
        assert!("ClaudeCode".parse::<AdapterName>().is_err());
        assert_eq!(AdapterName::ClaudeCode.root("s1").0, "cc:s1");
    }
}

impl serde::Serialize for CanonicalTool {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for CanonicalTool {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        CanonicalTool::parse(&name).map_err(serde::de::Error::custom)
    }
}

/// Identity of one trajectory (root or child). The adapter derives it
/// from the harness's own ids with a harness prefix; there is no
/// translation table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrajectoryId(pub String);

/// A model-directed tool call at the harness's execution boundary. The
/// arguments are the JSON spelling the harness would execute. The engine
/// canonicalizes them; the runtime and adapter do not parse or rewrite them.
/// `tool` is the host's raw spelling on the client side of the wire and
/// the canonical identity ([`CanonicalTool`]) once the runtime has read
/// the event; a host that embeds the runtime and calls it directly names
/// tools the way its own policy does.
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

/// A host's shape translation, run on the client side of the wire (the
/// `appa hook` client for Claude Code): parse the host's own hook bytes
/// into at most one typed event, whose tool spelling is still the raw
/// one; render one decision, for the event it answers, into the host's
/// hook JSON. Plain `fn` pointers — no trait, no captured state —
/// because an adapter that could hold state or reach the runtime would
/// breach the boundary this crate declares. Nothing here is trusted by
/// the runtime: what the runtime derives from a call, it derives itself
/// ([`Adapter::derive`]).
#[derive(Clone, Copy)]
pub struct Codec {
    pub parse: fn(&[u8]) -> Result<Option<HookEvent>, ParseRefusal>,
    pub render: fn(&HookEvent, &HookDecision) -> serde_json::Value,
}
