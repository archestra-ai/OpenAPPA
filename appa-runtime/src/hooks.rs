//! The hook dispatcher: one typed `HookEvent` in, one `HookDecision`
//! out.

use appa_runtime_api::{Actor, Codec, HookDecision, HookEvent, ParseRefusal, ProposedCall, Ruling, TrajectoryId};

use crate::api::{
    ChildReturnDecision, EventError, LateOpen, OfferId, Runtime, Session, SpawnResultDecision, ToolCallDecision,
    ToolResultDecision, is_control_tool,
};

/// One hook call, wire to wire: parse through the codec, dispatch,
/// render back, with the HTTP status the answer travels under. A
/// non-2xx status makes the hook command exit 2, which blocks the
/// action — hooks fail closed.
pub async fn answer(runtime: &Runtime, codec: &Codec, body: &[u8]) -> (u16, serde_json::Value) {
    let event = match (codec.parse)(body) {
        Ok(Some(event)) => event,
        Ok(None) => return (200, serde_json::json!({})),
        Err(ParseRefusal::Unreadable { detail }) => return (400, serde_json::json!({ "error": detail })),
        Err(ParseRefusal::Malformed { detail }) => return (409, serde_json::json!({ "error": detail })),
    };
    if let HookEvent::ToolCall { actor, call, .. } = &event {
        match runtime.opened_among(&actor.root, &(codec.names_children)(actor, call)) {
            Ok(Some(child)) => {
                tracing::debug!(root = %actor.root.0, child = %child.0, "a call names a family child's transcript");
                return (200, (codec.render)(&event, &deny(NAMED_TRANSCRIPT.to_string())));
            }
            Ok(None) => {}
            Err(error) => return (409, (codec.render)(&event, &refuse(error.to_string()))),
        }
    }
    let decision = handle(runtime, event.clone()).await;
    let status = match decision {
        HookDecision::Refuse { .. } => 409,
        _ => 200,
    };
    (status, (codec.render)(&event, &decision))
}

/// A subagent's words reach its parent through the checked return only; a call that
/// names the file the harness keeps them in is refused before it runs.
const NAMED_TRANSCRIPT: &str = "this call names a subagent's transcript or output file; a subagent's words \
                                reach this session only through its checked return";

