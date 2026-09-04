//! The canonical hook wire: the one JSON shape `POST /hook` accepts and
//! the one it answers with, versioned by [`PROTOCOL`].
//!
//! A host's adapter translates its own event shape into a [`WireEvent`]
//! on the client side (Claude Code through `appa hook`, kagent inside its
//! ADK plugin) and reads a [`WireDecision`] back. The wire carries the
//! host's raw tool spelling and nothing the runtime would have to trust:
//! whether a call is a spawn, which canonical tool it names, and whether
//! it is the runtime's own control tool are all derived on the server
//! from the configured adapter and the raw spelling
//! ([`Adapter::derive`]). A result's lifecycle follows that same
//! derivation and not the event name it arrived under, so `tool_result`
//! and `spawn_result` differ only in the fields they may carry;
//! whether a proposed call's arguments name a
//! child's transcript is the same adapter's separate answer
//! ([`Adapter::names_children`]), asked at the call, where it is used.
//! Ids cross unprefixed and the server applies the configured adapter's
//! prefix, so no caller can speak for another adapter's trajectories.
//! A person's
//! [`Ruling`] crosses only under a host that reviews through its own
//! channel ([`AdapterName::review_channel`]), and there only on the
//! control call that quotes the offer it answers. Under any other host,
//! on any other call, or on any other event — no other event reads one —
//! the envelope asserting one is refused.
//!
//! A decision that stands in for a result says on the wire which of the
//! two it carries, because a client may rewrite one and may not rewrite
//! the other. `deliver_value` and `child_return` carry a `value` the
//! engine admitted: the client delivers those bytes as they crossed. A
//! decision carrying the runtime's own words — `replace_output`,
//! `deny_call`, `block`, `refuse` — carries text authored here, which
//! names tools by the spelling that host sent, so a client whose model
//! dispatches other names spells them back before the model reads it.
//! No client tells the two apart by reading the payload.
//!
//! The event and decision structs are flat with optional fields rather
//! than tagged enums because `arguments` is a `RawValue`, which serde's
//! internally tagged representation cannot carry.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{
    Actor, AdapterName, CanonicalTool, HookDecision, HookEvent, OfferedRemedy, OfferedReturn, OutcomeBody,
    ParseRefusal, ProposedCall, Review, ReviewChannel, Ruling, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId,
};

/// The protocol this crate speaks. A wire event or decision carrying
/// another number is refused; there is no negotiation.
pub const PROTOCOL: u32 = 1;

/// What the server derives from a configured adapter and the raw tool
/// spelling of one call. The runtime keys every fact on `canonical`;
/// the raw spelling never enters an engine fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub canonical: CanonicalTool,
    /// The call starts a child trajectory (Claude Code's `Agent`, a
    /// kagent agent called as a tool).
    pub spawn: bool,
}

/// The server-side derivation for one raw tool spelling: total over the
/// adapter's raw domain, a refusal outside it. The spelling is all it
/// reads — who calls and with which arguments decides no identity, and
/// the one argument-dependent question an adapter answers is
/// [`NamesChildrenFn`]. A plain `fn` pointer — no state, no runtime
/// access — for the same reason [`Codec`](crate::Codec) is.
pub type DeriveFn = fn(&str) -> Result<Derived, ParseRefusal>;

/// The family children one call's arguments name by the host's own
/// on-disk spellings of a child's transcript or output file. A
/// recognizer of the default spellings, not a guarantee that no other
/// path reaches the file.
///
/// A separate question from [`DeriveFn`] because only a tool call has an
/// answer to it: an outcome is observed after the fact and names no
/// child, so the scan over the arguments is never asked for there. A
/// plain `fn` pointer for the same reason [`DeriveFn`] is.
pub type NamesChildrenFn = fn(&Actor, &ProposedCall) -> Vec<TrajectoryId>;

/// The inverse of [`DeriveFn`]: the host's own spelling of one canonical
/// identity — the name this host's model can dispatch. Both identities
/// are kept, the canonical one to key every fact and this one to address
/// the model and the host.
///
/// Total over the canonical ids the adapter's [`DeriveFn`] produces, and
/// that derivation's inverse there. Any other canonical id — one no raw
/// spelling of this host maps onto — has no host spelling and answers
/// `None`. A plain `fn` pointer for the same reason [`DeriveFn`] is.
pub type SpellFn = fn(&CanonicalTool) -> Option<String>;

/// One adapter as the runtime serves it: its name, which fixes the
/// trajectory prefix, the spawn coverage rule and the review channel,
/// the two directions of its tool identity map, and the children a
/// proposed call names.
#[derive(Clone, Copy)]
pub struct Adapter {
    pub name: AdapterName,
    pub derive: DeriveFn,
    pub names_children: NamesChildrenFn,
    pub spell: SpellFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventName {
    Ping,
    SessionStart,
    Prompt,
    TurnEnd,
    ToolCall,
    ToolResult,
    SpawnResult,
    ChildStart,
    ChildEnd,
}

/// One optional field of the flat envelope, as a value the reading
/// rule below can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    RootId,
    ChildId,
    Text,
    Tool,
    Arguments,
    Ruling,
    Outcome,
    SpawnedId,
    Value,
    SpawnBinding,
}

impl Field {
    const ALL: [Field; 10] = [
        Field::RootId,
        Field::ChildId,
        Field::Text,
        Field::Tool,
        Field::Arguments,
        Field::Ruling,
        Field::Outcome,
        Field::SpawnedId,
        Field::Value,
        Field::SpawnBinding,
    ];

    fn spelling(self) -> &'static str {
        match self {
            Field::RootId => "root_id",
            Field::ChildId => "child_id",
            Field::Text => "text",
            Field::Tool => "tool",
            Field::Arguments => "arguments",
            Field::Ruling => "ruling",
            Field::Outcome => "outcome",
            Field::SpawnedId => "spawned_id",
            Field::Value => "value",
            Field::SpawnBinding => "spawn_binding",
        }
    }
}

/// The fields one event reads. The envelope is flat, so a field the
/// named event never reads would cross to no reader and be dropped
/// unread — a `turn_end` carrying a result closes as unsettled the
/// dispatch that result reports, and a ruling on an event that spends
/// none is a person's answer lost. What an event does not read, it
/// refuses.
fn fields_read(name: EventName) -> &'static [Field] {
    match name {
        // A probe reads nothing, but a client that builds every
        // envelope the same way carries its actor into one; that costs
        // no reader and is admitted. What a probe may not carry is a
        // dispatch — no call, no result, no ruling.
        EventName::Ping => &[Field::RootId, Field::ChildId],
        EventName::SessionStart => &[Field::RootId],
        EventName::Prompt => &[Field::RootId, Field::ChildId, Field::Text],
        EventName::TurnEnd => &[Field::RootId, Field::ChildId],
        EventName::ToolCall => &[
            Field::RootId,
            Field::ChildId,
            Field::Tool,
            Field::Arguments,
            Field::Ruling,
        ],
        EventName::ToolResult | EventName::SpawnResult => &[
            Field::RootId,
            Field::ChildId,
            Field::Tool,
            Field::Arguments,
            Field::Outcome,
            Field::SpawnedId,
            Field::Value,
        ],
        EventName::ChildStart => &[Field::RootId, Field::ChildId, Field::SpawnBinding],
        EventName::ChildEnd => &[Field::RootId, Field::ChildId, Field::Value],
    }
}

