//! Real Linear command annotator and shipped policies, with deterministic tool outcomes.
mod common;

use appa_runtime::{
    api::{RemedyOutcome, Runtime},
    config::Config,
    hooks,
};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use common::{actor, offer_of, propose, ran, raw, repo_root, root};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/linear/{tool}"),
        arguments: raw(args),
    }
}

async fn runtime(dir: &tempfile::TempDir, profile: &str) -> Arc<Runtime> {
    runtime_with(dir, profile, "").await
}

async fn runtime_with(dir: &tempfile::TempDir, profile: &str, extra: &str) -> Arc<Runtime> {
    let policy = if profile == "approved-writes" {
        "appa.toml".to_string()
    } else {
        format!("{profile}.toml")
    };
    let local = dir.path().join("linear");
    std::fs::create_dir(&local).unwrap();
    for entry in std::fs::read_dir(repo_root().join("marketplace/batteries/linear")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), local.join(entry.file_name())).unwrap();
        }
    }
    std::fs::copy(
        repo_root().join("marketplace/batteries/github/appa.toml"),
        dir.path().join("github.toml"),
    )
    .unwrap();
    let include = format!("linear/{policy}");
    let github = "github.toml";
    let hint = serde_json::json!({"rules":{
        "get_issue":[{"match":{"id":"ENG-1"},"audience":["alice@corp.example"]}],
        "save_comment":[{"match":{"issueId":"ENG-1"},"audience":["alice@corp.example"],"production":true}],
        "save_issue":[{"match":{"id":"ENG-1"},"audience":["alice@corp.example"]}]
    }})
    .to_string();
    let text = format!(
        r#"include = [{include:?}, {github:?}]
[policy]
version = 2
[[policy.annotator]]
name = "linear.{profile}"
hint = {hint:?}
audiences = ["alice@corp.example"]
marks = ["linear-review"]
effects = ["linear.changed", "linear.sensitive"]
[externals]
timeout_ms = 5000
max_body_bytes = 1048576
{extra}
"#,
        include = include,
        github = github
    );
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, text).unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

#[tokio::test]
async fn a_real_linear_read_taints_and_cannot_publish_to_github() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir, "approved-writes").await;
    let read = call("get_issue", serde_json::json!({"id":"ENG-1"}));
    let offered = propose(&runtime, read.clone()).await;
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&offered)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;
    let write = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(
            serde_json::json!({"owner":"example","repo":"public","title":"leak","body":"restricted Linear content"}),
        ),
    };
    assert!(!matches!(
        propose(&runtime, write).await,
        HookDecision::AllowCall { .. }
    ));
}

#[tokio::test]
async fn profiles_refuse_mutations_without_required_review_or_production_permission() {
    for profile in ["read-only", "approved-writes", "production-lockdown"] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir, profile).await;
        let write = call("save_issue", serde_json::json!({"id":"ENG-1","title":"update"}));
        let decision = propose(&runtime, write).await;
        assert!(
            !matches!(decision, HookDecision::AllowCall { .. }),
            "{profile}: {decision:?}"
        );
    }
}

#[tokio::test]
async fn team_profile_allows_a_mapped_routine_write_but_not_reparenting() {
    let dir = tempfile::tempdir().unwrap();
    // Stand-in for a trust review, deliberately unable to grant linear-review.
    let runtime = runtime_with(
        &dir,
        "team-use",
        r#"
[[policy.authority]]
name = "trust-review"
permits = { trust_below = "trusted" }
[externals.authorities.trust-review]
builtin = "approve"
"#,
    )
    .await;
    let changed_scope = call(
        "save_issue",
        serde_json::json!({"id":"ENG-1","title":"x","team":"outside"}),
    );
    assert!(!matches!(
        propose(&runtime, changed_scope).await,
        HookDecision::AllowCall { .. }
    ));
    let write = call("save_issue", serde_json::json!({"id":"ENG-1","title":"update"}));
    let decision = propose(&runtime, write.clone()).await;
    assert!(
        matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ),
        "{decision:?}"
    );
    assert_eq!(
        propose(&runtime, write.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, write).await;
}

