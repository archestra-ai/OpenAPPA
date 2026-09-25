//! Fixtures the crate's own unit tests share. The integration suites under `tests/` keep
//! theirs in `tests/common`, which cannot reach a `pub(crate)` item.

use std::net::SocketAddr;

/// Serve `router` on an ephemeral loopback port for the rest of the test, and answer the
/// bound address.
pub(crate) async fn serve(router: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("the stub serves");
    });
    addr
}

/// A fake `claude` executable in `dir` running `script` in place of the model, for the
/// Claude Code backend's `command`.
#[cfg(unix)]
pub(crate) fn fake_claude(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("fake-claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("the fake claude writes");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("the fake claude is executable");
    path
}
