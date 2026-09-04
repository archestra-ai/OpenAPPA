//! The canonical hook wire: the one JSON shape `POST /hook` accepts and
//! the one it answers with, versioned by [`PROTOCOL`].
//!
//! A host's adapter translates its own event shape into a [`WireEvent`]
//! on the client side (Claude Code through `appa hook`, kagent inside its
//! ADK plugin) and reads a [`WireDecision`] back. The wire carries the
//! host's raw tool spelling and nothing the runtime would have to trust:
//! whether a call is a spawn, which canonical tool it names, whether its
//! arguments name a child's transcript, and whether it is the runtime's
//! own control tool are all derived on the server from the configured
//! adapter and the raw spelling ([`Adapter::derive`]). Ids cross
//! unprefixed and the server applies the configured adapter's prefix,
//! so no caller can speak for another adapter's trajectories.
//!
//! The event and decision structs are flat with optional fields rather
//! than tagged enums because `arguments` is a `RawValue`, which serde's
//! internally tagged representation cannot carry.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{
    Actor, AdapterName, CanonicalTool, HookDecision, HookEvent, OfferedRemedy, OfferedReturn, OutcomeBody,
    ParseRefusal, ProposedCall, Review, Ruling, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId,
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
    /// The family children the arguments name by the host's own on-disk
    /// spellings of a child's transcript or output file. A recognizer of
    /// the default spellings, not a guarantee that no other path reaches
    /// the file.
    pub names_children: Vec<TrajectoryId>,
}

/// The server-side derivation for one call: total over the adapter's raw
/// domain, a refusal outside it. A plain `fn` pointer — no state, no
/// runtime access — for the same reason [`Codec`](crate::Codec) is.
pub type DeriveFn = fn(&Actor, &ProposedCall) -> Result<Derived, ParseRefusal>;

