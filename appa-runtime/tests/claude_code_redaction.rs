mod common;
use common::claude_hook;

use appa_runtime::api::{Actor, OfferId, RemedyOutcome, Runtime, TrajectoryId};
use appa_runtime::config::Config;

/// The session every hook here carries; the root trajectory is what the runtime derives
/// from it.
const SESSION: &str = "redaction-through-the-codec";

/// The key material the tool's result carried. None of it may reach the model.
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

fn actor() -> Actor {
    Actor {
        root: TrajectoryId(format!("cc:{SESSION}")),
        child: None,
    }
}

/// A deployment whose one declared tool is confined to the requester, beside the stock
/// masker that can release it. No Annotator and no fallback: the tool is declared, so
/// every decision here is the static policy's and no model is consulted.
fn deployment(dir: &std::path::Path, tool: &str) -> Runtime {
    let path = dir.join("appa.toml");
    std::fs::write(
        &path,
        format!(
            r#"[policy]
version = 2

[[policy.tool]]
name = "host/claude-code/{tool}"
delta = {{ audience = ["self"] }}

[[policy.sanitizer]]
name = "redact-secrets"
on = ["tool_output"]

[policy.sanitizer.permits]
audience = {{ from = ["self"], to = ["public"] }}

[policy.deployment]
confined_results = ["host/claude-code/{tool}"]

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.sanitizers.redact-secrets]
builtin = "redact-secrets"
"#
        ),
    )
    .expect("the deployment writes");
    let config = Config::load(&path).expect("the deployment loads");
    Runtime::open(config, dir.join("appa.db"), None).expect("the deployment opens")
}

fn hook(name: &str, fields: serde_json::Value) -> serde_json::Value {
    let mut event = serde_json::json!({
        "session_id": SESSION,
        "transcript_path": "/recorded/session.jsonl",
        "cwd": "/recorded/work",
        "permission_mode": "auto",
        "hook_event_name": name,
    });
    let object = event.as_object_mut().expect("a hook event is an object");
    for (key, value) in fields.as_object().expect("the extra fields are an object") {
        object.insert(key.clone(), value.clone());
    }
    event
}

fn proposal(tool: &str, input: serde_json::Value) -> serde_json::Value {
    hook(
        "PreToolUse",
        serde_json::json!({ "tool_name": tool, "tool_input": input }),
    )
}

fn result(tool: &str, input: serde_json::Value, response: serde_json::Value) -> serde_json::Value {
    hook(
        "PostToolUse",
        serde_json::json!({ "tool_name": tool, "tool_input": input, "tool_response": response }),
    )
}

/// The masker authorized for the call the proposal names, through the denial the runtime
/// renders back as a hook answer. Returns the released proposal's answer.
async fn through_the_masker(runtime: &Runtime, proposal: &serde_json::Value) -> serde_json::Value {
    let (status, _) = claude_hook(runtime, &hook("SessionStart", serde_json::json!({}))).await;
    assert_eq!(status, 200, "the session opens");

    let (status, denied) = claude_hook(runtime, proposal).await;
    assert_eq!(status, 200, "the proposal is answered: {denied}");
    assert_eq!(
        denied["hookSpecificOutput"]["permissionDecision"], "deny",
        "narrowing the session to `self` is not APPA's to decide alone: {denied}"
    );
    let feedback = denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("a denial carries its reason");
    assert!(
        matches!(
            runtime.execute_remedy(&actor(), masker_offer(feedback)).await,
            RemedyOutcome::Authorized { .. }
        ),
        "the masker is offered and executable as offered: {feedback}"
    );

    let (status, allowed) = claude_hook(runtime, proposal).await;
    assert_eq!(status, 200, "the re-proposed call is answered: {allowed}");
    assert_eq!(
        allowed["hookSpecificOutput"]["permissionDecision"], "allow",
        "the authorized plan releases the call: {allowed}"
    );
    allowed
}

/// The offer a rendered denial attributes to the masker. The denial also offers the plain
/// narrowing, which would settle the session at `self` and leave the result unmasked; this
/// test is about the other end, where the sanitizer runs instead.
fn masker_offer(feedback: &str) -> OfferId {
    let line = feedback
        .lines()
        .skip_while(|line| !line.contains("sanitizer redact-secrets"))
        .nth(1)
        .unwrap_or_else(|| panic!("no redact-secrets offer in the rendered denial: {feedback}"));
    let after = line.split("offer_id:").nth(1).expect("the offer line names an id");
    let rest = after.trim_start().strip_prefix('"').expect("the id is quoted");
    OfferId(rest[..rest.find('"').expect("the id closes its quote")].to_string())
}

