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
