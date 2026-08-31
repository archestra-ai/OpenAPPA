//! Fixtures every integration test binary that declares `mod common;`
//! compiles into itself. Only what several suites need verbatim lives
//! here; a stub's own routes and state stay with the suite that reads
//! them.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

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
/// `into` by the same script the release runs.
pub fn stage_bundle(into: &Path) -> PathBuf {
    let staged = into.join("plugin-source");
    let status = std::process::Command::new("sh")
        .arg(repo_root().join("scripts/appa-stage-plugin-bundle.sh"))
        .arg(&staged)
        .status()
        .expect("the staging script runs");
    assert!(status.success(), "the staging script failed");
    staged
}

/// Every offer a feedback body names, in the order the feedback lists
/// them. Which end a suite takes is its own assertion: a remedy plan
/// that stages several offers surfaces one line each.
pub fn offers(feedback: &str) -> Vec<appa_runtime::api::OfferId> {
    feedback
        .lines()
        .filter_map(|line| {
            let after = line.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            Some(appa_runtime::api::OfferId(rest[..rest.find('"')?].to_string()))
        })
        .collect()
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
