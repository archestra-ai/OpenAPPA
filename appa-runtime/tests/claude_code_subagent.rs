mod common;
use common::{offers, repo_root};

use std::path::Path;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime, TrajectoryId};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::Actor;

/// One recorded Claude Code session: the hook bodies it delivered, in
/// order, scrubbed of local paths.
struct Recording {
    file: &'static str,
    session: &'static str,
}

/// `claude -p` on Claude Code 2.1.257: the Agent tool waits for the
/// subagent, so the subagent's stop precedes the parent's Agent result,
/// which repeats the subagent's final message verbatim.
const SYNC: Recording = Recording {
    file: "hooks-sync.jsonl",
    session: "0ac20467-d438-4197-9653-b3ba99f47601",
};

/// An interactive session on Claude Code 2.1.257: the Agent tool answers
/// with a launch acknowledgement at once and the parent's turn ends while
/// the subagent runs; two of Claude Code's own helpers stop with an empty
/// `agent_type` and no start of their own.
const ASYNC: Recording = Recording {
    file: "hooks-async.jsonl",
    session: "43902e36-65fc-4350-a315-1ea874368609",
};

impl Recording {
    fn events(&self) -> Vec<serde_json::Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(self.file);
        let events: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .expect("the recorded hook fixture is readable")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("each fixture line is JSON"))
            .collect();
        assert!(
            events.iter().all(|event| event["session_id"] == self.session),
            "the fixture holds one session"
        );
        events
    }

    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("cc:{}", self.session))
    }

    fn child(&self) -> TrajectoryId {
        let events = self.events();
        let start = hook(&events, "SubagentStart", None, true);
        TrajectoryId(format!(
            "cc:{}:{}",
            self.session,
            start["agent_id"].as_str().expect("the start names the subagent")
        ))
    }
}

fn hook(events: &[serde_json::Value], name: &str, tool: Option<&str>, subagent: bool) -> serde_json::Value {
    events
        .iter()
        .find(|event| {
            event["hook_event_name"] == name
                && tool.is_none_or(|tool| event["tool_name"] == tool)
                && event.get("agent_id").is_some() == subagent
        })
        .unwrap_or_else(|| panic!("the fixture carries a {name} {tool:?} (subagent: {subagent})"))
        .clone()
}

/// The subagent's own stop: the one whose `agent_type` names an agent
/// definition. Claude Code's helpers stop with an empty type.
fn child_stop(events: &[serde_json::Value]) -> serde_json::Value {
    events
        .iter()
        .find(|event| {
            event["hook_event_name"] == "SubagentStop" && event["agent_type"].as_str().is_some_and(|t| !t.is_empty())
        })
        .expect("the fixture carries the subagent's stop")
        .clone()
}

fn index_of(events: &[serde_json::Value], event: &serde_json::Value) -> usize {
    events
        .iter()
        .position(|e| e == event)
        .expect("the event is in the recording")
}

fn as_root(mut event: serde_json::Value) -> serde_json::Value {
    let fields = event.as_object_mut().expect("a hook event is an object");
    fields.remove("agent_id");
    fields.remove("agent_type");
    event
}

fn re_fired(mut stop: serde_json::Value) -> serde_json::Value {
    stop["stop_hook_active"] = serde_json::json!(true);
    stop
}

/// The shipped example with `policy_extra` spliced in after the deployment
/// table, plus deterministic `Bash` and `Read` tools. These tests exercise
/// trajectory binding, not the default's model-backed compatibility
/// fallback; `bash_delta` is what the recorded Bash output carries.
fn deployment(policy_extra: &str, externals_extra: &str, bash_delta: &str) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("integrations/claude-code/examples/claude-code.appa.toml"))
        .expect("the shipped example is readable");
    let (policy, externals) = example
        .split_once("[externals]")
        .expect("the example carries an [externals] table");
    let deployment = "[policy.deployment]\ncontext_control = true\n";
    let (before_deployment, after_deployment) = policy
        .split_once(deployment)
        .expect("the example carries the context-controlling deployment");
    let tools = format!(
        "[[policy.tool]]\nname = \"Bash\"\n{bash_delta}\n\
         [[policy.tool]]\nname = \"Read\"\ndelta = {{}}\n"
    );
    let text = format!(
        "{before_deployment}{deployment}{policy_extra}\n{tools}{after_deployment}[externals]{externals}\n{externals_extra}"
    );
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).expect("the deployment writes");
    let config = Config::load(&path).expect("the deployment loads");
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens")
}

