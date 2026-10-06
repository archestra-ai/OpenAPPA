mod common;
use common::{claude_event, claude_hook, offers, repo_root};

use std::path::Path;

use appa_runtime::api::{AuditEvent, LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime, TrajectoryId};
use appa_runtime::config::Config;
use appa_runtime::hooks;
use appa_runtime_api::{Actor, AdapterName, HookDecision, HookEvent, LaunchStart, LaunchToken, WireEvent};

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

/// `claude -p` running a `Workflow` script that pipes one agent's final message into the next
/// agent's prompt: the second agent starts after the first one's stop.
const PIPELINE: Recording = Recording {
    file: "hooks-workflow-pipeline.jsonl",
    session: "f4eb717d-91df-4af3-9bc0-ca4117b95c71",
};

/// `claude -p` running a `Workflow` script with two agents: one called with a schema, which
/// returns through `StructuredOutput` and stops with no message, and one that runs `Bash` and
/// returns at its stop.
const STRUCTURED: Recording = Recording {
    file: "hooks-workflow-structured.jsonl",
    session: "b9898015-3117-476b-9040-7aebc4912ca4",
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

/// The shipped root with `policy_extra` spliced in after the deployment
/// table, plus deterministic `Bash` and `Read` tools under the canonical
/// names the served adapter maps, in place of the battery's model-backed
/// rules. These tests exercise trajectory binding; `bash_delta` is what the
/// recorded Bash output carries.
fn deployment(policy_extra: &str, externals_extra: &str, bash_delta: &str) -> Runtime {
    let example = std::fs::read_to_string(repo_root().join("marketplace/plugins/claude-code/default.appa.toml"))
        .expect("the shipped example is readable");
    let (policy, externals) = example
        .split_once("[externals]")
        .expect("the example carries an [externals] table");
    let deployment = "[policy.deployment]\ncontext_control = true\n";
    let (before_deployment, after_deployment) = policy
        .split_once(deployment)
        .expect("the example carries the context-controlling deployment");
    let tools = format!(
        "[[policy.tool]]\nname = \"host/claude-code/Read\"\ndelta = {{}}\n\
         [[policy.tool]]\nname = \"host/claude-code/Bash\"\n{bash_delta}\n"
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

/// One recorded hook through the served dispatcher, as `appa hook` carries it.
async fn call(runtime: &Runtime, event: &serde_json::Value) -> (u16, serde_json::Value) {
    claude_hook(runtime, event).await
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
            "PreToolUse" => assert_released(event, &answer),
            _ => assert_eq!(answer, serde_json::json!({}), "{name} carries no opinion: {answer}"),
        }
    }
}

/// A released call answers `allow`, except in auto mode, where it is left to Claude Code's
/// classifier with no decision.
fn assert_released(event: &serde_json::Value, answer: &serde_json::Value) {
    match event["permission_mode"].as_str() {
        Some("auto") => assert_eq!(answer, &serde_json::json!({}), "released to the classifier: {answer}"),
        _ => assert_eq!(
            answer["hookSpecificOutput"]["permissionDecision"], "allow",
            "released: {answer}"
        ),
    }
}

fn blocked(answer: &serde_json::Value) -> &str {
    assert_eq!(answer["decision"], "block", "{answer}");
    answer["reason"].as_str().expect("a block carries its reason")
}

/// The recorded hook as the served runtime reads it: its tool is the canonical one.
fn parsed(event: &serde_json::Value) -> HookEvent {
    claude_event(event).expect("the recorded event maps to a hook event")
}

/// The parent's own Agent or Workflow call: the spawn the return menu gates.
fn is_parent_spawn(event: &serde_json::Value) -> bool {
    event["hook_event_name"] == "PreToolUse"
        && (event["tool_name"] == "Agent" || event["tool_name"] == "Workflow")
        && event.get("agent_id").is_none()
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

fn attested_at(audience: Option<&str>) -> RemedyArguments {
    RemedyArguments {
        label: Some(LabelSpelling {
            trust: None,
            audience: audience.map(|audience| vec![audience.to_string()]),
        }),
        return_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "status": { "type": "string", "enum": ["posted", "failed"] } },
            "required": ["status"],
        })),
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

    let mut next_child_call = hook(&events, "PreToolUse", Some("Bash"), true);
    next_child_call["tool_use_id"] = serde_json::json!("toolu_test_after_return");
    let (status, answer) = call(&runtime, &next_child_call).await;
    assert_eq!(status, 200);
    // A return leaves the subagent live to work on.
    assert_released(&next_child_call, &answer);
    let proposal = as_root(hook(&events, "PreToolUse", Some("Bash"), true));
    let (status, answer) = call(&runtime, &proposal).await;
    assert_eq!(status, 200);
    assert_released(&proposal, &answer);
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

    // The acknowledgement closed the spawn call, so the parent proposes freely.
    let proposal = as_root(hook(&events, "PreToolUse", Some("Bash"), true));
    let (status, answer) = call(&runtime, &proposal).await;
    assert_eq!(status, 200);
    assert_released(&proposal, &answer);

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
        assert!(
            runtime.audit(&root).is_none(),
            "a stop opens neither a child nor its family"
        );
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