/// A ruling is a person's answer the host obtained through its own
/// review channel, and the runtime spends it as the human authority's.
/// Under a host that has no such channel ([`ReviewChannel::Runtime`])
/// nothing on the wire could have carried a person's answer, so the
/// envelope is refused — a local process that reaches `/hook` cannot
/// spell an approval that host never asked for.
fn checked_ruling(adapter: AdapterName, ruling: Option<Ruling>) -> Result<Option<Ruling>, ParseRefusal> {
    match (ruling, adapter.review_channel()) {
        (None, _) => Ok(None),
        (Some(ruling), ReviewChannel::Host) => Ok(Some(ruling)),
        (Some(_), ReviewChannel::Runtime) => Err(malformed(format!(
            "a ruling under adapter {adapter}, whose host reviews through no channel of its own"
        ))),
    }
}

/// The status a wire outcome carries. `success_without_body` is the
/// success whose body the wire does not carry, so that an available
/// body is exactly the `body` field — JSON `null` included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Success,
    SuccessWithoutBody,
    Failure,
    Indeterminate,
}

/// A status as the wire spells it, so a refusal names what was posted.
fn status_name(status: OutcomeStatus) -> &'static str {
    match status {
        OutcomeStatus::Success => "success",
        OutcomeStatus::SuccessWithoutBody => "success_without_body",
        OutcomeStatus::Failure => "failure",
        OutcomeStatus::Indeterminate => "indeterminate",
    }
}

/// A dispatched tool's outcome as the host observed it. Each status has
/// exactly one shape: `success` carries `body` and no `message`,
/// `failure` carries `message` and no `body`, and
/// `success_without_body` and `indeterminate` carry neither. Any other
/// combination is malformed — a field the status has no place for is an
/// observation the host made, and dropping it would lose it from the
/// trajectory.
///
/// `body` is the JSON value as spelled, so a present `null` is a body
/// that is `null` and not an absent one. `success` without the field is
/// malformed, never a guess: the host that carries no body says so with
/// `success_without_body`.
///
/// `message` keeps its presence the same way, and its type is checked
/// apart from it: the outer `Option` is whether the host spelled the
/// field, the inner one whether it spelled a string there. A status
/// with no place for a message refuses one that is `null` exactly as it
/// refuses one that is a string, and a `failure` whose message is
/// `null` carries none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireOutcome {
    pub status: OutcomeStatus,
    #[serde(default, deserialize_with = "present_body", skip_serializing_if = "Option::is_none")]
    pub body: Option<Box<RawValue>>,
    #[serde(
        default,
        deserialize_with = "present_message",
        skip_serializing_if = "Option::is_none"
    )]
    pub message: Option<Option<String>>,
}

/// `Option`'s own deserializer reads a present `null` as absent, which
/// would lose the difference this encoding exists to keep. Serde calls
/// this only for a field that is there, so `default` is the absent case
/// and everything else — `null` included — is the value as spelled.
fn present_body<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

/// The same for `message`, whose value is a string: the field's
/// presence is this function's `Some`, and the `null` a host spelled
/// there stays the inner `None` instead of folding into an absent
/// field.
fn present_message<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

impl WireOutcome {
    fn of(outcome: &ToolOutcome) -> Result<Self, ParseRefusal> {
        Ok(match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } => Self {
                status: OutcomeStatus::Success,
                body: Some(
                    RawValue::from_string(body.clone())
                        .map_err(|error| malformed(format!("a success outcome whose body is not JSON: {error}")))?,
                ),
                message: None,
            },
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => Self {
                status: OutcomeStatus::SuccessWithoutBody,
                body: None,
                message: None,
            },
            ToolOutcome::Failure { message } => Self {
                status: OutcomeStatus::Failure,
                body: None,
                message: Some(Some(message.clone())),
            },
            ToolOutcome::Indeterminate => Self {
                status: OutcomeStatus::Indeterminate,
                body: None,
                message: None,
            },
        })
    }

    /// The one shape each status has, and a refusal naming the status
    /// and the offending field for everything else. A field the status
    /// has no place for is refused rather than dropped: the host
    /// observed it, and an envelope that fails closed keeps it out of
    /// the trajectory without losing that it was there. Presence alone
    /// decides that refusal: a field spelled `null` is one the host
    /// spelled, and the value's type is checked after it, where a
    /// `failure` reads the message it carries.
    fn into_outcome(self) -> Result<ToolOutcome, ParseRefusal> {
        let status = status_name(self.status);
        let without = |field: &str| malformed(format!("a {status} outcome without its {field}"));
        let carrying = |field: &str| malformed(format!("a {status} outcome carrying a {field}"));
        let spelled_null = |field: &str| malformed(format!("a {status} outcome whose {field} is null"));
        match (self.status, self.body, self.message) {
            // The body is the largest thing on this path: it moves out of
            // its box as the string it already is, rather than being copied
            // back out of it.
            (OutcomeStatus::Success, Some(body), None) => Ok(ToolOutcome::Success {
                body: OutcomeBody::Available(String::from(Box::<str>::from(body))),
            }),
            (OutcomeStatus::Success, Some(_), Some(_)) => Err(carrying("message")),
            (OutcomeStatus::Success, None, _) => Err(without("body")),
            (OutcomeStatus::SuccessWithoutBody, None, None) => Ok(ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            }),
            (OutcomeStatus::SuccessWithoutBody, Some(_), _) => Err(carrying("body")),
            (OutcomeStatus::SuccessWithoutBody, None, Some(_)) => Err(carrying("message")),
            (OutcomeStatus::Failure, None, Some(Some(message))) => Ok(ToolOutcome::Failure { message }),
            (OutcomeStatus::Failure, None, Some(None)) => Err(spelled_null("message")),
            (OutcomeStatus::Failure, Some(_), _) => Err(carrying("body")),
            (OutcomeStatus::Failure, None, None) => Err(without("message")),
            (OutcomeStatus::Indeterminate, None, None) => Ok(ToolOutcome::Indeterminate),
            (OutcomeStatus::Indeterminate, Some(_), _) => Err(carrying("body")),
            (OutcomeStatus::Indeterminate, None, Some(_)) => Err(carrying("message")),
        }
    }
}

/// One hook event on the wire. Which fields an event kind requires is
/// [`WireEvent::into_event`]'s to check; a client builds one with
/// [`WireEvent::from_event`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEvent {
    pub protocol: u32,
    pub adapter: AdapterName,
    pub event: EventName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The host's raw tool spelling, inside the adapter's raw domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruling: Option<Ruling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<WireOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_binding: Option<String>,
}

/// A parsed wire event with what the server derived from it.
#[derive(Debug, Clone)]
pub struct Accepted {
    pub event: HookEvent,
    /// Set on a tool call only; empty otherwise, because a tool call is
    /// the one event whose named children are asked for.
    pub names_children: Vec<TrajectoryId>,
}

fn malformed(detail: impl Into<String>) -> ParseRefusal {
    ParseRefusal::Malformed { detail: detail.into() }
}

impl WireEvent {
    fn bare(adapter: AdapterName, event: EventName) -> Self {
        Self {
            protocol: PROTOCOL,
            adapter,
            event,
            root_id: None,
            child_id: None,
            text: None,
            tool: None,
            arguments: None,
            ruling: None,
            outcome: None,
            spawned_id: None,
            value: None,
            spawn_binding: None,
        }
    }

    /// Whether this envelope carries `field` at all. An empty string is
    /// carried: only the reading arms decide what an empty value means.
    fn carries(&self, field: Field) -> bool {
        match field {
            Field::RootId => self.root_id.is_some(),
            Field::ChildId => self.child_id.is_some(),
            Field::Text => self.text.is_some(),
            Field::Tool => self.tool.is_some(),
            Field::Arguments => self.arguments.is_some(),
            Field::Ruling => self.ruling.is_some(),
            Field::Outcome => self.outcome.is_some(),
            Field::SpawnedId => self.spawned_id.is_some(),
            Field::Value => self.value.is_some(),
            Field::SpawnBinding => self.spawn_binding.is_some(),
        }
    }

