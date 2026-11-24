use crate::common;
use common::{actor, audit_len, last_offer, propose, ran, raw, root, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::{ProbeError, Remedies, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, Outcome, Outcomes, ProposedCall, RetryHint};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const POLICY: &str = r#"
[policy]
version = 2

[policy.audience.group.team]
from = ["slack:user-group/team"]

[policy.audience.group.nobody]
from = ["slack:user-group/nobody"]

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
effects = ["egress"]
delta = {}

[[policy.tool]]
name = "send_direct"
requires = { audience = { contains = ["@slack:user-group/direct"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
"#;

/// The `[externals.audience]` block of `POLICY`, for the variants that replace it.
const SLACK_BINDING: &str = "[externals.audience.slack]\nurl = \"AUDIENCE_URL\"\nselectors = [{ template = \"viewer\", feeds = \"self\" }, { template = \"full-members\", feeds = \"internal\" }, { template = \"user-group/<handle>\" }]\n";

/// Slack's member lookups answered in process from a roster.
const ROSTER_BINDINGS: &str = r#"
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
lookup = "people"

[externals.audience.people]
readers = { "slack:U-bob" = "bob@corp.example" }
"#;

/// Slack's member lookups answered by a second HTTP entry — the same stub, under another
/// name.
const LOOKUP_URL_BINDINGS: &str = r#"
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "full-members", feeds = "internal" }, { template = "user-group/<handle>" }]
lookup = "people"

[externals.audience.people]
url = "AUDIENCE_URL"
"#;

/// What the stub answers: the members of any selector, and the principal of any member.
/// `None` on either answers a 500.
#[derive(Clone, Default)]
struct Answers {
    members: Option<Vec<&'static str>>,
    principal: Option<Option<&'static str>>,
}

#[derive(Clone)]
struct Source {
    answers: Arc<Mutex<Answers>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Source {
    fn members(&self, members: Option<Vec<&'static str>>) {
        self.answers.lock().unwrap().members = members;
    }

    fn principal(&self, principal: Option<Option<&'static str>>) {
        self.answers.lock().unwrap().principal = principal;
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_source() -> (String, Source) {
    let source = Source {
        answers: Arc::new(Mutex::new(Answers {
            members: Some(vec![]),
            principal: None,
        })),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/audience",
            post(|State(source): State<Source>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let lookup = request["artifact"]["member"].is_string();
                source.requests.lock().unwrap().push(request);
                let answers = source.answers.lock().unwrap().clone();
                let answer = if lookup {
                    answers
                        .principal
                        .map(|principal| serde_json::json!({ "principal": principal }))
                } else {
                    answers.members.map(|members| serde_json::json!({ "members": members }))
                };
                match answer {
                    Some(answer) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "answer": answer }).to_string(),
                    ),
                    None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(source.clone());
    (format!("{}/audience", serve(router).await), source)
}

fn read_hr() -> ProposedCall {
    ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

fn send(to: &str) -> ProposedCall {
    ProposedCall {
        tool: "send".to_string(),
        arguments: raw(serde_json::json!({ "to": to })),
        cwd: None,
    }
}

async fn narrowed(dir: &tempfile::TempDir, audience_url: &str) -> Arc<Runtime> {
    narrowed_under(dir, POLICY, audience_url).await
}

/// The runtime opened on `policy`, its audience narrowed to Alice and Bob by the `read_hr`
/// call every scenario starts from.
async fn narrowed_under(dir: &tempfile::TempDir, policy: &str, audience_url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy.replace("AUDIENCE_URL", audience_url)).expect("the fixture writes");
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
                settings: Default::default(),
            }
        )
        .await,
        HookDecision::Ack
    );
    let blocked = propose(&runtime, read_hr()).await;
    let HookDecision::DenyCall { feedback, .. } = blocked else {
        panic!("the narrowing read is offered for acceptance, got {blocked:?}");
    };
    assert!(feedback.contains("Continue:"), "{feedback}");
    assert!(!feedback.contains("No remedy is offered"), "{feedback}");
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_hr()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_hr()).await;
    runtime
}

#[tokio::test]
async fn a_group_argument_is_checked_against_the_sources_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // The source reports Alice by the address the delta wrote: the same reader.
    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["version"], 1);
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["name"], "slack");
    assert_eq!(
        requests[0]["declaration"]["templates"],
        serde_json::json!(["viewer", "full-members", "user-group/<handle>"])
    );
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );

    // A member reported by provider id is that qualified reader, which the narrowed
    // audience does not hold.
    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);

    // An empty member list is a complete answer.
    source.members(Some(vec![]));
    assert_eq!(
        propose(&runtime, send("@nobody")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@nobody")).await;
    assert_eq!(
        source.requests()[2]["artifact"],
        serde_json::json!({ "selector": "user-group/nobody" })
    );
}

#[tokio::test]
async fn no_answer_leaves_the_call_unchecked_and_the_log_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.members(None);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), before, "no answer is no engine act");

    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(audit_len(&runtime) > before);
    ran(&runtime, send("@team")).await;
}

/// A source that answers 500 leaves the check without the fact it reads: the call is denied
/// on the missing fact alone, with no plan, and the proposal retried once it answers runs.
#[tokio::test]
async fn an_unanswered_source_denies_on_a_missing_fact() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    source.members(None);
    let decision = propose(&runtime, send("@team")).await;
    let HookDecision::DenyCall { offers, outcomes, .. } = decision else {
        panic!("the call is denied, got {decision:?}");
    };
    assert!(offers.is_empty());
    assert_eq!(outcomes, Some(Outcomes::MissingFact(RetryHint::Later)));

    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
}

