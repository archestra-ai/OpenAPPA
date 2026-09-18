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
