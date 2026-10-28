//! Interceptors and the components bound to unblock a tool: a prompt an Interceptor refuses
//! is blocked; a call it refuses is unblocked by the one component bound to the tool, applied
//! by the runtime itself, or left to the model when two are bound.

mod common;
use common::{actor, offers, propose, raw, root, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{OfferId, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome};

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// A loopback external answering every consult with `answer(request)`, keeping each request.
async fn external(answer: fn(&serde_json::Value) -> serde_json::Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let kept = Arc::clone(&seen);
    let router = axum::Router::new().route(
        "/",
        axum::routing::post(move |body: String| {
            let kept = Arc::clone(&kept);
            async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the consult is JSON");
                let reply = serde_json::json!({ "version": 1, "answer": answer(&request) });
                kept.lock().expect("the stub's log is not poisoned").push(request);
                reply.to_string()
            }
        }),
    );
    (format!("{}/", serve(router).await), seen)
}

/// Replace every "Orlando" in the arguments the sanitizer is given.
fn strip_orlando(request: &serde_json::Value) -> serde_json::Value {
    let body = request["artifact"]["body"]
        .as_str()
        .expect("a sanitizer rewrites a body");
    serde_json::json!({ "body": body.replace("Orlando", "the client") })
}

fn approve(_: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ruling": "approve" })
}

fn hateful(request: &serde_json::Value) -> serde_json::Value {
    let message = request["artifact"]["arguments"]["message"].as_str().unwrap_or_default();
    match message.contains("hate") {
        true => serde_json::json!({ "verdict": "no", "reason": "the message is hostile" }),
        false => serde_json::json!({ "verdict": "yes" }),
    }
}

async fn runtime_over(dir: &tempfile::TempDir, policy: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
            }
        )
        .await,
        HookDecision::Ack
    );
    runtime
}

fn call(tool: &str, arguments: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(arguments),
        cwd: None,
    }
}

async fn prompt(runtime: &Arc<Runtime>, text: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::Prompt {
            actor: actor(),
            text: text.to_string(),
            settles: None,
            peer: None,
            title: None,
        },
    )
    .await
}

async fn ran(runtime: &Arc<Runtime>, call: ProposedCall) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call,
            call_id: None,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available("done".to_string()),
            },
        },
    )
    .await
}

fn feedback(decision: &HookDecision) -> &str {
    match decision {
        HookDecision::DenyCall { feedback, .. } => feedback,
        other => panic!("expected a deny, got {other:?}"),
    }
}

const EXTERNALS: &str = "[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n";

#[tokio::test]
async fn a_prompt_with_a_card_number_never_reaches_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let policy = format!(
        r#"
[policy]
version = 2

[[policy.interceptor]]
name = "no-cards"
builtin = "regex"
settings = {{ pattern = '\d{{4}}( ?\d{{4}}){{3}}' }}
on = ["prompt"]

{EXTERNALS}"#
    );
    let runtime = runtime_over(&dir, &policy).await;
    match prompt(&runtime, "pay with 4111 1111 1111 1111").await {
        HookDecision::Block { reason } => assert!(reason.contains("no-cards"), "{reason}"),
        other => panic!("the prompt is blocked, got {other:?}"),
    }
    assert_eq!(prompt(&runtime, "pay later").await, HookDecision::Ack);
}

fn commit_policy(sanitizer: &str, extra: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.tool]]
name = "commit"

[[policy.interceptor]]
name = "orlando"
builtin = "regex"
settings = {{ pattern = "(?i)orlando" }}
on = ["tool_call"]
tools = ["commit"]

[[policy.sanitizer]]
name = "strip"
on = ["tool_input"]
hint = "Replace the name Orlando with a neutral word."
unblocks = ["commit"]

{extra}
{EXTERNALS}
[externals.sanitizers.strip]
url = "{sanitizer}"
"#
    )
}

