mod common;
use common::raw;

use std::sync::Arc;

use appa_runtime::api::Runtime;
use appa_runtime::{config::Config, hooks, mcp};
use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};
use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
use rmcp::service::{RequestContext, RoleClient};
use rmcp::{ClientHandler, ServiceExt};

fn policy(review_timeout_ms: u64) -> String {
    POLICY.replace("REVIEW_TIMEOUT_MS", &review_timeout_ms.to_string())
}

const POLICY: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "read_notes"
delta = {}

[[policy.tool]]
name = "publish"
delta = {}
requires = { attention = ["signoff"] }

[[policy.authority]]
name = "operator"
hint = "The person at the keyboard."

[policy.authority.mandate]
attends = ["signoff"]

[externals]
timeout_ms = 1000
review_timeout_ms = REVIEW_TIMEOUT_MS
max_body_bytes = 4096

[externals.authorities.operator]
builtin = "hitl"
"#;

#[derive(Clone)]
struct Reviewer {
    answer: ElicitationAction,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Reviewer {
    fn new(answer: ElicitationAction) -> Reviewer {
        Reviewer {
            answer,
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn reviews(&self) -> Vec<String> {
        self.seen.lock().expect("the reviewer mutex is never poisoned").clone()
    }
}

impl ClientHandler for Reviewer {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        let mut info = rmcp::model::ClientInfo::default();
        info.capabilities.elicitation = Some(Default::default());
        info
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = &request {
            self.seen
                .lock()
                .expect("the reviewer mutex is never poisoned")
                .push(message.clone());
        }
        Ok(ElicitResult::new(self.answer.clone()))
    }
}

#[derive(Clone)]
struct Absent;

impl ClientHandler for Absent {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        rmcp::model::ClientInfo::default()
    }
}

#[derive(Clone)]
struct Silent;

impl ClientHandler for Silent {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        let mut info = rmcp::model::ClientInfo::default();
        info.capabilities.elicitation = Some(Default::default());
        info
    }

    async fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        std::future::pending().await
    }
}

struct Deployment {
    url: String,
    offer: String,
    runtime: Arc<Runtime>,
    root: TrajectoryId,
    _dir: tempfile::TempDir,
}

async fn deployment() -> Deployment {
    deployment_with(5000).await
}

async fn deployment_with(review_timeout_ms: u64) -> Deployment {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy(review_timeout_ms)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));

    let root = TrajectoryId("cc:hitl-test".to_string());
    let acked = hooks::handle(&runtime, HookEvent::SessionStart { root: root.clone() }).await;
    assert!(matches!(acked, HookDecision::Ack), "the session opens: {acked:?}");

    let blocked = hooks::handle(
        &runtime,
        HookEvent::ToolCall {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: ProposedCall {
                tool: "publish".to_string(),
                arguments: raw(serde_json::json!({"body": "the quarterly figures"})),
            },
            spawn: false,
        },
    )
    .await;
    let HookDecision::DenyCall { feedback } = blocked else {
        panic!("a tool behind an attention mark must not release unruled: {blocked:?}");
    };
    let offer = offer_id(&feedback);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let url = format!(
        "http://{}/mcp",
        listener.local_addr().expect("the socket has an address")
    );
    let app = axum::Router::new().nest_service("/mcp", mcp::service(Arc::clone(&runtime)));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Deployment {
        url,
        offer,
        runtime,
        root,
        _dir: dir,
    }
}

fn offer_id(feedback: &str) -> String {
    let after = feedback
        .split("offer_id:")
        .nth(1)
        .unwrap_or_else(|| panic!("the feedback surfaces an offer id: {feedback}"));
    let rest = after.trim_start().strip_prefix('"').expect("the offer id is quoted");
    let end = rest.find('"').expect("the offer id closes its quote");
    rest[..end].to_string()
}