#[test]
fn native_host_names_resolve_to_the_same_linear_contracts() {
    let tools: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("marketplace/batteries/linear/schema-lock.json")).unwrap(),
    )
    .unwrap();
    for name in tools["surfaces"]["read-write"]["tools"].as_object().unwrap().keys() {
        let claude = (appa_adapter_claude_code::adapter().derive)(&format!("mcp__linear__{name}")).unwrap();
        let kagent = (appa_adapter_kagent::adapter().derive)(&format!("mcp:linear/{name}")).unwrap();
        assert_eq!(claude.canonical, kagent.canonical);
    }
}

#[tokio::test]
async fn human_review_is_exactly_scoped_and_records_the_write_effect() {
    use appa_runtime::api::AuditEvent;
    use appa_runtime_api::Ruling;
    for ruling in [Ruling::Approve, Ruling::Deny] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with(
            &dir,
            "approved-writes",
            r#"
[[policy.authority]]
name = "operator"
permits = { trust_below = "trusted", attention = ["linear-review"] }
[externals.authorities.operator]
builtin = "hitl"
"#,
        )
        .await;
        let write = call(
            "save_comment",
            serde_json::json!({"issueId":"ENG-1","body":"reviewed text"}),
        );
        let offer = offer_of(&propose(&runtime, write.clone()).await);
        // The native kagent confirmation hook supplies the person's ruling.
        let control = ProposedCall {
            tool: appa_runtime_api::CONTROL_TOOL.into(),
            arguments: raw(serde_json::json!({"offer_id":offer.0.clone()})),
        };
        let decision = hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: actor(),
                call: control,
                spawn: false,
                ruling: Some(ruling),
            },
        )
        .await;
        assert_eq!(decision, HookDecision::PassControl);
        use rmcp::ServiceExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = axum::Router::new().nest_service(
            "/mcp",
            appa_runtime::mcp::service(runtime.clone(), appa_runtime::runtime_cli::Adapter::Kagent),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = ().serve(rmcp::transport::StreamableHttpClientTransport::from_uri(url)).await.unwrap();
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = "execute_remedy_plan".into();
        params.arguments = serde_json::json!({"offer_id":offer.0}).as_object().cloned();
        let result = format!("{:?}", client.call_tool(params).await.unwrap().content);
        client.cancel().await.unwrap();
        server.abort();
        if ruling == Ruling::Approve {
            assert!(result.contains("Authorized"), "{result}");
            let changed = call(
                "save_comment",
                serde_json::json!({"issueId":"ENG-1","body":"unreviewed replacement"}),
            );
            assert!(!matches!(
                propose(&runtime, changed).await,
                HookDecision::AllowCall { .. }
            ));
            assert_eq!(
                propose(&runtime, write.clone()).await,
                HookDecision::AllowCall { spawn: None }
            );
            ran(&runtime, write.clone()).await;
            assert!(
                !matches!(propose(&runtime, write).await, HookDecision::AllowCall { .. }),
                "a consumed approval cannot authorize another mutation"
            );
        } else {
            assert!(!result.contains("Authorized"), "{result}");
        }
        let effects: Vec<_> = runtime
            .audit(&root())
            .unwrap()
            .into_iter()
            .filter_map(|entry| match entry.event {
                AuditEvent::Released { tool, effects, .. } if tool == "mcp/linear/save_comment" => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects.len(), usize::from(ruling == Ruling::Approve));
        if !effects.is_empty() {
            assert_eq!(effects[0], vec!["linear.changed"]);
        }
    }
}