/// Dispatch one typed event to its session and fold the outcome into
/// one decision. The dispatcher holds nothing between calls; every id
/// it needs is in the event or in the runtime's persistence.
pub async fn handle(runtime: &Runtime, event: HookEvent) -> HookDecision {
    match event {
        HookEvent::SessionStart { root } => match open_or_reopen(runtime, &root) {
            Ok(_) => match runtime.live(&root, &root) {
                Ok(()) => HookDecision::Ack,
                Err(error) => refuse(error.to_string()),
            },
            Err(error) => refuse(error.to_string()),
        },
        HookEvent::Prompt { actor, .. } => {
            // The prompt text is not an engine event: nothing is reported,
            // nothing is recorded, and offer freshness stays the engine's
            // judgment. The prompt is only noted as the sign that the
            // previous turn is over; a queued message arrives here while
            // its turn's call still runs, so the call is settled at the
            // first proposal of the new turn, when that result is in.
            runtime.note_prompt(&actor);
            HookDecision::Ack
        }
        HookEvent::TurnEnd { actor } => {
            // A turn end gates nothing, so it answers `Ack` whatever
            // happens. The refusal families both mean "do not end the
            // turn" on this hook, which would hold the harness in a turn
            // it has finished; a close that failed leaves the call open
            // and the next proposal refuses on its own.
            runtime.take_prompted(&actor);
            if let Err(error) = on_actor(runtime, &actor, MissingStart::Refuse, |session| async move {
                session.on_turn_end().await
            })
            .await
            {
                tracing::warn!(root = %actor.root.0, %error, "the turn end closed no abandoned call");
            }
            runtime.release_vouches(&actor);
            HookDecision::Ack
        }
        HookEvent::ToolCall {
            actor,
            call,
            spawn,
            ruling,
        } => {
            if runtime.take_prompted(&actor) {
                // The first proposal after a prompt that no turn end preceded:
                // the user interrupted the previous turn, and whatever it left
                // open is settled before this turn's first call, control tools
                // included, so a vouch this turn records is never released here.
                if let Err(error) = on_actor(runtime, &actor, MissingStart::OpenLate, |session| async move {
                    session.on_turn_end().await
                })
                .await
                {
                    runtime.note_prompt(&actor);
                    return fold(error, deny);
                }
                runtime.release_vouches(&actor);
            }
            if is_control_tool(&call.tool) {
                return control_call(runtime, &actor, &call, ruling);
            }
            match on_actor(runtime, &actor, MissingStart::OpenLate, |session| {
                let call = call.clone();
                async move { session.on_tool_call(call, spawn).await }
            })
            .await
            {
                Ok(ToolCallDecision::Allow { spawn }) => HookDecision::AllowCall { spawn },
                Ok(ToolCallDecision::Deny {
                    feedback,
                    offers,
                    review,
                }) => HookDecision::DenyCall {
                    feedback,
                    offers,
                    review,
                },
                Err(error) => fold(error, deny),
            }
        }
        HookEvent::ToolResult { actor, call, outcome } => {
            if is_control_tool(&call.tool) {
                tracing::debug!(trajectory = %actor.root.0, "control tool outcome absorbed");
                return HookDecision::Ack;
            }
            match on_actor(runtime, &actor, MissingStart::Refuse, |session| {
                let (call, outcome) = (call.clone(), outcome.clone());
                async move { session.on_tool_result(call, outcome).await }
            })
            .await
            {
                Ok(decision) => outcome_decision(decision),
                Err(error) => fold(error, block),
            }
        }
        HookEvent::SpawnResult {
            actor,
            call,
            outcome,
            child,
            value,
        } => {
            let said = value.clone();
            match on_actor(runtime, &actor, MissingStart::Refuse, |session| {
                let (call, outcome, child, value) = (call.clone(), outcome.clone(), child.clone(), value.clone());
                async move { session.on_spawn_result(call, outcome, child, value).await }
            })
            .await
            {
                Ok(SpawnResultDecision::Return(decision)) => return_decision(said, decision),
                Ok(SpawnResultDecision::Outcome(decision)) => outcome_decision(decision),
                Err(error) => fold(error, block),
            }
        }
        HookEvent::ChildStart { root, child, spawn } => {
            // The child is told what its return must look like where the
            // fork's policy shapes it; a return that crosses as spoken
            // needs no word.
            let root = match open_or_reopen(runtime, &root) {
                Ok(session) => session,
                Err(error) => return refuse(error.to_string()),
            };
            match root.start_child(child, spawn) {
                Ok((_, Some(text))) => HookDecision::Context { text },
                Ok((_, None)) => HookDecision::Ack,
                Err(error) => refuse(error.to_string()),
            }
        }
        HookEvent::ChildEnd { root, child, value } => {
            // The subagent's return is checked here and blocked when it
            // may not cross; a block keeps the subagent running until it
            // returns what may. A child the family never saw start is
            // blocked too: its return has no fork to cross on.
            let said = value.clone();
            match on_child(runtime, &root, &child, MissingStart::Refuse, |session| {
                let value = value.clone();
                async move { session.on_child_end(value).await }
            })
            .await
            {
                Ok(decision) => return_decision(said, decision),
                Err(error) => fold(error, block),
            }
        }
    }
}

fn outcome_decision(decision: ToolResultDecision) -> HookDecision {
    match decision {
        ToolResultDecision::Keep => HookDecision::Ack,
        ToolResultDecision::Replace { placeholder } => HookDecision::ReplaceOutput { output: placeholder },
    }
}

/// A crossing as spoken answers with no opinion. What crosses otherwise — the canonical
/// form of a shaped return, a sanitizer's derivation — goes back as the value the child
/// must return, or the harness deliver, for it to reach the parent.
fn return_decision(said: Option<String>, decision: ChildReturnDecision) -> HookDecision {
    match decision {
        ChildReturnDecision::Returned { value } if said.as_deref() != Some(value.as_str()) => {
            HookDecision::ChildReturn { value }
        }
        ChildReturnDecision::Staged { value } => HookDecision::ChildReturn { value },
        ChildReturnDecision::Returned { .. } | ChildReturnDecision::NoValue => HookDecision::Ack,
        ChildReturnDecision::Blocked { feedback } => block(feedback),
    }
}