async fn call(runtime: &Runtime, event: &serde_json::Value) -> (u16, serde_json::Value) {
    let body = serde_json::to_vec(event).expect("the event serializes");
    hooks::answer(runtime, &appa_adapter_claude_code::codec(), &body).await
}

fn forks(runtime: &Runtime, root: &TrajectoryId) -> usize {
    runtime
        .audit(root)
        .expect("the audit reads")
        .iter()
        .filter(|entry| matches!(entry.event, AuditEvent::Forked { .. }))
        .count()
}

fn returns(runtime: &Runtime, root: &TrajectoryId) -> Vec<Option<String>> {
    runtime
        .audit(root)
        .expect("the audit reads")
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::ChildReturn { sanitizer, .. } => Some(sanitizer),
            _ => None,
        })
        .collect()
}

/// Replay `events` as recorded, asserting each answers as the recording
/// had it: every call released, every other hook without an opinion.
async fn replay(runtime: &Runtime, events: &[serde_json::Value]) {
    for event in events {
        let name = event["hook_event_name"].as_str().expect("each event names its hook");
        let (status, answer) = call(runtime, event).await;
        assert_eq!(status, 200, "{name} answered {answer}");
        match name {
            "PreToolUse" => assert_eq!(
                answer["hookSpecificOutput"]["permissionDecision"], "allow",
                "{name} {} is released: {answer}",
                event["tool_name"]
            ),
            _ => assert_eq!(answer, serde_json::json!({}), "{name} carries no opinion: {answer}"),
        }
    }
}

fn blocked(answer: &serde_json::Value) -> &str {
    assert_eq!(answer["decision"], "block", "{answer}");
    answer["reason"].as_str().expect("a block carries its reason")
}

#[tokio::test]
async fn the_synchronous_recording_crosses_the_return_at_the_subagents_stop() {
    let runtime = deployment("", "", "delta = {}");
    let root = SYNC.root();
    let events = SYNC.events();
    let stop = index_of(&events, &child_stop(&events));

    replay(&runtime, &events[..=stop]).await;
    assert_eq!(forks(&runtime, &root), 1, "the start bound the one spawn in flight");
    assert_eq!(
        returns(&runtime, &root),
        vec![None],
        "the return crossed at the stop, as the subagent spelled it"
    );

    replay(&runtime, &events[stop + 1..]).await;
    assert_eq!(
        returns(&runtime, &root),
        vec![None],
        "the parent's Agent result repeats the return and crosses nothing new"
    );

    let (status, answer) = call(&runtime, &hook(&events, "PreToolUse", Some("Bash"), true)).await;
    assert_eq!(status, 200);
    assert_eq!(
        answer["hookSpecificOutput"]["permissionDecision"], "deny",
        "the returned subagent proposes nothing more: {answer}"
    );
    let (status, answer) = call(&runtime, &as_root(hook(&events, "PreToolUse", Some("Bash"), true))).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
}