    /// Read one wire body. Not JSON is `Unreadable`; JSON that is not a
    /// wire event, or names another protocol or adapter, is `Malformed`.
    /// Both block the action.
    pub fn read(body: &[u8]) -> Result<Self, ParseRefusal> {
        match serde_json::from_slice::<WireEvent>(body) {
            Ok(event) => Ok(event),
            Err(error) => match serde_json::from_slice::<serde_json::Value>(body) {
                Ok(_) => Err(malformed(format!("not a wire event: {error}"))),
                Err(_) => Err(ParseRefusal::Unreadable {
                    detail: format!("unreadable hook event: {error}"),
                }),
            },
        }
    }

    /// The client's translation: the host's typed event, whose tool
    /// spelling is still the raw one and whose ids carry the adapter's
    /// prefix, onto the wire without the prefix.
    pub fn from_event(adapter: AdapterName, event: &HookEvent) -> Result<Self, ParseRefusal> {
        let ids = |actor: &Actor| host_ids(adapter, actor);
        let wire = match event {
            HookEvent::SessionStart { root } => {
                let (root_id, _) = ids(&Actor {
                    root: root.clone(),
                    child: None,
                })?;
                Self {
                    root_id: Some(root_id),
                    ..Self::bare(adapter, EventName::SessionStart)
                }
            }
            HookEvent::Prompt { actor, text } => {
                let (root_id, child_id) = ids(actor)?;
                Self {
                    root_id: Some(root_id),
                    child_id,
                    text: Some(text.clone()),
                    ..Self::bare(adapter, EventName::Prompt)
                }
            }
            HookEvent::TurnEnd { actor } => {
                let (root_id, child_id) = ids(actor)?;
                Self {
                    root_id: Some(root_id),
                    child_id,
                    ..Self::bare(adapter, EventName::TurnEnd)
                }
            }
            HookEvent::ToolCall {
                actor, call, ruling, ..
            } => {
                let (root_id, child_id) = ids(actor)?;
                Self {
                    root_id: Some(root_id),
                    child_id,
                    tool: Some(call.tool.clone()),
                    arguments: Some(call.arguments.clone()),
                    ruling: checked_ruling(adapter, *ruling)?,
                    ..Self::bare(adapter, EventName::ToolCall)
                }
            }
            HookEvent::ToolResult { actor, call, outcome } => {
                let (root_id, child_id) = ids(actor)?;
                Self {
                    root_id: Some(root_id),
                    child_id,
                    tool: Some(call.tool.clone()),
                    arguments: Some(call.arguments.clone()),
                    outcome: Some(WireOutcome::of(outcome)?),
                    ..Self::bare(adapter, EventName::ToolResult)
                }
            }
            HookEvent::SpawnResult {
                actor,
                call,
                outcome,
                child,
                value,
            } => {
                let (root_id, child_id) = ids(actor)?;
                let spawned_id = match child {
                    Some(child) => Some(child_host_id(&actor.root, child)?),
                    None => None,
                };
                Self {
                    root_id: Some(root_id),
                    child_id,
                    tool: Some(call.tool.clone()),
                    arguments: Some(call.arguments.clone()),
                    outcome: Some(WireOutcome::of(outcome)?),
                    spawned_id,
                    value: value.clone(),
                    ..Self::bare(adapter, EventName::SpawnResult)
                }
            }
            HookEvent::ChildStart { root, child, spawn } => {
                let (root_id, _) = ids(&Actor {
                    root: root.clone(),
                    child: None,
                })?;
                Self {
                    root_id: Some(root_id),
                    child_id: Some(child_host_id(root, child)?),
                    spawn_binding: match spawn {
                        SpawnRef::Binding(binding) => Some(binding.0.clone()),
                        SpawnRef::InFlight => None,
                    },
                    ..Self::bare(adapter, EventName::ChildStart)
                }
            }
            HookEvent::ChildEnd { root, child, value } => {
                let (root_id, _) = ids(&Actor {
                    root: root.clone(),
                    child: None,
                })?;
                Self {
                    root_id: Some(root_id),
                    child_id: Some(child_host_id(root, child)?),
                    value: value.clone(),
                    ..Self::bare(adapter, EventName::ChildEnd)
                }
            }
        };
        Ok(wire)
    }

    /// The server's reading: the typed event with the configured
    /// adapter's prefixes and derivation applied, or `None` for a ping.
    pub fn into_event(self, served: &Adapter) -> Result<Option<Accepted>, ParseRefusal> {
        if self.protocol != PROTOCOL {
            return Err(malformed(format!(
                "protocol {} is not served; this runtime speaks protocol {PROTOCOL}",
                self.protocol
            )));
        }
        if self.adapter != served.name {
            return Err(malformed(format!(
                "an event for adapter {}; this runtime serves {}",
                self.adapter, served.name
            )));
        }
        let name = self.event;
        // A field the named event does not read crosses to no reader.
        // Refusing it here keeps one claim per envelope: a result is
        // reported by a result event, a ruling is spent by the call
        // that quotes its offer, and neither is dropped in silence.
        let read = fields_read(name);
        for field in Field::ALL {
            if self.carries(field) && !read.contains(&field) {
                return Err(malformed(format!(
                    "{name:?} carrying {}, which that event reads nowhere",
                    field.spelling()
                )));
            }
        }
        // Whether this host can assert a person's ruling at all is
        // settled here, on the envelope, before any event exists.
        let ruling = checked_ruling(served.name, self.ruling)?;
        // The envelope's fields, moved out of it now that the rule above
        // has read which of them are there. Arguments and a result's body
        // are the large payloads on this path, and one reader below takes
        // each: nothing here is copied to be read twice.
        let Self {
            root_id,
            child_id,
            text,
            tool,
            arguments,
            outcome,
            spawned_id,
            value,
            spawn_binding,
            ..
        } = self;
        let root = || -> Result<TrajectoryId, ParseRefusal> {
            match root_id.as_deref() {
                Some(root_id) if !root_id.is_empty() => Ok(served.name.root(root_id)),
                _ => Err(malformed(format!("{name:?} without a root_id"))),
            }
        };
        let actor = || -> Result<Actor, ParseRefusal> {
            let root = root()?;
            let child = child_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|id| child_of(&root, id));
            Ok(Actor { root, child })
        };
        let named_child = || -> Result<(TrajectoryId, TrajectoryId), ParseRefusal> {
            let root = root()?;
            match child_id.as_deref().filter(|id| !id.is_empty()) {
                Some(id) => Ok((root.clone(), child_of(&root, id))),
                None => Err(malformed(format!("{name:?} without a child_id"))),
            }
        };
        // The call as the host spelled it, beside the identity the
        // adapter derives for it. Every event that carries a call reads
        // both: the raw spelling is what a child scan and a diagnostic
        // need, and the canonical id is what the runtime keys on. The two
        // fields are handed in rather than read from the envelope, so the
        // arguments reach the call by moving.
        let derived_call =
            |tool: Option<String>, arguments: Option<Box<RawValue>>| -> Result<(ProposedCall, Derived), ParseRefusal> {
                let raw = match (tool, arguments) {
                    (Some(tool), Some(arguments)) => ProposedCall { tool, arguments },
                    _ => return Err(malformed(format!("{name:?} without its tool call"))),
                };
                let derived = (served.derive)(&raw.tool)?;
                Ok((raw, derived))
            };
        let value = value.filter(|value| !value.is_empty());

