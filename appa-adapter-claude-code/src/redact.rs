//! Replacing what the model sees. A `PostToolUse` hook replaces a tool
//! result through `hookSpecificOutput.updatedToolOutput`, and Claude
//! Code applies the replacement only when it has the tool's own output
//! shape — otherwise it silently keeps the original. So the codec never
//! answers with a bare placeholder: it restates the response it was
//! handed with its leaves redacted. Which restatement one result gets
//! follows the tool that produced it, as the runtime's own identification
//! does, and never the event it arrived as. For the spawn's result the
//! swap is the `content` text — the one field of the `Agent` response
//! Claude Code shows the parent model; the rest stays for the transcript
//! where it is one of the metadata keys that response carries, in that
//! key's known shape, and is redacted like any other leaf where it is
//! not, so a response under the
//! spawn's name that is not the spawn's own shape carries nothing to the
//! model. For every other builtin tool every leaf is
//! redacted: the text takes the tool's content field — `Bash` `stdout`,
//! `Read` `file.content`, `Grep` `content`, `WebFetch` `result`, `Write`
//! `content` — or, where the shape has no known one, the place of its
//! longest string; every other string becomes `[appa] redacted`, numbers
//! `0`, booleans `false`, and an array keeps one element, so a match
//! count, a line count or a result count carries nothing either and the
//! answer never grows with the leaf count. A shape with no string to
//! carry the text — counts and flags only — gets it as the hook's
//! `additionalContext` beside the redacted output. The one exception to
//! the redaction is a string under a key Claude Code validates as a
//! fixed value (`type`, `mode`, `status`) when it is one of the fixed
//! values its output shapes use; any other string there is content. An
//! MCP tool's result (`mcp__…`) is restated as one text block instead:
//! Claude Code accepts any shape there, and an MCP result's keys are
//! content too. A withheld result additionally carries the reason as
//! `decision: block`, which Claude Code shows next to the (replaced)
//! result. Verified live on Claude Code 2.1.233 for `Agent`, `Bash`,
//! `Read`, `Glob`, `Grep`, `Write`, `Edit`, `WebFetch`, and honored on a
//! non-2xx answer too — so a runtime refusal at `PostToolUse` also
//! withholds. `PowerShell`'s response, captured on Claude Code 2.1.285 for
//! Windows, has Bash's shape (`stdout`, `stderr`, `interrupted`); a
//! restatement there is not yet verified live. A tool whose output shape validates another fixed-value
//! string field would keep the original; the fixed-value list is the
//! codec's to extend. A `PostToolUse` this codec cannot read at all is
//! withheld too, from the tool and response its bytes still carry: the
//! result has run either way, and a hook that only exits non-zero leaves
//! that output in front of the model.
use appa_runtime_api::{HookEvent, OutcomeBody, SpawnKind, ToolOutcome};

use crate::identity::{is_mcp_tool, spawn_kind};

pub(crate) const REDACTED: &str = "[appa] redacted";

pub(crate) struct Replacement {
    pub(crate) output: serde_json::Value,
    pub(crate) context: Option<String>,
}

/// Which restatement one result gets, keyed on the tool that produced it exactly as the
/// runtime's own identification is. The event a result arrived as decides nothing here: a
/// spawn's response is restated as one under the spawn's tools and by the ordinary
/// redaction under every other, so the two readings of one result cannot disagree.
enum Restatement {
    /// The spawn's own response: the text takes `content` and the run's metadata stays.
    Spawn,
    /// An MCP result, as one text block. Claude Code accepts any shape there, and an MCP
    /// result's keys are content too.
    TextBlock,
    /// Every other tool's own output shape, with its leaves redacted.
    Leaves,
}

impl Restatement {
    fn of(tool: &str) -> Self {
        match (spawn_kind(tool), is_mcp_tool(tool)) {
            (Some(SpawnKind::Single), _) => Self::Spawn,
            (Some(SpawnKind::FanOut) | None, true) => Self::TextBlock,
            (Some(SpawnKind::FanOut) | None, false) => Self::Leaves,
        }
    }
}