/// Delegation can isolate a suspicious, private read only when the return declaration permits
/// both dimensions in the child. Attestation unbinds trust, but a public audience floor still
/// refuses the read. Declaring the private audience admits it, and an empty return then ends the
/// child without narrowing the parent.
#[tokio::test]
async fn delegation_guidance_leads_to_a_floor_that_admits_the_private_read_before_an_empty_return() {
    let make_runtime = || deployment("", "", "delta = { trust = \"suspicious\", audience = [\"self\"] }");
    let events = ASYNC.events();
    let spawn = hook(&events, "PreToolUse", Some("Agent"), false);
    let read = hook(&events, "PreToolUse", Some("Bash"), true);
    let start = index_of(&events, &hook(&events, "SubagentStart", None, true));
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));

    let incompatible = make_runtime();
    replay(&incompatible, &events[..1]).await;
    let (status, answer) = call(&incompatible, &as_root(read.clone())).await;
    assert_eq!(status, 200);
    let guidance = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the root's narrowing block carries delegation guidance");
    assert!(
        guidance.contains("choose a floor and sanitizer whose combined route permits every narrowing"),
        "{guidance}"
    );
    assert!(
        guidance.contains("Returning nothing controls what crosses back; it does not relax what the child may"),
        "{guidance}"
    );

    let HookDecision::DenyCall { feedback, .. } = hooks::handle(&incompatible, parsed(&spawn)).await else {
        panic!("the spawn asks for its return declaration");
    };
    assert!(
        feedback.contains(
            "`attest-schema` unbinds trust by raising the return's trust; it does not widen or unbind audience"
        ),
        "{feedback}"
    );
    assert!(
        feedback.contains("label: {audience: [\"<audience-entry>\"]}"),
        "the attestation route asks for its bound audience dimension: {feedback}"
    );

    declare_spawn(&incompatible, &spawn, Some("attest-schema"), attested_at(None)).await;
    replay(&incompatible, &events[1..start]).await;
    let (status, answer) = call(&incompatible, &events[start]).await;
    assert_eq!(status, 200);
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_string(),
        "the attested spawn tells the child its return schema: {answer}"
    );
    replay(&incompatible, &events[start + 1..=ack]).await;
    let (status, answer) = call(&incompatible, &read).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the incompatible child read carries its refusal");
    assert!(
        offers(reason).is_empty(),
        "attestation does not unbind the public audience floor: {reason}"
    );

    let compatible = make_runtime();
    replay(&compatible, &events[..1]).await;
    declare_spawn(&compatible, &spawn, Some("attest-schema"), attested_at(Some("self"))).await;
    replay(&compatible, &events[1..start]).await;
    let (status, answer) = call(&compatible, &events[start]).await;
    assert_eq!(status, 200);
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_string(),
        "the attested spawn tells the child its return schema: {answer}"
    );
    replay(&compatible, &events[start + 1..=ack]).await;
    let (status, answer) = call(&compatible, &read).await;
    assert_eq!(status, 200);
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the compatible child read carries its acceptance offer");
    let offer = offers(reason)
        .pop()
        .expect("the private audience floor permits accepting the suspicious/private read");
    let accepted = compatible
        .execute_remedy(
            &Actor {
                root: ASYNC.root(),
                child: Some(ASYNC.child()),
            },
            offer,
        )
        .await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    replay(&compatible, &[read, hook(&events, "PostToolUse", Some("Bash"), true)]).await;

    let mut empty_stop = child_stop(&events);
    empty_stop["last_assistant_message"] = serde_json::json!("");
    let (status, answer) = call(&compatible, &empty_stop).await;
    assert_eq!((status, answer), (200, serde_json::json!({})));
    assert!(
        returns(&compatible, &ASYNC.root()).is_empty(),
        "an empty stop crosses no value"
    );
    let parent = compatible.status(&ASYNC.root()).expect("the parent answers");
    assert_eq!((parent.trust.as_str(), parent.audience.as_str()), ("trusted", "public"));
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
    answer["hookSpecificOutput"]["updatedToolOutput"]["content"][0]["text"]
        .as_str()
        .expect("the withheld result restates the delivered shape");
    let stopped = child_stop(&events);
    let said = stopped["last_assistant_message"]
        .as_str()
        .expect("the recorded stop carries the child's message");
    assert!(
        !answer.to_string().contains(said),
        "a result naming a child the family never bound carries none of what that child said: {answer}"
    );
    assert_eq!(
        answer["hookSpecificOutput"]["updatedToolOutput"]["agentId"], "someone-else",
        "the rest of the response is restated as delivered",
    );
    assert_eq!(
        returns(&runtime, &root),
        vec![None],
        "only the subagent's own stop crossed"
    );

    let proposal = as_root(hook(&events, "PreToolUse", Some("Bash"), true));
    let (status, answer) = call(&runtime, &proposal).await;
    assert_eq!(status, 200);
    assert_released(&proposal, &answer);
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
    assert!(runtime.audit(&root).is_none(), "a child's events open no family");
}