#[tokio::test]
async fn configured_server_alias_works_and_an_unbound_server_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _runtime = runtime_with(&dir, "approved-writes", "[server_aliases]\nlinear = 'work-linear'\n").await;
    let served = common::serve_runtime(&dir.path().join("appa.toml"), &dir.path().join("served.db"));
    let hook = |event: serde_json::Value| {
        common::http(&format!("{}/hook", served.url), "POST", Some(&event.to_string())).unwrap()
    };
    hook(serde_json::json!({"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"alias-test"}));
    let event = |server: &str| {
        serde_json::json!({"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"alias-test",
        "tool":format!("mcp__{server}__get_issue"),"arguments":{"id":"ENG-1"},"spawn":false})
    };
    let decision = hook(event("work-linear"));
    assert!(
        decision.contains("Accept this change"),
        "alias must reach the actual helper: {decision}"
    );
    assert!(
        common::http(
            &format!("{}/hook", served.url),
            "POST",
            Some(&event("other-linear").to_string())
        )
        .is_none(),
        "unbound namespace is refused with a non-success HTTP response"
    );
}

#[test]
fn shipped_root_examples_load_with_their_actual_helpers() {
    for profile in ["read-only", "team-use", "approved-writes", "production-lockdown"] {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_root().join(format!("examples/linear-battery/{profile}.toml"));
        Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap();
    }
}

#[tokio::test]
async fn restricted_linear_content_can_reach_a_github_destination_with_the_same_readers() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime_with(
        &dir,
        "approved-writes",
        r#"
[[policy.tool]]
name = "mcp/github/issue_write(repo:private)"
delta = {}
requires = { trust = "trusted", audience = { contains = ["alice@corp.example"] } }
[[policy.authority]]
name = "trust-review"
permits = { trust_below = "trusted" }
[externals.authorities.trust-review]
builtin = "approve"
"#,
    )
    .await;
    let read = call("get_issue", serde_json::json!({"id":"ENG-1"}));
    let offer = offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;
    let write = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(serde_json::json!({
        "owner":"example","repo":"private","title":"Reviewed summary","body":"Restricted fixture content"})),
    };
    let offer = offer_of(&propose(&runtime, write.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, write.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, write).await;
    let public = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(serde_json::json!({
        "owner":"example","repo":"public","title":"Leak","body":"Restricted fixture content"})),
    };
    assert!(!matches!(
        propose(&runtime, public).await,
        HookDecision::AllowCall { .. }
    ));
}

#[tokio::test]
async fn an_explicit_email_only_fixture_can_use_the_existing_input_sanitizer() {
    let dir = tempfile::tempdir().unwrap();
    // This fixture's only sensitive payload is the email below. This permit is
    // deliberately absent from the shipped examples: it cannot declassify real
    // issue prose, nor is an email redactor a general trust validator.
    let runtime = runtime_with(
        &dir,
        "approved-writes",
        r#"
[[policy.tool]]
name = "mcp/github/issue_write"
parameters = { type = "object", properties = { owner = { type = "string" }, repo = { type = "string" }, title = { type = "string" }, body = { type = "string" } }, required = ["owner", "repo", "title", "body"] }
delta = {}
requires = { trust = "suspicious", audience = { contains = ["public"] } }
[[policy.sanitizer]]
name = "fixture-email-only"
on = ["tool_input"]
permits = { audience = { from = ["alice@corp.example"], to = ["public"] } }
[externals.sanitizers.fixture-email-only]
builtin = "redact-email"
"#,
    )
    .await;
    let read = call("get_issue", serde_json::json!({"id":"ENG-1"}));
    let offer = offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;
    let write = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(serde_json::json!({
        "owner":"example","repo":"public","title":"Contact","body":"mail alice@corp.example today"})),
    };
    let decision = propose(&runtime, write.clone()).await;
    let result = runtime.execute_remedy(&actor(), offer_of(&decision)).await;
    let RemedyOutcome::Substituted { call: replacement } = result else {
        panic!("expected an actual sanitizer substitution: {result:?}; {decision:?}");
    };
    let args: serde_json::Value = serde_json::from_str(replacement.arguments.get()).unwrap();
    assert_eq!(args["body"], "mail [redacted-email] today");
    assert_eq!(
        propose(&runtime, replacement.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, replacement).await;
    assert!(!matches!(
        propose(&runtime, write).await,
        HookDecision::AllowCall { .. }
    ));
}