pub(crate) fn replacement(event: &HookEvent, text: &str) -> Option<Replacement> {
    match event {
        HookEvent::SpawnResult { call, outcome, .. } | HookEvent::ToolResult { call, outcome, .. } => {
            Some(restated(&call.tool, delivered(outcome)?, text))
        }
        _ => None,
    }
}

/// Whether the response `event` delivered already says `text` and nothing else, so restating
/// it rewrites nothing. A spawn's response says nothing else only when its every field is
/// `content` carrying exactly `text` or run metadata of its known shape: a redaction fixed
/// point is no proof there, since a null, a zero or a key name survives redaction as it is.
/// Every other restatement is the response with its leaves redacted, which is all that
/// crosses either way, so its fixed point is the response it restates.
pub(crate) fn restates(event: &HookEvent, text: &str) -> bool {
    match event {
        HookEvent::SpawnResult { call, outcome, .. } | HookEvent::ToolResult { call, outcome, .. } => {
            delivered(outcome).is_some_and(|response| match Restatement::of(&call.tool) {
                Restatement::Spawn => spawn_restates(&response, text),
                Restatement::TextBlock | Restatement::Leaves => {
                    let replacement = restated(&call.tool, response.clone(), text);
                    replacement.context.is_none() && replacement.output == response
                }
            })
        }
        _ => false,
    }
}

/// One delivered response restated in place of itself.
pub(crate) fn restated(tool: &str, response: serde_json::Value, text: &str) -> Replacement {
    match Restatement::of(tool) {
        Restatement::Spawn => spawn_replacement(response, text),
        Restatement::TextBlock => Replacement {
            output: text_block(text),
            context: None,
        },
        Restatement::Leaves => swap_leaves(tool, response, text),
    }
}

fn delivered(outcome: &ToolOutcome) -> Option<serde_json::Value> {
    match outcome {
        ToolOutcome::Success {
            body: OutcomeBody::Available(body),
        } => serde_json::from_str(body).ok(),
        _ => None,
    }
}

/// The shape one field of the `Agent` response's run metadata has, closed to what the
/// recorded responses carry: a field of any other shape is a payload this codec does not
/// recognize, whatever key it sits under.
#[derive(Clone, Copy)]
enum Shape {
    Text,
    Count,
    Flag,
    /// An object carrying only these keys, each of its own shape, any of them absent.
    Fields(&'static [(&'static str, Shape)]),
    /// An array whose every element has this shape.
    List(&'static Shape),
}

impl Shape {
    fn admits(self, value: &serde_json::Value) -> bool {
        match (self, value) {
            (Self::Text, serde_json::Value::String(_))
            | (Self::Count, serde_json::Value::Number(_))
            | (Self::Flag, serde_json::Value::Bool(_)) => true,
            (Self::Fields(known), serde_json::Value::Object(fields)) => fields.iter().all(|(key, field)| {
                known
                    .iter()
                    .find(|(name, _)| name == key)
                    .is_some_and(|(_, shape)| shape.admits(field))
            }),
            (Self::List(item), serde_json::Value::Array(items)) => items.iter().all(|element| item.admits(element)),
            _ => false,
        }
    }
}

const CACHE_CREATION: Shape = Shape::Fields(&[
    ("ephemeral_1h_input_tokens", Shape::Count),
    ("ephemeral_5m_input_tokens", Shape::Count),
]);

const ITERATION: Shape = Shape::Fields(&[
    ("input_tokens", Shape::Count),
    ("output_tokens", Shape::Count),
    ("cache_read_input_tokens", Shape::Count),
    ("cache_creation_input_tokens", Shape::Count),
    ("cache_creation", CACHE_CREATION),
    ("type", Shape::Text),
]);

const USAGE: Shape = Shape::Fields(&[
    ("input_tokens", Shape::Count),
    ("cache_creation_input_tokens", Shape::Count),
    ("cache_read_input_tokens", Shape::Count),
    ("output_tokens", Shape::Count),
    (
        "output_tokens_details",
        Shape::Fields(&[("thinking_tokens", Shape::Count)]),
    ),
    (
        "server_tool_use",
        Shape::Fields(&[
            ("web_search_requests", Shape::Count),
            ("web_fetch_requests", Shape::Count),
        ]),
    ),
    ("service_tier", Shape::Text),
    ("cache_creation", CACHE_CREATION),
    ("inference_geo", Shape::Text),
    ("iterations", Shape::List(&ITERATION)),
    ("speed", Shape::Text),
]);