fn structured_output(events: &[serde_json::Value]) -> usize {
    index_of(events, &hook(events, "PreToolUse", Some("StructuredOutput"), true))
}

/// A root `Read` of `path`, under the recorded session's prompt.
fn root_read(events: &[serde_json::Value], path: &str, call: &str) -> serde_json::Value {
    let mut read = hook(events, "PreToolUse", Some("Workflow"), false);
    read["tool_name"] = serde_json::json!("Read");
    read["tool_input"] = serde_json::json!({ "file_path": path });
    read["tool_use_id"] = serde_json::json!(call);
    read
}

#[tokio::test]
async fn a_workflow_pipeline_forks_each_agent_under_one_declaration_until_its_notice() {
    let runtime = deployment("", "", "delta = {}");
    let root = PIPELINE.root();
    let events = PIPELINE.events();
    let notice = events
        .iter()
        .position(|event| {
            event["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.starts_with("<task-notification>"))
        })
        .expect("the recording carries the workflow's completion notice");

    replay(&runtime, &events[..notice]).await;
    assert_eq!(
        forks(&runtime, &root),
        2,
        "each agent binds its own fork of the one declaration"
    );
    assert_eq!(
        returns(&runtime, &root),
        vec![None, None],
        "each agent's stop crossed as spoken, the first before the second started"
    );

    let mut late = hook(&events, "SubagentStart", None, true);
    late["agent_id"] = serde_json::json!("alate");
    replay(&runtime, &events[notice..]).await;
    let (_, answer) = call(&runtime, &late).await;
    assert!(
        answer["error"].is_string(),
        "an agent starting after the workflow's notice has no fork to bind: {answer}"
    );
    assert_eq!(forks(&runtime, &root), 2);
}

#[tokio::test]
async fn a_structured_output_crosses_as_the_agents_return_and_its_empty_stop_crosses_nothing() {
    let runtime = deployment("", "", "delta = {}");
    let root = STRUCTURED.root();
    let events = STRUCTURED.events();
    replay(&runtime, &events).await;
    assert_eq!(forks(&runtime, &root), 2);
    assert_eq!(
        returns(&runtime, &root),
        vec![None, None],
        "the StructuredOutput input and the Bash agent's stop crossed; the empty stop crossed nothing"
    );

    let transcript = hook(&events, "SubagentStop", None, true)["agent_transcript_path"]
        .as_str()
        .expect("the start names the agent's transcript")
        .to_string();
    let (_, answer) = call(&runtime, &root_read(&events, &transcript, "toolu_read_transcript")).await;
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    let journal = transcript.rsplit_once('/').expect("a path").0.to_string() + "/journal.jsonl";
    let (_, answer) = call(&runtime, &root_read(&events, &journal, "toolu_read_journal")).await;
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow", "{answer}");
}

#[tokio::test]
async fn a_workflow_return_routed_through_a_sanitizer_is_denied_with_the_input_that_crosses() {
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
    let root = STRUCTURED.root();
    let events = STRUCTURED.events();
    let output = structured_output(&events);
    declare_spawn(
        &runtime,
        &hook(&events, "PreToolUse", Some("Workflow"), false),
        Some("redactor"),
        as_spoken(),
    )
    .await;
    let start = index_of(&events, &hook(&events, "SubagentStart", None, true));
    replay(&runtime, &events[..start]).await;
    let (_, answer) = call(&runtime, &events[start]).await;
    assert!(
        answer["hookSpecificOutput"]["additionalContext"].is_string(),
        "the agent is told its return goes through the sanitizer: {answer}"
    );
    replay(&runtime, &events[start + 1..output]).await;

    let mut returned = events[output].clone();
    returned["tool_input"] = serde_json::json!({ "summary": "ask bob@example.com today" });
    let HookDecision::ChildReturn { value } = hooks::handle(&runtime, parsed(&returned)).await else {
        panic!("the return is held with the sanitized input to pass instead");
    };
    assert!(!value.contains("bob@example.com"), "{value}");
    let (_, answer) = call(&runtime, &returned).await;
    assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
    assert!(returns(&runtime, &root).is_empty(), "nothing crossed yet");

    returned["tool_input"] = serde_json::from_str(&value).unwrap_or_else(|error| panic!("{error}: {value}"));
    let (_, answer) = call(&runtime, &returned).await;
    assert_eq!(
        answer["hookSpecificOutput"]["permissionDecision"], "allow",
        "the exact sanitized input crosses: {answer}"
    );
    assert_eq!(returns(&runtime, &root), vec![Some("redactor".to_string())]);
}

const LAUNCH: &str = "0b6c1f5e-8f0a-4a57-9d6e-2f7c3e1a9b10";
const OTHER_LAUNCH: &str = "5d0f7b2a-4c1e-4f3b-8a9d-6e2c1b0a7f34";
const CLEARED: &str = "9ca30274-2b1e-4c4f-9d0a-1f2e3d4c5b6a";

/// A session start as `appa hook` posts it inside a protected launch: Claude Code's own
/// start, with the launch (and a fork's parent) the launcher put in its environment. The
/// answer is the wire decision.
async fn launched_start(
    runtime: &Runtime,
    session: &str,
    source: &str,
    launch: &str,
    forked_from: Option<&str>,
) -> (u16, serde_json::Value) {
    let host = serde_json::json!({"hook_event_name": "SessionStart", "session_id": session, "source": source});
    let HookEvent::SessionStart {
        root,
        principal,
        address,
        title,
        start,
        launch: None,
    } = parsed(&host)
    else {
        panic!("a session start parses as one");
    };
    let event = HookEvent::SessionStart {
        root,
        principal,
        address,
        title,
        start,
        launch: Some(LaunchStart {
            launch: LaunchToken::parse(launch).expect("the token parses"),
            forked_from: forked_from.map(|session| TrajectoryId(format!("cc:{session}"))),
        }),
    };
    let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("the start translates");
    let wire = serde_json::to_vec(&wire).expect("the start serializes");
    hooks::answer(runtime, &appa_adapter_claude_code::adapter(), &wire).await
}

fn decision(answer: &serde_json::Value) -> &str {
    answer["decision"].as_str().expect("a wire decision names itself")
}

/// The recorded events as Claude Code delivers them after it moved the session to `session`.
fn under(events: &[serde_json::Value], session: &str) -> Vec<serde_json::Value> {
    events
        .iter()
        .cloned()
        .map(|mut event| {
            event["session_id"] = serde_json::json!(session);
            event
        })
        .collect()
}

fn cc(session: &str) -> TrajectoryId {
    TrajectoryId(format!("cc:{session}"))
}

fn trust(runtime: &Runtime, root: &TrajectoryId) -> String {
    runtime.status(root).expect("the family has a status").trust
}

fn family_of(runtime: &Runtime, host: &TrajectoryId) -> Option<String> {
    runtime.status(host).map(|status| status.trajectory)
}

/// The root runs the recorded subagent's narrowing `Bash` call itself, accepting the
/// narrowing it is offered, so the family's label falls.
async fn root_bash(runtime: &Runtime, events: &[serde_json::Value]) {
    let call_event = as_root(hook(events, "PreToolUse", Some("Bash"), true));
    let (_, answer) = call(runtime, &call_event).await;
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("the narrowing call is denied with its offers");
    let offer = offers(reason).pop().expect("the narrowing is offered for acceptance");
    let accepted = runtime
        .execute_remedy(
            &Actor {
                root: ASYNC.root(),
                child: None,
            },
            offer,
        )
        .await;
    assert!(matches!(accepted, RemedyOutcome::Authorized { .. }), "{accepted:?}");
    replay(
        runtime,
        &[call_event, as_root(hook(events, "PostToolUse", Some("Bash"), true))],
    )
    .await;
}

/// Claude Code keeps a subagent running across `/clear` and reports its later hooks under
/// the cleared session's new id. The family it started in is still owed its return, so the
/// clear keeps the family, and the subagent's calls and return are that family's.
#[tokio::test]
async fn a_subagent_running_across_a_clear_stays_its_familys_child() {
    let runtime = deployment("", "", "delta = {}");
    let family = ASYNC.root();
    let events = ASYNC.events();
    let ack = index_of(&events, &hook(&events, "PostToolUse", Some("Agent"), false));
    let stop = index_of(&events, &child_stop(&events));

    let (status, _) = launched_start(&runtime, ASYNC.session, "startup", LAUNCH, None).await;
    assert_eq!(status, 200);
    replay(&runtime, &events[..=ack]).await;

    let (status, answer) = launched_start(&runtime, CLEARED, "clear", LAUNCH, None).await;
    assert_eq!(
        (status, decision(&answer)),
        (200, "context"),
        "the session is told its label stayed"
    );

    replay(&runtime, &under(&events[ack + 1..=stop], CLEARED)).await;
    let cleared = cc(CLEARED);
    assert!(
        runtime.audit(&cleared).is_none(),
        "the clear opened no family of its own"
    );
    assert_eq!(
        returns(&runtime, &family),
        vec![None],
        "the subagent's return crossed in its family"
    );
    assert_eq!(family_of(&runtime, &cleared), Some(family.0.clone()));

    // A later launch that resumes the cleared id reopens the family it continued.
    let (status, _) = launched_start(&runtime, CLEARED, "resume", OTHER_LAUNCH, None).await;
    assert_eq!(status, 200);
    assert!(
        runtime.audit(&cleared).is_none(),
        "the resume reopened the family, not a new one"
    );
}

/// A clear with nothing in flight or owed starts a family that carries none of the old one,
/// and the launch runs that family from then on.
#[tokio::test]
async fn a_clear_with_nothing_owed_starts_a_clean_family() {
    let runtime = deployment("", "", "delta = { trust = \"suspicious\" }");
    let family = ASYNC.root();
    let events = ASYNC.events();

    let (status, _) = launched_start(&runtime, ASYNC.session, "startup", LAUNCH, None).await;
    assert_eq!(status, 200);
    let clean = trust(&runtime, &family);
    root_bash(&runtime, &events).await;
    assert_ne!(trust(&runtime, &family), clean, "the call narrowed the family");

    let (status, answer) = launched_start(&runtime, CLEARED, "clear", LAUNCH, None).await;
    assert_eq!((status, decision(&answer)), (200, "ack"));
    assert_eq!(trust(&runtime, &cc(CLEARED)), clean, "the cleared session starts clean");

    let branched = cc("7194365e-0c1d-4e2f-8a3b-5c6d7e8f9a0b");
    let (status, _) = launched_start(&runtime, &branched.0[3..], "fork", LAUNCH, None).await;
    assert_eq!(status, 200);
    assert_eq!(family_of(&runtime, &branched), Some(cc(CLEARED).0));
}

/// `clappa --resume <id> --fork-session` copies a conversation APPA holds: the fork starts
/// from that family's label. A fork of a conversation APPA never held starts fresh, as a
/// resume of it does.
#[tokio::test]
async fn a_launched_fork_starts_from_its_parents_label() {
    let runtime = deployment("", "", "delta = { trust = \"suspicious\" }");
    let family = ASYNC.root();
    let events = ASYNC.events();
    let (status, _) = launched_start(&runtime, ASYNC.session, "startup", LAUNCH, None).await;
    assert_eq!(status, 200);
    let clean = trust(&runtime, &family);
    root_bash(&runtime, &events).await;
    let narrowed = trust(&runtime, &family);
    assert_ne!(narrowed, clean, "the call narrowed the family");

    let fork = "3172ca61-5e4d-4c3b-9a2f-1e0d9c8b7a65";
    let (status, _) = launched_start(&runtime, fork, "fork", OTHER_LAUNCH, Some(ASYNC.session)).await;
    assert_eq!(status, 200);
    assert_eq!(trust(&runtime, &cc(fork)), narrowed);

    let stranger = "a2637ad1-6f5e-4d4c-8b3a-2f1e0d9c8b7a";
    let (status, _) = launched_start(&runtime, stranger, "fork", THIRD_LAUNCH, Some("never-seen")).await;
    assert_eq!(status, 200);
    assert_eq!(trust(&runtime, &cc(stranger)), clean);
}

const THIRD_LAUNCH: &str = "c4e9a1b7-2d3f-4a5b-9c8d-7e6f5a4b3c2d";

/// A subagent's stop whose return APPA cannot check is held, and the stop that carries the
/// withheld return ends it.
#[tokio::test]
async fn a_stop_appa_cannot_check_ends_on_the_withheld_return() {
    let runtime = deployment("", "", "delta = {}");
    let events = ASYNC.events();
    replay(&runtime, &events[..1]).await;

    let stop = child_stop(&events);
    let (status, answer) = call(&runtime, &stop).await;
    assert_eq!(status, 200, "{answer}");
    blocked(&answer);

    let mut withheld = re_fired(stop);
    withheld["last_assistant_message"] = serde_json::json!(format!("{}\n", hooks::WITHHELD_RETURN));
    assert_eq!(call(&runtime, &withheld).await, (200, serde_json::json!({})));
}