/// A call that reads two groups: one source answers 404, which only a repair fixes; the other
/// reports a member whose lookup answers 503, which a later try may get past. The repair
/// decides the hint, whichever consult round failed last.
#[tokio::test]
async fn a_lost_selector_outranks_a_later_lookup_failure() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let router = Router::new().route(
        "/audience",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
            match (
                request["artifact"]["member"].is_string(),
                body.contains("user-group/gone"),
            ) {
                (true, _) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy".to_string()),
                (false, true) => (axum::http::StatusCode::NOT_FOUND, "missing".to_string()),
                (false, false) => (
                    axum::http::StatusCode::OK,
                    serde_json::json!({ "version": 1, "answer": { "members": ["slack:U-bob"] } }).to_string(),
                ),
            }
        }),
    );
    let url = format!("{}/audience", serve(router).await);
    let both = "[[policy.tool]]\nname = \"send_both\"\nrequires = { audience = { contains = [\"@slack:user-group/gone\", \"@slack:user-group/team\"] } }\ndelta = {}\n\n";
    let policy = POLICY
        .replace(SLACK_BINDING, LOOKUP_URL_BINDINGS)
        .replace("[externals]", &format!("{both}[externals]"));
    let runtime = narrowed_under(&dir, &policy, &url).await;

    let decision = propose(
        &runtime,
        ProposedCall {
            tool: "send_both".to_string(),
            arguments: raw(serde_json::json!({})),
            cwd: None,
        },
    )
    .await;
    let HookDecision::DenyCall { outcomes, .. } = decision else {
        panic!("the call is denied, got {decision:?}");
    };
    assert_eq!(outcomes, Some(Outcomes::MissingFact(RetryHint::AfterRepair)));
}

/// A selector is the operator's to read: two calls whose recipients' sources fail alike are
/// told the same, whichever group each named.
#[tokio::test]
async fn a_failed_selector_reads_alike_whatever_it_named() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let router = Router::new().route(
        "/audience",
        post(|| async { (axum::http::StatusCode::NOT_FOUND, "missing".to_string()) }),
    );
    let url = format!("{}/audience", serve(router).await);
    let send_to = "[[policy.tool]]\nname = \"send_to\"\nparameters = { type = \"object\", properties = { to = { type = \"string\" } }, required = [\"to\"] }\nrequires = { audience = { contains = [\"$to\"] } }\ndelta = {}\n\n";
    let policy = POLICY
        .replace(SLACK_BINDING, LOOKUP_URL_BINDINGS)
        .replace("[externals]", &format!("{send_to}[externals]"));
    let runtime = narrowed_under(&dir, &policy, &url).await;
    let mut told = Vec::new();
    for group in ["@slack:user-group/alpha", "@slack:user-group/beta"] {
        let decision = propose(
            &runtime,
            ProposedCall {
                tool: "send_to".to_string(),
                arguments: raw(serde_json::json!({ "to": group })),
                cwd: None,
            },
        )
        .await;
        let HookDecision::DenyCall { feedback, outcomes, .. } = decision else {
            panic!("the call is denied, got {decision:?}");
        };
        assert_eq!(outcomes, Some(Outcomes::MissingFact(RetryHint::AfterRepair)));
        told.push(feedback);
    }
    assert_eq!(told[0], told[1]);
}

/// A narrowed session sends to `@team`. Whether the directory resolves it to bob alone, whose
/// lookup answers 503, or to bob and carol, whose lookup answers 404, the model reads the
/// same bytes through the adapter; only the host's typed hint tells the two apart.
#[tokio::test]
async fn which_members_failed_and_how_never_reaches_the_model() {
    let mut rendered = Vec::new();
    for team in [vec!["slack:U-bob"], vec!["slack:U-bob", "slack:U-carol"]] {
        let router = Router::new().route(
            "/audience",
            post(move |body: String| {
                let team = team.clone();
                async move {
                    let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                    match request["artifact"]["member"].as_str() {
                        Some("slack:U-bob") => (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy".to_string()),
                        Some(_) => (axum::http::StatusCode::NOT_FOUND, "missing".to_string()),
                        None => (
                            axum::http::StatusCode::OK,
                            serde_json::json!({ "version": 1, "answer": { "members": team } }).to_string(),
                        ),
                    }
                }
            }),
        );
        let url = format!("{}/audience", serve(router).await);
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = narrowed_under(&dir, &POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS), &url).await;
        let call = ProposedCall {
            tool: "send".to_string(),
            arguments: raw(serde_json::json!({ "to": "@team" })),
            cwd: None,
        };
        let event = HookEvent::ToolCall {
            actor: actor(),
            call,
            call_id: None,
            spawn: None,
            prompt: None,
            ruling: None,
            declares: None,
        };
        let decision = hooks::handle(&runtime, event.clone()).await;
        let HookDecision::DenyCall { outcomes, .. } = &decision else {
            panic!("the call is denied, got {decision:?}");
        };
        let rendered_for_model = (appa_adapter_claude_code::codec().render)(b"{}", &event, &decision);
        rendered.push((rendered_for_model, *outcomes));
    }
    let [(bob, bob_hint), (both, both_hint)] = &rendered[..] else {
        panic!("both directories were asked");
    };
    assert_eq!(
        (bob_hint, both_hint),
        (
            &Some(Outcomes::MissingFact(RetryHint::Later)),
            &Some(Outcomes::MissingFact(RetryHint::AfterRepair))
        )
    );
    assert_eq!(
        serde_json::to_vec(bob).expect("serializes"),
        serde_json::to_vec(both).expect("serializes")
    );
}

/// `send` with the reserved `blocked` mark beside its recipient check.
const BLOCKED_SEND: &str = r#"
[[policy.tool]]
name = "send_blocked"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] }, attention = ["blocked"] }
delta = {}

"#;

/// The recipient check needs `@team`'s members, which no answer pins yet. The `blocked` mark
/// decides the call first: a hard block with no plan, and the source is never asked.
#[tokio::test]
async fn a_blocked_call_is_a_hard_block_before_membership_is_known() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace("[externals]", &format!("{BLOCKED_SEND}[externals]"));
    let runtime = narrowed_under(&dir, &policy, &url).await;

    let decision = propose(
        &runtime,
        ProposedCall {
            tool: "send_blocked".to_string(),
            arguments: raw(serde_json::json!({ "to": "@team" })),
            cwd: None,
        },
    )
    .await;
    let HookDecision::DenyCall { offers, outcomes, .. } = decision else {
        panic!("the call is denied, got {decision:?}");
    };
    assert!(offers.is_empty());
    assert_eq!(outcomes, Some(Outcomes::HardBlock));
    assert!(source.requests().is_empty(), "no membership is read for a blocked call");
}

