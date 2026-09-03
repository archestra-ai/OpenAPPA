mod common;
use common::{offers, repo_root};

use std::path::Path;

use appa_runtime::api::{AuditEvent, LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime, TrajectoryId};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent};

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
/// had it: every call released, every other hook without an opinion. The
/// parent's spawn is declared as spoken first unless a test declared it already.
async fn replay(runtime: &Runtime, events: &[serde_json::Value]) {
    for event in events {
        let name = event["hook_event_name"].as_str().expect("each event names its hook");
        if is_parent_spawn(event) {
            match hooks::handle(runtime, parsed(event)).await {
                HookDecision::DenyCall { .. } => declare_spawn(runtime, event, None, as_spoken()).await,
                HookDecision::AllowCall { spawn: Some(_) } => continue,
                other => panic!("the recorded spawn answered {other:?}"),
            }
        }
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

fn parsed(event: &serde_json::Value) -> HookEvent {
    let body = serde_json::to_vec(event).expect("the event serializes");
    (appa_adapter_claude_code::codec().parse)(&body)
        .expect("the recorded event parses")
        .expect("the recorded event maps to a hook event")
}

/// The parent's own Agent call: the spawn the return menu gates.
fn is_parent_spawn(event: &serde_json::Value) -> bool {
    event["hook_event_name"] == "PreToolUse" && event["tool_name"] == "Agent" && event.get("agent_id").is_none()
}

/// The bare declaration: the return crosses as spoken, floored at the parent's current label.
fn as_spoken() -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling::default()),
        return_schema: None,
    }
}

fn floored_at(trust: &str) -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling {
            trust: Some(trust.to_string()),
            audience: None,
        }),
        return_schema: None,
    }
}

/// Propose the recorded spawn: blocked on the return menu, the parent declares the
/// return through `route` (none: as spoken) with `arguments`. The re-proposed spawn
/// then releases, as `replay` asserts.
async fn declare_spawn(runtime: &Runtime, spawn: &serde_json::Value, route: Option<&str>, arguments: RemedyArguments) {
    let HookEvent::ToolCall { actor, .. } = parsed(spawn) else {
        panic!("the spawn is a tool call");
    };
    let decision = hooks::handle(runtime, parsed(spawn)).await;
    let HookDecision::DenyCall { offers, .. } = decision else {
        panic!("a marked spawn blocks until its return is declared, got {decision:?}");
    };
    let offer = offers
        .iter()
        .find(|offer| {
            offer.returns.as_ref().map(|offered| match offered {
                appa_runtime_api::OfferedReturn::AsSpoken => None,
                appa_runtime_api::OfferedReturn::Sanitized { sanitizer } => Some(sanitizer.as_str()),
            }) == Some(route)
        })
        .expect("the menu offers the requested return route");
    let declared = runtime
        .execute_remedy_with(&actor, OfferId(offer.id.clone()), arguments)
        .await;
    assert!(
        matches!(declared, RemedyOutcome::Authorized { .. }),
        "the declaration approves the spawn, got {declared:?}"
    );
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
        answer["hookSpecificOutput"]["permissionDecision"], "allow",
        "a return leaves the subagent live to work on: {answer}"
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
async fn a_repeated_stop_answers_as_before_and_a_different_return_crosses_again() {
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
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "a later stop with something new is a later return"
    );
    assert_eq!(
        returns(&runtime, &root),
        vec![None, None],
        "a child returns as often as it stops with something new"
    );
}

/// Claude Code's Agent result names the subagent by id; when the start hook
/// never came, the result is where the child binds to the spawn in flight.
#[tokio::test]
async fn an_agent_result_binds_the_child_whose_start_never_came() {
    let runtime = deployment("", "", "delta = {}");
    let root = ASYNC.root();
    let events = ASYNC.events();
    let start = index_of(&events, &hook(&events, "SubagentStart", None, true));
    let ack = hook(&events, "PostToolUse", Some("Agent"), false);

    replay(&runtime, &events[..start]).await;
    assert_eq!(forks(&runtime, &root), 0, "no start yet, no binding");
    replay(&runtime, std::slice::from_ref(&ack)).await;
    assert_eq!(forks(&runtime, &root), 1, "the Agent result bound the child it names");

    let stop = child_stop(&events);
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "the bound child's stop crosses"
    );
    assert_eq!(returns(&runtime, &root), vec![None]);
}

