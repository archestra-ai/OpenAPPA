//! Microsoft Learn battery: public documentation reads, untrusted results, no write.
mod common;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, members_source, offer_of, propose, ran, raw};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/microsoft-learn/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn other(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(args),
        cwd: None,
    }
}

/// The battery under a root that adds three tools of its own: the battery declares no
/// write and narrows no audience, so its label is only observable against a tool that
/// needs trusted data, one that needs a public audience, and one that restricts the
/// trajectory to `internal`.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let source = members_source().await;
    battery_runtime(
        dir.path(),
        "microsoft-learn",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
internal = ["people:members"]

[[policy.tool]]
name = "mcp/shell/run_command"
requires = {{ trust = "trusted" }}
delta = {{}}

[[policy.tool]]
name = "mcp/mail/send"
requires = {{ audience = {{ contains = ["public"] }} }}
delta = {{}}

[[policy.tool]]
name = "mcp/crm/get_ticket"
delta = {{ audience = ["internal"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .await
}

/// Accept one call's restriction for the rest of the session, then run it.
async fn accept_and_run(runtime: &Arc<Runtime>, call: ProposedCall) {
    let decision = propose(runtime, call.clone()).await;
    assert!(
        !matches!(decision, HookDecision::AllowCall { .. }),
        "{} restricts the trajectory",
        call.tool
    );
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(runtime, call.clone()).await,
        HookDecision::AllowCall { spawn: None },
        "{}",
        call.tool
    );
    ran(runtime, call).await;
}

/// What the server returns is `suspicious`, so the first read lowers the session's trust
/// and a tool needing trusted data is refused afterwards. It stays public, so the other
/// two reads run outright and a public destination still receives the result.
#[tokio::test]
async fn a_documentation_read_is_suspicious_and_stays_public() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call(
            "microsoft_docs_search",
            serde_json::json!({ "query": "azure functions bindings" }),
        ),
    )
    .await;

    for read in [
        call(
            "microsoft_code_sample_search",
            serde_json::json!({ "query": "BlobClient upload", "language": "csharp" }),
        ),
        call(
            "microsoft_docs_fetch",
            serde_json::json!({ "url": "https://learn.microsoft.com/en-us/azure/azure-functions/" }),
        ),
        other("mcp/mail/send", serde_json::json!({ "to": "anyone@example.com" })),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }

    assert!(!matches!(
        propose(
            &runtime,
            other("mcp/shell/run_command", serde_json::json!({ "command": "deploy" }))
        )
        .await,
        HookDecision::AllowCall { .. }
    ));
}

/// A query leaves for a public Microsoft endpoint, so a trajectory that holds `internal`
/// data cannot search or fetch. Its trust has already fallen, so the public audience is
/// the only requirement left to refuse.
#[tokio::test]
async fn restricted_data_cannot_leave_in_a_query() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call(
            "microsoft_docs_search",
            serde_json::json!({ "query": "azure functions bindings" }),
        ),
    )
    .await;
    accept_and_run(&runtime, other("mcp/crm/get_ticket", serde_json::json!({ "id": "42" }))).await;

    for read in [
        call(
            "microsoft_docs_search",
            serde_json::json!({ "query": "azure functions bindings" }),
        ),
        call(
            "microsoft_code_sample_search",
            serde_json::json!({ "query": "BlobClient upload" }),
        ),
        call(
            "microsoft_docs_fetch",
            serde_json::json!({ "url": "https://learn.microsoft.com/en-us/azure/azure-functions/" }),
        ),
    ] {
        assert!(
            !matches!(propose(&runtime, read.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            read.tool
        );
    }
}