const TOOL_STATS: Shape = Shape::Fields(&[
    ("readCount", Shape::Count),
    ("searchCount", Shape::Count),
    ("bashCount", Shape::Count),
    ("editFileCount", Shape::Count),
    ("linesAdded", Shape::Count),
    ("linesRemoved", Shape::Count),
    ("otherToolCount", Shape::Count),
]);

/// The keys Claude Code's `Agent` response carries beside `content`, each with its shape:
/// the run's own metadata, which names no part of the subagent's message. Every key and
/// every nested key of every recorded `Agent` response in `runtime/tests/fixtures`, over
/// the synchronous and the asynchronous shape. A key or a nested key a later version adds
/// is redacted until it is listed here, so the list going stale withholds more, never less.
fn spawn_metadata(key: &str) -> Option<Shape> {
    match key {
        "agentId" | "agentType" | "description" | "harnessSectionHash" | "outputFile" | "prompt" | "resolvedModel"
        | "status" => Some(Shape::Text),
        "harnessNoteCount" | "harnessTailCount" | "totalDurationMs" | "totalTokens" | "totalToolUseCount" => {
            Some(Shape::Count)
        }
        "canReadOutputFile" | "isAsync" => Some(Shape::Flag),
        "toolStats" => Some(TOOL_STATS),
        "usage" => Some(USAGE),
        _ => None,
    }
}

/// Whether this field of the spawn's response is run metadata of its known shape.
fn is_run_metadata(key: &str, field: &serde_json::Value) -> bool {
    spawn_metadata(key).is_some_and(|shape| shape.admits(field))
}

fn text_block(text: &str) -> serde_json::Value {
    serde_json::json!([{ "type": "text", "text": text }])
}

/// Whether the spawn's response is `text` as its `content` beside run metadata alone.
fn spawn_restates(response: &serde_json::Value, text: &str) -> bool {
    match response {
        serde_json::Value::Object(fields) => {
            fields.contains_key("content")
                && fields.iter().all(|(key, field)| match key.as_str() {
                    "content" => *field == text_block(text),
                    key => is_run_metadata(key, field),
                })
        }
        serde_json::Value::String(said) => said == text,
        _ => false,
    }
}

/// The spawn's result restated: the swap is the `content` text, the one field of the
/// `Agent` response Claude Code shows the parent model, and the run's own metadata stays
/// for the transcript. A field that is not one of the response's known metadata keys in
/// its known shape is a payload this codec does not recognize, so its leaves are redacted
/// as an ordinary tool's are — redacted rather than dropped, since Claude Code keeps the
/// original response when its replacement lacks the tool's own shape: a response under the
/// spawn's name that is not the spawn's own shape — the bytes a refused hook still carries
/// included — crosses nothing.
fn spawn_replacement(response: serde_json::Value, text: &str) -> Replacement {
    let output = match response {
        serde_json::Value::Object(fields) => {
            // The text takes the `content` field below, never a leaf.
            let mut placement = Placement::Taken;
            let mut object: serde_json::Map<String, serde_json::Value> = fields
                .into_iter()
                .map(|(key, field)| match is_run_metadata(&key, &field) {
                    true => (key, field),
                    false => {
                        let redacted = redact(field, Some(&key), text, &mut placement);
                        (key, redacted)
                    }
                })
                .collect();
            object.insert("content".to_string(), text_block(text));
            serde_json::Value::Object(object)
        }
        _ => serde_json::Value::String(text.to_string()),
    };
    Replacement { output, context: None }
}

/// The field of one builtin tool's response that carries the text the tool produced, which
/// is where a restatement puts the text that stands in for it.
fn content_slot(tool: &str) -> Option<&'static str> {
    match tool {
        "Bash" | "PowerShell" => Some("/stdout"),
        "Read" => Some("/file/content"),
        "Grep" => Some("/content"),
        "WebFetch" => Some("/result"),
        "Write" => Some("/content"),
        _ => None,
    }
}

