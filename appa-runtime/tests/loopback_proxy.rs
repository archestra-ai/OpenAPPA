//! A loopback endpoint's request must not leave this machine. `HTTP_PROXY` and its
//! siblings are process-wide and read when a client is built, so this suite owns a
//! test binary of its own: setting them here cannot disturb any other suite.

mod common;
use common::{raw, serve};

use std::sync::{Arc, Mutex};

use appa_runtime::api::Runtime;
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const TOKEN_VAR: &str = "APPA_LOOPBACK_PROXY_TEST_TOKEN";
const TOKEN: &str = "must-not-be-relayed";

/// The bearer tokens the loopback annotator was presented with.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

/// An annotator that answers only a request carrying the expected bearer token, and
/// records every token it is shown.
async fn serve_annotator() -> (String, Seen) {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/resolve",
            post(|State(seen): State<Seen>, headers: axum::http::HeaderMap| async move {
                let bearer = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .unwrap_or_default()
                    .to_string();
                seen.0.lock().unwrap().push(bearer.clone());
                match bearer == TOKEN {
                    true => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({
                            "version": 1,
                            "answer": {
                                "delta": { "trust": "trusted" },
                                "requires": { "history": [], "attention": [] },
                                "emits": [],
                            },
                        })
                        .to_string(),
                    ),
                    false => (axum::http::StatusCode::FORBIDDEN, "no token".to_string()),
                }
            }),
        )
        .with_state(seen.clone());
    (format!("{}/resolve", serve(router).await), seen)
}

/// A proxy that accepts a connection and drops it. Anything relayed here fails; the
/// listener stays bound for the test, so no other process can take the port back.
async fn serve_hostile_proxy() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let addr = listener.local_addr().expect("the bound address is readable");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    format!("http://{addr}")
}

fn policy(url: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.classifier]
url = "{url}"
token_env = "{TOKEN_VAR}"
"#
    )
}

#[tokio::test]
async fn a_token_bearing_loopback_annotator_is_reached_directly_when_a_proxy_is_configured() {
    let (url, seen) = serve_annotator().await;
    let proxy = serve_hostile_proxy().await;
    // Set before the runtime opens: reqwest reads the proxy environment once, when it
    // builds a client. `ALL_PROXY` covers the cleartext scheme a loopback endpoint uses.
    unsafe {
        std::env::set_var("ALL_PROXY", &proxy);
        std::env::set_var(TOKEN_VAR, TOKEN);
    }

    let dir = tempfile::tempdir().expect("the fixture directory is created");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy(&url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    let root = TrajectoryId("loopback-proxy-test".to_string());
    let actor = Actor {
        root: root.clone(),
        child: None,
    };
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root }).await,
        HookDecision::Ack
    );

    let decision = hooks::handle(
        &runtime,
        HookEvent::ToolCall {
            actor,
            call: ProposedCall {
                tool: "fetch".to_string(),
                arguments: raw(serde_json::json!({ "url": "https://a.example" })),
            },
            spawn: false,
        },
    )
    .await;

    unsafe {
        std::env::remove_var("ALL_PROXY");
        std::env::remove_var(TOKEN_VAR);
    }

    assert_eq!(
        decision,
        HookDecision::AllowCall { spawn: None },
        "the annotator answered, so its consult was not relayed through the proxy"
    );
    assert_eq!(
        seen.0.lock().unwrap().as_slice(),
        [TOKEN.to_string()],
        "the annotator itself received the token, and received it exactly once"
    );
}
