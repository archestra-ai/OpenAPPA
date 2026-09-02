//! Fixtures every integration test binary that declares `mod common;`
//! compiles into itself. Only what several suites need verbatim lives
//! here; a stub's own routes and state stay with the suite that reads
//! them.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use appa_runtime::api::{OfferId, Runtime};
use appa_runtime::hooks;
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

/// Fixture arguments, from a `json!` value to the bytes a harness would
/// have sent. The adapter holds the harness's bytes already, so
/// production never takes this direction.
pub fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}

/// The repository root, for the suites that read the shipped
/// integration files rather than a fixture of their own.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the runtime crate sits one level below the repo root")
        .to_path_buf()
}

/// The marketplace root a developer passes to `--plugin-source`, staged into
/// `into` by the same mapping the build and init use.
pub fn stage_bundle(into: &Path) -> PathBuf {
    let staged = into.join("plugin-source");
    appa_runtime::plugin_bundle::stage_repository(&repo_root(), &staged).expect("the checkout stages");
    staged
}

/// Every offer a feedback body names, in the order the feedback lists
/// them. Which end a suite takes is its own assertion: a remedy plan
/// that stages several offers surfaces one line each.
pub fn offers(feedback: &str) -> Vec<OfferId> {
    feedback
        .lines()
        .filter_map(|line| {
            let after = line.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            Some(OfferId(rest[..rest.find('"')?].to_string()))
        })
        .collect()
}

/// The last offer a feedback body names.
pub fn last_offer(feedback: &str) -> OfferId {
    offers(feedback)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no offer id in feedback: {feedback}"))
}

/// The last offer a denial's feedback names.
pub fn offer_of(decision: &HookDecision) -> OfferId {
    let HookDecision::DenyCall { feedback, .. } = decision else {
        panic!("expected a deny carrying feedback, got {decision:?}")
    };
    last_offer(feedback)
}

/// The one root trajectory a suite's session runs as. Every suite opens
/// its own database, so the id needs no suite-specific spelling.
pub fn root() -> TrajectoryId {
    TrajectoryId("test-root".to_string())
}

pub fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

/// The root actor proposes one call.
pub async fn propose(runtime: &Arc<Runtime>, call: ProposedCall) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call,
            spawn: false,
            ruling: None,
        },
    )
    .await
}

/// The root actor reports one call as run with a plain body, and the
/// runtime acknowledges it.
pub async fn ran(runtime: &Arc<Runtime>, call: ProposedCall) {
    assert_eq!(
        hooks::handle(
            runtime,
            HookEvent::ToolResult {
                actor: actor(),
                call,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available("done".to_string()),
                },
            },
        )
        .await,
        HookDecision::Ack
    );
}

pub fn audit_len(runtime: &Runtime) -> usize {
    runtime.audit(&root()).expect("the audit reads").len()
}

/// Serve one router on an ephemeral loopback port for the rest of the
/// test, and answer with its base URL.
pub async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("the stub serves");
    });
    format!("http://{addr}")
}