#[tokio::test]
async fn the_one_bound_sanitizer_rewrites_the_call_and_it_runs_as_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let (sanitizer, asked) = external(strip_orlando).await;
    let runtime = runtime_over(&dir, &commit_policy(&sanitizer, "")).await;

    let decision = propose(
        &runtime,
        call("commit", serde_json::json!({ "message": "fix for Orlando" })),
    )
    .await;
    let HookDecision::AllowRewrittenCall { arguments } = decision else {
        panic!("the runtime applies the one bound sanitizer, got {decision:?}");
    };
    let rewritten: serde_json::Value = serde_json::from_str(&arguments).unwrap();
    assert_eq!(rewritten, serde_json::json!({ "message": "fix for the client" }));

    // The sanitizer was told why the call is blocked.
    let request = asked.lock().unwrap()[0].clone();
    let interceptors = &request["artifact"]["unblock"]["interceptors"];
    assert_eq!(interceptors[0]["kind"], "interceptor");
    assert_eq!(interceptors[0]["name"], "orlando");
    assert_eq!(interceptors[0]["verdict"], "no");

    // The harness runs the rewritten arguments, and that is the call APPA released.
    assert_eq!(ran(&runtime, call("commit", rewritten)).await, HookDecision::Ack);
    // A clean commit needs nobody.
    assert_eq!(
        propose(&runtime, call("commit", serde_json::json!({ "message": "fix" }))).await,
        HookDecision::AllowCall { spawn: None }
    );
}

#[tokio::test]
async fn two_bound_unblockers_leave_the_choice_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let (sanitizer, _) = external(strip_orlando).await;
    let (authority, _) = external(approve).await;
    let policy = commit_policy(
        &sanitizer,
        "[[policy.authority]]\nname = \"lead\"\nunblocks = [\"commit\"]\n",
    ) + &format!("\n[externals.authorities.lead]\nurl = \"{authority}\"\n");
    let runtime = runtime_over(&dir, &policy).await;
    let decision = propose(
        &runtime,
        call("commit", serde_json::json!({ "message": "fix for Orlando" })),
    )
    .await;
    let text = feedback(&decision);
    assert!(text.contains("interceptor orlando refuses the call"), "{text}");
    assert_eq!(offers(text).len(), 2, "{text}");
}

#[tokio::test]
async fn the_one_bound_authority_approves_and_the_call_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (moderation, _) = external(hateful).await;
    let (moderator, asked) = external(approve).await;
    let policy = format!(
        r#"
[policy]
version = 2

[[policy.tool]]
name = "send_my_message_to_cat_game"

[[policy.interceptor]]
name = "hate"
on = ["tool_call"]
tools = ["send_my_message_to_cat_game"]

[[policy.authority]]
name = "moderator"
unblocks = ["send_my_message_to_cat_game"]

{EXTERNALS}
[externals.interceptors.hate]
url = "{moderation}"

[externals.authorities.moderator]
url = "{moderator}"
"#
    );
    let runtime = runtime_over(&dir, &policy).await;
    let friendly = call("send_my_message_to_cat_game", serde_json::json!({ "message": "meow" }));
    assert_eq!(
        propose(&runtime, friendly.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(ran(&runtime, friendly).await, HookDecision::Ack);
    let hostile = call(
        "send_my_message_to_cat_game",
        serde_json::json!({ "message": "I hate cats" }),
    );
    assert_eq!(
        propose(&runtime, hostile).await,
        HookDecision::AllowCall { spawn: None }
    );
    let request = asked.lock().unwrap()[0].clone();
    assert_eq!(request["artifact"]["requirements"][0]["name"], "hate");
    assert_eq!(
        request["artifact"]["requirements"][0]["reason"],
        "the message is hostile"
    );
    assert!(request["artifact"]["ifc"]["label"].is_object(), "{request}");
}

#[tokio::test]
async fn a_bound_hitl_stays_an_offer_the_model_takes() {
    let dir = tempfile::tempdir().unwrap();
    let (moderation, _) = external(hateful).await;
    let policy = format!(
        r#"
[policy]
version = 2

[[policy.tool]]
name = "post"

[[policy.interceptor]]
name = "hate"
on = ["tool_call"]

[[policy.authority]]
name = "hitl"
unblocks = ["*"]

{EXTERNALS}
[externals.interceptors.hate]
url = "{moderation}"

[externals.authorities.hitl]
builtin = "hitl"
"#
    );
    let runtime = runtime_over(&dir, &policy).await;
    let decision = propose(&runtime, call("post", serde_json::json!({ "message": "I hate cats" }))).await;
    assert_eq!(offers(feedback(&decision)).len(), 1);
}

#[tokio::test]
async fn an_interceptor_that_does_not_answer_refuses_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let unreachable = common::RefusingPort::new();
    let policy = format!(
        r#"
[policy]
version = 2

[[policy.tool]]
name = "post"

[[policy.interceptor]]
name = "fraud"
on = ["tool_call"]

[[policy.authority]]
name = "lead"
unblocks = ["post"]

{EXTERNALS}
[externals.interceptors.fraud]
url = "{}/"

[externals.authorities.lead]
builtin = "approve"
"#,
        unreachable.url()
    );
    let runtime = runtime_over(&dir, &policy).await;
    let decision = propose(&runtime, call("post", serde_json::json!({ "message": "hi" }))).await;
    assert!(matches!(decision, HookDecision::Refuse { .. }), "{decision:?}");
}