#[tokio::test]
async fn the_asynchronous_recording_returns_while_the_parent_is_free() {
    let runtime = deployment("", "", "delta = {}");
    let root = ASYNC.root();
    let events = ASYNC.events();
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));
    let stop = index_of(&events, &child_stop(&events));

    replay(&runtime, &events[..=ack]).await;
    assert_eq!(forks(&runtime, &root), 1, "the start bound the one spawn in flight");
    assert!(
        returns(&runtime, &root).is_empty(),
        "the launch acknowledgement crosses nothing"
    );

    let (status, answer) = call(&runtime, &as_root(hook(&events, "PreToolUse", Some("Bash"), true))).await;
    assert_eq!(status, 200);
    assert_eq!(
        answer["hookSpecificOutput"]["permissionDecision"], "allow",
        "the acknowledgement closed the spawn call, so the parent proposes freely: {answer}"
    );

    // The parent's turn ends, a helper stops, and the subagent works on.
    replay(&runtime, &events[ack + 1..stop]).await;
    assert_eq!(forks(&runtime, &root), 1, "a helper's stop binds nothing");
    assert!(returns(&runtime, &root).is_empty(), "a helper's stop crosses nothing");

    replay(&runtime, &events[stop..]).await;
    assert_eq!(
        returns(&runtime, &root),
        vec![None],
        "the subagent's stop crossed its return; the later helper and parent turns crossed nothing"
    );
}

#[tokio::test]
async fn a_repeated_stop_answers_as_before_and_a_different_return_is_blocked() {
    let runtime = deployment("", "", "delta = {}");
    let root = ASYNC.root();
    let events = ASYNC.events();
    replay(&runtime, &events).await;
    let stop = child_stop(&events);

    for again in [stop.clone(), re_fired(stop.clone())] {
        let (status, answer) = call(&runtime, &again).await;
        assert_eq!(
            (status, answer),
            (200, serde_json::json!({})),
            "the same stop answers as it did"
        );
    }

    let mut other = re_fired(stop);
    other["last_assistant_message"] = serde_json::json!("a different report");
    let (status, answer) = call(&runtime, &other).await;
    assert_eq!(status, 200, "{answer}");
    blocked(&answer);
    assert_eq!(returns(&runtime, &root), vec![None], "a child returns once");
}

#[tokio::test]
async fn a_stop_from_a_child_the_family_never_saw_start_is_blocked() {
    let runtime = deployment("", "", "delta = {}");
    let root = ASYNC.root();
    let events = ASYNC.events();
    let start = index_of(&events, &hook(&events, "SubagentStart", None, true));
    let ack = hook(&events, "PostToolUse", Some("Agent"), false);

    replay(&runtime, &events[..start]).await;
    replay(&runtime, std::slice::from_ref(&ack)).await;
    assert_eq!(forks(&runtime, &root), 0, "no start, no binding");

    let stop = child_stop(&events);
    for attempt in [stop.clone(), re_fired(stop)] {
        let (status, answer) = call(&runtime, &attempt).await;
        assert_eq!(status, 200, "{answer}");
        blocked(&answer);
        assert!(returns(&runtime, &root).is_empty(), "nothing crossed");
        assert_eq!(forks(&runtime, &root), 0, "a stop never opens a child");
    }
}

/// The subagent read something the policy ranks as suspicious, so its
/// return would narrow the parent. The stop is blocked and the subagent
/// held while the parent, free since the launch acknowledgement, decides;
/// once the parent accepts the narrowing the held stop crosses.
#[tokio::test]
async fn a_narrowing_return_is_held_at_the_stop_until_the_parent_accepts_it() {
    let runtime = deployment("", "", "delta = { trust = \"suspicious\" }");
    let root = ASYNC.root();
    let child = ASYNC.child();
    let events = ASYNC.events();
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));
    replay(&runtime, &events[..=ack]).await;

    let read = hook(&events, "PreToolUse", Some("Bash"), true);
    let (status, answer) = call(&runtime, &read).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the block names its offers");
    let offer = offers(reason)
        .pop()
        .expect("the suspicious read is offered for acceptance");
    let acting_child = Actor {
        root: root.clone(),
        child: Some(child.clone()),
    };
    let accepted = runtime.execute_remedy(&acting_child, offer).await;
    assert!(
        matches!(accepted, RemedyOutcome::Authorized { .. }),
        "the subagent accepts the narrowing: {accepted:?} (offered by: {reason})"
    );
    replay(&runtime, &[read, hook(&events, "PostToolUse", Some("Bash"), true)]).await;

    let stop = child_stop(&events);
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(status, 200, "{answer}");
    let reason = blocked(&answer).to_string();
    assert!(returns(&runtime, &root).is_empty(), "the held return crossed nothing");
    let (status, answer) = call(&runtime, &re_fired(stop.clone())).await;
    assert_eq!(status, 200, "{answer}");
    blocked(&answer);

    let acceptance = offers(&reason)
        .pop()
        .expect("the held return offers the parent its acceptance");
    let acting_root = Actor {
        root: root.clone(),
        child: None,
    };
    let RemedyOutcome::Returned { value } = runtime.execute_remedy(&acting_root, acceptance).await else {
        panic!("the parent's acceptance crosses the held return");
    };
    assert_eq!(Some(value.as_str()), stop["last_assistant_message"].as_str());
    assert_eq!(returns(&runtime, &root), vec![None]);

    let (status, answer) = call(&runtime, &re_fired(stop)).await;
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "the re-fired stop finds its return crossed and lets the subagent finish"
    );
}

