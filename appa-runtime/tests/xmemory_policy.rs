//! xmemory battery: internal reads that keep their trust, trusted writes, admin and
//! schema changes, reviewed schema migrations, instance deletion, membership edits, and
//! API key creation.
use crate::common;

use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{actor, battery_runtime, offer_of, propose, ran, raw, root, serve};
use std::sync::Arc;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/xmemory/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn structured_write(tool: &str) -> ProposedCall {
    call(
        tool,
        serde_json::json!({
            "text": "",
            "structured_mutations": [{
                "object_mutation": { "object_type": "Person", "delete": { "key": { "name": "Bob" } } }
            }],
        }),
    )
}

/// Runs `call`, accepting the label change it offers when it first narrows the trajectory.
/// The offer must not ask a reviewer: this helper runs only what needs no review.
async fn accept_and_run(runtime: &Arc<Runtime>, call: ProposedCall) {
    let decision = propose(runtime, call.clone()).await;
    if let HookDecision::DenyCall { feedback, .. } = &decision {
        assert!(!feedback.contains("approval"), "{}: {feedback}", call.tool);
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
    }
    ran(runtime, call).await;
}

/// A loopback audience source: the session acts for Alice, and the organisation is
/// Alice and Bob, so `self` is narrower than `internal`.
async fn people_source() -> String {
    let router = axum::Router::new().route(
        "/audience",
        axum::routing::post(|body: String| async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let members = match request["artifact"]["selector"].as_str().unwrap() {
                "viewer" => vec!["alice@corp.example"],
                "members" => vec!["alice@corp.example", "bob@corp.example"],
                other => panic!("unexpected selector {other}"),
            };
            serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string()
        }),
    );
    format!("{}/audience", serve(router).await)
}

async fn runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let source = people_source().await;
    battery_runtime(
        dir.path(),
        "xmemory",
        &format!(
            r#"[policy]
version = 2

[policy.audience]
self = ["people:viewer"]
internal = ["people:members"]

[[policy.tool]]
name = "mcp/web/fetch"
delta = {{ trust = "suspicious" }}

[[policy.authority]]
name = "xmemory-operator"
permits = {{ trust_below = "trusted", attention = ["xmemory-review"] }}

[externals]
timeout_ms = 30000
review_timeout_ms = 600000
max_body_bytes = 1048576

[externals.authorities.xmemory-operator]
# Approves when asked. The stock "approve" is applied at the block, which this test reads.
command = ["/bin/sh", "-c", 'cat > /dev/null; printf "%s" "{{\"version\":1,\"answer\":{{\"ruling\":\"approve\"}}}}"']

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "viewer", feeds = "self" }}, {{ template = "members", feeds = "internal" }}]
"#
        ),
    )
    .await
}

/// A read keeps the trajectory's trust, since memory is written only from trusted data:
/// writes, schema decisions, and metadata changes run after it, and so they do after the
/// context pack and the admin reads of memberships, schemas, keys, and roles. Once outside
/// text lowers the trust, none of them runs.
#[tokio::test]
async fn a_read_keeps_trust_and_suspicious_data_cannot_enter_memory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call("read", serde_json::json!({ "query": "Who owns the Q3 plan?" })),
    )
    .await;
    for read in [
        call("get_context_pack", serde_json::json!({})),
        call(
            "admin_list_instance_memberships",
            serde_json::json!({ "instance_id": "1" }),
        ),
        call(
            "admin_validate_yaml_schema",
            serde_json::json!({ "schema_yaml": "xmd_version: v1\nobjects: {}\nrelations: {}\n" }),
        ),
        call(
            "admin_convert_json_schema",
            serde_json::json!({ "json_schema": { "type": "object", "properties": {} } }),
        ),
        call("admin_list_api_keys", serde_json::json!({})),
        call("admin_get_api_key", serde_json::json!({ "key": "ci" })),
        call("admin_list_mintable_roles", serde_json::json!({})),
    ] {
        accept_and_run(&runtime, read).await;
    }

    let changes = || {
        [
            call("write", serde_json::json!({ "text": "Bob reviews the Q3 plan." })),
            call("write_async", serde_json::json!({ "text": "Bob reviews the Q3 plan." })),
            structured_write("write"),
            structured_write("write_async"),
            call(
                "decide_suggestions",
                serde_json::json!({ "proposal_version": "v1", "decisions": [] }),
            ),
            call(
                "admin_patch_instance_metadata_by_id",
                serde_json::json!({ "instance_id": "1", "agent_owner_instructions": "Review weekly." }),
            ),
        ]
    };
    for change in changes() {
        assert_eq!(
            propose(&runtime, change.clone()).await,
            HookDecision::AllowCall { spawn: None },
            "{}",
            change.tool
        );
        ran(&runtime, change).await;
    }

    accept_and_run(
        &runtime,
        ProposedCall {
            tool: "mcp/web/fetch".into(),
            arguments: raw(serde_json::json!({ "url": "https://example.com" })),
            cwd: None,
        },
    )
    .await;
    for change in changes() {
        assert!(
            !matches!(propose(&runtime, change.clone()).await, HookDecision::AllowCall { .. }),
            "{}",
            change.tool
        );
    }
}

