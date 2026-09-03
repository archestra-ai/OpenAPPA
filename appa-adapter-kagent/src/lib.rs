//! The kagent codec: adapter wire JSON to the runtime's vocabulary and
//! back.
//!
//! A pure codec — no policy, no state, no runtime calls; the compiler
//! enforces the boundary, since this crate depends only on
//! `appa-runtime-api`. It derives trajectory ids from the kagent
//! harness's own ids with the `kagent:` prefix, maps each wire event
//! onto one `HookEvent`, and renders every `HookDecision` into the
//! decision envelope the `AppaPluginKagent` plugins enforce.
//!
//! Unlike a harness-owned hook wire, OpenAPPA owns both ends of this
//! one: the plugins (`integrations/kagent/`) emit it and this codec
//! parses it. So the wire is minimal, and an unknown event kind is
//! version skew between plugin and runtime, not an ungated harness
//! hook — it refuses as `Malformed` and the action blocks. The one
//! deliberate no-event kind is `ping`: the plugins hold the ADK model
//! and emission callbacks as liveness gates, and a ping answering
//! 200 `{}` is the proof the `/hook` channel is up.
//!
//! Wire events, one JSON object per `POST /hook`:
//!
//! | `event` | fields beside the ids | `HookEvent` |
//! |---|---|---|
//! | `session_start` | — | `SessionStart` |
//! | `prompt` | `text` | `Prompt` |
//! | `turn_end` | — | `TurnEnd` |
//! | `tool_call` | `tool`, `arguments`, `spawn`, `ruling`? | `ToolCall` |
//! | `tool_result` | `tool`, `arguments`, `outcome` | `ToolResult` |
//! | `spawn_result` | `tool`, `arguments`, `outcome`, `spawned_id`?, `value`? | `SpawnResult` |
//! | `child_start` | `child_id`, `spawn_binding`? | `ChildStart` |
//! | `ping` | — | none — the liveness probe |
//!
//! Ids. `root_id` is the harness id of the root trajectory: the ADK
//! session id, or, in a delegated child workload, the root id the
//! plugin read from the inbound call metadata. `child_id` is the
//! delegated child scope's own id, present when the event belongs to
//! one. The codec derives `kagent:<root_id>` and
//! `kagent:<root_id>:<child_id>`; it never invents an id.
//!
//! Outcomes. `outcome.status` is `success` (with the tool response
//! JSON under `body`, as spelled), `failure` (with `message`), or
//! `indeterminate`. The plugin owns the mapping from ADK callback
//! moments to these three; the codec carries them.
//!
//! By design, nothing feeds `ChildEnd`: return substitution is
//! enforceable only where the parent receives the value, so returns
//! cross at `spawn_result`. The agent's outbound A2A reply crosses no
//! wire event either — the plugin holds `on_event_callback` as a
//! liveness gate, and the implemented model defines no emission event.
//!
//! Decisions render into one envelope, independent of the event:
//!
//! | `HookDecision` | wire |
//! |---|---|
//! | `Ack` | `{"decision":"ack"}` |
//! | `AllowCall` | `{"decision":"allow_call"}` (+ `spawn_binding`) |
//! | `PassControl` | `{"decision":"pass_control"}` |
//! | `DenyCall` | `{"decision":"deny_call","feedback":…,"review":[{"offer_id":…,"text":…}]}` |
//! | `Block` | `{"decision":"block","reason":…}` |
//! | `ReplaceOutput` | `{"decision":"replace_output","output":…}` |
//! | `ChildReturn` | `{"decision":"child_return","value":…}` |
//! | `Context` | `{"decision":"context","text":…}` |
//! | `Refuse` | `{"decision":"refuse","detail":…}` |
//!
//! The plugin owns the ADK mechanics per callback: a `deny_call`
//! becomes the returned dict that skips execution, a `replace_output`
//! becomes the dict that replaces the result, a `block` on a prompt
//! raises pre-append, and a `refuse` raises wherever it lands. The
//! codec renders semantics, not ADK shapes — that keeps one wire and
//! one codec serving both runtime images.

use serde::Deserialize;

use appa_runtime_api::{
    Actor, Codec, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, Ruling, SpawnBinding, SpawnRef,
    ToolOutcome, TrajectoryId,
};

pub fn codec() -> Codec {
    Codec {
        parse,
        render,
        names_children,
    }
}

