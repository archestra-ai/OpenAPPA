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

/// How long a freshly written fixture script may take to start and act. Under a loaded
/// parallel suite its cold execs take seconds on macOS; this bounds a hang, not the
/// latency under test. Recording a pid and awaiting its end together stay under the
/// fixtures' `sleep 30`, so a descendant cannot pass by exiting on its own.
#[cfg(unix)]
pub(crate) const PROCESS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Poll `probe` every 10ms until it yields a value or `deadline` passes.
#[cfg(unix)]
pub(crate) async fn wait_until<T>(deadline: tokio::time::Instant, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    while tokio::time::Instant::now() < deadline {
        if let Some(value) = probe() {
            return Some(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    None
}

/// The pid a fixture wrote to `path`, once it has.
#[cfg(unix)]
pub(crate) async fn recorded_pid(path: &std::path::Path) -> i32 {
    wait_until(tokio::time::Instant::now() + PROCESS_BUDGET, || {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    })
    .await
    .expect("the fixture did not record its descendant pid")
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Wait out [`PROCESS_BUDGET`] for `pid` to be gone.
#[cfg(unix)]
pub(crate) async fn assert_process_gone(pid: i32) {
    wait_until(tokio::time::Instant::now() + PROCESS_BUDGET, || {
        (!process_exists(pid)).then_some(())
    })
    .await
    .unwrap_or_else(|| panic!("descendant {pid} survived process-group cleanup"));
}
