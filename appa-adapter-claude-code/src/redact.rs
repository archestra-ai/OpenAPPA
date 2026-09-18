//! Replacing what the model sees. A `PostToolUse` hook replaces a tool
//! result through `hookSpecificOutput.updatedToolOutput`, and Claude
//! Code applies the replacement only when it has the tool's own output
//! shape — otherwise it silently keeps the original. So the codec never
//! answers with a bare placeholder: it restates the response it was
//! handed with its leaves redacted. Which restatement one result gets
//! follows the tool that produced it, as the runtime's own derivation
//! does, and never the event it arrived as. For the spawn's result the
//! swap is the `content` text — the one field of the `Agent` response
//! Claude Code shows the parent model; the rest stays for the transcript
//! where it is one of the metadata keys that response carries, and is
//! redacted like any other leaf where it is not, so a response under the
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
//! withholds. A tool whose output shape validates another fixed-value
//! string field would keep the original; the fixed-value list is the
//! codec's to extend. A `PostToolUse` this codec cannot read at all is
//! withheld too, from the tool and response its bytes still carry: the
//! result has run either way, and a hook that only exits non-zero leaves
//! that output in front of the model.

use appa_runtime_api::{HookEvent, OutcomeBody, ToolOutcome};

use crate::identity::{is_mcp_tool, is_spawn_tool};

pub(crate) fn is_fixed_value(key: &str, value: &str) -> bool {
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

pub(crate) const REDACTED: &str = "[appa] redacted";

pub(crate) fn content_slot(tool: &str) -> Option<&'static str> {
    match tool {
        "Bash" => Some("/stdout"),
        "Read" => Some("/file/content"),
        "Grep" => Some("/content"),
        "WebFetch" => Some("/result"),
        "Write" => Some("/content"),
        _ => None,
    }
}

pub(crate) struct Replacement {
    pub(crate) output: serde_json::Value,
    pub(crate) context: Option<String>,
}

pub(crate) fn replacement(event: &HookEvent, text: &str) -> Option<Replacement> {
    match event {
        HookEvent::SpawnResult { call, outcome, .. } | HookEvent::ToolResult { call, outcome, .. } => {
            Some(restated(&call.tool, delivered(outcome)?, text))
        }
        _ => None,
    }
}

/// One delivered response restated in place of itself, keyed on the tool that produced it
/// exactly as the runtime's own derivation is. The event a result arrived as decides
/// nothing here: a spawn's response is restated as one under the spawn's tools and by the
/// ordinary redaction under every other, so the two readings of one result cannot disagree.
pub(crate) fn restated(tool: &str, response: serde_json::Value, text: &str) -> Replacement {
    match is_spawn_tool(tool) {
        true => spawn_replacement(response, text),
        false => tool_replacement(tool, response, text),
    }
}

/// The keys Claude Code's `Agent` response carries beside `content`: the run's own
/// metadata, which names no part of the subagent's message. Every key of every recorded
/// `Agent` response in `runtime/tests/fixtures`, over the synchronous and the asynchronous
/// shape. A key a later version adds is redacted until it is listed here, so the list
/// going stale withholds more, never less.
pub(crate) fn is_spawn_metadata(key: &str) -> bool {
    matches!(
        key,
        "agentId"
            | "agentType"
            | "canReadOutputFile"
            | "description"
            | "harnessNoteCount"
            | "harnessSectionHash"
            | "harnessTailCount"
            | "isAsync"
            | "outputFile"
            | "prompt"
            | "resolvedModel"
            | "status"
            | "toolStats"
            | "totalDurationMs"
            | "totalTokens"
            | "totalToolUseCount"
            | "usage"
    )
}