/// An authority whose mandate reads `@team`, and an input sanitizer that would widen the
/// call to its recipient: remedies for the recipient gap a `blocked` mark leaves terminal.
const BLOCKED_REMEDIES: [&str; 2] = [
    r#"
[[policy.authority]]
name = "reviewer"
[policy.authority.permits]
audience_missing = ["@team"]

"#,
    r#"
[[policy.sanitizer]]
name = "widen"
on = ["tool_input"]
permits = { audience = { from = ["alice@corp.example", "bob@corp.example"], to = ["carol@corp.example"] } }

"#,
];

const BLOCKED_REMEDY_BINDINGS: [&str; 2] = [
    "[externals.authorities.reviewer]\nbuiltin = \"hitl\"\n\n",
    "[externals.sanitizers.widen]\nbuiltin = \"redact-email\"\n\n",
];

/// A recipient outside the label is a gap decided without membership. Beside a `blocked`
/// mark nothing remedies it: no mandate is read, no source asked, nothing offered.
#[tokio::test]
async fn a_blocked_call_gathers_and_offers_nothing_for_its_other_gaps() {
    for (remedy, binding) in BLOCKED_REMEDIES.into_iter().zip(BLOCKED_REMEDY_BINDINGS) {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (url, source) = serve_source().await;
        source.members(None);
        let policy = POLICY
            .replace("[externals]", &format!("{BLOCKED_SEND}{remedy}[externals]"))
            .replace(
                "[externals.audience.slack]",
                &format!("{binding}[externals.audience.slack]"),
            );
        let runtime = narrowed_under(&dir, &policy, &url).await;

        let decision = propose(
            &runtime,
            ProposedCall {
                tool: "send_blocked".to_string(),
                arguments: raw(serde_json::json!({ "to": "carol@corp.example" })),
                cwd: None,
            },
        )
        .await;
        let HookDecision::DenyCall { offers, outcomes, .. } = decision else {
            panic!("the call is denied, got {decision:?}");
        };
        assert!(offers.is_empty(), "{remedy}");
        assert_eq!(outcomes.map(|outcomes| outcomes.deciding()), Some(Outcome::HardBlock));
        assert!(source.requests().is_empty(), "{remedy}");
    }
}