/// A kagent child's words reach its parent through the after-tool callback only: no
/// file on the parent's disk holds its transcript, so no call names one.
fn names_children(_: &Actor, _: &ProposedCall) -> Vec<TrajectoryId> {
    Vec::new()
}

#[derive(Debug, Deserialize)]
struct WireEvent {
    event: String,
    #[serde(default)]
    root_id: Option<String>,
    #[serde(default)]
    child_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    /// The JSON spelling of the arguments the ADK dispatcher would
    /// execute. The plugin serializes them once; this codec passes the
    /// bytes through unparsed.
    #[serde(default)]
    arguments: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    spawn: Option<bool>,
    /// A person's ruling the plugin obtained through kagent's own
    /// confirmation for the offer this control call quotes:
    /// `approve` or `deny`. Absent on every ordinary call.
    #[serde(default)]
    ruling: Option<String>,
    #[serde(default)]
    outcome: Option<WireOutcome>,
    #[serde(default)]
    spawned_id: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    spawn_binding: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireOutcome {
    status: String,
    #[serde(default)]
    body: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    message: Option<String>,
}

impl WireEvent {
    fn root(&self) -> Result<TrajectoryId, ParseRefusal> {
        match self.root_id.as_deref() {
            Some(root) => Ok(TrajectoryId(format!("kagent:{root}"))),
            None => Err(malformed(&format!("{} without a root_id", self.event))),
        }
    }

    fn child(&self, root: &TrajectoryId) -> Option<TrajectoryId> {
        self.child_id
            .as_deref()
            .map(|child| TrajectoryId(format!("{}:{child}", root.0)))
    }

    fn actor(&self) -> Result<Actor, ParseRefusal> {
        let root = self.root()?;
        let child = self.child(&root);
        Ok(Actor { root, child })
    }

    /// The ruling the plugin attached, spelled `approve` or `deny`; any
    /// other spelling is malformed, never a guess.
    fn ruling(&self) -> Result<Option<Ruling>, ParseRefusal> {
        match self.ruling.as_deref() {
            None => Ok(None),
            Some("approve") => Ok(Some(Ruling::Approve)),
            Some("deny") => Ok(Some(Ruling::Deny)),
            Some(other) => Err(malformed(&format!("tool_call with an unknown ruling {other:?}"))),
        }
    }

    fn call(&self) -> Result<ProposedCall, ParseRefusal> {
        match (self.tool.clone(), self.arguments.clone()) {
            (Some(tool), Some(arguments)) => Ok(ProposedCall { tool, arguments }),
            _ => Err(malformed(&format!("{} without its tool call", self.event))),
        }
    }