/// The spawn's result restated: the swap is the `content` text, the one field of the
/// `Agent` response Claude Code shows the parent model, and the run's own metadata stays
/// for the transcript. A field that is not one of the response's known metadata keys is
/// a payload this codec does not recognize, so its leaves are redacted as an ordinary
/// tool's are: a response under the spawn's name that is not the spawn's own shape — the
/// bytes a refused hook still carries included — crosses nothing.
pub(crate) fn spawn_replacement(response: serde_json::Value, text: &str) -> Replacement {
    let output = match response {
        serde_json::Value::Object(fields) => {
            // The text takes the `content` field, never a leaf.
            let mut placed = true;
            let mut object: serde_json::Map<String, serde_json::Value> = fields
                .into_iter()
                .map(|(key, field)| match is_spawn_metadata(&key) {
                    true => (key, field),
                    false => {
                        let redacted = redact(field, Some(&key), text, None, &mut placed);
                        (key, redacted)
                    }
                })
                .collect();
            object.insert(
                "content".to_string(),
                serde_json::json!([{ "type": "text", "text": text }]),
            );
            serde_json::Value::Object(object)
        }
        _ => serde_json::Value::String(text.to_string()),
    };
    Replacement { output, context: None }
}

/// One tool's result restated: an MCP result as a single text block, whose keys are content
/// too, and every other tool's own output shape with its leaves redacted.
pub(crate) fn tool_replacement(tool: &str, response: serde_json::Value, text: &str) -> Replacement {
    match is_mcp_tool(tool) {
        true => Replacement {
            output: serde_json::json!([{ "type": "text", "text": text }]),
            context: None,
        },
        false => swap_leaves(tool, response, text),
    }
}

pub(crate) fn delivered(outcome: &ToolOutcome) -> Option<serde_json::Value> {
    match outcome {
        ToolOutcome::Success {
            body: OutcomeBody::Available(body),
        } => serde_json::from_str(body).ok(),
        _ => None,
    }
}

pub(crate) fn swap_leaves(tool: &str, value: serde_json::Value, text: &str) -> Replacement {
    let slot = content_slot(tool).filter(|slot| value.pointer(slot).is_some_and(serde_json::Value::is_string));
    let longest = match slot {
        Some(_) => None,
        None => longest_content(&value, None),
    };
    let mut placed = slot.is_some();
    let mut output = redact(value, None, text, longest, &mut placed);
    if let Some(slot) = slot {
        *output.pointer_mut(slot).expect("redaction keeps the response's shape") =
            serde_json::Value::String(text.to_string());
    }
    Replacement {
        output,
        context: (!placed).then(|| text.to_string()),
    }
}

pub(crate) fn longest_content(value: &serde_json::Value, key: Option<&str>) -> Option<usize> {
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

pub(crate) fn redact(
    value: serde_json::Value,
    key: Option<&str>,
    text: &str,
    longest: Option<usize>,
    placed: &mut bool,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(kept_text) if kept(key, &kept_text) => serde_json::Value::String(kept_text),
        serde_json::Value::String(content) => {
            if !*placed && Some(content.len()) == longest {
                *placed = true;
                serde_json::Value::String(text.to_string())
            } else {
                serde_json::Value::String(REDACTED.to_string())
            }
        }
        serde_json::Value::Number(_) => serde_json::Value::from(0),
        serde_json::Value::Bool(_) => serde_json::Value::Bool(false),
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .take(1)
                .map(|item| redact(item, None, text, longest, placed))
                .collect(),
        ),
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, field)| {
                    let redacted = redact(field, Some(&key), text, longest, placed);
                    (key, redacted)
                })
                .collect(),
        ),
    }
}

pub(crate) fn kept(key: Option<&str>, text: &str) -> bool {
    key.is_some_and(|key| is_fixed_value(key, text))
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
            },
            HookDecision::ReplaceOutput {
                output: "the output is confined".to_string(),
            },
            HookDecision::DeliverValue {
                value: "the admitted derivation".to_string(),
            },
            HookDecision::Refuse {
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
                },
                call_id: None,
                outcome,
            };
            assert_eq!(
                render(
                    &event,
                    &HookDecision::Block {
                        reason: "held".to_string(),
                    }
                ),
                serde_json::json!({"decision": "block", "reason": "held"}),
                "{name} has no shape to restate, so the answer is the reason",
            );
        }
    }
}
