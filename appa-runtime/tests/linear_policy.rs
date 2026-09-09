//! Battery integration: shipped examples, write requirements/effects and cross-provider confidentiality.
mod common;

use appa_runtime::{
    api::{AuditEvent, RemedyOutcome, Runtime},
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

async fn runtime(dir: &tempfile::TempDir, extra: &str) -> Arc<Runtime> {
    for battery in ["linear", "github"] {
        let source = repo_root().join("marketplace/batteries").join(battery);
        let target = dir.path().join("marketplace/batteries").join(battery);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::copy(source.join("appa.toml"), target.join("appa.toml")).unwrap();
    }
    // Use the documented configuration; substitute a deterministic review authority.
    let text = std::fs::read_to_string(repo_root().join("examples/linear-battery/appa.toml"))
        .unwrap()
        .replace("../../marketplace/", "marketplace/")
        .replace("builtin = \"hitl\"", "builtin = \"approve\"");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, format!("{text}\n{extra}")).unwrap();
    let runtime = Arc::new(Runtime::open(Config::load(&path).unwrap(), dir.path().join("runtime.db"), None).unwrap());
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

#[tokio::test]
async fn linear_write_requires_review_and_records_its_effect() {
    for (tool, args) in [
        (
            "save_comment",
            serde_json::json!({"issueId":"ENG-1","body":"reviewed text"}),
        ),
        ("save_issue", serde_json::json!({"id":"ENG-2","title":"reviewed text"})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir, "").await;
        let write = call(tool, args);
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }));
        let outcome = runtime.execute_remedy(&actor(), offer_of(&decision)).await;
        if tool == "save_issue" {
            // The unscoped default requires internal sources, absent in this example.
            assert!(matches!(outcome, RemedyOutcome::NoAnswer { .. }));
            continue;
        }
        assert!(matches!(outcome, RemedyOutcome::Authorized { .. }), "{outcome:?}");
        assert_eq!(
            propose(&runtime, write.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, write).await;
        let effects: Vec<_> = runtime
            .audit(&root())
            .unwrap()
            .into_iter()
            .filter_map(|entry| match entry.event {
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/linear/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec!["linear.changed"]]);
    }
}

#[tokio::test]
async fn restricted_linear_content_can_reach_a_github_destination_with_the_same_readers() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(
        &dir,
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