fn swap_leaves(tool: &str, value: serde_json::Value, text: &str) -> Replacement {
    let slot = content_slot(tool).filter(|slot| value.pointer(slot).is_some_and(serde_json::Value::is_string));
    let mut placement = match slot {
        Some(_) => Placement::Taken,
        None => match longest_content(&value, None) {
            Some(length) => Placement::Seeking { length },
            None => Placement::Nowhere,
        },
    };
    let mut output = redact(value, None, text, &mut placement);
    if let Some(slot) = slot {
        *output.pointer_mut(slot).expect("redaction keeps the response's shape") =
            serde_json::Value::String(text.to_string());
    }
    Replacement {
        output,
        context: (!matches!(placement, Placement::Taken)).then(|| text.to_string()),
    }
}

fn longest_content(value: &serde_json::Value, key: Option<&str>) -> Option<usize> {
    match value {
        serde_json::Value::String(text) => (!kept(key, text)).then_some(text.len()),
        serde_json::Value::Array(items) => items.iter().take(1).find_map(|item| longest_content(item, None)),
        serde_json::Value::Object(fields) => fields
            .iter()
            .filter_map(|(key, field)| longest_content(field, Some(key)))
            .max(),
        _ => None,
    }
}

/// Where the text goes in the response being restated, as the redaction walks it. One
/// question with three answers, and the walk answers it at most once.
enum Placement {
    /// The text has a home already — a named content slot, or the spawn's `content` — so
    /// every string this walk meets is redacted.
    Taken,
    /// No named slot, so the first string of this length takes the text and the walk
    /// becomes `Taken`. [`longest_content`] measured that string, so one exists.
    Seeking { length: usize },
    /// Nothing in this shape can carry the text: counts and flags only. Every string is
    /// redacted, and the caller says the text on the hook's context channel instead.
    Nowhere,
}

impl Placement {
    /// Whether this string is the one the text takes, which it is at most once.
    fn takes(&mut self, content: &str) -> bool {
        match self {
            Self::Seeking { length } if *length == content.len() => {
                *self = Self::Taken;
                true
            }
            _ => false,
        }
    }
}

fn redact(value: serde_json::Value, key: Option<&str>, text: &str, placement: &mut Placement) -> serde_json::Value {
    match value {
        serde_json::Value::String(kept_text) if kept(key, &kept_text) => serde_json::Value::String(kept_text),
        serde_json::Value::String(content) => match placement.takes(&content) {
            true => serde_json::Value::String(text.to_string()),
            false => serde_json::Value::String(REDACTED.to_string()),
        },
        serde_json::Value::Number(_) => serde_json::Value::from(0),
        serde_json::Value::Bool(_) => serde_json::Value::Bool(false),
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .take(1)
                .map(|item| redact(item, None, text, placement))
                .collect(),
        ),
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, field)| {
                    let redacted = redact(field, Some(&key), text, placement);
                    (key, redacted)
                })
                .collect(),
        ),
    }
}

/// Whether Claude Code validates this string as a fixed value of its output shape, in
/// which case it is protocol rather than content and the redaction leaves it alone.
fn kept(key: Option<&str>, text: &str) -> bool {
    key.is_some_and(|key| is_fixed_value(key, text))
}