#[tokio::test]
async fn an_unconfigured_group_argument_fails_operationally() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // `@offsite` is supplied dynamically and configured nowhere: an operational
    // refusal that consults nothing and decides nothing.
    assert!(matches!(
        propose(&runtime, send("@offsite")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty());
    assert_eq!(audit_len(&runtime), before);
}

/// A policy that maps `internal` to no sources loads, and a contract can still require
/// it. Once the trajectory narrows to `self`, that requirement needs the members of
/// `internal` and can never obtain them: a static policy gap, denied as one — the same
/// answer on every proposal, no consult, no engine act.
#[tokio::test]
async fn an_unmapped_built_in_level_denies_without_a_retry_hint() {
    const UNMAPPED: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "read_secret"
delta = { audience = ["self"] }

[[policy.tool]]
name = "send_internal"
requires = { audience = { contains = ["internal"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, UNMAPPED).expect("the fixture writes");
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
                settings: Default::default(),
            }
        )
        .await,
        HookDecision::Ack
    );
    let read_secret = || ProposedCall {
        tool: "read_secret".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    let send_internal = || ProposedCall {
        tool: "send_internal".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    let HookDecision::DenyCall { feedback, .. } = propose(&runtime, read_secret()).await else {
        panic!("the narrowing read is offered for acceptance");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read_secret()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read_secret()).await;
    let before = audit_len(&runtime);

    for _ in 0..2 {
        let HookDecision::DenyCall {
            feedback,
            offers,
            outcomes,
            ..
        } = propose(&runtime, send_internal()).await
        else {
            panic!("the call is denied");
        };
        // The feedback points at the key the operator must fill, not at a retry.
        assert!(feedback.contains("[policy.audience] internal"), "{feedback}");
        assert!(offers.is_empty());
        assert_eq!(
            outcomes,
            Some(Outcomes::Integrity),
            "a misconfigured policy, not a missing fact"
        );
        assert_eq!(audit_len(&runtime), before);
    }
}

#[tokio::test]
async fn public_and_literal_arguments_never_consult_the_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    assert!(matches!(
        propose(&runtime, send("public")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        propose(&runtime, send("alice@corp.example")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("alice@corp.example")).await;
    assert!(matches!(
        propose(&runtime, send("mallory")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "no spelling here names a group");
}

#[tokio::test]
async fn a_refusal_without_a_continuation_explains_how_to_finish() {
    let (url, _) = serve_source().await;
    // A requirement gap alone, and the same gap with a narrowing. Neither can be
    // cleared by delegating to a child that inherits this session's label.
    for delta in ["{}", "{ trust = \"suspicious\", audience = [\"self\"] }"] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = POLICY.replace(
            "effects = [\"egress\"]\ndelta = {}",
            &format!("effects = [\"egress\"]\ndelta = {delta}"),
        );
        let policy = format!(
            "{policy}\n[[policy.tool]]\nname = \"trusted_work\"\nrequires = {{ trust = \"trusted\" }}\ndelta = {{}}\n"
        );
        let runtime = narrowed_under(&dir, &policy, &url).await;
        let HookDecision::DenyCall { feedback, offers, .. } = propose(&runtime, send("public")).await else {
            panic!("public is outside the private read's audience");
        };
        assert!(offers.is_empty());
        assert!(feedback.starts_with("[appa] Blocked: send cannot run."), "{feedback}");
        for guidance in [
            "No remedy is offered for this call in the current state.",
            "Do not retry this flow or reuse an earlier offer for it.",
            "Tell the user that the policy refused this part of the task, and finish any permitted work.",
        ] {
            assert!(feedback.contains(guidance), "{feedback}");
        }
        for misleading in [
            "cannot run yet",
            "Accepting the narrowing",
            "After accepting",
            "Continue:",
            "delegate",
        ] {
            assert!(!feedback.contains(misleading), "{feedback}");
        }
        // The refusal does not stop permitted work in the same trajectory.
        assert_eq!(
            propose(&runtime, read_hr()).await,
            HookDecision::AllowCall { spawn: None }
        );
    }
}

/// The probe reads every selector the policy references, once each, and refuses the first
/// source that fails or reports a reader outside the shape rule — naming the provider and
/// the selector.
#[tokio::test]
async fn the_probe_reads_every_referenced_selector_and_refuses_a_malformed_source() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    runtime
        .probe_sources()
        .await
        .expect("well-formed answers pass the probe");
    let mut probed: Vec<String> = source
        .requests()
        .iter()
        .map(|request| {
            request["artifact"]["selector"]
                .as_str()
                .expect("a selector read")
                .to_string()
        })
        .collect();
    probed.sort();
    assert_eq!(probed, ["user-group/direct", "user-group/nobody", "user-group/team"]);

    source.members(Some(vec!["alice@corp.example", "finance"]));
    let refused = runtime
        .probe_sources()
        .await
        .expect_err("a malformed reader refuses the probe");
    let ProbeError::Selector { provider, selector, .. } = &refused else {
        panic!("a malformed member names its selector: {refused}");
    };
    assert_eq!(provider, "slack");
    assert!(selector.starts_with("user-group/"), "{refused}");

    source.members(None);
    assert!(
        runtime.probe_sources().await.is_err(),
        "a source that is down refuses the probe"
    );
}

/// The probe also asks the lookup entry for one qualified member the selectors report,
/// whatever their number, and holds the principal to the shape rule.
#[tokio::test]
async fn the_probe_asks_each_lookup_entry_for_one_owed_member() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    std::fs::write(&path, policy.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    source.members(Some(vec!["alice@corp.example", "slack:U-bob", "slack:U-carol"]));
    source.principal(Some(Some("bob@corp.example")));
    runtime
        .probe_sources()
        .await
        .expect("a well-formed principal passes the probe");
    let lookups: Vec<serde_json::Value> = source
        .requests()
        .into_iter()
        .filter(|request| request["artifact"]["member"].is_string())
        .collect();
    assert_eq!(lookups.len(), 1, "two owed members under one provider cost one lookup");
    assert_eq!(lookups[0]["name"], "people");
    assert_eq!(lookups[0]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));

    source.principal(Some(Some("github:bob")));
    let refused = runtime
        .probe_sources()
        .await
        .expect_err("a principal outside the member's namespace refuses the probe");
    let ProbeError::Lookup { provider, member, .. } = &refused else {
        panic!("a malformed principal names its member: {refused}");
    };
    assert_eq!((provider.as_str(), member.as_str()), ("slack", "slack:U-bob"));

    source.principal(None);
    assert!(
        matches!(runtime.probe_sources().await, Err(ProbeError::Lookup { .. })),
        "a lookup entry that is down refuses the probe"
    );
}

/// A reload is prepared, probed, then installed: a probe that fails leaves the previous
/// deployment serving, and the next successful reload of the same file is the change.
#[tokio::test]
async fn a_refused_reload_probe_leaves_the_previous_deployment_serving() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY.replace("AUDIENCE_URL", &url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

    let revised = || {
        let text = POLICY
            .replace("AUDIENCE_URL", &url)
            .replace("name = \"send_direct\"", "name = \"send_routed\"");
        std::fs::write(&path, text).expect("the revision writes");
        Config::load(&path).expect("the revision validates")
    };
    source.members(Some(vec!["finance"]));
    let prepared = runtime.prepare_deployment(revised()).expect("the revision loads");
    assert!(
        prepared.probe_sources().await.is_err(),
        "the revision's source answers garbage"
    );
    drop(prepared);

    source.members(Some(vec!["bob@corp.example"]));
    let prepared = runtime.prepare_deployment(revised()).expect("the revision loads again");
    prepared.probe_sources().await.expect("the source recovered");
    let reloaded = runtime.install(prepared).expect("the revision publishes");
    assert!(
        reloaded.changed,
        "the refused probe installed nothing, so this reload is the change"
    );
}

/// A trajectory opened under one routing follows its deployment when a reload retargets the
/// provider: the revision that decides its next call is the one whose externals answer it, so
/// its lookups reach the entry the reloaded policy names.
#[tokio::test]
async fn a_trajectory_takes_the_lookup_routing_its_deployment_now_serves() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;

    // Slack's lookups move to `directory`; a new GitHub group keeps `people` bound.
    let retargeted = format!(
        "{}\n[externals.audience.directory]\nurl = \"{url}\"\n\n[externals.audience.github]\nurl = \"{url}\"\nlookup = \"people\"\nselectors = [{{ template = \"org/<org>/members\", feeds = \"internal\" }}]\n",
        policy
            .replace("AUDIENCE_URL", &url)
            .replace("lookup = \"people\"", "lookup = \"directory\"")
            .replace(
                "[policy.audience.group.nobody]\n",
                "[policy.audience.group.engineers]\nfrom = [\"github:org/acme/members\"]\n\n[policy.audience.group.nobody]\n"
            )
    );
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, retargeted).expect("the revision writes");
    let revised = Config::load(&path).expect("the revision validates");
    source.members(Some(vec!["slack:U-bob"]));
    source.principal(Some(Some("bob@corp.example")));
    let reloaded = runtime.reload(revised).expect("the revision installs");
    assert!(reloaded.changed);

    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let lookups: Vec<serde_json::Value> = source
        .requests()
        .into_iter()
        .filter(|request| request["artifact"]["member"].is_string())
        .collect();
    assert_eq!(lookups.len(), 1);
    assert_eq!(
        lookups[0]["name"], "directory",
        "the trajectory's lookup goes where the head's policy sends it"
    );
}

#[test]
fn a_referenced_audience_source_must_be_bound() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    let unbound = POLICY.replace(SLACK_BINDING, "");
    std::fs::write(&path, unbound).expect("the fixture writes");
    // The binding carries the provider's declaration, so without it the policy's own
    // `slack:` references have no provider to resolve against.
    let config = Config::load(&path).expect("the file validates");
    assert!(matches!(
        Runtime::open(config, dir.path().join("appa.db"), None),
        Err(appa_runtime::api::OpenError::Policy(error))
            if matches!(*error, appa_policy::ConfigError::UndeclaredProvider { .. })
    ));
}

