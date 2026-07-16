
use std::time::Duration;

use appa_contracts::Contracts;
use appa_core::{Speaker, UserId};
use appa_edge::{ProposedCall, Session, Verdict, WebhookResolver};
use axum::Router;
use axum::routing::post;

const POLICY: &str = r#"
    [[tool]]
    name = "mystery_tool"
    output = { trust = "trusted", audience = "public" }

    [[authority]]
    name = "auditor"
    rule = "escalate"
    acknowledge_unknown = true
"#;

fn session() -> Session {
    let contracts = Contracts::from_toml(POLICY).expect("test policy parses");
    let label = contracts.trajectory_label.clone();
    let mut session = Session::new(contracts).unwrap();
    session
        .user_turn(Speaker::user(UserId::new("user")), label, "hi")
        .unwrap();
    session
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn verdict_via(url: &str, timeout: Duration) -> Verdict {
    let resolver = WebhookResolver::new(url, timeout).unwrap();
    let proposed = ProposedCall {
        id: "w1",
        tool: "mystery_tool",
        arguments: "{}",
    };
    session().verdict("{}", proposed, &resolver).await.unwrap()
}

#[tokio::test]
async fn an_approving_webhook_grants() {
    let base = serve(Router::new().route("/", post(async || r#"{"ruling":"approve","reason":"looks fine"}"#))).await;
    match verdict_via(&base, Duration::from_secs(5)).await {
        Verdict::Granted { trail } => assert!(trail.contains("auditor"), "trail: {trail}"),
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn a_denying_webhook_is_terminal() {
    let base = serve(Router::new().route("/", post(async || r#"{"ruling":"deny","reason":"nope"}"#))).await;
    assert!(matches!(
        verdict_via(&base, Duration::from_secs(5)).await,
        Verdict::Terminal { .. }
    ));
}

#[tokio::test]
async fn a_malformed_ruling_fails_closed() {
    let base = serve(Router::new().route("/", post(async || r#"{"ruling":"maybe","reason":"?"}"#))).await;
    assert!(matches!(
        verdict_via(&base, Duration::from_secs(5)).await,
        Verdict::Unresolved { .. }
    ));

    let base = serve(Router::new().route(
        "/",
        post(async || r#"{"ruling":"approve","reason":"ok","extra":"field"}"#),
    ))
    .await;
    assert!(matches!(
        verdict_via(&base, Duration::from_secs(5)).await,
        Verdict::Unresolved { .. }
    ));
}

#[tokio::test]
async fn a_server_error_fails_closed() {
    let base = serve(Router::new().route(
        "/",
        post(async || (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")),
    ))
    .await;
    assert!(matches!(
        verdict_via(&base, Duration::from_secs(5)).await,
        Verdict::Unresolved { .. }
    ));
}

#[tokio::test]
async fn a_timeout_fails_closed() {
    let base = serve(Router::new().route(
        "/",
        post(async || {
            tokio::time::sleep(Duration::from_secs(2)).await;
            r#"{"ruling":"approve","reason":"too late"}"#
        }),
    ))
    .await;
    assert!(matches!(
        verdict_via(&base, Duration::from_millis(100)).await,
        Verdict::Unresolved { .. }
    ));
}