#[tokio::test]
async fn a_stop_with_no_spawn_at_all_is_blocked() {
    let runtime = deployment("", "", "delta = {}");
    let root = ASYNC.root();
    let events = ASYNC.events();
    replay(&runtime, &events[..1]).await;

    let stop = child_stop(&events);
    for attempt in [stop.clone(), re_fired(stop)] {
        let (status, answer) = call(&runtime, &attempt).await;
        assert_eq!(status, 200, "{answer}");
        blocked(&answer);
        assert!(returns(&runtime, &root).is_empty(), "nothing crossed");
        assert_eq!(forks(&runtime, &root), 0, "a stop never opens a child");
    }
}

/// The subagent's read ranks as suspicious. Under the floor the parent declared as
/// spoken — its own trusted label — the subagent is offered no acceptance: nothing it
/// admits may fall below what its return could carry.
#[tokio::test]
async fn a_subagent_under_the_parents_own_floor_cannot_accept_a_suspicious_read() {
    let runtime = deployment("", "", "delta = { trust = \"suspicious\" }");
    let events = ASYNC.events();
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));
    replay(&runtime, &events[..=ack]).await;

    let read = hook(&events, "PreToolUse", Some("Bash"), true);
    let (status, answer) = call(&runtime, &read).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the block carries its reason");
    assert!(
        offers(reason).is_empty(),
        "no acceptance below the declared floor is offered: {reason}"
    );
}

/// The parent declared at the spawn that it takes a suspicious return. The subagent
/// accepts its suspicious read, its stop crosses at once, and the parent stands
/// narrowed to the floor it declared.
#[tokio::test]
async fn a_parent_that_declared_a_suspicious_floor_takes_the_narrowing_return_at_the_stop() {
    let runtime = deployment("", "", "delta = { trust = \"suspicious\" }");
    let root = ASYNC.root();
    let child = ASYNC.child();
    let events = ASYNC.events();
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));
    declare_spawn(
        &runtime,
        &hook(&events, "PreToolUse", Some("Agent"), false),
        None,
        floored_at("suspicious"),
    )
    .await;
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
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "the return crosses at the stop: the parent settled the narrowing at the spawn"
    );
    assert_eq!(returns(&runtime, &root), vec![None]);
    assert_eq!(
        runtime.status(&root).expect("the root answers").trust,
        "suspicious",
        "the crossing narrowed the parent to the floor it declared"
    );
}

/// The parent routed the return through a sanitizer at the spawn. The subagent's
/// stop is held with the sanitized message to return instead; its next stop with
/// exactly that message crosses.
#[tokio::test]
async fn a_return_routed_through_a_sanitizer_is_echoed_sanitized_before_it_crosses() {
    let runtime = deployment(
        r#"
[[policy.sanitizer]]
name = "redactor"
on = ["tool_output"]
permits = { audience = { from = ["internal"], to = ["public"] } }
"#,
        "[externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n",
        "delta = {}",
    );
    let root = SYNC.root();
    let events = SYNC.events();
    let start = index_of(&events, &hook(&events, "SubagentStart", None, true));
    let stop = index_of(&events, &child_stop(&events));
    declare_spawn(
        &runtime,
        &hook(&events, "PreToolUse", Some("Agent"), false),
        Some("redactor"),
        as_spoken(),
    )
    .await;
    replay(&runtime, &events[..start]).await;
    let (status, answer) = call(&runtime, &events[start]).await;
    assert_eq!(status, 200, "{answer}");
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_string(),
        "the start tells the subagent its return goes through the sanitizer: {answer}"
    );
    replay(&runtime, &events[start + 1..stop]).await;

    let mut stop = events[stop].clone();
    stop["last_assistant_message"] = serde_json::json!("one file; ask bob@example.com for more");
    let HookDecision::ChildReturn { value } = hooks::handle(&runtime, parsed(&stop)).await else {
        panic!("the stop is held with the sanitized message to return");
    };
    assert!(!value.contains("bob@example.com"), "the raw address is gone: {value}");
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(status, 200, "{answer}");
    assert!(
        !blocked(&answer).contains("bob@example.com"),
        "the raw return is not echoed: {answer}"
    );
    assert!(returns(&runtime, &root).is_empty(), "nothing crossed yet");

    stop["last_assistant_message"] = serde_json::json!(value);
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(
        (status, answer),
        (200, serde_json::json!({})),
        "the echoed message crosses"
    );
    assert_eq!(returns(&runtime, &root), vec![Some("redactor".to_string())]);
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