/// A confined result released through the masker and rendered back as a hook answer, for
/// a tool whose response shape the codec holds no content slot for. The engine decides;
/// the codec places the sanitizer's text at the response's longest content leaf, redacts
/// every other leaf, and keeps the fixed values the model reads as protocol. What this
/// pins is the whole hop, from the hook's own bytes to the hook's own answer: the tool's
/// output reaches the model only as the sanitizer wrote it.
///
/// `BashOutput` reads a background shell's buffer: the same command output `Bash` returns,
/// under a tool name the codec holds no content slot for.
#[tokio::test]
async fn a_confined_result_the_codec_has_no_slot_for_crosses_only_as_the_masker_wrote_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(dir.path(), "BashOutput");
    let input = serde_json::json!({ "bash_id": "bash_1" });
    through_the_masker(&runtime, &proposal("BashOutput", input.clone())).await;

    // `status` is one of the protocol's fixed values, `shellId` names the shell, and
    // `exitCode` is a number; the key material is in the buffer.
    let response = serde_json::json!({
        "stdout": format!("AWS_SECRET_ACCESS_KEY={SECRET}\nREGION=eu-west-1\n"),
        "stderr": "warning: one shell still running",
        "shellId": "bash_1",
        "status": "running",
        "exitCode": 0,
    });
    let (status, answer) = claude_hook(&runtime, &result("BashOutput", input, response)).await;
    assert_eq!(status, 200, "the result is answered: {answer}");
    let output = &answer["hookSpecificOutput"]["updatedToolOutput"];
    assert!(
        !output.is_null(),
        "a confined result is replaced rather than passed through: {answer}"
    );
    assert!(
        !answer.to_string().contains(SECRET),
        "no part of the answer carries the key material: {answer}"
    );
    assert_eq!(
        output["status"], "running",
        "the fixed value the model reads as protocol is kept: {answer}"
    );
    assert_eq!(
        output["exitCode"], 0,
        "a number carries no content and is zeroed: {answer}"
    );
    assert_eq!(
        output["shellId"], "[appa] redacted",
        "every leaf but the one the text took is redacted: {answer}"
    );
    assert_eq!(
        output["stderr"], "[appa] redacted",
        "every leaf but the one the text took is redacted: {answer}"
    );
    let stdout = output["stdout"].as_str().expect("the longest leaf takes the text");
    assert!(
        stdout.contains("[redacted-secret]"),
        "the longest content leaf takes what the masker returned: {answer}"
    );
    // The sanitizer's subject is the result body as the tool returned it, which for this
    // harness is the response object's own JSON. So what the masker writes back is that
    // whole object, masked, and the codec then places it at one leaf of the response it
    // came from. The model reads the masked response nested inside its own `stdout`.
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout).is_ok(),
        "what the masker returned is the response's own JSON, masked: {answer}"
    );
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_null(),
        "the text was placed in the response, so nothing falls back to the context channel: {answer}"
    );
}

/// The same hop for a spawn's own result. `Agent` returns the subagent's message beside
/// the run's metadata, and the codec restates it by the shape rather than by the longest
/// leaf: the text takes `content`, and the metadata the parent reads as the run's record
/// is kept as the harness wrote it. The engine's decision is a result decision either way
/// — a spawn's result is a result — so this is the same release as above, seen through the
/// restatement a spawn gets.
#[tokio::test]
async fn a_confined_spawn_result_takes_the_maskers_text_at_content_and_keeps_its_metadata() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = deployment(dir.path(), "Agent");
    let input = serde_json::json!({ "prompt": "Read the deploy key.", "subagent_type": "general-purpose" });
    through_the_masker(&runtime, &proposal("Agent", input.clone())).await;

    let response = serde_json::json!({
        "content": [{ "type": "text", "text": format!("the child read AWS_SECRET_ACCESS_KEY={SECRET}") }],
        "agentId": "a1",
        "agentType": "general-purpose",
        "status": "completed",
        "totalTokens": 1234,
    });
    let (status, answer) = claude_hook(&runtime, &result("Agent", input, response)).await;
    assert_eq!(status, 200, "the spawn's result is answered: {answer}");
    let output = &answer["hookSpecificOutput"]["updatedToolOutput"];
    assert!(
        !answer.to_string().contains(SECRET),
        "no part of the answer carries the key material: {answer}"
    );
    assert_eq!(
        output["agentId"], "a1",
        "the run's own metadata names no part of the subagent's message and is kept: {answer}"
    );
    assert_eq!(output["agentType"], "general-purpose", "{answer}");
    assert_eq!(output["status"], "completed", "{answer}");
    assert_eq!(output["totalTokens"], 1234, "{answer}");
    assert_eq!(
        output["content"][0]["type"], "text",
        "the text takes the one field the parent model reads: {answer}"
    );
    let text = output["content"][0]["text"]
        .as_str()
        .expect("the content block carries the text");
    assert!(
        text.contains("[redacted-secret]"),
        "what the parent reads is what the masker returned: {answer}"
    );
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_null(),
        "`content` always takes the text, so a spawn's result never falls back: {answer}"
    );
}