async fn execute<H: ClientHandler>(deployment: &Deployment, reviewer: H) -> String {
    let vouched = hooks::handle(
        &deployment.runtime,
        HookEvent::ToolCall {
            actor: Actor {
                root: deployment.root.clone(),
                child: None,
            },
            call: ProposedCall {
                tool: "execute_remedy_plan".to_string(),
                arguments: raw(serde_json::json!({ "offer_id": deployment.offer })),
            },
            spawn: false,
        },
    )
    .await;
    assert!(
        matches!(vouched, HookDecision::PassControl),
        "the root's own offer is admitted: {vouched:?}"
    );
    let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(deployment.url.clone());
    let client = reviewer.serve(transport).await.expect("the client initializes");
    let mut params = rmcp::model::CallToolRequestParams::default();
    params.name = "execute_remedy_plan".into();
    params.arguments = serde_json::json!({ "offer_id": deployment.offer }).as_object().cloned();
    let result = client.call_tool(params).await.expect("the control tool answers");
    let text = format!("{:?}", result.content);
    client.cancel().await.ok();
    text
}

#[tokio::test]
async fn accepting_authorizes_the_exact_call() {
    let deployment = deployment().await;
    let reviewer = Reviewer::new(ElicitationAction::Accept);
    let answer = execute(&deployment, reviewer.clone()).await;
    assert!(answer.contains("Authorized"), "an approval authorizes: {answer}");

    let reviews = reviewer.reviews();
    assert_eq!(reviews.len(), 1, "one decision asks one time");
    let review = &reviews[0];
    assert!(review.contains("publish"), "the review names the tool: {review}");
    assert!(
        review.contains("the quarterly figures"),
        "the review carries the exact arguments the engine would dispatch: {review}",
    );
    assert!(
        review.contains("The person at the keyboard."),
        "the review carries the authority's hint: {review}",
    );
    assert!(
        review.contains("signoff"),
        "the review states the gap the ruling would cover: {review}",
    );
}

#[tokio::test]
async fn declining_denies_and_retires_the_offer() {
    let deployment = deployment().await;
    let answer = execute(&deployment, Reviewer::new(ElicitationAction::Decline)).await;
    assert!(!answer.contains("Authorized"), "a denial authorizes nothing: {answer}");

    let again = execute(&deployment, Reviewer::new(ElicitationAction::Accept)).await;
    assert!(
        !again.contains("Authorized"),
        "a denied offer is gone; a second review cannot revive it: {again}",
    );
}

#[tokio::test]
async fn dismissing_answers_nothing_and_leaves_the_offer_standing() {
    let deployment = deployment().await;
    let answer = execute(&deployment, Reviewer::new(ElicitationAction::Cancel)).await;
    assert!(
        !answer.contains("Authorized"),
        "a dismissal authorizes nothing: {answer}"
    );

    let retried = execute(&deployment, Reviewer::new(ElicitationAction::Accept)).await;
    assert!(
        retried.contains("Authorized"),
        "the offer still stood, so the same id rules on a second try: {retried}",
    );
}

#[tokio::test]
async fn a_client_that_cannot_ask_abstains_without_waiting() {
    let deployment = deployment().await;
    let started = std::time::Instant::now();
    let answer = execute(&deployment, Absent).await;
    assert!(
        !answer.contains("Authorized"),
        "no channel is not an approval: {answer}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "it abstained at once instead of waiting out the review window",
    );

    let reached = execute(&deployment, Reviewer::new(ElicitationAction::Accept)).await;
    assert!(
        reached.contains("Authorized"),
        "no channel left the offer standing, so a reachable reviewer still rules: {reached}",
    );
}

#[tokio::test]
async fn an_unanswered_review_closes_on_its_window_and_the_offer_stands() {
    let deployment = deployment_with(300).await;
    let started = std::time::Instant::now();
    let answer = execute(&deployment, Silent).await;
    assert!(!answer.contains("Authorized"), "silence is not an approval: {answer}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the review window closed the review rather than hanging",
    );

    let retried = execute(&deployment, Reviewer::new(ElicitationAction::Accept)).await;
    assert!(
        retried.contains("Authorized"),
        "an unanswered review took nothing away; the same offer still rules: {retried}",
    );
}