        let accepted = |event: HookEvent| {
            Ok(Some(Accepted {
                event,
                names_children: Vec::new(),
            }))
        };
        match name {
            EventName::Ping => Ok(None),
            EventName::SessionStart => accepted(HookEvent::SessionStart { root: root()? }),
            EventName::Prompt => match text {
                Some(text) => accepted(HookEvent::Prompt { actor: actor()?, text }),
                None => Err(malformed("prompt without its text")),
            },
            EventName::TurnEnd => accepted(HookEvent::TurnEnd { actor: actor()? }),
            EventName::ToolCall => {
                let actor = actor()?;
                let (raw, derived) = derived_call(tool, arguments)?;
                // A ruling answers the review of the offer a control
                // call quotes, and only the control call spends one.
                // On any other call the runtime would judge the flow
                // and drop the ruling unread, so an asserted denial is
                // refused here rather than silently ignored.
                let ruling = match (ruling, derived.canonical.is_control()) {
                    (None, _) => None,
                    (Some(ruling), true) => Some(ruling),
                    (Some(_), false) => {
                        return Err(malformed(format!(
                            "a ruling on {}, which is not the runtime's control call",
                            derived.canonical
                        )));
                    }
                };
                let names_children = (served.names_children)(&actor, &raw);
                let spawn = derived.spawn;
                Ok(Some(Accepted {
                    event: HookEvent::ToolCall {
                        actor,
                        call: ProposedCall {
                            tool: derived.canonical.into_string(),
                            arguments: raw.arguments,
                        },
                        spawn,
                        ruling,
                    },
                    names_children,
                }))
            }
            // One result event, and the derivation alone says which
            // lifecycle it is. The two names differ only in the fields
            // they may carry: an event name is a caller's claim, and a
            // caller that could pick the lifecycle could skip a child's
            // settlement or spend a spawn's metadata on an ordinary
            // call. The spawn-only fields are refused, not dropped,
            // where the derivation gives no spawn to spend them on.
            EventName::ToolResult | EventName::SpawnResult => {
                let actor = actor()?;
                let (raw, derived) = derived_call(tool, arguments)?;
                let spawn = derived.spawn;
                let call = ProposedCall {
                    tool: derived.canonical.into_string(),
                    arguments: raw.arguments,
                };
                let outcome = match outcome {
                    Some(outcome) => outcome.into_outcome()?,
                    None => return Err(malformed(format!("{name:?} without an outcome"))),
                };
                let child = spawned_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .map(|id| child_of(&actor.root, id));
                match spawn {
                    true => accepted(HookEvent::SpawnResult {
                        actor,
                        call,
                        outcome,
                        child,
                        value,
                    }),
                    false => match (child, value) {
                        (Some(_), _) => Err(malformed(format!(
                            "a result carrying spawned_id for {}, which this adapter derives as an ordinary call",
                            call.tool
                        ))),
                        (None, Some(_)) => Err(malformed(format!(
                            "a result carrying value for {}, which this adapter derives as an ordinary call",
                            call.tool
                        ))),
                        (None, None) => accepted(HookEvent::ToolResult { actor, call, outcome }),
                    },
                }
            }
            EventName::ChildStart => {
                let (root, child) = named_child()?;
                // A binding is a token this runtime minted, and the empty
                // string is none of them. Read as the absent field it is
                // not, it would bind the child to whatever spawn the
                // family has in flight — a different spawn than the one
                // the envelope claims. A claim the wire cannot honour is
                // refused rather than answered with another.
                let spawn = match spawn_binding {
                    Some(binding) if binding.is_empty() => {
                        return Err(malformed(format!("{name:?} carrying an empty spawn_binding")));
                    }
                    Some(binding) => SpawnRef::Binding(SpawnBinding(binding)),
                    None => SpawnRef::InFlight,
                };
                accepted(HookEvent::ChildStart { root, child, spawn })
            }
            EventName::ChildEnd => {
                let (root, child) = named_child()?;
                accepted(HookEvent::ChildEnd { root, child, value })
            }
        }
    }
}

fn child_of(root: &TrajectoryId, child_id: &str) -> TrajectoryId {
    TrajectoryId(format!("{}:{child_id}", root.0))
}

/// The host's own ids from an actor the client already prefixed.
fn host_ids(adapter: AdapterName, actor: &Actor) -> Result<(String, Option<String>), ParseRefusal> {
    let root_id = actor
        .root
        .0
        .strip_prefix(adapter.prefix())
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| malformed(format!("{} is not a {} root id", actor.root.0, adapter)))?
        .to_string();
    let child_id = match &actor.child {
        Some(child) => Some(child_host_id(&actor.root, child)?),
        None => None,
    };
    Ok((root_id, child_id))
}