/// A source that only an included battery declares is undeclared while that battery is
/// absent or declares nothing; including it again composes, and declaring it in the root
/// as well collides with the battery's own declaration.
#[test]
fn an_audience_source_declared_by_an_absent_include_is_undeclared() {
    use appa_runtime::config::{ConfigError, HostDefaults, HostedBattery};

    const ROOT: &str = "[policy]\nversion = 2\n\n[policy.audience]\ninternal = [\"team:members\"]\n";
    const ROOT_BINDING: &str = "\n[externals.audience.team]\nurl = \"http://127.0.0.1:9/audience\"\nselectors = [{ template = \"members\", feeds = \"internal\" }]\n";
    let declaring = format!("[policy]\nversion = 2\n{ROOT_BINDING}");
    let battery = |base| HostedBattery {
        name: "team",
        base,
        target: None,
        token_env: &[],
    };
    let compose = |root: &str, batteries: &[HostedBattery<'_>]| {
        Config::hosted_composed(
            root,
            batteries,
            HostDefaults::new(std::time::Duration::from_millis(1000), 4096),
            |_| None,
        )
    };

    for batteries in [vec![], vec![battery("[policy]\nversion = 2\n")]] {
        let config = compose(ROOT, &batteries).expect("the document composes");
        let Err(appa_runtime::api::OpenError::Policy(error)) = Runtime::open_in_memory(config, None, Remedies::StandIn)
        else {
            panic!("an undeclared audience source must not open");
        };
        let appa_policy::ConfigError::UndeclaredProvider { context, provider } = *error else {
            panic!("expected an undeclared audience source, got {error}");
        };
        assert_eq!((context.as_str(), provider.as_str()), ("[audience] internal", "team"));
    }

    let restored = compose(ROOT, &[battery(&declaring)]).expect("the restored include composes");
    Runtime::open_in_memory(restored, None, Remedies::StandIn).expect("the restored include declares the source");

    let doubled = format!("{ROOT}{ROOT_BINDING}");
    assert!(matches!(
        compose(&doubled, &[battery(&declaring)]),
        Err(ConfigError::DuplicateExternal { section, name, .. }) if section == "audience" && name == "team"
    ));
}

#[tokio::test]
async fn a_qualified_recipient_is_checked_through_a_member_lookup() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // The lookup canonicalizes the recipient to the principal the narrowed audience holds.
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("slack:U-bob")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one lookup per member");
    assert_eq!(requests[0]["kind"], "audience");
    assert_eq!(requests[0]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));
}

#[tokio::test]
async fn a_malformed_principal_is_refused_and_not_re_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // A principal that is neither an address nor a slack-qualified id is a broken answer:
    // the call is refused operationally after exactly one consult, and no decision is
    // recorded.
    source.principal(Some(Some("finance")));
    assert!(matches!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 1, "a broken answer is not re-asked");
    assert_eq!(audit_len(&runtime), before);

    // A principal in another provider's namespace is refused the same way.
    source.principal(Some(Some("github:bob")));
    assert!(matches!(
        propose(&runtime, send("slack:U-bob")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);
    assert_eq!(audit_len(&runtime), before);
}

#[tokio::test]
async fn a_malformed_member_refuses_the_whole_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    // One member that is neither an address nor a slack-qualified id refuses the answer:
    // the call is refused operationally after one consult, and no decision is recorded.
    for members in [
        vec!["alice@corp.example", "finance"],
        vec!["github:alice"],
        vec!["public"],
    ] {
        let asked = source.requests().len();
        source.members(Some(members));
        assert!(matches!(
            propose(&runtime, send("@team")).await,
            HookDecision::DenyCall { .. }
        ));
        assert_eq!(source.requests().len(), asked + 1, "a broken answer is not re-asked");
        assert_eq!(audit_len(&runtime), before);
    }
}