    fn outcome(&self) -> Result<ToolOutcome, ParseRefusal> {
        let Some(outcome) = self.outcome.as_ref() else {
            return Err(malformed(&format!("{} without an outcome", self.event)));
        };
        match outcome.status.as_str() {
            "success" => match outcome.body.as_ref() {
                Some(body) => Ok(ToolOutcome::Success {
                    body: OutcomeBody::Available(body.get().to_string()),
                }),
                None => Err(malformed("a success outcome without its body")),
            },
            "failure" => match outcome.message.clone() {
                Some(message) => Ok(ToolOutcome::Failure { message }),
                None => Err(malformed("a failure outcome without its message")),
            },
            "indeterminate" => Ok(ToolOutcome::Indeterminate),
            other => Err(malformed(&format!("an outcome status outside the wire: {other}"))),
        }
    }
}

fn malformed(detail: &str) -> ParseRefusal {
    ParseRefusal::Malformed {
        detail: detail.to_string(),
    }
}

fn parse(body: &[u8]) -> Result<Option<HookEvent>, ParseRefusal> {
    let event: WireEvent = serde_json::from_slice(body).map_err(|error| ParseRefusal::Unreadable {
        detail: format!("unreadable adapter event: {error}"),
    })?;
    tracing::debug!(event = %event.event, root = event.root_id.as_deref().unwrap_or(""), "adapter event");
    match event.event.as_str() {
        "ping" => Ok(None),
        "session_start" => Ok(Some(HookEvent::SessionStart { root: event.root()? })),
        "prompt" => match event.text.clone() {
            Some(text) => Ok(Some(HookEvent::Prompt {
                actor: event.actor()?,
                text,
            })),
            None => Err(malformed("prompt without its text")),
        },
        "turn_end" => Ok(Some(HookEvent::TurnEnd { actor: event.actor()? })),
        "tool_call" => match event.spawn {
            Some(spawn) => Ok(Some(HookEvent::ToolCall {
                actor: event.actor()?,
                call: event.call()?,
                spawn,
                ruling: event.ruling()?,
            })),
            None => Err(malformed("tool_call without its spawn classification")),
        },
        "tool_result" => Ok(Some(HookEvent::ToolResult {
            actor: event.actor()?,
            call: event.call()?,
            outcome: event.outcome()?,
        })),
        "spawn_result" => {
            let actor = event.actor()?;
            let child = event
                .spawned_id
                .as_deref()
                .map(|spawned| TrajectoryId(format!("{}:{spawned}", actor.root.0)));
            Ok(Some(HookEvent::SpawnResult {
                call: event.call()?,
                outcome: event.outcome()?,
                child,
                value: event.value.clone().filter(|value| !value.is_empty()),
                actor,
            }))
        }
        "child_start" => {
            let root = event.root()?;
            let Some(child) = event.child(&root) else {
                return Err(malformed("child_start without a child_id"));
            };
            let spawn = match event.spawn_binding.clone() {
                Some(binding) => SpawnRef::Binding(SpawnBinding(binding)),
                None => SpawnRef::InFlight,
            };
            Ok(Some(HookEvent::ChildStart { root, child, spawn }))
        }
        other => Err(malformed(&format!(
            "an event kind outside the adapter wire: {other} — plugin and runtime versions disagree"
        ))),
    }
}

fn render(_event: &HookEvent, decision: &HookDecision) -> serde_json::Value {
    match decision {
        HookDecision::Ack => serde_json::json!({"decision": "ack"}),
        HookDecision::AllowCall { spawn } => match spawn {
            Some(binding) => serde_json::json!({"decision": "allow_call", "spawn_binding": binding.0}),
            None => serde_json::json!({"decision": "allow_call"}),
        },
        HookDecision::PassControl => serde_json::json!({"decision": "pass_control"}),
        HookDecision::DenyCall { feedback, review, .. } => {
            let review: Vec<serde_json::Value> = review
                .iter()
                .map(|entry| serde_json::json!({"offer_id": entry.offer, "text": entry.text}))
                .collect();
            serde_json::json!({"decision": "deny_call", "feedback": feedback, "review": review})
        }
        HookDecision::Block { reason } => serde_json::json!({"decision": "block", "reason": reason}),
        HookDecision::ReplaceOutput { output } => serde_json::json!({"decision": "replace_output", "output": output}),
        HookDecision::ChildReturn { value } => serde_json::json!({"decision": "child_return", "value": value}),
        HookDecision::Context { text } => serde_json::json!({"decision": "context", "text": text}),
        HookDecision::Refuse { detail } => serde_json::json!({"decision": "refuse", "detail": detail}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_value(event: &serde_json::Value) -> Result<Option<HookEvent>, ParseRefusal> {
        parse(&serde_json::to_vec(event).expect("the fixture serializes"))
    }

    fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
        serde_json::value::to_raw_value(&value).expect("the fixture serializes")
    }

    fn root() -> TrajectoryId {
        TrajectoryId("kagent:s1".to_string())
    }

    #[test]
    fn an_unreadable_body_is_refused_with_the_wire_detail() {
        match parse(b"not json") {
            Err(ParseRefusal::Unreadable { detail }) => {
                assert!(
                    detail.starts_with("unreadable adapter event: "),
                    "the detail must carry the wire prefix, got {detail:?}",
                );
            }
            other => panic!("expected an Unreadable refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_ping_probes_liveness_and_feeds_no_event() {
        let event = serde_json::json!({"event": "ping"});
        assert_eq!(parse_value(&event), Ok(None));
    }

    #[test]
    fn an_event_kind_outside_the_wire_is_a_refusal_not_a_pass() {
        for kind in ["model_call", "emission", "somethingnew"] {
            let event = serde_json::json!({"event": kind, "root_id": "s1"});
            match parse_value(&event) {
                Err(ParseRefusal::Malformed { detail }) => {
                    assert!(detail.contains(kind), "the refusal names the kind, got {detail:?}");
                }
                other => panic!("the {kind} kind must fail closed, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_session_start_parses_to_the_prefixed_root() {
        let event = serde_json::json!({"event": "session_start", "root_id": "s1"});
        assert_eq!(parse_value(&event), Ok(Some(HookEvent::SessionStart { root: root() })));
    }

    #[test]
    fn a_prompt_carries_its_text_and_actor() {
        let event = serde_json::json!({"event": "prompt", "root_id": "s1", "text": "deploy the chart"});
        assert_eq!(
            parse_value(&event),
            Ok(Some(HookEvent::Prompt {
                actor: Actor {
                    root: root(),
                    child: None,
                },
                text: "deploy the chart".to_string(),
            })),
        );
    }

    #[test]
    fn a_child_id_attributes_the_event_to_the_child() {
        let event = serde_json::json!({"event": "turn_end", "root_id": "s1", "child_id": "c1"});
        assert_eq!(
            parse_value(&event),
            Ok(Some(HookEvent::TurnEnd {
                actor: Actor {
                    root: root(),
                    child: Some(TrajectoryId("kagent:s1:c1".to_string())),
                },
            })),
        );
    }

    #[test]
    fn a_tool_call_parses_with_its_spawn_classification() {
        for spawn in [false, true] {
            let event = serde_json::json!({
                "event": "tool_call",
                "root_id": "s1",
                "tool": "k8s_get_pods",
                "arguments": {"namespace": "prod"},
                "spawn": spawn,
            });
            assert_eq!(
                parse_value(&event),
                Ok(Some(HookEvent::ToolCall {
                    actor: Actor {
                        root: root(),
                        child: None,
                    },
                    call: ProposedCall {
                        tool: "k8s_get_pods".to_string(),
                        arguments: raw(serde_json::json!({"namespace": "prod"})),
                    },
                    spawn,
                    ruling: None,
                })),
            );
        }
    }

    #[test]
    fn duplicate_argument_members_reach_the_runtime_unresolved() {
        let body = br#"{"event":"tool_call","root_id":"s1","tool":"t","arguments":{"a":1,"a":2},"spawn":false}"#;
        let Ok(Some(HookEvent::ToolCall { call, .. })) = parse(body) else {
            panic!("the event parses to a tool call");
        };
        assert_eq!(call.arguments.get(), r#"{"a":1,"a":2}"#);
    }

    #[test]
    fn a_tool_result_maps_each_outcome_status() {
        let result = |outcome: serde_json::Value| {
            let event = serde_json::json!({
                "event": "tool_result",
                "root_id": "s1",
                "tool": "k8s_get_pods",
                "arguments": {"namespace": "prod"},
                "outcome": outcome,
            });
            match parse_value(&event) {
                Ok(Some(HookEvent::ToolResult { outcome, .. })) => outcome,
                other => panic!("expected a ToolResult event, got {other:?}"),
            }
        };
        assert_eq!(
            result(serde_json::json!({"status": "success", "body": {"pods": ["api-1"]}})),
            ToolOutcome::Success {
                body: OutcomeBody::Available(r#"{"pods":["api-1"]}"#.to_string()),
            },
            "the body crosses as spelled",
        );
        assert_eq!(
            result(serde_json::json!({"status": "failure", "message": "connection refused"})),
            ToolOutcome::Failure {
                message: "connection refused".to_string(),
            },
        );
        assert_eq!(
            result(serde_json::json!({"status": "indeterminate"})),
            ToolOutcome::Indeterminate,
        );
    }

    #[test]
    fn a_spawn_result_names_the_child_and_carries_the_value() {
        let event = serde_json::json!({
            "event": "spawn_result",
            "root_id": "s1",
            "tool": "billing-agent",
            "arguments": {"message": "total the invoices"},
            "outcome": {"status": "success", "body": {"result": "the total is 42"}},
            "spawned_id": "c1",
            "value": "the total is 42",
        });
        assert_eq!(
            parse_value(&event),
            Ok(Some(HookEvent::SpawnResult {
                actor: Actor {
                    root: root(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "billing-agent".to_string(),
                    arguments: raw(serde_json::json!({"message": "total the invoices"})),
                },
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(r#"{"result":"the total is 42"}"#.to_string()),
                },
                child: Some(TrajectoryId("kagent:s1:c1".to_string())),
                value: Some("the total is 42".to_string()),
            })),
        );
    }

    #[test]
    fn a_spawn_result_without_a_child_or_value_carries_neither() {
        let event = serde_json::json!({
            "event": "spawn_result",
            "root_id": "s1",
            "tool": "billing-agent",
            "arguments": {"message": "go"},
            "outcome": {"status": "indeterminate"},
            "value": "",
        });
        match parse_value(&event) {
            Ok(Some(HookEvent::SpawnResult { child, value, .. })) => {
                assert_eq!(child, None, "no spawned_id names no child");
                assert_eq!(value, None, "an empty value is no value");
            }
            other => panic!("expected a SpawnResult event, got {other:?}"),
        }
    }

    #[test]
    fn a_child_start_names_its_spawn_ref() {
        let bound = serde_json::json!({
            "event": "child_start",
            "root_id": "s1",
            "child_id": "c1",
            "spawn_binding": "b1",
        });
        assert_eq!(
            parse_value(&bound),
            Ok(Some(HookEvent::ChildStart {
                root: root(),
                child: TrajectoryId("kagent:s1:c1".to_string()),
                spawn: SpawnRef::Binding(SpawnBinding("b1".to_string())),
            })),
        );
        let unbound = serde_json::json!({"event": "child_start", "root_id": "s1", "child_id": "c1"});
        assert_eq!(
            parse_value(&unbound),
            Ok(Some(HookEvent::ChildStart {
                root: root(),
                child: TrajectoryId("kagent:s1:c1".to_string()),
                spawn: SpawnRef::InFlight,
            })),
        );
    }

    #[test]
    fn missing_required_fields_are_named_refusals() {
        for (event, detail) in [
            (
                serde_json::json!({"event": "session_start"}),
                "session_start without a root_id",
            ),
            (
                serde_json::json!({"event": "prompt", "root_id": "s1"}),
                "prompt without its text",
            ),
            (
                serde_json::json!({"event": "tool_call", "root_id": "s1", "tool": "t", "arguments": {}}),
                "tool_call without its spawn classification",
            ),
            (
                serde_json::json!({"event": "tool_call", "root_id": "s1", "spawn": false}),
                "tool_call without its tool call",
            ),
            (
                serde_json::json!({"event": "tool_result", "root_id": "s1", "tool": "t", "arguments": {}}),
                "tool_result without an outcome",
            ),
            (
                serde_json::json!({"event": "child_start", "root_id": "s1"}),
                "child_start without a child_id",
            ),
        ] {
            assert_eq!(
                parse_value(&event),
                Err(ParseRefusal::Malformed {
                    detail: detail.to_string()
                }),
                "the {} refusal drifted",
                event["event"],
            );
        }
    }

    #[test]
    fn an_incomplete_outcome_is_a_named_refusal() {
        let result = |outcome: serde_json::Value| {
            parse_value(&serde_json::json!({
                "event": "tool_result",
                "root_id": "s1",
                "tool": "t",
                "arguments": {},
                "outcome": outcome,
            }))
        };
        assert_eq!(
            result(serde_json::json!({"status": "success"})),
            Err(ParseRefusal::Malformed {
                detail: "a success outcome without its body".to_string()
            }),
        );
        assert_eq!(
            result(serde_json::json!({"status": "failure"})),
            Err(ParseRefusal::Malformed {
                detail: "a failure outcome without its message".to_string()
            }),
        );
        assert_eq!(
            result(serde_json::json!({"status": "crashed"})),
            Err(ParseRefusal::Malformed {
                detail: "an outcome status outside the wire: crashed".to_string()
            }),
        );
    }

    #[test]
    fn every_decision_renders_its_exact_envelope() {
        let event = HookEvent::ToolCall {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "k8s_get_pods".to_string(),
                arguments: raw(serde_json::json!({"namespace": "prod"})),
            },
            spawn: false,
            ruling: None,
        };
        for (decision, wire) in [
            (HookDecision::Ack, serde_json::json!({"decision": "ack"})),
            (
                HookDecision::AllowCall { spawn: None },
                serde_json::json!({"decision": "allow_call"}),
            ),
            (
                HookDecision::AllowCall {
                    spawn: Some(SpawnBinding("b1".to_string())),
                },
                serde_json::json!({"decision": "allow_call", "spawn_binding": "b1"}),
            ),
            (
                HookDecision::PassControl,
                serde_json::json!({"decision": "pass_control"}),
            ),
            (
                HookDecision::DenyCall {
                    feedback: "blocked: the recipient cannot read this".to_string(),
                    offers: Vec::new(),
                    review: Vec::new(),
                },
                serde_json::json!({"decision": "deny_call", "feedback": "blocked: the recipient cannot read this", "review": []}),
            ),
            (
                HookDecision::Block {
                    reason: "the prompt does not cross".to_string(),
                },
                serde_json::json!({"decision": "block", "reason": "the prompt does not cross"}),
            ),
            (
                HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                },
                serde_json::json!({"decision": "replace_output", "output": "the output is confined"}),
            ),
            (
                HookDecision::ChildReturn {
                    value: "the redacted summary".to_string(),
                },
                serde_json::json!({"decision": "child_return", "value": "the redacted summary"}),
            ),
            (
                HookDecision::Refuse {
                    detail: "storage failure: disk full".to_string(),
                },
                serde_json::json!({"decision": "refuse", "detail": "storage failure: disk full"}),
            ),
        ] {
            assert_eq!(render(&event, &decision), wire, "the {decision:?} envelope drifted");
        }
    }

    #[test]
    fn the_shared_fixtures_parse_as_their_named_kinds() {
        let fixtures = include_str!("../../integrations/kagent/fixtures/wire-events.jsonl");
        let mut seen = 0;
        for line in fixtures.lines().filter(|line| !line.trim().is_empty()) {
            let fixture: serde_json::Value = serde_json::from_str(line).expect("the fixture line parses");
            let wire = &fixture["wire"];
            let parsed = parse_value(wire);
            match fixture["parses"].as_str().expect("the fixture names its expectation") {
                "event" => assert!(
                    matches!(parsed, Ok(Some(_))),
                    "fixture {} must parse to an event, got {parsed:?}",
                    fixture["name"],
                ),
                "none" => assert_eq!(parsed, Ok(None), "fixture {} must feed no event", fixture["name"]),
                other => panic!("fixture {} names an unknown expectation {other}", fixture["name"]),
            }
            seen += 1;
        }
        assert!(seen >= 8, "the shared fixture file covers every wire kind, saw {seen}");
    }

    #[test]
    fn a_tool_call_carries_the_ruling_the_plugin_obtained() {
        for (spelling, ruling) in [("approve", Ruling::Approve), ("deny", Ruling::Deny)] {
            let event = serde_json::json!({
                "event": "tool_call",
                "root_id": "s1",
                "tool": "execute_remedy_plan",
                "arguments": {"offer_id": "offer-1"},
                "spawn": false,
                "ruling": spelling,
            });
            let Ok(Some(HookEvent::ToolCall { ruling: parsed, .. })) = parse_value(&event) else {
                panic!("a tool_call with a ruling parses: {spelling}");
            };
            assert_eq!(parsed, Some(ruling), "the wire spelling {spelling} is the ruling");
        }
        let unknown = serde_json::json!({
            "event": "tool_call",
            "root_id": "s1",
            "tool": "execute_remedy_plan",
            "arguments": {"offer_id": "offer-1"},
            "spawn": false,
            "ruling": "maybe",
        });
        assert_eq!(
            parse_value(&unknown),
            Err(ParseRefusal::Malformed {
                detail: "tool_call with an unknown ruling \"maybe\"".to_string()
            }),
            "an unknown spelling is malformed, never a guess",
        );
    }

    #[test]
    fn a_deny_renders_the_reviews_for_the_plugin_s_own_channel() {
        let event = HookEvent::TurnEnd {
            actor: Actor {
                root: root(),
                child: None,
            },
        };
        let decision = HookDecision::DenyCall {
            feedback: "blocked".to_string(),
            offers: Vec::new(),
            review: vec![appa_runtime_api::Review {
                offer: "offer-1".to_string(),
                text: "APPA asks you to rule as the authority \"oncall\".".to_string(),
            }],
        };
        assert_eq!(
            render(&event, &decision),
            serde_json::json!({
                "decision": "deny_call",
                "feedback": "blocked",
                "review": [{"offer_id": "offer-1", "text": "APPA asks you to rule as the authority \"oncall\"."}],
            }),
        );
    }
}