/// One adapter as the runtime serves it: its name, which fixes the
/// trajectory prefix and the spawn coverage rule, and its derivation.
#[derive(Clone, Copy)]
pub struct Adapter {
    pub name: AdapterName,
    pub derive: DeriveFn,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRuling {
    Approve,
    Deny,
}

impl From<WireRuling> for Ruling {
    fn from(ruling: WireRuling) -> Self {
        match ruling {
            WireRuling::Approve => Ruling::Approve,
            WireRuling::Deny => Ruling::Deny,
        }
    }
}

impl From<Ruling> for WireRuling {
    fn from(ruling: Ruling) -> Self {
        match ruling {
            Ruling::Approve => WireRuling::Approve,
            Ruling::Deny => WireRuling::Deny,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Success,
    Failure,
    Indeterminate,
}

/// A dispatched tool's outcome as the host observed it. `success`
/// carries `body`, `failure` carries `message`, `indeterminate` carries
/// neither.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireOutcome {
    pub status: OutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
                status: OutcomeStatus::Success,
                body: Some(RawValue::from_string("null".to_string()).expect("null is JSON")),
                message: None,
            },
            ToolOutcome::Failure { message } => Self {
                status: OutcomeStatus::Failure,
                body: None,
                message: Some(message.clone()),
            },
            ToolOutcome::Indeterminate => Self {
                status: OutcomeStatus::Indeterminate,
                body: None,
                message: None,
            },
        })
    }

    fn into_outcome(self) -> Result<ToolOutcome, ParseRefusal> {
        match (self.status, self.body, self.message) {
            (OutcomeStatus::Success, Some(body), _) => Ok(ToolOutcome::Success {
                body: OutcomeBody::Available(body.get().to_string()),
            }),
            (OutcomeStatus::Success, None, _) => Err(malformed("a success outcome without its body")),
            (OutcomeStatus::Failure, _, Some(message)) => Ok(ToolOutcome::Failure { message }),
            (OutcomeStatus::Failure, _, None) => Err(malformed("a failure outcome without its message")),
            (OutcomeStatus::Indeterminate, _, _) => Ok(ToolOutcome::Indeterminate),
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
    pub ruling: Option<WireRuling>,
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
    /// Set on a tool call only; empty otherwise.
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
        let mut wire = match event {
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
                    ruling: ruling.map(WireRuling::from),
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
        wire.protocol = PROTOCOL;
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
        let root = || -> Result<TrajectoryId, ParseRefusal> {
            match self.root_id.as_deref() {
                Some(root_id) if !root_id.is_empty() => Ok(served.name.root(root_id)),
                _ => Err(malformed(format!("{name:?} without a root_id"))),
            }
        };
        let actor = || -> Result<Actor, ParseRefusal> {
            let root = root()?;
            let child = self
                .child_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|id| child_of(&root, id));
            Ok(Actor { root, child })
        };
        let named_child = || -> Result<(TrajectoryId, TrajectoryId), ParseRefusal> {
            let root = root()?;
            match self.child_id.as_deref().filter(|id| !id.is_empty()) {
                Some(id) => Ok((root.clone(), child_of(&root, id))),
                None => Err(malformed(format!("{name:?} without a child_id"))),
            }
        };
        let raw_call = || -> Result<ProposedCall, ParseRefusal> {
            match (self.tool.clone(), self.arguments.clone()) {
                (Some(tool), Some(arguments)) => Ok(ProposedCall { tool, arguments }),
                _ => Err(malformed(format!("{name:?} without its tool call"))),
            }
        };
        let derived_call = |actor: &Actor| -> Result<(ProposedCall, Derived), ParseRefusal> {
            let raw = raw_call()?;
            let derived = (served.derive)(actor, &raw)?;
            let call = ProposedCall {
                tool: derived.canonical.as_str().to_string(),
                arguments: raw.arguments,
            };
            Ok((call, derived))
        };
        let outcome = || -> Result<ToolOutcome, ParseRefusal> {
            match self.outcome.clone() {
                Some(outcome) => outcome.into_outcome(),
                None => Err(malformed(format!("{name:?} without an outcome"))),
            }
        };
        let value = || self.value.clone().filter(|value| !value.is_empty());

        let accepted = |event: HookEvent| {
            Ok(Some(Accepted {
                event,
                names_children: Vec::new(),
            }))
        };
        match name {
            EventName::Ping => Ok(None),
            EventName::SessionStart => accepted(HookEvent::SessionStart { root: root()? }),
            EventName::Prompt => match self.text.clone() {
                Some(text) => accepted(HookEvent::Prompt { actor: actor()?, text }),
                None => Err(malformed("prompt without its text")),
            },
            EventName::TurnEnd => accepted(HookEvent::TurnEnd { actor: actor()? }),
            EventName::ToolCall => {
                let actor = actor()?;
                let (call, derived) = derived_call(&actor)?;
                Ok(Some(Accepted {
                    event: HookEvent::ToolCall {
                        actor,
                        call,
                        spawn: derived.spawn,
                        ruling: self.ruling.map(Ruling::from),
                    },
                    names_children: derived.names_children,
                }))
            }
            EventName::ToolResult => {
                let actor = actor()?;
                let (call, _) = derived_call(&actor)?;
                accepted(HookEvent::ToolResult {
                    actor,
                    call,
                    outcome: outcome()?,
                })
            }
            EventName::SpawnResult => {
                let actor = actor()?;
                let (call, _) = derived_call(&actor)?;
                let child = self
                    .spawned_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .map(|id| child_of(&actor.root, id));
                accepted(HookEvent::SpawnResult {
                    actor,
                    call,
                    outcome: outcome()?,
                    child,
                    value: value(),
                })
            }
            EventName::ChildStart => {
                let (root, child) = named_child()?;
                let spawn = match self.spawn_binding.clone() {
                    Some(binding) => SpawnRef::Binding(SpawnBinding(binding)),
                    None => SpawnRef::InFlight,
                };
                accepted(HookEvent::ChildStart { root, child, spawn })
            }
            EventName::ChildEnd => {
                let (root, child) = named_child()?;
                accepted(HookEvent::ChildEnd {
                    root,
                    child,
                    value: value(),
                })
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

    fn derive(actor: &Actor, call: &ProposedCall) -> Result<Derived, ParseRefusal> {
        let canonical =
            CanonicalTool::parse(&format!("host/test/{}", call.tool)).map_err(|error| malformed(error.to_string()))?;
        Ok(Derived {
            canonical,
            spawn: call.tool == "spawn",
            names_children: if call.arguments.get().contains("child-1") {
                vec![TrajectoryId(format!("{}:child-1", actor.root.0))]
            } else {
                Vec::new()
            },
        })
    }

    const SERVED: Adapter = Adapter {
        name: AdapterName::Kagent,
        derive,
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
        let served = Adapter {
            name: AdapterName::ClaudeCode,
            derive,
        };
        let back = WireEvent::read(&bytes)
            .expect("reads")
            .into_event(&served)
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
        let foreign: WireDecision = serde_json::from_str(r#"{"protocol":9,"decision":"ack"}"#).expect("reads");
        assert!(matches!(foreign.into_decision(), Err(ParseRefusal::Malformed { .. })));
    }
}