/// Under `lookup`, every member a source reports by provider id is looked up before it
/// seats, and the principal the lookup names is the reader the audience holds.
#[tokio::test]
async fn a_redirected_providers_qualified_members_seat_through_a_roster() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, ROSTER_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;

    // The roster maps Bob to the address the narrowed audience holds, in process.
    source.members(Some(vec!["slack:U-bob"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    assert_eq!(source.requests().len(), 1, "a roster lookup reaches no service");

    // A member the roster does not list keeps its qualified identity.
    source.members(Some(vec!["slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 2);

    // An address needs no lookup.
    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
}

#[tokio::test]
async fn a_redirected_providers_lookups_go_to_the_named_entry() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let policy = POLICY.replace(SLACK_BINDING, LOOKUP_URL_BINDINGS);
    let runtime = narrowed_under(&dir, &policy, &url).await;
    let before = audit_len(&runtime);

    source.members(Some(vec!["slack:U-bob"]));
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, send("@team")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 2, "one selector read, one lookup");
    assert_eq!(requests[0]["name"], "slack");
    assert_eq!(
        requests[1]["name"], "people",
        "the lookup is asked of the entry slack names"
    );
    assert_eq!(requests[1]["artifact"], serde_json::json!({ "member": "slack:U-bob" }));

    // `null` is a definitive answer: the member keeps its qualified identity.
    source.principal(Some(None));
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(source.requests().len(), 4);

    // No answer from the lookup entry is an operational refusal and no decision.
    let decided = audit_len(&runtime);
    source.principal(None);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), decided);
    assert!(decided > before);
    let asked = source.requests().len();
    drop(runtime);

    // The reopened deployment replays the pinned lookups without consulting anyone; only
    // the fresh act reads the source and the lookup entry again.
    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let reopened = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    source.principal(Some(Some("bob@corp.example")));
    assert_eq!(
        propose(&reopened, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(
        source.requests().len(),
        asked + 2,
        "the fresh act reads the selector and the lookup entry once each"
    );
}

#[tokio::test]
async fn a_bare_provider_prefix_recipient_is_a_literal_reader() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;

    // "slack:" names no member, so it denotes itself: the check decides without any
    // consult instead of looping against the source.
    assert!(matches!(
        propose(&runtime, send("slack:")).await,
        HookDecision::DenyCall { .. }
    ));
    assert!(source.requests().is_empty(), "a bare prefix is never looked up");
}

#[tokio::test]
async fn a_group_recipient_is_read_per_act_and_replayed_from_the_pin() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let before = audit_len(&runtime);

    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&runtime, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    let requests = source.requests();
    assert_eq!(requests.len(), 1, "one act, one consult per selector");
    assert_eq!(
        requests[0]["artifact"],
        serde_json::json!({ "selector": "user-group/team" })
    );
    ran(&runtime, send("@team")).await;

    source.members(Some(vec!["carol@corp.example"]));
    let HookDecision::DenyCall { feedback, .. } = propose(&runtime, send("@team")).await else {
        panic!("a team member outside the readers blocks");
    };
    assert!(
        !feedback.contains("carol"),
        "the model hears a directory member: {feedback}"
    );
    assert_eq!(source.requests().len(), 2);

    let undecided = audit_len(&runtime);
    source.members(None);
    assert!(matches!(
        propose(&runtime, send("@team")).await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(audit_len(&runtime), undecided);
    assert!(audit_len(&runtime) > before);

    let audit = serde_json::to_string(&runtime.audit(&root()).expect("the audit reads")).expect("the audit serializes");
    assert!(!audit.contains("carol"), "the audit leaks a directory member: {audit}");
    let asked = source.requests().len();
    drop(runtime);

    // The reopened deployment replays the log — its pinned answers included — without
    // consulting the source; only the fresh act reads it again.
    let config = Config::load(&dir.path().join("appa.toml")).expect("the fixture validates");
    let reopened = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment reopens"));
    source.members(Some(vec!["bob@corp.example"]));
    assert_eq!(
        propose(&reopened, send("@team")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(
        source.requests().len(),
        asked + 1,
        "the fresh act reads the selector once"
    );
}

/// `POLICY` with a channel-keyed sink: posting to a channel requires that channel's members
/// among the current readers, the channel read from the call.
const CHANNEL_POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "post"
parameters = { type = "object", properties = { channel_id = { type = "string" }, text = { type = "string" } }, required = ["channel_id", "text"] }
requires = { audience = { contains = ["@slack:channel/$channel_id"] } }
effects = ["egress"]
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{ template = "viewer", feeds = "self" }, { template = "channel/<id>" }]
"#;

fn post_to(channel_id: &str) -> ProposedCall {
    ProposedCall {
        tool: "post".to_string(),
        arguments: raw(serde_json::json!({ "channel_id": channel_id, "text": "hi" })),
        cwd: None,
    }
}

/// A selector placeholder is instantiated from each call: the source is asked for exactly the
/// collection the call's argument spells, and its answer is checked as a static mention's is.
#[tokio::test]
async fn a_selector_placeholder_asks_the_source_for_the_collection_the_call_spells() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed_under(&dir, CHANNEL_POLICY, &url).await;

    source.members(Some(vec!["alice@corp.example"]));
    assert_eq!(
        propose(&runtime, post_to("C1")).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, post_to("C1")).await;
    let requests = source.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["declaration"]["templates"],
        serde_json::json!(["viewer", "channel/<id>"])
    );
    assert_eq!(requests[0]["artifact"], serde_json::json!({ "selector": "channel/C1" }));

    // Another channel is another collection, asked for by its own selector.
    source.members(Some(vec!["alice@corp.example", "slack:U-carol"]));
    assert!(matches!(
        propose(&runtime, post_to("C2")).await,
        HookDecision::DenyCall { .. }
    ));
    let requests = source.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["artifact"], serde_json::json!({ "selector": "channel/C2" }));
    assert!(audit_len(&runtime) > 0, "the decided acts read back");
}

/// `groups` input sanitizers that each widen to a different group, served by a redirected
/// directory: each group's selector and its member's lookup are separate asks.
fn staged_groups_policy(groups: u32) -> String {
    let each = |template: &str| -> String { (1..=groups).map(|k| template.replace("{k}", &k.to_string())).collect() };
    let named = each("\n[policy.audience.group.g{k}]\nfrom = [\"slack:user-group/g{k}\"]\n");
    let sanitizers = each(
        "\n[[policy.sanitizer]]\nname = \"via-g{k}\"\non = [\"tool_input\"]\npermits = { audience = { from = [\"alice@corp.example\", \"bob@corp.example\"], to = [\"@g{k}\"] } }\n",
    );
    let bound = each("\n[externals.sanitizers.via-g{k}]\nbuiltin = \"redact-email\"\n");
    format!(
        r#"
[policy]
version = 2
{named}
[[policy.tool]]
name = "read_hr"
delta = {{ audience = ["alice@corp.example", "bob@corp.example"] }}

[[policy.tool]]
name = "publish"
parameters = {{ type = "object", properties = {{ body = {{ type = "string" }} }}, required = ["body"] }}
requires = {{ audience = {{ contains = ["partner@corp.example"] }} }}
effects = ["egress"]
delta = {{}}
{sanitizers}
[externals]
timeout_ms = 1000
max_body_bytes = 4096
{bound}
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{{ template = "user-group/<handle>" }}]
lookup = "people"

[externals.audience.people]
url = "AUDIENCE_URL"
"#
    )
}