fn is_fixed_value(key: &str, value: &str) -> bool {
    let fixed: &[&str] = match key {
        "type" => &[
            "text",
            "image",
            "notebook",
            "pdf",
            "create",
            "update",
            "resource",
            "resource_link",
            "audio",
        ],
        "mode" => &["content", "files_with_matches", "count"],
        "status" => &[
            "completed",
            "async_launched",
            "remote_launched",
            "pending",
            "in_progress",
            "running",
            "failed",
            "killed",
            "paused",
        ],
        _ => &[],
    };
    fixed.contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::render::{render, withheld, withholding};
    use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome};
    #[test]
    fn only_a_fixed_value_under_a_discriminator_key_keeps_its_text() {
        let replacement = swap_leaves(
            "Other",
            serde_json::json!({
                "type": "text",
                "mode": "count",
                "status": "the secret is under status",
                "detail": {"type": "not a fixed value", "status": {"note": "secret", "count": 4}},
                "modes": ["a"],
                "n": null,
            }),
            "x",
        );
        assert_eq!(
            replacement.output,
            serde_json::json!({
                "type": "text",
                "mode": "count",
                "status": "x",
                "detail": {"type": REDACTED, "status": {"note": REDACTED, "count": 0}},
                "modes": [REDACTED],
                "n": null,
            }),
        );
        assert_eq!(replacement.context, None);
    }

    #[test]
    fn a_known_tools_content_field_carries_the_text() {
        assert_eq!(
            swap_leaves(
                "Read",
                serde_json::json!({
                    "type": "text",
                    "file": {"filePath": "/a/very/long/path/to/notes.txt", "content": "hi\n", "numLines": 1},
                }),
                "the output is confined",
            )
            .output,
            serde_json::json!({
                "type": "text",
                "file": {"filePath": REDACTED, "content": "the output is confined", "numLines": 0},
            }),
        );
        assert_eq!(
            swap_leaves(
                "Grep",
                serde_json::json!({"mode": "files_with_matches", "filenames": ["a.rs", "src/b.rs"], "numFiles": 2}),
                "x",
            )
            .output,
            serde_json::json!({"mode": "files_with_matches", "filenames": ["x"], "numFiles": 0}),
        );
    }

    #[test]
    fn an_mcp_result_is_restated_as_one_text_block() {
        let event = HookEvent::ToolResult {
            actor: Actor {
                root: root(),
                child: None,
            },
            call: ProposedCall {
                tool: "mcp__vault__lookup".to_string(),
                arguments: raw(serde_json::json!({"key": "prod"})),
                cwd: None,
            },
            call_id: None,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(
                    serde_json::json!([{"type": "text", "text": "sk_live_secret"}, {"sk_live_secret": false}])
                        .to_string(),
                ),
            },
        };
        for decision in [
            HookDecision::Block {
                reason: "nothing crossed".to_string(),
                outcomes: None,
            },
            HookDecision::ReplaceOutput {
                output: "the output is confined".to_string(),
                outcomes: None,
            },
            HookDecision::DeliverValue {
                value: "the admitted derivation".to_string(),
            },
            HookDecision::Refuse {
                class: appa_runtime_api::RefusalClass::Storage,
                detail: "storage failure".to_string(),
            },
        ] {
            let answer = render(&event, &decision);
            assert!(!answer.to_string().contains("sk_live_secret"), "{answer}");
            assert!(answer["hookSpecificOutput"]["updatedToolOutput"].is_array(), "{answer}");
        }
    }

    #[test]
    fn the_text_takes_the_place_of_the_longest_string_once() {
        assert_eq!(
            swap_leaves(
                "Other",
                serde_json::json!({"a": "long/path/b.rs", "b": "short", "c": [{"n": "x"}, {"n": "y"}, {"n": "z"}]}),
                "the output is confined",
            )
            .output,
            serde_json::json!({"a": "the output is confined", "b": REDACTED, "c": [{"n": REDACTED}]}),
        );
        let replacement = swap_leaves(
            "Other",
            serde_json::json!({"type": "text", "count": 2, "hits": []}),
            "x",
        );
        assert_eq!(
            replacement.output,
            serde_json::json!({"type": "text", "count": 0, "hits": []})
        );
        assert_eq!(replacement.context.as_deref(), Some("x"));
        let event = tool_result(serde_json::json!({"count": 2}));
        assert_eq!(
            render(
                &event,
                &HookDecision::ReplaceOutput {
                    output: "the output is confined".to_string(),
                    outcomes: None,
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": {"count": 0},
                    "additionalContext": "the output is confined",
                }
            }),
        );
    }

    #[test]
    fn a_withheld_spawn_result_replaces_the_subagents_message() {
        let event = spawn_result(agent_response());
        let mut withheld = agent_response();
        withheld["content"] =
            serde_json::json!([{"type": "text", "text": "[appa] the tool result was withheld: nothing crossed"}]);
        assert_eq!(
            render(
                &event,
                &HookDecision::Block {
                    reason: "nothing crossed".to_string(),
                    outcomes: None,
                }
            ),
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": withheld,
                },
                "decision": "block",
                "reason": "nothing crossed",
            }),
        );
        assert_eq!(render(&event, &HookDecision::Ack), serde_json::json!({}));
    }

    /// A response the codec does not recognize as the spawn's own shape crosses no leaf,
    /// whichever side reads it: the parsed spawn result and the bytes a refused hook still
    /// carries are both restated leaf by leaf, as an ordinary tool's response is. The
    /// metadata the `Agent` response does carry is what stays.
    #[test]
    fn an_unrecognized_response_under_the_spawns_tool_carries_nothing_across() {
        let response = serde_json::json!({
            "result": "the secret",
            "nested": {"note": "the secret", "count": 3},
            "flag": true,
        });
        let refused = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Agent",
            "tool_response": response.clone(),
        });
        assert!(parse_value(&refused).is_err(), "the fixture is one parse refuses");
        let salvaged = withholding(
            &serde_json::to_vec(&refused).expect("the fixture serializes"),
            "unreadable",
        )
        .expect("a post-use hook reports a result");
        let rendered = render(
            &spawn_result(response),
            &HookDecision::Block {
                reason: "unreadable".to_string(),
                outcomes: None,
            },
        );
        for answer in [salvaged, rendered] {
            let output = &answer["hookSpecificOutput"]["updatedToolOutput"];
            assert!(
                !output.to_string().contains("the secret"),
                "an unrecognized payload never reaches the model: {answer}",
            );
            assert_eq!(
                output["content"],
                serde_json::json!([{"type": "text", "text": withheld("unreadable")}]),
                "the swap is still the message the parent model reads: {answer}",
            );
            assert_eq!(output["result"], REDACTED);
            assert_eq!(output["nested"]["note"], REDACTED);
            assert_eq!(output["nested"]["count"], 0);
            assert_eq!(output["flag"], false);
        }

        let launched = serde_json::json!({
            "isAsync": true,
            "status": "async_launched",
            "agentId": "a2",
            "agentType": "Explore",
            "description": "Compute 6*7",
            "prompt": "Compute 6*7",
            "outputFile": "/tmp/a2.md",
            "canReadOutputFile": false,
            "resolvedModel": "the model",
            "totalDurationMs": 15484,
        });
        let answer = render(
            &spawn_result(launched.clone()),
            &HookDecision::Block {
                reason: "unreadable".to_string(),
                outcomes: None,
            },
        );
        let output = &answer["hookSpecificOutput"]["updatedToolOutput"];
        for (key, value) in launched.as_object().expect("the fixture is an object") {
            assert_eq!(&output[key], value, "the run's own metadata stays: {answer}");
        }
    }

    /// The two content slots no test reached, and the three outcomes that deliver no body at
    /// all. A slot puts the text where that tool's own output carries it; an outcome with
    /// nothing delivered has no shape to restate, so the answer falls back to a bare block
    /// and the result the tool produced is still taken out of the model's way by the reason.
    #[test]
    fn the_remaining_slots_carry_the_text_and_an_undelivered_outcome_falls_back() {
        for (tool, response, slot) in [
            ("WebFetch", serde_json::json!({"result": "the page body"}), "/result"),
            ("Write", serde_json::json!({"content": "the file body"}), "/content"),
            (
                "PowerShell",
                serde_json::json!({"stdout": "", "stderr": "a warning longer than the output", "interrupted": false}),
                "/stdout",
            ),
        ] {
            let replacement = swap_leaves(tool, response, "[appa] withheld");
            assert_eq!(
                replacement.output.pointer(slot).and_then(serde_json::Value::as_str),
                Some("[appa] withheld"),
                "{tool} carries the text in its own content field",
            );
            assert_eq!(replacement.context, None, "{tool} placed the text in its slot");
        }

        let undelivered = [
            (
                "a failed run",
                ToolOutcome::Failure {
                    message: "the tool run failed".to_string(),
                },
            ),
            (
                "a success with no body",
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
            ),
            (
                "a body that is not JSON",
                ToolOutcome::Success {
                    body: OutcomeBody::Available("not json at all".to_string()),
                },
            ),
        ];
        for (name, outcome) in undelivered {
            let event = HookEvent::ToolResult {
                actor: Actor {
                    root: root(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: raw(serde_json::json!({"command": "ls"})),
                    cwd: None,
                },
                call_id: None,
                outcome,
            };
            assert_eq!(
                render(
                    &event,
                    &HookDecision::Block {
                        reason: "held".to_string(),
                        outcomes: None,
                    }
                ),
                serde_json::json!({"decision": "block", "reason": "held"}),
                "{name} has no shape to restate, so the answer is the reason",
            );
        }
    }

    fn delivered(value: &str) -> HookDecision {
        HookDecision::DeliverValue {
            value: value.to_string(),
        }
    }

    /// The shapes the run metadata is checked against are the recorded ones: every recorded
    /// `Agent` response that carries a message says that message and nothing else, so the
    /// value it crossed with rewrites nothing.
    #[test]
    fn every_recorded_spawn_response_restates_its_own_message() {
        let recordings = [
            include_str!("../../appa-runtime/tests/fixtures/hooks.jsonl"),
            include_str!("../../appa-runtime/tests/fixtures/hooks-sync.jsonl"),
        ];
        for recording in recordings {
            let responses: Vec<serde_json::Value> = recording
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("each fixture line is JSON"))
                .filter(|event| event["hook_event_name"] == "PostToolUse" && event["tool_name"] == "Agent")
                .map(|event| event["tool_response"].clone())
                .collect();
            assert!(!responses.is_empty(), "each recording carries a spawn's result");
            for response in responses {
                let message = response["content"][0]["text"]
                    .as_str()
                    .expect("a recorded spawn's result carries its message")
                    .to_string();
                assert_eq!(
                    render(&spawn_result(response), &delivered(&message)),
                    serde_json::json!({}),
                    "a recorded response is its message beside run metadata alone",
                );
            }
        }
        assert_eq!(
            render(&spawn_result(agent_response()), &delivered("one file: readme.txt")),
            serde_json::json!({})
        );
        assert_eq!(
            render(
                &spawn_result(serde_json::json!("one file: readme.txt")),
                &delivered("one file: readme.txt")
            ),
            serde_json::json!({}),
        );
    }

    /// A field that survives redaction as it is proves nothing about what it carries: an
    /// unknown key, or a metadata key whose value is not its known shape, is a payload
    /// beside the message, so the response does not restate the value, and the replacement
    /// redacts that field as it redacts any payload it does not recognize.
    #[test]
    fn a_spawn_response_with_a_payload_beside_its_message_does_not_restate_it() {
        let cases = [
            (
                "an unknown key",
                "extra",
                serde_json::json!({"unchecked-key": null}),
                serde_json::json!({"unchecked-key": null}),
            ),
            (
                "nested content in a text slot",
                "status",
                serde_json::json!({"note": "unchecked", "count": 3}),
                serde_json::json!({"note": REDACTED, "count": 0}),
            ),
            (
                "a string in a count slot",
                "totalDurationMs",
                serde_json::json!("unchecked"),
                serde_json::json!(REDACTED),
            ),
            (
                "an unknown nested key",
                "toolStats",
                serde_json::json!({"readCount": 1, "unchecked": "words"}),
                serde_json::json!({"readCount": 0, "unchecked": REDACTED}),
            ),
            (
                "a nested element of another shape",
                "usage",
                serde_json::json!({"input_tokens": 2, "iterations": [{"type": "message", "note": "unchecked"}]}),
                serde_json::json!({"input_tokens": 0, "iterations": [{"type": REDACTED, "note": REDACTED}]}),
            ),
        ];
        for (name, key, payload, redacted) in cases {
            let mut response = agent_response();
            response[key] = payload;
            let mut replaced = agent_response();
            replaced[key] = redacted;
            assert_eq!(
                render(&spawn_result(response), &delivered("one file: readme.txt")),
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "updatedToolOutput": replaced,
                    }
                }),
                "{name} is not run metadata, so the reply is restated with it redacted",
            );
        }
    }
}
