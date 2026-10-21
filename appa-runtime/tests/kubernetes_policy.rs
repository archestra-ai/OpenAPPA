//! Kubernetes battery: internal untrusted cluster reads, viewer-only credentials, every
//! write blocked.
mod common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall, RefusalClass};
use axum::{Router, routing::post};
use common::{actor, battery_runtime, offer_of, propose, ran, raw, root, serve};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/kubernetes/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn probe(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/probe/{tool}"),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

/// The battery under a root with a catch-all reviewer, so a write's denial shows the
/// battery's `blocked` mark rather than a missing authority.
async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let router = Router::new().route(
        "/audience",
        post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let members = if request["artifact"]["selector"] == "viewer" {
                vec!["alice@corp.example"]
            } else {
                vec!["alice@corp.example", "bob@corp.example"]
            };
            serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string()
        }),
    );
    let source = serve(router).await;
    battery_runtime(
        dir.path(),
        "kubernetes",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
self = ["people:viewer"]
internal = ["people:members"]

[[policy.tool]]
name = "mcp/probe/trusted"
delta = {{}}
requires = {{ trust = "trusted" }}

[[policy.tool]]
name = "mcp/probe/internal"
delta = {{}}
requires = {{ audience = {{ contains = ["internal"] }} }}

[[policy.authority]]
name = "operator"
[policy.authority.permits]
trust_below = "trusted"
attention = ["*"]

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.operator]
builtin = "approve"

[externals.audience.people]
url = "{source}/audience"
selectors = [{{ template = "viewer", feeds = "self" }}, {{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .await
}

async fn accept_read(runtime: &Arc<Runtime>, read: ProposedCall) {
    let decision = propose(runtime, read.clone()).await;
    if !matches!(decision, HookDecision::AllowCall { .. }) {
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
    }
    ran(runtime, read).await;
}

fn last_admitted_audience(runtime: &Runtime) -> String {
    runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label.audience),
            _ => None,
        })
        .next_back()
        .expect("a read admitted a value")
}

/// A pod log narrows the trajectory to internal and makes it suspicious; further cluster
/// reads then run outright, and what was read cannot reach a tool that needs trusted input.
#[tokio::test]
async fn a_cluster_read_is_internal_and_suspicious() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let log = call(
        "pods_log",
        serde_json::json!({ "namespace": "payments", "name": "api-7f9c", "tail": 100 }),
    );
    let offer = offer_of(&propose(&runtime, log.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, log.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, log).await;
    assert_eq!(last_admitted_audience(&runtime), "internal");

    for read in [
        call("events_list", serde_json::json!({ "namespace": "payments" })),
        call(
            "resources_get",
            serde_json::json!({ "apiVersion": "apps/v1", "kind": "Deployment", "namespace": "payments", "name": "api" }),
        ),
        call("helm_list", serde_json::json!({ "all_namespaces": true })),
    ] {
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            read.tool
        );
        ran(&runtime, read).await;
    }
    assert_eq!(last_admitted_audience(&runtime), "internal");

    assert_eq!(
        propose(&runtime, probe("internal")).await,
        HookDecision::AllowCall { spawn: None }
    );
    assert!(matches!(
        propose(&runtime, probe("trusted")).await,
        HookDecision::DenyCall { .. }
    ));
}

/// A Secret, a ConfigMap, or the kubeconfig narrows the trajectory to the viewer, so what
/// was read cannot reach an internal sink, a further cluster read among them.
#[tokio::test]
async fn credentials_stay_with_the_viewer() {
    for read in [
        call(
            "resources_get",
            serde_json::json!({ "apiVersion": "v1", "kind": "Secret", "namespace": "payments", "name": "db" }),
        ),
        call(
            "resources_list",
            serde_json::json!({ "apiVersion": "v1", "kind": "Secret", "namespace": "payments" }),
        ),
        call(
            "resources_get",
            serde_json::json!({ "apiVersion": "v1", "kind": "ConfigMap", "namespace": "payments", "name": "app" }),
        ),
        call(
            "resources_list",
            serde_json::json!({ "apiVersion": "v1", "kind": "ConfigMap" }),
        ),
        call("configuration_view", serde_json::json!({ "minified": false })),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir).await;
        accept_read(&runtime, read.clone()).await;
        assert_eq!(last_admitted_audience(&runtime), "self", "{}", read.tool);
        assert!(
            !matches!(
                propose(&runtime, probe("internal")).await,
                HookDecision::AllowCall { .. }
            ),
            "{}",
            read.tool
        );
        assert!(
            !matches!(
                propose(&runtime, call("pods_list", serde_json::json!({}))).await,
                HookDecision::AllowCall { .. }
            ),
            "{}",
            read.tool
        );
    }
}

/// Every write and every way to run code in the cluster is blocked: no remedy exists, even
/// under an authority that permits every mark, and even on a fresh trusted trajectory.
#[tokio::test]
async fn every_write_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    for write in [
        call(
            "resources_create_or_update",
            serde_json::json!({ "resource": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n" }),
        ),
        call(
            "resources_delete",
            serde_json::json!({ "apiVersion": "apps/v1", "kind": "Deployment", "namespace": "payments", "name": "api" }),
        ),
        call(
            "resources_scale",
            serde_json::json!({ "apiVersion": "apps/v1", "kind": "Deployment", "name": "api", "scale": 0 }),
        ),
        call("pods_delete", serde_json::json!({ "name": "api-7f9c" })),
        call(
            "pods_exec",
            serde_json::json!({ "name": "api-7f9c", "command": ["cat", "/etc/secret/token"] }),
        ),
        call("pods_run", serde_json::json!({ "image": "attacker/miner" })),
        call(
            "helm_install",
            serde_json::json!({ "chart": "oci://example.com/chart", "name": "x" }),
        ),
        call("helm_uninstall", serde_json::json!({ "name": "payments" })),
    ] {
        assert!(
            matches!(
                propose(&runtime, write.clone()).await,
                HookDecision::DenyCall { offers, .. } if offers.is_empty()
            ),
            "{}",
            write.tool
        );
    }
}

/// A tool from a toolset the battery does not name is blocked too.
#[tokio::test]
async fn an_unnamed_toolset_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;
    assert!(matches!(
        propose(
            &runtime,
            call("vm_lifecycle", serde_json::json!({ "name": "vm", "action": "stop" }))
        )
        .await,
        HookDecision::Refuse {
            class: RefusalClass::UndeclaredTool,
            ..
        }
    ));
}