/// From trusted data, a schema decision runs without review and keeps the trajectory
/// trusted; writes and creating an instance run and record their effects; a schema
/// migration, an instance deletion, and a membership edit wait for the reviewer.
#[tokio::test]
async fn writes_record_their_effects_and_admin_and_schema_changes_ask_the_authority() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    accept_and_run(
        &runtime,
        call(
            "decide_suggestions",
            serde_json::json!({ "proposal_version": "v1", "decisions": [] }),
        ),
    )
    .await;
    accept_and_run(&runtime, structured_write("write")).await;
    accept_and_run(
        &runtime,
        call("write", serde_json::json!({ "text": "Alice owns the Q3 plan." })),
    )
    .await;
    accept_and_run(
        &runtime,
        call(
            "admin_create_instance",
            serde_json::json!({
                "cluster_id": "c1",
                "name": "team-memory",
                "schema_yaml": "xmd_version: v1\nobjects:\n  Person:\n    fields:\n      name:\n        type: str\n        required: true\n    primary_key:\n    - name\nrelations: {}\n",
            }),
        ),
    )
    .await;

    for reviewed in [
        call(
            "update_instance_schema",
            serde_json::json!({ "schema_yml": "objects: {}" }),
        ),
        call("admin_delete_instance_by_id", serde_json::json!({ "instance_id": "1" })),
        call(
            "admin_edit_instance_memberships",
            serde_json::json!({
                "expected_revision": 3,
                "additions": [{ "instance_id": "1", "cluster_id": "c2" }],
            }),
        ),
    ] {
        let decision = propose(&runtime, reviewed.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{}", reviewed.tool);
        assert!(matches!(
            runtime.execute_remedy(&actor(), offer_of(&decision)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, reviewed.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, reviewed).await;
    }

    let effects: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/xmemory/") => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(
        effects,
        vec![
            vec!["xmemory.changed"],
            vec!["xmemory.changed"],
            vec!["xmemory.changed"],
            vec!["xmemory.admin"],
            vec!["xmemory.schema"],
            vec!["xmemory.deleted"],
            vec!["xmemory.admin"]
        ]
    );
}

/// A new API key's secret is a credential for the caller alone. Creating one waits for
/// the reviewer and records `xmemory.admin`; its result is `self`, so a memory write after
/// it, whose input `internal` must see, is refused with no remedy: this authority cannot
/// widen an audience.
#[tokio::test]
async fn a_new_api_key_is_reviewed_and_its_secret_stays_out_of_memory() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(&dir).await;

    let create = call(
        "admin_create_api_key",
        serde_json::json!({ "name": "ci", "role": "Data writer", "cluster": "c1" }),
    );
    let decision = propose(&runtime, create.clone()).await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{decision:?}");
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, create.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, create).await;

    let write = call("write", serde_json::json!({ "text": "The CI key belongs to Alice." }));
    let HookDecision::DenyCall { feedback, offers, .. } = propose(&runtime, write).await else {
        panic!("a write after a new key must be denied");
    };
    assert!(
        feedback.contains(r#"the readers do not include "internal""#),
        "{feedback}"
    );
    assert!(offers.is_empty(), "{offers:?}");
}