#[tokio::test]
async fn the_sanitizer_reads_which_tool_brought_the_internal_data() {
    let dir = tempfile::tempdir().unwrap();
    let (sanitizer, asked) = external(|request| {
        serde_json::json!({ "body": request["artifact"]["body"].as_str().unwrap().replace("Q3 numbers", "[removed]") })
    })
    .await;
    let policy = format!(
        r#"
[policy]
version = 2

[[policy.tool]]
name = "get_ticket"
delta = {{ audience = ["internal"] }}

[[policy.tool]]
name = "send_email"
requires = {{ audience = {{ contains = ["public"] }} }}

[[policy.sanitizer]]
name = "scrub"
on = ["tool_input"]
hint = "Remove internal data before the email leaves."
unblocks = ["send_email"]
permits = {{ audience = {{ from = ["internal"], to = ["public"] }} }}

{EXTERNALS}
[externals.sanitizers.scrub]
url = "{sanitizer}"
"#
    );
    let runtime = runtime_over(&dir, &policy).await;
    let read = call("get_ticket", serde_json::json!({ "id": 7 }));
    let narrowing = propose(&runtime, read.clone()).await;
    let accept = OfferId(offers(feedback(&narrowing))[0].0.clone());
    assert!(matches!(
        runtime.execute_remedy(&actor(), accept).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(ran(&runtime, read).await, HookDecision::Ack);

    let decision = propose(
        &runtime,
        call(
            "send_email",
            serde_json::json!({ "to": "a@consulting.com", "body": "the Q3 numbers" }),
        ),
    )
    .await;
    assert!(
        matches!(decision, HookDecision::AllowRewrittenCall { .. }),
        "the one bound sanitizer is applied: {decision:?}"
    );
    let request = asked.lock().unwrap()[0].clone();
    let sources = &request["artifact"]["unblock"]["ifc"]["sources"];
    assert_eq!(sources[0]["from"], "tool_result");
    assert_eq!(sources[0]["tool"], "get_ticket");
    assert_eq!(sources[0]["label"]["audience"], serde_json::json!([["internal"]]));
}

#[test]
fn claude_code_runs_the_rewritten_input() {
    let decision = HookDecision::AllowRewrittenCall {
        arguments: r#"{"message":"fix for the client"}"#.to_string(),
    };
    let event = common::claude_event(&serde_json::json!({
        "session_id": root().0,
        "hook_event_name": "PreToolUse",
        "tool_name": "commit",
        "tool_input": { "message": "fix for Orlando" },
        "cwd": "/tmp",
        "transcript_path": "/tmp/t.jsonl",
    }))
    .expect("PreToolUse is gated");
    let rendered = (appa_adapter_claude_code::codec().render)(b"{}", &event, &decision);
    assert_eq!(rendered["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(
        rendered["hookSpecificOutput"]["updatedInput"],
        serde_json::json!({ "message": "fix for the client" })
    );
}

fn example(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/interceptors")
        .join(file)
}

#[tokio::test]
async fn the_shipped_example_opens_and_blocks_a_card_number() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::load(&example("appa.toml")).expect("the example validates");
    let runtime = common::loaded_session_runtime(dir.path(), config).await;
    assert!(matches!(
        prompt(&runtime, "my card is 4111 1111 1111 1111").await,
        HookDecision::Block { .. }
    ));
    let commit = call(
        "host/claude-code/Bash",
        serde_json::json!({ "command": "git commit -m 'fix for Orlando'" }),
    );
    // A person and the cleaner are both bound to a commit: the model chooses.
    let decision = propose(&runtime, commit).await;
    assert_eq!(offers(feedback(&decision)).len(), 2, "{decision:?}");
}

#[cfg(feature = "cedar")]
#[tokio::test]
async fn the_cedar_example_refuses_a_force_push() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::load(&example("with-cedar.appa.toml")).expect("the example validates");
    let runtime = common::loaded_session_runtime(dir.path(), config).await;
    let push = call(
        "host/claude-code/Bash",
        serde_json::json!({ "command": "git push origin main --force" }),
    );
    let decision = propose(&runtime, push).await;
    assert!(
        feedback(&decision).contains("force-pushing is not allowed"),
        "{decision:?}"
    );
}

#[cfg(not(feature = "cedar"))]
#[test]
fn the_cedar_example_needs_the_cedar_feature() {
    let config = Config::load(&example("with-cedar.appa.toml")).expect("the example parses");
    let opened = Runtime::open_in_memory(config, None, appa_runtime::api::Remedies::StandIn);
    assert!(opened.is_err_and(|error| error.to_string().contains("--features cedar")));
}

#[tokio::test]
async fn a_rewrite_that_fixes_nothing_leaves_a_fresh_block() {
    let dir = tempfile::tempdir().unwrap();
    // The sanitizer answers the arguments unchanged: the Interceptor still refuses them.
    let (sanitizer, _) = external(|request| serde_json::json!({ "body": request["artifact"]["body"] })).await;
    let runtime = runtime_over(&dir, &commit_policy(&sanitizer, "")).await;
    let decision = propose(
        &runtime,
        call("commit", serde_json::json!({ "message": "fix for Orlando" })),
    )
    .await;
    let text = feedback(&decision);
    assert!(text.contains("interceptor orlando refuses the call"), "{text}");
    // The block the model reads is the fresh one: every offer it names still stands.
    for offer in offers(text) {
        if let RemedyOutcome::Declined { presentation } = runtime.execute_remedy(&actor(), offer).await {
            assert!(
                !presentation.feedback.contains("no longer stands"),
                "a spent offer is shown: {text}"
            );
        }
    }
}

#[tokio::test]
async fn a_rewrite_that_clears_one_refusal_shows_the_one_left() {
    let dir = tempfile::tempdir().unwrap();
    let (sanitizer, _) = external(strip_orlando).await;
    let second = r#"[[policy.interceptor]]
name = "secrets"
builtin = "regex"
settings = { pattern = "password" }
on = ["tool_call"]
tools = ["commit"]
"#;
    let runtime = runtime_over(&dir, &commit_policy(&sanitizer, second)).await;
    let decision = propose(
        &runtime,
        call("commit", serde_json::json!({ "message": "Orlando's password" })),
    )
    .await;
    let text = feedback(&decision);
    assert!(text.contains("interceptor secrets refuses the call"), "{text}");
    assert!(
        !text.contains("interceptor orlando"),
        "the rewrite cleared orlando: {text}"
    );
}

#[tokio::test]
async fn a_restriction_from_before_the_trajectory_is_named_inherited() {
    let dir = tempfile::tempdir().unwrap();
    let (sanitizer, asked) = external(|request| serde_json::json!({ "body": request["artifact"]["body"] })).await;
    let policy = format!(
        r#"
[policy]
version = 2

[policy.deployment]
starting_label = {{ audience = ["internal"] }}

[[policy.tool]]
name = "get_ticket"
delta = {{ audience = ["internal"] }}

[[policy.tool]]
name = "send_email"
requires = {{ audience = {{ contains = ["public"] }} }}

[[policy.sanitizer]]
name = "scrub"
on = ["tool_input"]
unblocks = ["send_email"]
permits = {{ audience = {{ from = ["internal"], to = ["public"] }} }}

{EXTERNALS}
[externals.sanitizers.scrub]
url = "{sanitizer}"
"#
    );
    let runtime = runtime_over(&dir, &policy).await;
    // The read restricts nothing new: the trajectory opened `internal` already.
    let read = call("get_ticket", serde_json::json!({ "id": 7 }));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(ran(&runtime, read).await, HookDecision::Ack);
    propose(&runtime, call("send_email", serde_json::json!({ "body": "hi" }))).await;
    let request = asked.lock().unwrap()[0].clone();
    assert_eq!(
        request["artifact"]["unblock"]["ifc"]["sources"][1]["tool"], "get_ticket",
        "{request}"
    );
    assert_eq!(
        request["artifact"]["unblock"]["ifc"]["sources"][0]["from"], "inherited",
        "{request}"
    );
}