fn child_host_id(root: &TrajectoryId, child: &TrajectoryId) -> Result<String, ParseRefusal> {
    child
        .0
        .strip_prefix(&root.0)
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| malformed(format!("{} is not a child of {}", child.0, root.0)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionName {
    Ack,
    AllowCall,
    PassControl,
    DenyCall,
    Block,
    ReplaceOutput,
    DeliverValue,
    ChildReturn,
    Context,
    Refuse,
}

/// One offer a block carries: the id `execute_remedy_plan` takes and,
/// where the plan declares a child's return, the route it crosses. A
/// plan that declares no return carries no `returns`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireOffer {
    pub offer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<WireReturn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireReturn {
    AsSpoken(AsSpoken),
    Sanitized { sanitizer: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsSpoken {
    AsSpoken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireReview {
    pub offer_id: String,
    pub text: String,
}

/// The runtime's answer on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireDecision {
    pub protocol: u32,
    pub decision: DecisionName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offers: Option<Vec<WireOffer>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Vec<WireReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WireDecision {
    fn bare(decision: DecisionName) -> Self {
        Self {
            protocol: PROTOCOL,
            decision,
            spawn_binding: None,
            feedback: None,
            offers: None,
            review: None,
            reason: None,
            output: None,
            value: None,
            text: None,
            detail: None,
        }
    }

    pub fn of(decision: &HookDecision) -> Self {
        match decision {
            HookDecision::Ack => Self::bare(DecisionName::Ack),
            HookDecision::AllowCall { spawn } => Self {
                spawn_binding: spawn.as_ref().map(|binding| binding.0.clone()),
                ..Self::bare(DecisionName::AllowCall)
            },
            HookDecision::PassControl => Self::bare(DecisionName::PassControl),
            HookDecision::DenyCall {
                feedback,
                offers,
                review,
            } => Self {
                feedback: Some(feedback.clone()),
                offers: Some(
                    offers
                        .iter()
                        .map(|offer| WireOffer {
                            offer_id: offer.id.clone(),
                            returns: offer.returns.as_ref().map(|returns| match returns {
                                OfferedReturn::AsSpoken => WireReturn::AsSpoken(AsSpoken::AsSpoken),
                                OfferedReturn::Sanitized { sanitizer } => WireReturn::Sanitized {
                                    sanitizer: sanitizer.clone(),
                                },
                            }),
                        })
                        .collect(),
                ),
                review: Some(
                    review
                        .iter()
                        .map(|entry| WireReview {
                            offer_id: entry.offer.clone(),
                            text: entry.text.clone(),
                        })
                        .collect(),
                ),
                ..Self::bare(DecisionName::DenyCall)
            },
            HookDecision::Block { reason } => Self {
                reason: Some(reason.clone()),
                ..Self::bare(DecisionName::Block)
            },
            HookDecision::ReplaceOutput { output } => Self {
                output: Some(output.clone()),
                ..Self::bare(DecisionName::ReplaceOutput)
            },
            HookDecision::DeliverValue { value } => Self {
                value: Some(value.clone()),
                ..Self::bare(DecisionName::DeliverValue)
            },
            HookDecision::ChildReturn { value } => Self {
                value: Some(value.clone()),
                ..Self::bare(DecisionName::ChildReturn)
            },
            HookDecision::Context { text } => Self {
                text: Some(text.clone()),
                ..Self::bare(DecisionName::Context)
            },
            HookDecision::Refuse { detail } => Self {
                detail: Some(detail.clone()),
                ..Self::bare(DecisionName::Refuse)
            },
        }
    }

    /// The client's reading of an answer. A decision missing the field its
    /// kind requires is malformed, never a guess.
    pub fn into_decision(self) -> Result<HookDecision, ParseRefusal> {
        if self.protocol != PROTOCOL {
            return Err(malformed(format!(
                "a decision under protocol {}; this client speaks protocol {PROTOCOL}",
                self.protocol
            )));
        }
        let name = self.decision;
        let required = |field: &str, value: Option<String>| {
            value.ok_or_else(|| malformed(format!("{name:?} without its {field}")))
        };
        Ok(match name {
            DecisionName::Ack => HookDecision::Ack,
            DecisionName::AllowCall => HookDecision::AllowCall {
                spawn: self.spawn_binding.map(SpawnBinding),
            },
            DecisionName::PassControl => HookDecision::PassControl,
            DecisionName::DenyCall => HookDecision::DenyCall {
                feedback: required("feedback", self.feedback)?,
                offers: self
                    .offers
                    .unwrap_or_default()
                    .into_iter()
                    .map(|offer| OfferedRemedy {
                        id: offer.offer_id,
                        returns: offer.returns.map(|returns| match returns {
                            WireReturn::AsSpoken(_) => OfferedReturn::AsSpoken,
                            WireReturn::Sanitized { sanitizer } => OfferedReturn::Sanitized { sanitizer },
                        }),
                    })
                    .collect(),
                review: self
                    .review
                    .unwrap_or_default()
                    .into_iter()
                    .map(|entry| Review {
                        offer: entry.offer_id,
                        text: entry.text,
                    })
                    .collect(),
            },
            DecisionName::Block => HookDecision::Block {
                reason: required("reason", self.reason)?,
            },
            DecisionName::ReplaceOutput => HookDecision::ReplaceOutput {
                output: required("output", self.output)?,
            },
            DecisionName::DeliverValue => HookDecision::DeliverValue {
                value: required("value", self.value)?,
            },
            DecisionName::ChildReturn => HookDecision::ChildReturn {
                value: required("value", self.value)?,
            },
            DecisionName::Context => HookDecision::Context {
                text: required("text", self.text)?,
            },
            DecisionName::Refuse => HookDecision::Refuse {
                detail: required("detail", self.detail)?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> Box<RawValue> {
        RawValue::from_string(json.to_string()).expect("the fixture is JSON")
    }

    /// The one raw spelling this fixture host gives the control tool,
    /// as every real adapter gives it one.
    const CONTROL_RAW: &str = "execute_remedy_plan";

    fn derive(raw: &str) -> Result<Derived, ParseRefusal> {
        if raw == CONTROL_RAW {
            return Ok(Derived {
                canonical: CanonicalTool::control(),
                spawn: false,
            });
        }
        let canonical =
            CanonicalTool::parse(&format!("host/test/{raw}")).map_err(|error| malformed(error.to_string()))?;
        Ok(Derived {
            canonical,
            spawn: matches!(raw, "spawn" | "Agent"),
        })
    }

    fn names_children(actor: &Actor, call: &ProposedCall) -> Vec<TrajectoryId> {
        if call.arguments.get().contains("child-1") {
            vec![TrajectoryId(format!("{}:child-1", actor.root.0))]
        } else {
            Vec::new()
        }
    }

    /// The scan an outcome would discard: asking for it at all is the failure.
    fn unasked_children(_: &Actor, _: &ProposedCall) -> Vec<TrajectoryId> {
        panic!("the children of a call are asked for at the call only")
    }

    fn spell(canonical: &CanonicalTool) -> Option<String> {
        canonical.as_str().strip_prefix("host/test/").map(|raw| raw.to_string())
    }

    const SERVED: Adapter = Adapter {
        name: AdapterName::Kagent,
        derive,
        names_children,
        spell,
    };

    /// The same adapter, refusing to answer the question an outcome has no
    /// use for.
    const NEVER_SCANNED: Adapter = Adapter {
        name: AdapterName::Kagent,
        derive,
        names_children: unasked_children,
        spell,
    };

    #[test]
    fn a_tool_call_crosses_with_its_raw_spelling_and_returns_derived() {
        let body = br#"{"protocol":1,"adapter":"kagent","event":"tool_call","root_id":"r1","tool":"spawn","arguments":{"a":1,"a":2}}"#;
        let accepted = WireEvent::read(body)
            .expect("reads")
            .into_event(&SERVED)
            .expect("parses")
            .expect("is an event");
        match accepted.event {
            HookEvent::ToolCall {
                actor,
                call,
                spawn,
                ruling,
            } => {
                assert_eq!(actor.root.0, "kagent:r1");
                assert_eq!(call.tool, "host/test/spawn");
                assert_eq!(call.arguments.get(), r#"{"a":1,"a":2}"#, "arguments cross unparsed");
                assert!(spawn, "spawn is derived, never read from the wire");
                assert_eq!(ruling, None);
            }
            other => panic!("{other:?}"),
        }
        assert!(accepted.names_children.is_empty());
    }

    #[test]
    fn a_wire_spawn_field_is_ignored_and_names_children_derives() {
        let body = br#"{"protocol":1,"adapter":"kagent","event":"tool_call","root_id":"r1","tool":"read","spawn":true,"arguments":{"path":"tasks/child-1.output"}}"#;
        let accepted = WireEvent::read(body)
            .expect("reads")
            .into_event(&SERVED)
            .expect("parses")
            .expect("event");
        match accepted.event {
            HookEvent::ToolCall { spawn, .. } => assert!(!spawn),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            accepted.names_children,
            vec![TrajectoryId("kagent:r1:child-1".to_string())]
        );
    }

    /// Only a tool call is asked which children its arguments name; an
    /// outcome discards the answer, so the wire never asks for it and the
    /// adapter's scan never runs over a result's payload.
    #[test]
    fn an_outcome_is_never_asked_which_children_it_names() {
        let outcome = r#"{"status":"success","body":{"content":"done"}}"#;
        for event in ["tool_result", "spawn_result"] {
            let body = format!(
                r#"{{"protocol":1,"adapter":"kagent","event":"{event}","root_id":"r1","tool":"read","arguments":{{"path":"tasks/child-1.output"}},"outcome":{outcome}}}"#
            );
            let accepted = WireEvent::read(body.as_bytes())
                .expect("reads")
                .into_event(&NEVER_SCANNED)
                .expect("parses")
                .expect("is an event");
            assert!(accepted.names_children.is_empty(), "{event}");
        }
    }

    #[test]
    fn another_protocol_or_adapter_is_refused_and_a_ping_is_no_event() {
        let refused = WireEvent::read(br#"{"protocol":2,"adapter":"kagent","event":"ping"}"#)
            .expect("reads")
            .into_event(&SERVED);
        assert!(matches!(refused, Err(ParseRefusal::Malformed { .. })));
        let refused = WireEvent::read(br#"{"protocol":1,"adapter":"claude-code","event":"ping"}"#)
            .expect("reads")
            .into_event(&SERVED);
        assert!(matches!(refused, Err(ParseRefusal::Malformed { .. })));
        let ping = WireEvent::read(br#"{"protocol":1,"adapter":"kagent","event":"ping"}"#)
            .expect("reads")
            .into_event(&SERVED)
            .expect("parses");
        assert!(ping.is_none());
        assert!(matches!(
            WireEvent::read(b"not json"),
            Err(ParseRefusal::Unreadable { .. })
        ));
        assert!(matches!(
            WireEvent::read(br#"{"protocol":1,"adapter":"kagent","event":"teleport"}"#),
            Err(ParseRefusal::Malformed { .. })
        ));
    }

    #[test]
    fn a_client_event_round_trips_through_the_wire() {
        let event = HookEvent::SpawnResult {
            actor: Actor {
                root: TrajectoryId("cc:s1".to_string()),
                child: Some(TrajectoryId("cc:s1:a1".to_string())),
            },
            call: ProposedCall {
                tool: "Agent".to_string(),
                arguments: raw(r#"{"prompt":"go"}"#),
            },
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(r#"{"content":"done"}"#.to_string()),
            },
            child: Some(TrajectoryId("cc:s1:a2".to_string())),
            value: Some("done".to_string()),
        };
        let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("translates");
        assert_eq!(wire.root_id.as_deref(), Some("s1"));
        assert_eq!(wire.child_id.as_deref(), Some("a1"));
        assert_eq!(wire.spawned_id.as_deref(), Some("a2"));
        let bytes = serde_json::to_vec(&wire).expect("serializes");
        let text = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!text.contains("spawn\""), "the wire carries no spawn claim: {text}");
        let back = WireEvent::read(&bytes)
            .expect("reads")
            .into_event(&CLAUDE_CODE)
            .expect("parses")
            .expect("event");
        match back.event {
            HookEvent::SpawnResult {
                actor,
                child,
                value,
                call,
                ..
            } => {
                assert_eq!(actor.child.map(|c| c.0), Some("cc:s1:a1".to_string()));
                assert_eq!(child.map(|c| c.0), Some("cc:s1:a2".to_string()));
                assert_eq!(value.as_deref(), Some("done"));
                assert_eq!(call.tool, "host/test/Agent");
            }
            other => panic!("{other:?}"),
        }
        let foreign = HookEvent::SessionStart {
            root: TrajectoryId("kagent:r1".to_string()),
        };
        assert!(WireEvent::from_event(AdapterName::ClaudeCode, &foreign).is_err());
    }

    const CLAUDE_CODE: Adapter = Adapter {
        name: AdapterName::ClaudeCode,
        derive,
        names_children,
        spell,
    };

    /// A field the named event does not read reaches no reader, so the
    /// envelope carrying it is refused rather than read with that field
    /// dropped. The turn end is the costly one: it closes every call
    /// still open, so a result mislabelled as one would settle as
    /// unreported the dispatch it was reporting.
    #[test]
    fn an_event_carrying_a_field_it_never_reads_is_refused() {
        let carried = |event: &str, field: &str| {
            format!(r#"{{"protocol":1,"adapter":"kagent","event":"{event}","root_id":"r1","child_id":"c1",{field}}}"#)
        };
        let result_fields = r#""tool":"builtin_read_file","arguments":{},"outcome":{"status":"success","body":1}"#;
        let refused = [
            // The reported result of a call the turn end would close.
            ("turn_end", result_fields),
            ("session_start", result_fields),
            ("child_start", result_fields),
            ("child_end", result_fields),
            ("prompt", r#""outcome":{"status":"indeterminate"}"#),
            ("turn_end", r#""text":"a prompt no turn end reads""#),
            (
                "tool_call",
                r#""tool":"builtin_read_file","arguments":{},"spawned_id":"s1""#,
            ),
            ("ping", r#""tool":"builtin_read_file""#),
            ("child_start", r#""value":"a return no start carries""#),
            ("child_end", r#""spawn_binding":"b1""#),
        ];
        for (event, field) in refused {
            let row = carried(event, field);
            let read = WireEvent::read(row.as_bytes()).expect("reads").into_event(&SERVED);
            assert!(
                matches!(read, Err(ParseRefusal::Malformed { .. })),
                "{row} must be refused, got {read:?}"
            );
        }

        // The same fields on the event that does read them still cross.
        let result = carried("tool_result", result_fields);
        assert!(
            WireEvent::read(result.as_bytes())
                .expect("reads")
                .into_event(&SERVED)
                .expect("a result event reads a result")
                .is_some(),
            "{result} is the event those fields belong to"
        );
    }

    /// A spawn binding is a token this runtime minted, so an envelope
    /// carrying the empty string names no spawn at all. Reading it as the
    /// absent field would bind the child to whatever spawn the family has
    /// in flight, which is a different spawn than the one claimed, so it is
    /// refused — while the field left out still means exactly that.
    #[test]
    fn an_empty_spawn_binding_is_refused_rather_than_read_as_no_binding() {
        let posted = |binding: &str| {
            format!(
                r#"{{"protocol":1,"adapter":"kagent","event":"child_start","root_id":"r1","child_id":"c1"{binding}}}"#
            )
        };
        let crossed = |row: &str| WireEvent::read(row.as_bytes()).expect("reads").into_event(&SERVED);
        let spawn_of = |row: String| match crossed(&row) {
            Ok(Some(accepted)) => match accepted.event {
                HookEvent::ChildStart { spawn, .. } => spawn,
                other => panic!("{row} crossed as {other:?}"),
            },
            other => panic!("{row} is a child start, got {other:?}"),
        };
        assert_eq!(
            spawn_of(posted(r#","spawn_binding":"{\"fork\":\"f1\"}""#)),
            SpawnRef::Binding(SpawnBinding(r#"{"fork":"f1"}"#.to_string()))
        );
        assert_eq!(spawn_of(posted("")), SpawnRef::InFlight);
        let empty = posted(r#","spawn_binding":"""#);
        assert!(
            matches!(crossed(&empty), Err(ParseRefusal::Malformed { .. })),
            "{empty} names a spawn no fork holds"
        );
    }

    /// A ruling is a person's answer the host obtained itself for the
    /// offer one control call quotes, so it crosses on exactly that
    /// call under exactly that kind of host. Claude Code obtains no
    /// answer of its own; an ordinary call has no offer a ruling could
    /// answer; and no other event reads one at all, so a result, a turn
    /// end or a ping carrying one asserts what nothing would spend. Each
    /// is refused on the envelope, never read with the ruling dropped.
    #[test]
    fn a_ruling_crosses_only_on_the_control_call_of_a_host_that_reviews_itself() {
        #[derive(Debug)]
        enum Expected {
            Ruled(Option<Ruling>),
            /// An event with no ruling to read: it crosses only when none
            /// is asserted.
            Crossed,
            Refused,
        }
        let posted = |adapter: &str, event: &str, tool: &str, ruling: &str| {
            let fields = match event {
                "tool_call" => format!(r#","tool":"{tool}","arguments":{{"offer_id":"o1"}}"#),
                "tool_result" | "spawn_result" => format!(
                    r#","tool":"{tool}","arguments":{{"offer_id":"o1"}},"outcome":{{"status":"success","body":{{"content":"done"}}}}"#
                ),
                "child_start" | "child_end" => r#","child_id":"c1""#.to_string(),
                _ => String::new(),
            };
            format!(r#"{{"protocol":1,"adapter":"{adapter}","event":"{event}","root_id":"r1"{fields}{ruling}}}"#)
        };
        let approve = r#","ruling":"approve""#;
        let deny = r#","ruling":"deny""#;
        let table = [
            // The host reviews through its own channel and the call is
            // the one that quotes the offer.
            (
                &SERVED,
                "tool_call",
                CONTROL_RAW,
                approve,
                Expected::Ruled(Some(Ruling::Approve)),
            ),
            (
                &SERVED,
                "tool_call",
                CONTROL_RAW,
                deny,
                Expected::Ruled(Some(Ruling::Deny)),
            ),
            (&SERVED, "tool_call", CONTROL_RAW, "", Expected::Ruled(None)),
            // The same host, an ordinary call: nothing would spend the
            // ruling, so asserting one is refused.
            (&SERVED, "tool_call", "builtin_read_file", approve, Expected::Refused),
            (&SERVED, "tool_call", "builtin_read_file", deny, Expected::Refused),
            (&SERVED, "tool_call", "builtin_read_file", "", Expected::Ruled(None)),
            // Every other event of that same host: none of them reads a
            // ruling, so one asserted there is refused rather than dropped.
            (&SERVED, "tool_result", CONTROL_RAW, approve, Expected::Refused),
            (&SERVED, "tool_result", "builtin_read_file", deny, Expected::Refused),
            (&SERVED, "spawn_result", "spawn", approve, Expected::Refused),
            (&SERVED, "turn_end", "", approve, Expected::Refused),
            (&SERVED, "turn_end", "", deny, Expected::Refused),
            (&SERVED, "session_start", "", approve, Expected::Refused),
            (&SERVED, "child_start", "", approve, Expected::Refused),
            (&SERVED, "child_end", "", deny, Expected::Refused),
            (&SERVED, "ping", "", approve, Expected::Refused),
            // The same events assert nothing and cross.
            (&SERVED, "tool_result", "builtin_read_file", "", Expected::Crossed),
            (&SERVED, "turn_end", "", "", Expected::Crossed),
            (&SERVED, "session_start", "", "", Expected::Crossed),
            (&SERVED, "child_end", "", "", Expected::Crossed),
            (&SERVED, "ping", "", "", Expected::Crossed),
            // A host with no review channel of its own.
            (&CLAUDE_CODE, "tool_call", CONTROL_RAW, approve, Expected::Refused),
            (&CLAUDE_CODE, "tool_call", CONTROL_RAW, deny, Expected::Refused),
            (&CLAUDE_CODE, "tool_call", CONTROL_RAW, "", Expected::Ruled(None)),
            (&CLAUDE_CODE, "tool_call", "builtin_read_file", deny, Expected::Refused),
            (&CLAUDE_CODE, "turn_end", "", approve, Expected::Refused),
        ];
        for (served, event, tool, asserted, expected) in table {
            let row = posted(served.name.as_str(), event, tool, asserted);
            let read = WireEvent::read(row.as_bytes()).expect("reads").into_event(served);
            match (&expected, read) {
                (Expected::Ruled(ruled), Ok(Some(accepted))) => match accepted.event {
                    HookEvent::ToolCall { ruling, .. } => assert_eq!(ruling, *ruled, "{row}"),
                    other => panic!("{row} crossed as {other:?}"),
                },
                (Expected::Crossed, Ok(_)) => {}
                (Expected::Refused, Err(ParseRefusal::Malformed { .. })) => {}
                (expected, other) => panic!("{row} is {expected:?}, got {other:?}"),
            }
        }
    }

    /// The client side of the same rule: a ruling never reaches the wire
    /// under a host that could not have obtained one.
    #[test]
    fn the_client_asserts_no_ruling_under_a_host_without_a_review_channel() {
        let call = |ruling: Option<Ruling>| HookEvent::ToolCall {
            actor: Actor {
                root: TrajectoryId("kagent:r1".to_string()),
                child: None,
            },
            call: ProposedCall {
                tool: "appa:execute_remedy_plan".to_string(),
                arguments: raw(r#"{"offer_id":"o1"}"#),
            },
            spawn: false,
            ruling,
        };
        let wire = WireEvent::from_event(AdapterName::Kagent, &call(Some(Ruling::Deny))).expect("translates");
        assert_eq!(wire.ruling, Some(Ruling::Deny));
        let refused = WireEvent::from_event(AdapterName::ClaudeCode, &call(Some(Ruling::Approve)));
        assert!(matches!(refused, Err(ParseRefusal::Malformed { .. })), "{refused:?}");
    }

    /// Every outcome the runtime can hold survives the crossing as
    /// itself: a body that is JSON `null`, a body the host did not
    /// carry, and an absent body are three different answers.
    #[test]
    fn every_outcome_round_trips_through_the_wire_bytes() {
        let outcomes = [
            ToolOutcome::Success {
                body: OutcomeBody::Available("null".to_string()),
            },
            ToolOutcome::Success {
                body: OutcomeBody::Available(r#"{"content":"done"}"#.to_string()),
            },
            ToolOutcome::Success {
                body: OutcomeBody::Available(r#""plain text""#.to_string()),
            },
            ToolOutcome::Success {
                body: OutcomeBody::Available("false".to_string()),
            },
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
            ToolOutcome::Failure {
                message: "connection refused".to_string(),
            },
            ToolOutcome::Indeterminate,
        ];
        let actor = || Actor {
            root: TrajectoryId("kagent:r1".to_string()),
            child: None,
        };
        let call = |tool: &str| ProposedCall {
            tool: tool.to_string(),
            arguments: raw(r#"{"path":"notes.txt"}"#),
        };
        for outcome in outcomes {
            let events = [
                HookEvent::ToolResult {
                    actor: actor(),
                    call: call("read"),
                    outcome: outcome.clone(),
                },
                HookEvent::SpawnResult {
                    actor: actor(),
                    call: call("spawn"),
                    outcome: outcome.clone(),
                    child: Some(TrajectoryId("kagent:r1:c1".to_string())),
                    value: Some("done".to_string()),
                },
            ];
            for event in events {
                let wire = WireEvent::from_event(AdapterName::Kagent, &event).expect("translates");
                let bytes = serde_json::to_vec(&wire).expect("serializes");
                let back = WireEvent::read(&bytes)
                    .expect("reads")
                    .into_event(&SERVED)
                    .unwrap_or_else(|refusal| panic!("{outcome:?} is admitted: {refusal:?}"))
                    .expect("is an event");
                let crossed = match &back.event {
                    HookEvent::ToolResult { outcome, .. } | HookEvent::SpawnResult { outcome, .. } => outcome.clone(),
                    other => panic!("{other:?}"),
                };
                assert_eq!(
                    crossed,
                    outcome,
                    "{outcome:?} crossed as {crossed:?}: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }

    /// Every status crossed with every presence of `body` and
    /// `message`, each field spelled as JSON so that a present `null`
    /// is one of the rows. Each status admits exactly one shape and
    /// crosses as itself there; every other combination is refused
    /// rather than read with the field the status has no place for
    /// dropped. A success says whether it carries a body; the wire
    /// never guesses one for it, and a body that is `null` stays one.
    /// A message that is `null` is a field the host spelled, so it is
    /// refused wherever a string message is, and it carries no message
    /// for the one status that reads one.
    #[test]
    fn an_outcome_is_admitted_only_in_the_one_shape_its_status_has() {
        let posted = |outcome: &str| {
            format!(
                r#"{{"protocol":1,"adapter":"kagent","event":"tool_result","root_id":"r1","tool":"read","arguments":{{}},"outcome":{outcome}}}"#
            )
        };
        for status in ["success", "success_without_body", "failure", "indeterminate"] {
            for body in [None, Some("null"), Some(r#"{"content":"done"}"#)] {
                for message in [None, Some("null"), Some(r#""connection refused""#)] {
                    let mut outcome = format!(r#"{{"status":"{status}""#);
                    if let Some(body) = body {
                        outcome.push_str(&format!(r#","body":{body}"#));
                    }
                    if let Some(message) = message {
                        outcome.push_str(&format!(r#","message":{message}"#));
                    }
                    outcome.push('}');
                    let expected = match (status, body, message) {
                        ("success", Some(body), None) => Some(ToolOutcome::Success {
                            body: OutcomeBody::Available(body.to_string()),
                        }),
                        ("success_without_body", None, None) => Some(ToolOutcome::Success {
                            body: OutcomeBody::Unavailable,
                        }),
                        ("failure", None, Some(r#""connection refused""#)) => Some(ToolOutcome::Failure {
                            message: "connection refused".to_string(),
                        }),
                        ("indeterminate", None, None) => Some(ToolOutcome::Indeterminate),
                        _ => None,
                    };
                    let read = WireEvent::read(posted(&outcome).as_bytes())
                        .expect("reads")
                        .into_event(&SERVED);
                    match (expected, read) {
                        (Some(expected), Ok(Some(accepted))) => match accepted.event {
                            HookEvent::ToolResult { outcome: crossed, .. } => {
                                assert_eq!(crossed, expected, "{outcome}")
                            }
                            other => panic!("{outcome} crossed as {other:?}"),
                        },
                        (Some(_), other) => panic!("{outcome} is admitted, got {other:?}"),
                        (None, Err(ParseRefusal::Malformed { .. })) => {}
                        (None, other) => panic!("{outcome} is refused, got {other:?}"),
                    }
                }
            }
        }
    }

    /// The derivation alone says which lifecycle a result runs: the
    /// event name it arrived under selects nothing, so no caller can
    /// skip a child's settlement by naming the ordinary result, or
    /// spend a spawn's metadata on a tool the adapter derives as an
    /// ordinary call. Where the derivation gives no spawn, `spawned_id`
    /// and `value` are refused rather than dropped.
    #[test]
    fn a_results_lifecycle_follows_the_derivation_and_never_the_event_name() {
        #[derive(Debug)]
        enum Expected {
            Ordinary,
            Spawn {
                child: Option<&'static str>,
                value: Option<&'static str>,
            },
            Refused,
        }
        let posted = |event: &str, tool: &str, spawn_fields: &str| {
            format!(
                r#"{{"protocol":1,"adapter":"kagent","event":"{event}","root_id":"r1","tool":"{tool}","arguments":{{}},"outcome":{{"status":"success","body":1}}{spawn_fields}}}"#
            )
        };
        let named = r#","spawned_id":"c1","value":"done""#;
        let table = [
            ("tool_result", "read", "", Expected::Ordinary),
            // The name claims a spawn the derivation does not give.
            ("spawn_result", "read", "", Expected::Ordinary),
            // The name claims an ordinary call for the derived spawn:
            // the child's settlement still runs.
            (
                "tool_result",
                "spawn",
                "",
                Expected::Spawn {
                    child: None,
                    value: None,
                },
            ),
            (
                "spawn_result",
                "spawn",
                "",
                Expected::Spawn {
                    child: None,
                    value: None,
                },
            ),
            (
                "spawn_result",
                "spawn",
                named,
                Expected::Spawn {
                    child: Some("kagent:r1:c1"),
                    value: Some("done"),
                },
            ),
            (
                "tool_result",
                "spawn",
                named,
                Expected::Spawn {
                    child: Some("kagent:r1:c1"),
                    value: Some("done"),
                },
            ),
            (
                "spawn_result",
                "spawn",
                r#","spawned_id":"c1""#,
                Expected::Spawn {
                    child: Some("kagent:r1:c1"),
                    value: None,
                },
            ),
            (
                "spawn_result",
                "spawn",
                r#","value":"done""#,
                Expected::Spawn {
                    child: None,
                    value: Some("done"),
                },
            ),
            // An empty id or value is the absent one, under either name.
            (
                "spawn_result",
                "read",
                r#","spawned_id":"","value":"""#,
                Expected::Ordinary,
            ),
            // Spawn metadata for a tool that is not the spawn.
            ("spawn_result", "read", named, Expected::Refused),
            ("tool_result", "read", named, Expected::Refused),
            ("spawn_result", "read", r#","spawned_id":"c1""#, Expected::Refused),
            ("spawn_result", "read", r#","value":"done""#, Expected::Refused),
            ("tool_result", "read", r#","value":"done""#, Expected::Refused),
        ];
        for (event, tool, spawn_fields, expected) in table {
            let row = posted(event, tool, spawn_fields);
            let read = WireEvent::read(row.as_bytes()).expect("reads").into_event(&SERVED);
            match (&expected, read) {
                (Expected::Ordinary, Ok(Some(accepted))) => match accepted.event {
                    HookEvent::ToolResult { call, .. } => assert_eq!(call.tool, format!("host/test/{tool}")),
                    other => panic!("{row} crossed as {other:?}"),
                },
                (Expected::Spawn { child, value }, Ok(Some(accepted))) => match accepted.event {
                    HookEvent::SpawnResult {
                        call,
                        child: crossed,
                        value: said,
                        ..
                    } => {
                        assert_eq!(call.tool, format!("host/test/{tool}"));
                        assert_eq!(crossed.map(|id| id.0), child.map(str::to_string), "{row}");
                        assert_eq!(said.as_deref(), *value, "{row}");
                    }
                    other => panic!("{row} crossed as {other:?}"),
                },
                (Expected::Refused, Err(ParseRefusal::Malformed { .. })) => {}
                (expected, other) => panic!("{row} is {expected:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn decisions_round_trip_and_a_missing_field_is_malformed() {
        let decisions = [
            HookDecision::Ack,
            HookDecision::AllowCall {
                spawn: Some(SpawnBinding("b1".to_string())),
            },
            HookDecision::PassControl,
            HookDecision::DenyCall {
                feedback: "no".to_string(),
                offers: vec![
                    OfferedRemedy {
                        id: "o1".to_string(),
                        returns: None,
                    },
                    OfferedRemedy {
                        id: "o2".to_string(),
                        returns: Some(OfferedReturn::AsSpoken),
                    },
                    OfferedRemedy {
                        id: "o3".to_string(),
                        returns: Some(OfferedReturn::Sanitized {
                            sanitizer: "s".to_string(),
                        }),
                    },
                ],
                review: vec![Review {
                    offer: "o1".to_string(),
                    text: "approve?".to_string(),
                }],
            },
            HookDecision::Block {
                reason: "r".to_string(),
            },
            HookDecision::ReplaceOutput {
                output: "o".to_string(),
            },
            HookDecision::DeliverValue { value: "v".to_string() },
            HookDecision::ChildReturn { value: "v".to_string() },
            HookDecision::Context { text: "t".to_string() },
            HookDecision::Refuse {
                detail: "d".to_string(),
            },
        ];
        for decision in decisions {
            let wire = WireDecision::of(&decision);
            let json = serde_json::to_string(&wire).expect("serializes");
            let back: WireDecision = serde_json::from_str(&json).expect("reads");
            assert_eq!(back.into_decision().expect("parses"), decision, "{json}");
        }
        let offers = serde_json::to_value(WireDecision::of(&HookDecision::DenyCall {
            feedback: "no".to_string(),
            offers: vec![OfferedRemedy {
                id: "o2".to_string(),
                returns: Some(OfferedReturn::AsSpoken),
            }],
            review: Vec::new(),
        }))
        .expect("serializes");
        assert_eq!(offers["offers"][0]["returns"], "as_spoken");
        let missing: WireDecision = serde_json::from_str(r#"{"protocol":1,"decision":"block"}"#).expect("reads");
        assert!(matches!(missing.into_decision(), Err(ParseRefusal::Malformed { .. })));
        let valueless: WireDecision =
            serde_json::from_str(r#"{"protocol":1,"decision":"deliver_value"}"#).expect("reads");
        assert!(matches!(valueless.into_decision(), Err(ParseRefusal::Malformed { .. })));
        let foreign: WireDecision = serde_json::from_str(r#"{"protocol":9,"decision":"ack"}"#).expect("reads");
        assert!(matches!(foreign.into_decision(), Err(ParseRefusal::Malformed { .. })));
    }
}
