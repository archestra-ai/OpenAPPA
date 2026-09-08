//! Fixtures every integration test binary that declares `mod common;`
//! compiles into itself. Only what several suites need verbatim lives
//! here; a stub's own routes and state stay with the suite that reads
//! them.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use appa_runtime::api::{OfferId, Runtime};
use appa_runtime::hooks;
use appa_runtime_api::{
    Actor, AdapterName, HookDecision, HookEvent, OutcomeBody, ParseRefusal, ProposedCall, ToolOutcome, TrajectoryId,
    WireDecision, WireEvent,
};

/// The Claude Code hook JSON a suite is written in, as the served runtime
/// reads it: translated onto the wire by the client-side codec (as `appa
/// hook --adapter claude-code` does) and derived back by the served
/// adapter, so the event's tool is the canonical one. `None` for a hook the
/// codec does not gate.
pub fn claude_event(hook_json: &serde_json::Value) -> Option<HookEvent> {
    let body = serde_json::to_vec(hook_json).expect("the event serializes");
    let event = (appa_adapter_claude_code::codec().parse)(&body).expect("the Claude Code event parses")?;
    let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("the event translates");
    let accepted = wire
        .into_event(&appa_adapter_claude_code::adapter())
        .expect("the wire event derives")
        .expect("a translated event is no ping");
    Some(accepted.event)
}

/// One Claude Code hook through the served `/hook` dispatcher, wire to wire,
/// with the wire decision rendered back into Claude Code's hook answer, as
/// `appa hook` prints it. A refusal before any event
/// exists comes back as the runtime's `{"error": …}` body.
pub async fn claude_hook(runtime: &Runtime, hook_json: &serde_json::Value) -> (u16, serde_json::Value) {
    let codec = appa_adapter_claude_code::codec();
    let body = serde_json::to_vec(hook_json).expect("the event serializes");
    let event = match (codec.parse)(&body) {
        Ok(Some(event)) => event,
        Ok(None) => return (200, serde_json::json!({})),
        Err(ParseRefusal::Unreadable { detail }) => return (400, serde_json::json!({ "error": detail })),
        Err(ParseRefusal::Malformed { detail }) => return (409, serde_json::json!({ "error": detail })),
    };
    let wire = WireEvent::from_event(AdapterName::ClaudeCode, &event).expect("the event translates");
    let wire = serde_json::to_vec(&wire).expect("the wire event serializes");
    let (status, answer) = hooks::answer(runtime, &appa_adapter_claude_code::adapter(), &wire).await;
    match serde_json::from_value::<WireDecision>(answer.clone()).map(WireDecision::into_decision) {
        Ok(Ok(decision)) => (status, (codec.render)(&event, &decision)),
        _ => (status, answer),
    }
}

/// A served `appa runtime` process on a free loopback port, killed on drop.
pub struct ServedRuntime {
    child: Child,
    pub url: String,
}

impl Drop for ServedRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port binds");
    let port = listener.local_addr().expect("the bound address is readable").port();
    drop(listener);
    port
}

/// Start the built binary as `appa runtime` over `config` and `db`, and wait until
/// `/health` answers.
pub fn serve_runtime(config: &Path, db: &Path) -> ServedRuntime {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
        .arg("--config")
        .arg(config)
        .arg("--db")
        .arg(db)
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary spawns");
    let mut served = ServedRuntime {
        child,
        url: format!("http://127.0.0.1:{port}"),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(status) = served.child.try_wait().expect("the child polls") {
            panic!("the runtime exited before becoming healthy: {status}");
        }
        if http(&format!("{}/health", served.url), "GET", None).is_some() {
            return served;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the runtime never became healthy within the deadline");
}

/// One plain HTTP request; the body on a 2xx answer, `None` otherwise.
pub fn http(url: &str, method: &str, body: Option<&str>) -> Option<String> {
    use std::io::{Read, Write};
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}")))?;
    let mut stream = std::net::TcpStream::connect(host).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("the read timeout sets");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, payload) = response.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 2").then(|| payload.to_string())
}

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