/// The parent receives the subagent's message through a channel no hook
/// rewrites, so a deployment whose policy would put a sanitized return in
/// the parent's hands is refused before the runtime serves it. Should one
/// reach the dispatcher anyway, the stop fails closed.
#[tokio::test]
async fn a_return_the_policy_would_sanitize_blocks_the_stop() {
    let runtime = deployment(
        r#"confined_child_return = true

[[policy.sanitizer]]
name = "redactor"
on = ["tool_output"]
permits = { audience = { from = ["internal"], to = ["public"] } }

[policy.child]
return_sanitizer = "redactor"
"#,
        "[externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n",
        "delta = {}",
    );
    let events = SYNC.events();
    let stop = index_of(&events, &child_stop(&events));
    replay(&runtime, &events[..stop]).await;

    let mut stop = events[stop].clone();
    stop["last_assistant_message"] = serde_json::json!("one file; ask bob@example.com for more");
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(status, 200, "{answer}");
    let reason = blocked(&answer);
    assert!(
        !reason.contains("bob@example.com"),
        "the raw return is not echoed: {reason}"
    );
}

#[tokio::test]
async fn an_agent_result_naming_another_subagent_is_withheld() {
    let runtime = deployment("", "", "delta = {}");
    let root = SYNC.root();
    let events = SYNC.events();
    let stop = index_of(&events, &child_stop(&events));
    replay(&runtime, &events[..=stop]).await;

    let mut result = hook(&events, "PostToolUse", Some("Agent"), false);
    result["tool_response"]["agentId"] = serde_json::json!("someone-else");
    let (status, answer) = call(&runtime, &result).await;
    assert_eq!(
        status, 200,
        "a mismatch is a decision the model hears, not a fault: {answer}"
    );
    assert_eq!(answer["decision"], "block");
    let text = answer["hookSpecificOutput"]["updatedToolOutput"]["content"][0]["text"]
        .as_str()
        .expect("the withheld result restates the delivered shape");
    assert!(text.starts_with("[appa] the tool result was withheld: "), "{answer}");
    assert_eq!(
        answer["hookSpecificOutput"]["updatedToolOutput"]["agentId"], "someone-else",
        "the rest of the response is restated as delivered",
    );
    assert_eq!(
        returns(&runtime, &root),
        vec![None],
        "only the subagent's own stop crossed"
    );

    let (status, answer) = call(&runtime, &as_root(hook(&events, "PreToolUse", Some("Bash"), true))).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
}

#[tokio::test]
async fn a_start_with_no_spawn_in_flight_refuses_and_its_calls_are_denied() {
    let runtime = deployment("", "", "delta = {}");
    let root = SYNC.root();
    let events = SYNC.events();
    replay(&runtime, &events[..1]).await;

    let (status, _) = call(&runtime, &hook(&events, "SubagentStart", None, true)).await;
    assert_eq!(status, 409, "no spawn in flight: the start refuses");
    let (status, answer) = call(&runtime, &hook(&events, "PreToolUse", Some("Bash"), true)).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    assert_eq!(forks(&runtime, &root), 0);
}