/// A directory that answers each selector and each member lookup from its own entry.
#[derive(Clone, Default)]
struct Directory {
    members: Arc<Mutex<std::collections::BTreeMap<String, Vec<String>>>>,
    principals: Arc<Mutex<std::collections::BTreeMap<String, Option<String>>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Directory {
    fn group(&self, selector: &str, member: &str, principal: Option<&str>) {
        self.members
            .lock()
            .unwrap()
            .insert(selector.to_string(), vec![member.to_string()]);
        self.principals
            .lock()
            .unwrap()
            .insert(member.to_string(), principal.map(str::to_string));
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve_directory() -> (String, Directory) {
    let directory = Directory::default();
    let router = Router::new()
        .route(
            "/audience",
            post(|State(directory): State<Directory>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let artifact = request["artifact"].clone();
                directory.requests.lock().unwrap().push(request);
                let answer = match (artifact["selector"].as_str(), artifact["member"].as_str()) {
                    (Some(selector), None) => directory
                        .members
                        .lock()
                        .unwrap()
                        .get(selector)
                        .map(|members| serde_json::json!({ "members": members })),
                    (None, Some(member)) => directory
                        .principals
                        .lock()
                        .unwrap()
                        .get(member)
                        .map(|principal| serde_json::json!({ "principal": principal })),
                    _ => None,
                };
                match answer {
                    Some(answer) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({ "version": 1, "answer": answer }).to_string(),
                    ),
                    None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(directory.clone());
    (format!("{}/audience", serve(router).await), directory)
}

fn publish(body: &str) -> ProposedCall {
    ProposedCall {
        tool: "publish".to_string(),
        arguments: raw(serde_json::json!({ "body": body })),
        cwd: None,
    }
}

/// Stage one rewrite per group for the same raw body: each hop takes its group's sanitizer
/// plan, and every hop derives the same bytes, which are returned.
async fn stage_group_rewrites(runtime: &Runtime, groups: u32) -> String {
    const RAW: &str = "mail alice@corp.example today";
    let mut derived: Option<String> = None;
    for k in 1..=groups {
        let blocked = hooks::handle_embedded(
            runtime,
            HookEvent::ToolCall {
                actor: actor(),
                call: publish(RAW),
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        let presentation = blocked
            .presentation
            .unwrap_or_else(|| panic!("the raw call blocks with remedies (hop {k}): {:?}", blocked.decision));
        let name = format!("via-g{k}");
        let hop = presentation
            .offers
            .iter()
            .find(|offer| {
                offer
                    .input_sanitizer
                    .as_ref()
                    .is_some_and(|sanitizer| sanitizer.name == name)
            })
            .map(|offer| appa_runtime::api::OfferId(offer.id.clone()))
            .unwrap_or_else(|| panic!("{name} is offered: {:?}", presentation.offers));
        let RemedyOutcome::Authorized { call } = runtime.execute_remedy(&actor(), hop).await else {
            panic!("{name}'s hop stages a derivation");
        };
        let arguments: serde_json::Value = serde_json::from_str(call.arguments.get()).expect("canonical JSON");
        let body = arguments["body"].as_str().expect("a body").to_string();
        match &derived {
            None => derived = Some(body),
            Some(first) => assert_eq!(first, &body, "every rewrite derives the same bytes"),
        }
    }
    derived.expect("every hop ran")
}

/// One ordinary proposal can owe eight consult batches that all answer: four rewrites staged
/// for the same bytes widen to four groups of a redirected directory, and the proposal is
/// judged under each in subject order, a selector read then a member lookup apiece. The
/// engine pass after the eighth batch decides. Subject order follows the random batch ids,
/// so the first proposal, where no group clears, reads the order off the directory.
#[tokio::test]
async fn four_staged_group_rewrites_resolve_through_eight_consult_batches() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    for k in 1..=4 {
        directory.group(
            &format!("user-group/g{k}"),
            &format!("slack:U{k}"),
            Some("partner@corp.example"),
        );
    }
    let runtime = narrowed_under(&dir, &staged_groups_policy(4), &url).await;

    let derived = stage_group_rewrites(&runtime, 4).await;

    // The asks one proposal of the derived bytes makes, as (entry, selector or member).
    let asks_of = |runtime: Arc<Runtime>, directory: Directory, derived: String| async move {
        let asked = directory.requests().len();
        let decision = propose(&runtime, publish(&derived)).await;
        let asks: Vec<(String, String)> = directory.requests()[asked..]
            .iter()
            .map(|request| {
                let artifact = &request["artifact"];
                let asked_for = artifact["selector"].as_str().or(artifact["member"].as_str());
                (
                    request["name"].as_str().expect("an entry name").to_string(),
                    asked_for.expect("a selector or a member").to_string(),
                )
            })
            .collect();
        (decision, asks)
    };

    // No group holds the partner desk any more: every derivation is judged and none clears.
    for k in 1..=4 {
        directory.group(
            &format!("user-group/g{k}"),
            &format!("slack:U{k}"),
            Some(&format!("other{k}@corp.example")),
        );
    }
    let (decision, asks) = asks_of(runtime.clone(), directory.clone(), derived.clone()).await;
    assert!(
        matches!(decision, HookDecision::DenyCall { .. }),
        "the eighth batch's answers reach the engine, which blocks: {decision:?}"
    );
    assert_eq!(asks.len(), 8, "four selector reads and four lookups: {asks:?}");
    let mut groups: Vec<u32> = Vec::new();
    for pair in asks.chunks(2) {
        let [(source, selector), (lookup, member)] = pair else {
            unreachable!("eight asks pair up");
        };
        let k: u32 = selector
            .strip_prefix("user-group/g")
            .and_then(|k| k.parse().ok())
            .unwrap_or_else(|| panic!("a group selector: {selector}"));
        assert_eq!((source.as_str(), lookup.as_str()), ("slack", "people"));
        assert_eq!(
            member,
            &format!("slack:U{k}"),
            "each selector's member is looked up next"
        );
        groups.push(k);
    }
    groups.sort_unstable();
    assert_eq!(
        groups,
        vec![1, 2, 3, 4],
        "each derivation's group is read once: {asks:?}"
    );

    // Only the group judged last holds the partner desk again: the first three derivations
    // fail on their answers, and the fourth clears on the eighth batch's.
    let last = &asks[6].1;
    let last_member = &asks[7].1;
    directory.group(last, last_member, Some("partner@corp.example"));
    let (decision, again) = asks_of(runtime.clone(), directory.clone(), derived.clone()).await;
    assert_eq!(decision, HookDecision::AllowCall { spawn: None });
    assert_eq!(again, asks, "the same eight asks, in the same order");
    ran(&runtime, publish(&derived)).await;
}

/// A proposal that owes a ninth consult batch refuses as diverged after the eighth, and the
/// ninth is never asked: five staged rewrites, none of whose groups clears the call.
#[tokio::test]
async fn a_ninth_consult_batch_is_refused_without_being_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    for k in 1..=5 {
        directory.group(
            &format!("user-group/g{k}"),
            &format!("slack:U{k}"),
            Some("partner@corp.example"),
        );
    }
    let runtime = narrowed_under(&dir, &staged_groups_policy(5), &url).await;
    let derived = stage_group_rewrites(&runtime, 5).await;
    for k in 1..=5 {
        directory.group(
            &format!("user-group/g{k}"),
            &format!("slack:U{k}"),
            Some(&format!("other{k}@corp.example")),
        );
    }
    let asked = directory.requests().len();
    let decision = propose(&runtime, publish(&derived)).await;
    assert!(
        matches!(
            decision,
            HookDecision::Refuse {
                class: appa_runtime_api::RefusalClass::ResolutionDiverged,
                ..
            }
        ),
        "{decision:?}"
    );
    assert_eq!(
        directory.requests().len() - asked,
        8,
        "four selector reads and four lookups"
    );
}

/// A release judgment reads the roster at the judgment, never a remembered one, and fails
/// closed when the roster does not answer. It records nothing.
#[tokio::test]
async fn a_release_reads_membership_at_the_judgment_and_fails_closed_without_an_answer() {
    use appa_runtime_api::{OpenCalls, ReleaseRefusal};
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, source) = serve_source().await;
    let runtime = narrowed(&dir, &url).await;
    let answer = hooks::handle_embedded(
        &runtime,
        HookEvent::TurnEnd {
            actor: actor(),
            open_calls: OpenCalls::Abandon,
            answer: Some("the summary".to_string()),
        },
    )
    .await
    .receipt
    .expect("a recorded answer carries its receipt");
    let before = audit_len(&runtime);
    let asked = source.requests().len();

    source.members(Some(vec!["alice@corp.example"]));
    let to_team = raw(serde_json::json!({ "to": "@team" }));
    assert_eq!(
        runtime
            .judge_release(std::slice::from_ref(&answer), "send", &to_team)
            .await,
        Ok(())
    );
    assert_eq!(source.requests().len(), asked + 1, "the roster is read at the judgment");

    source.members(Some(vec!["alice@corp.example", "eve@corp.example"]));
    assert_eq!(
        runtime
            .judge_release(std::slice::from_ref(&answer), "send", &to_team)
            .await,
        Err(ReleaseRefusal::HardBlock),
        "a roster that grew since is read again, not remembered"
    );

    source.members(None);
    let unanswered = runtime
        .judge_release(std::slice::from_ref(&answer), "send", &to_team)
        .await;
    assert!(
        matches!(unanswered, Err(ReleaseRefusal::MissingFact(_))),
        "{unanswered:?}"
    );
    assert_eq!(audit_len(&runtime), before);
}

/// `send` under `groups` stock `approve` authorities, each permitting the readers of its own
/// directory group, in declaration order `via-g1`, `via-g2`, ….
fn covered_by_groups_policy(groups: u32) -> String {
    let each = |template: &str| -> String { (1..=groups).map(|k| template.replace("{k}", &k.to_string())).collect() };
    let named = each("\n[policy.audience.group.g{k}]\nfrom = [\"slack:user-group/g{k}\"]\n");
    let authorities = each(
        "\n[[policy.authority]]\nname = \"via-g{k}\"\n[policy.authority.permits]\naudience_missing = [\"@g{k}\"]\n",
    );
    let bound = each("\n[externals.authorities.via-g{k}]\nbuiltin = \"approve\"\n");
    format!(
        r#"
[policy]
version = 2
{named}
[[policy.tool]]
name = "read_hr"
delta = {{ audience = ["alice@corp.example", "bob@corp.example"] }}

[[policy.tool]]
name = "send"
parameters = {{ type = "object", properties = {{ to = {{ type = "string" }} }}, required = ["to"] }}
requires = {{ audience = {{ contains = ["$to"] }} }}
delta = {{}}
{authorities}
[externals]
timeout_ms = 1000
max_body_bytes = 4096
{bound}
[externals.audience.slack]
url = "AUDIENCE_URL"
selectors = [{{ template = "user-group/<handle>" }}]
"#
    )
}

/// The root's answer, its audience narrowed to Alice and Bob, judged into `send` to Eve.
async fn answer_sent_to_eve(
    dir: &tempfile::TempDir,
    groups: u32,
    url: &str,
) -> Result<(), appa_runtime_api::ReleaseRefusal> {
    let runtime = narrowed_under(dir, &covered_by_groups_policy(groups), url).await;
    let answer = hooks::handle_embedded(
        &runtime,
        HookEvent::TurnEnd {
            actor: actor(),
            open_calls: appa_runtime_api::OpenCalls::Abandon,
            answer: Some("the summary".to_string()),
        },
    )
    .await
    .receipt
    .expect("a recorded answer carries its receipt");
    runtime
        .judge_release(
            std::slice::from_ref(&answer),
            "send",
            &raw(serde_json::json!({ "to": "eve@corp.example" })),
        )
        .await
}

fn selectors_read(directory: &Directory) -> Vec<String> {
    directory
        .requests()
        .iter()
        .filter_map(|request| request["artifact"]["selector"].as_str().map(str::to_string))
        .collect()
}

/// A cover whose roster does not answer leaves the next independent cover to be read: the
/// release is judged on the cover that answers.
#[tokio::test]
async fn a_release_whose_first_cover_does_not_answer_is_judged_on_the_next() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    directory.group("user-group/g2", "eve@corp.example", None);
    assert_eq!(answer_sent_to_eve(&dir, 2, &url).await, Ok(()));
    assert_eq!(selectors_read(&directory), ["user-group/g1", "user-group/g2"]);
}

/// Eight covers read one roster a batch, the eighth the one that holds the recipient: the
/// judgment after the eighth batch reads its answer.
#[tokio::test]
async fn a_release_is_judged_on_the_answer_of_its_eighth_consult_batch() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, directory) = serve_directory().await;
    for k in 1..=7 {
        directory.group(&format!("user-group/g{k}"), "zed@corp.example", None);
    }
    directory.group("user-group/g8", "eve@corp.example", None);
    assert_eq!(answer_sent_to_eve(&dir, 8, &url).await, Ok(()));
    assert_eq!(
        selectors_read(&directory),
        (1..=8).map(|k| format!("user-group/g{k}")).collect::<Vec<_>>()
    );
}