fn open_or_reopen(runtime: &Runtime, root: &appa_runtime_api::TrajectoryId) -> Result<Session, EventError> {
    match runtime.session(root, root) {
        Ok(session) => Ok(session),
        Err(EventError::UnknownTrajectory) => match runtime.create_session(root.clone()) {
            Ok(session) => Ok(session),
            Err(EventError::TrajectoryExists) => runtime.session(root, root),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

fn control_call(runtime: &Runtime, actor: &Actor, call: &ProposedCall, ruling: Option<Ruling>) -> HookDecision {
    let Some(quoted) = quoted_offer(call) else {
        tracing::debug!(trajectory = %actor.root.0, "control tool quotes no offer id");
        return HookDecision::PassControl;
    };
    let acting = actor.child.clone().unwrap_or_else(|| actor.root.clone());
    match runtime.resolve_in(&actor.root, &quoted) {
        Some((_, pursuer)) if pursuer == acting => {
            runtime.vouch(&quoted, actor, ruling);
            tracing::debug!(trajectory = %acting.0, "control tool names an offer this trajectory pursues");
            HookDecision::PassControl
        }
        _ => {
            tracing::debug!(trajectory = %acting.0, "control tool refused: no such offer here");
            deny("this offer no longer stands; re-propose the call".to_string())
        }
    }
}

fn quoted_offer(call: &ProposedCall) -> Option<OfferId> {
    let arguments: serde_json::Value = serde_json::from_str(call.arguments.get()).ok()?;
    Some(OfferId(arguments.get("offer_id")?.as_str()?.to_string()))
}

/// What a child's event does when the family has not opened that child.
/// A subagent's first tool call can overtake its start hook, so the
/// call opens the child against the one spawn in flight. Every other
/// event of an unopened child is refused: a return or a turn end claims
/// nothing a missing start could supply, and opening a child on its end
/// would let a stop the family never saw start cross a value.
#[derive(Clone, Copy)]
enum MissingStart {
    OpenLate,
    Refuse,
}

async fn on_actor<T, Run>(
    runtime: &Runtime,
    actor: &Actor,
    missing_start: MissingStart,
    event: impl Fn(Session) -> Run,
) -> Result<T, EventError>
where
    Run: Future<Output = Result<T, EventError>>,
{
    match &actor.child {
        Some(child) => on_child(runtime, &actor.root, child, missing_start, event).await,
        None => event(open_or_reopen(runtime, &actor.root)?).await,
    }
}

async fn on_child<T, Run>(
    runtime: &Runtime,
    root: &TrajectoryId,
    child: &TrajectoryId,
    missing_start: MissingStart,
    event: impl Fn(Session) -> Run,
) -> Result<T, EventError>
where
    Run: Future<Output = Result<T, EventError>>,
{
    let root_session = open_or_reopen(runtime, root)?;
    match (event(runtime.session(root, child)?).await, missing_start) {
        (Err(EventError::SpawnNotTaken), MissingStart::OpenLate) => {
            // `AlreadyOpen` means the start hook landed between the two
            // attempts; the event now finds its child, so it is not
            // refused for a race the harness has already resolved.
            match root_session.open_late(child.clone())? {
                LateOpen::Opened => {
                    tracing::debug!(root = %root.0, child = %child.0, "a child event arrived before its start: opened the child late");
                }
                LateOpen::AlreadyOpen => {
                    tracing::debug!(root = %root.0, child = %child.0, "the child's start landed while its event was in flight");
                }
            }
            event(runtime.session(root, child)?).await
        }
        (outcome, _) => outcome,
    }
}

fn fold(error: EventError, family: fn(String) -> HookDecision) -> HookDecision {
    if error.is_operational() {
        refuse(error.to_string())
    } else {
        family(error.to_string())
    }
}

/// A refusal of this dispatcher's own that the model will read names APPA
/// once, here, so a session can tell a runtime refusal from its harness's;
/// a block is marked by the adapter's withheld-result rendering instead.
fn deny(feedback: String) -> HookDecision {
    HookDecision::DenyCall {
        feedback: format!("[appa] {feedback}"),
        offers: Vec::new(),
        review: Vec::new(),
    }
}

fn block(reason: String) -> HookDecision {
    HookDecision::Block { reason }
}

fn refuse(detail: String) -> HookDecision {
    HookDecision::Refuse { detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::SpawnRef;

    use crate::api::Runtime;
    use crate::config::Config;

    fn codec() -> Codec {
        appa_adapter_claude_code::codec()
    }

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "Bash"

            [[policy.tool]]
            name = "Write"

            [[policy.tool]]
            name = "AskUserQuestion"

            [[policy.tool]]
            name = "Task"

            [[policy.tool]]
            name = "Agent"

            [[policy.tool]]
            name = "Read"

            [policy.deployment]
            context_control = true

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    fn open_runtime(dir: &tempfile::TempDir) -> Runtime {
        Runtime::open(config(), dir.path().join("appa.db"), None).expect("the fixture deployment opens")
    }

    async fn call_hook(runtime: &Runtime, body: &[u8]) -> (u16, serde_json::Value) {
        answer(runtime, &codec(), body).await
    }

    fn spawn_call() -> crate::api::ProposedCall {
        crate::api::ProposedCall {
            tool: "Task".to_string(),
            arguments: crate::api::raw(serde_json::json!({"prompt": "look it up"})),
        }
    }

    /// Declare the one return route the fixture offers — as spoken, floored at the
    /// parent's current label — for the offer the blocked spawn surfaced.
    async fn declare_return(runtime: &Runtime, root: &TrajectoryId, offers: &[appa_runtime_api::OfferedRemedy]) {
        let offer = offers
            .iter()
            .find(|offer| offer.returns == Some(appa_runtime_api::OfferedReturn::AsSpoken))
            .expect("the menu offers the bare floor");
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let arguments = crate::engine::RemedyArguments {
            label: Some(crate::engine::LabelSpelling::default()),
            return_schema: None,
        };
        let outcome = runtime
            .execute_remedy_with(&actor, OfferId(offer.id.clone()), arguments)
            .await;
        assert!(
            matches!(outcome, crate::api::RemedyOutcome::Authorized { .. }),
            "the declaration approves the spawn, got {outcome:?}"
        );
    }

    /// The marked spawn's round trip: blocked on the return menu, declared, released.
    async fn declared_spawn(runtime: &Runtime, root: &TrajectoryId) -> appa_runtime_api::SpawnBinding {
        let spawn = || HookEvent::ToolCall {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: spawn_call(),
            spawn: true,
            ruling: None,
        };
        let HookDecision::DenyCall { offers, .. } = handle(runtime, spawn()).await else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(runtime, root, &offers).await;
        let released = handle(runtime, spawn()).await;
        let HookDecision::AllowCall { spawn: Some(binding) } = released else {
            panic!("the declared spawn must release with its binding, got {released:?}");
        };
        binding
    }

    const CONTROL_TOOL_FIXTURE_NAME: &str = "mcp__plugin_appa-runtime_appa__execute_remedy_plan";

    fn fixtures() -> Vec<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks.jsonl");
        std::fs::read_to_string(path)
            .expect("the recorded hook fixtures are readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn the_recorded_session_replays_end_to_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for event in fixtures() {
            let name = event["hook_event_name"].as_str().expect("each fixture names its hook");
            let control = event["tool_name"] == CONTROL_TOOL_FIXTURE_NAME;
            let spawn = name == "PreToolUse" && event["tool_name"] == "Agent" && event.get("agent_id").is_none();

            let body = serde_json::to_vec(&event).expect("the fixture re-serializes");
            if spawn {
                // The parent's spawn blocks on the return menu; the model declares and
                // proposes the spawn again.
                let (status, answer) = call_hook(&runtime, &body).await;
                assert_eq!(status, 200, "hook {name} refused: {answer}");
                assert_eq!(
                    answer["hookSpecificOutput"]["permissionDecision"], "deny",
                    "a marked spawn blocks until its return is declared"
                );
                let root = TrajectoryId(format!("cc:{}", event["session_id"].as_str().expect("a session id")));
                let quoted = runtime
                    .minted_offers(&root, &root)
                    .into_iter()
                    .next()
                    .expect("the block surfaced the return declaration");
                let offers = vec![appa_runtime_api::OfferedRemedy {
                    id: quoted.0,
                    returns: Some(appa_runtime_api::OfferedReturn::AsSpoken),
                }];
                declare_return(&runtime, &root, &offers).await;
            }
            let (status, answer) = call_hook(&runtime, &body).await;

            assert_eq!(status, 200, "hook {name} refused: {answer}");
            match name {
                "PreToolUse" => {
                    let (decision, reason) = if control {
                        ("deny", "[appa] this offer no longer stands; re-propose the call")
                    } else {
                        ("allow", "appa: the call is released")
                    };
                    assert_eq!(
                        answer,
                        serde_json::json!({
                            "hookSpecificOutput": {
                                "hookEventName": "PreToolUse",
                                "permissionDecision": decision,
                                "permissionDecisionReason": reason,
                            }
                        }),
                        "the call must render as exactly its allow answer",
                    );
                }
                other => assert_eq!(
                    answer,
                    serde_json::json!({}),
                    "the {other} hook answers with no opinion",
                ),
            }
        }
    }

    /// On this hook every refusal family means "do not end the turn",
    /// so a turn end answers with no opinion whatever the close did —
    /// including for a session the runtime never opened.
    #[tokio::test]
    async fn a_turn_end_answers_with_no_opinion_even_over_an_unknown_session() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for hook in ["Stop", "StopFailure", "SubagentStop"] {
            let mut body = serde_json::json!({
                "hook_event_name": hook,
                "session_id": "never-opened",
                "last_assistant_message": "the summary",
            });
            if hook == "SubagentStop" {
                body["agent_id"] = serde_json::Value::String("a1".to_string());
            }
            let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&body).expect("re-serializes")).await;
            assert_eq!(
                (status, answer),
                (200, serde_json::json!({})),
                "the {hook} hook blocked"
            );
        }
    }

    /// The wedge this hook exists to clear: a released call the harness
    /// never ran refuses every later proposal until the turn ends.
    #[tokio::test]
    async fn a_turn_end_frees_a_trajectory_the_missing_outcome_wedged() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let propose = |command: &str| {
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": command},
            })
        };
        let released = call_hook(&runtime, &serde_json::to_vec(&propose("ls")).expect("re-serializes")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        // The harness refused it at its permission prompt: no outcome hook fires.
        let wedged = call_hook(
            &runtime,
            &serde_json::to_vec(&propose("echo hi")).expect("re-serializes"),
        )
        .await;
        assert_eq!(wedged.1["hookSpecificOutput"]["permissionDecision"], "deny");

        let stop = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"});
        assert_eq!(
            call_hook(&runtime, &serde_json::to_vec(&stop).expect("re-serializes")).await,
            (200, serde_json::json!({})),
        );

        let freed = call_hook(
            &runtime,
            &serde_json::to_vec(&propose("echo hi")).expect("re-serializes"),
        )
        .await;
        assert_eq!(freed.1["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    fn bash_call(command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": command},
        })
    }

    fn bash_result(command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": "done"},
        })
    }

    fn prompt() -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s1",
            "prompt": "never mind, list the files",
        })
    }

    async fn hook(runtime: &Runtime, body: &serde_json::Value) -> (u16, serde_json::Value) {
        call_hook(runtime, &serde_json::to_vec(body).expect("re-serializes")).await
    }

    fn closed_as_unknown(runtime: &Runtime) -> bool {
        runtime
            .audit(&appa_runtime_api::TrajectoryId("cc:s1".to_string()))
            .expect("the audit reads")
            .iter()
            .any(|entry| {
                matches!(
                    &entry.event,
                    crate::engine::AuditEvent::Closed {
                        outcome: crate::engine::DispatchOutcome::Unknown
                    }
                )
            })
    }

    /// The user pressed Esc while the command ran: Claude Code sends no
    /// outcome hook for the call and no `Stop` hook for the turn. The
    /// next prompt is the first hook to arrive; the new turn's first
    /// proposal closes the call as unreported and releases.
    #[tokio::test]
    async fn the_first_call_after_a_prompt_frees_a_call_an_interrupt_left_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &bash_call("ping -c 30 127.0.0.1")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        let actor = Actor {
            root: appa_runtime_api::TrajectoryId("cc:s1".to_string()),
            child: None,
        };
        let quoted = OfferId("offer-1".to_string());
        runtime.vouch(&quoted, &actor, None);

        // Interrupted: neither PostToolUse nor Stop arrives.
        assert_eq!(hook(&runtime, &prompt()).await, (200, serde_json::json!({})));
        assert!(!closed_as_unknown(&runtime), "the prompt itself records nothing");

        let freed = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(freed.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(closed_as_unknown(&runtime), "the interrupted call closed as unreported");
        assert_eq!(
            runtime.take_vouched(&quoted),
            None,
            "the interrupted turn's vouch is released"
        );

        let late = hook(&runtime, &bash_result("ping -c 30 127.0.0.1")).await;
        assert_eq!(late.1["decision"], "block", "a result for the closed call is refused");
    }

    /// The user typed while the command ran: Claude Code queues the message
    /// and fires the prompt hook at once, then reports the command's
    /// outcome when it finishes. The result must land on the still-open
    /// call, and the next turn's first proposal then releases freely.
    #[tokio::test]
    async fn a_prompt_queued_behind_a_running_call_keeps_its_result() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &bash_call("sleep 30")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        assert_eq!(hook(&runtime, &prompt()).await, (200, serde_json::json!({})));
        assert_eq!(
            hook(&runtime, &bash_result("sleep 30")).await,
            (200, serde_json::json!({})),
            "the running call's result lands on its open dispatch",
        );
        assert!(!closed_as_unknown(&runtime), "nothing was closed as unreported");

        let next = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(next.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(!closed_as_unknown(&runtime));
        let repeated = hook(&runtime, &bash_result("sleep 30")).await;
        assert_eq!(
            repeated.1["decision"], "block",
            "a second report of the same result is refused"
        );
    }

    /// A prompt writes nothing, so a store that refuses every append still
    /// acknowledges it; the failure surfaces at the proposal that needs the
    /// close, and the mark survives for the next proposal.
    #[tokio::test]
    async fn a_prompt_acknowledges_over_a_store_that_cannot_append() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &bash_call("ping -c 30 127.0.0.1")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        runtime.store().fail_commit_after(0);

        assert_eq!(hook(&runtime, &prompt()).await, (200, serde_json::json!({})));

        let (status, _) = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(status, 409, "the close the proposal needs is an operational refusal");
        runtime.store().fail_commit_after(u64::MAX - 1);
        let freed = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(freed.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(closed_as_unknown(&runtime));
    }

    /// A turn end settles the prompt's mark too: a proposal in the turn
    /// after a normally ended one closes nothing.
    #[tokio::test]
    async fn a_turn_end_settles_the_prompt_mark() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        assert_eq!(hook(&runtime, &prompt()).await, (200, serde_json::json!({})));
        let stop = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"});
        assert_eq!(hook(&runtime, &stop).await, (200, serde_json::json!({})));

        let released = hook(&runtime, &bash_call("sleep 30")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            hook(&runtime, &bash_result("sleep 30")).await,
            (200, serde_json::json!({}))
        );
        assert!(!closed_as_unknown(&runtime));
    }

    /// Every refusal of the dispatcher's own that reaches the model on the
    /// deny wire names APPA, whichever event error produced it.
    #[tokio::test]
    async fn a_deny_of_the_dispatchers_own_names_appa() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &bash_call("sleep 30")).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        let denied = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(denied.1["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = denied.1["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("a deny carries its reason");
        assert!(reason.starts_with("[appa] "), "the deny reads: {reason}");
    }

    #[tokio::test]
    async fn a_deny_renders_in_the_pre_tool_use_wire() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        // A declared tool with non-object arguments is a malformed call: the engine
        // answers it as model feedback, and the feedback rides the deny wire.
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": ["not", "an", "object"],
            "tool_use_id": "toolu_1",
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        let rendered = answer["hookSpecificOutput"]
            .as_object()
            .expect("a deny renders inside hookSpecificOutput");
        assert_eq!(
            rendered.keys().collect::<Vec<_>>(),
            ["hookEventName", "permissionDecision", "permissionDecisionReason"],
            "the deny wire carries exactly these three fields: {answer}",
        );
        assert_eq!(rendered["hookEventName"], "PreToolUse");
        assert_eq!(rendered["permissionDecision"], "deny");
        assert!(
            !rendered["permissionDecisionReason"]
                .as_str()
                .expect("the reason is a string")
                .is_empty(),
            "the deny carries the engine's feedback to the model",
        );
    }

    #[tokio::test]
    async fn an_event_error_renders_as_a_deny() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let body = serde_json::to_vec(&event).expect("serializes");
        assert_eq!(call_hook(&runtime, &body).await.0, 200);

        let second = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/x", "content": "y"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&second).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecision"], "deny",
            "lifecycle misuse renders as a deny",
        );
    }

    #[tokio::test]
    async fn a_replaced_output_renders_as_a_block_with_the_placeholder() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let pre = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "cat secret.txt"},
        });
        call_hook(&runtime, &serde_json::to_vec(&pre).expect("serializes")).await;

        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "cat secret.txt"},
            "tool_response": {"content": "the secret"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&post).expect("serializes")).await;
        assert_eq!(status, 200, "the outcome refused: {answer}");
        assert_eq!(answer, serde_json::json!({}), "a plain success answers with no opinion");
    }

    #[tokio::test]
    async fn a_mid_run_input_rewrite_still_matches_its_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let questions = serde_json::json!({"questions": [{"question": "Declare the tool?"}]});
        let pre = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "AskUserQuestion",
            "tool_input": questions,
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&pre).expect("serializes")).await;
        assert_eq!(status, 200, "the release refused: {answer}");

        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{"question": "Declare the tool?"}],
                "answers": {"Declare the tool?": "No, skip it"},
            },
            "tool_response": {"answers": {"Declare the tool?": "No, skip it"}},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&post).expect("serializes")).await;
        assert_eq!(status, 200, "the rewritten report refused: {answer}");
        assert_eq!(answer, serde_json::json!({}), "the rewritten report lands");

        let next = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let (_, answer) = call_hook(&runtime, &serde_json::to_vec(&next).expect("serializes")).await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[tokio::test]
    async fn the_control_tool_opens_no_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "mcp__appa__execute_remedy_plan",
            "tool_input": {},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecisionReason"],
            "appa: the runtime's own control tool",
        );
        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let (_, answer) = call_hook(&runtime, &serde_json::to_vec(&call).expect("serializes")).await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[tokio::test]
    async fn a_lookalike_control_tool_is_checked() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "mcp__evil__execute_remedy_plan",
            "tool_input": {"offer_id": "whatever"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        // Not the exemption's allow: the lookalike reaches the engine, and nothing
        // covers the name, so the refusal is typed and rides the error wire.
        assert_eq!(status, 409, "a colliding name must reach the engine, not the exemption");
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|detail| detail.contains("mcp__evil__execute_remedy_plan")),
            "the refusal names the tool: {answer}"
        );
    }

    #[tokio::test]
    async fn a_control_tools_post_hook_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "mcp__plugin_appa-runtime_appa__execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
            "tool_response": {"content": "Authorized."},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer, serde_json::json!({}));
    }

    #[tokio::test]
    async fn a_control_call_answers_without_a_session_lookup() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);

        let start = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s1",
            "agent_id": "a1",
        });
        assert_eq!(
            call_hook(&runtime, &serde_json::to_vec(&start).expect("serializes"))
                .await
                .0,
            409,
            "an uncorrelated subagent start refuses",
        );

        let control = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": "execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&control).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");

        let outcome = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": "execute_remedy_plan",
            "tool_input": {"offer_id": "offer-1"},
            "tool_response": {"ok": true},
        });
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&outcome).expect("serializes")).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer,
            serde_json::json!({}),
            "the control outcome is absorbed without a session lookup"
        );
    }

    #[tokio::test]
    async fn a_child_return_that_crosses_unchanged_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());

        let binding = declared_spawn(&runtime, &root).await;
        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildStart {
                    root: root.clone(),
                    child: child.clone(),
                    spawn: SpawnRef::Binding(binding),
                },
            )
            .await,
            HookDecision::Ack,
        );

        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildEnd {
                    root,
                    child,
                    value: Some("the report, nothing sensitive".to_string()),
                },
            )
            .await,
            HookDecision::Ack,
            "an unchanged crossing needs no answer",
        );
    }

    #[tokio::test]
    async fn an_uncorrelated_child_return_blocks_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());

        let decision = handle(
            &runtime,
            HookEvent::ChildEnd {
                root: root.clone(),
                child: appa_runtime_api::TrajectoryId("cc:s1:a1".to_string()),
                value: Some("late".to_string()),
            },
        )
        .await;
        assert!(
            matches!(decision, HookDecision::Block { .. }),
            "an uncorrelated child blocks; only an operational failure refuses. Got {decision:?}",
        );
    }

    #[tokio::test]
    async fn a_spawn_result_on_an_unforked_call_answers_as_an_ordinary_outcome() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let call = || crate::api::ProposedCall {
            tool: "Agent".to_string(),
            arguments: crate::api::raw(serde_json::json!({"prompt": "list files"})),
        };
        let result = || HookEvent::SpawnResult {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: call(),
            outcome: appa_runtime_api::ToolOutcome::Success {
                body: appa_runtime_api::OutcomeBody::Available(r#"{"agentId":"a1"}"#.to_string()),
            },
            child: Some(appa_runtime_api::TrajectoryId("cc:s1:a1".to_string())),
            value: Some("one file".to_string()),
        };

        let decision = handle(&runtime, result()).await;
        assert!(matches!(decision, HookDecision::Block { .. }), "got {decision:?}");

        let released = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: None,
                },
                call: call(),
                spawn: false,
                ruling: None,
            },
        )
        .await;
        assert_eq!(released, HookDecision::AllowCall { spawn: None });
        assert_eq!(handle(&runtime, result()).await, HookDecision::Ack);
    }

    #[tokio::test]
    async fn a_child_event_before_its_start_opens_the_child_to_the_one_spawn_in_flight() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());
        declared_spawn(&runtime, &root).await;

        let decision = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: Some(child.clone()),
                },
                call: crate::api::ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                },
                spawn: false,
                ruling: None,
            },
        )
        .await;
        assert_eq!(decision, HookDecision::AllowCall { spawn: None });
        let forked = runtime
            .audit(&root)
            .expect("the audit reads")
            .into_iter()
            .filter(|entry| matches!(entry.event, crate::api::AuditEvent::Forked { .. }))
            .count();
        assert_eq!(forked, 1, "the late open bound the one spawn in flight");

        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildStart {
                    root: root.clone(),
                    child: child.clone(),
                    spawn: SpawnRef::InFlight,
                },
            )
            .await,
            HookDecision::Ack,
        );

        let denied = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: Some(appa_runtime_api::TrajectoryId("cc:s1:a2".to_string())),
                },
                call: crate::api::ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                },
                spawn: false,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(denied, HookDecision::DenyCall { .. }), "got {denied:?}");
    }

    #[tokio::test]
    async fn an_operational_failure_refuses_instead_of_answering_the_model() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        runtime.store().fail_commit_after(0);
        let (status, answer) = hook(&runtime, &bash_call("ls")).await;
        assert_eq!(status, 409, "the harness must fail closed on a storage failure");
        assert!(
            answer.get("error").is_some(),
            "an operational failure renders as a refusal, never as model-facing feedback: {answer}",
        );
    }

    #[tokio::test]
    async fn an_unreadable_hook_event_is_a_400() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let (status, answer) = call_hook(&runtime, b"not json").await;
        assert_eq!(status, 400);
        assert!(
            answer["error"]
                .as_str()
                .expect("the refusal names its cause")
                .starts_with("unreadable hook event: "),
        );
    }

    #[tokio::test]
    async fn a_malformed_hook_event_is_a_409() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1"});
        let (status, answer) = call_hook(&runtime, &serde_json::to_vec(&event).expect("serializes")).await;
        assert_eq!(status, 409);
        assert_eq!(answer, serde_json::json!({"error": "PreToolUse without a tool call"}));
    }
}
