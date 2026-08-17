
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use appa_example_agent::Endpoint;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use website_chat_playground::api::{AppState, router};
use website_chat_playground::session::Sessions;
use website_chat_playground::systems::System;

const PRESET: &str = include_str!("../policies/default.toml");
const MODEL: &str = "openai/gpt-4o";
const ORIGIN: &str = "http://localhost:4321";
const TURN_DEADLINE: Duration = Duration::from_secs(30);

struct Service {
    router: Router,
    sessions: Arc<Sessions>,
    _worlds: tempfile::TempDir,
}

impl Service {
    fn new() -> Service {
        Service::with("http://127.0.0.1:1/v1", Some("sk-test"), 30)
    }

    fn with(inference: &str, key: Option<&str>, max_turns: u32) -> Service {
        let worlds = tempfile::tempdir().expect("a temp dir is creatable");
        let sessions = Arc::new(Sessions::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("world"),
            worlds.path().join("sessions"),
            Duration::from_secs(600),
            Endpoint::new(inference),
        ));
        Service {
            router: router(AppState {
                sessions: Arc::clone(&sessions),
                origins: Arc::new(vec![ORIGIN.to_string()]),
                openrouter_key: key.map(|key| Arc::new(key.to_string())),
                max_turns,
            }),
            sessions,
            _worlds: worlds,
        }
    }

    async fn send(&self, request: Request<Body>) -> Response<Body> {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers every request")
    }

    async fn get(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("a well-formed request");
        read_json(self.send(request).await).await
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        read_json(self.send(json_request("POST", path, body)).await).await
    }

    async fn delete(&self, path: &str) -> StatusCode {
        let request = Request::builder()
            .method("DELETE")
            .uri(path)
            .body(Body::empty())
            .expect("a well-formed request");
        self.send(request).await.status()
    }

    async fn open(&self) -> String {
        let (status, body) = self
            .post(
                "/session",
                serde_json::json!({ "policy": PRESET, "systems": every_system(), "model": MODEL }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "the preset opens: {body}");
        body["session"].as_str().expect("a session id").to_string()
    }

    async fn turn(&self, session: &str, text: &str) -> Turn {
        let request = json_request(
            "POST",
            &format!("/session/{session}/message"),
            serde_json::json!({ "text": text }),
        );
        let response = self.send(request).await;
        assert_eq!(response.status(), StatusCode::OK, "a turn starts");
        Turn {
            body: response.into_body(),
            buffer: String::new(),
        }
    }
}

fn json_request(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("a well-formed request")
}

async fn read_json(response: Response<Body>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("a bounded response body");
    let body = match bytes.is_empty() {
        true => serde_json::Value::Null,
        false => serde_json::from_slice(&bytes).expect("every JSON route answers JSON"),
    };
    (status, body)
}

fn every_system() -> Vec<&'static str> {
    System::ALL.iter().map(|system| system.id()).collect()
}

struct Turn {
    body: Body,
    buffer: String,
}

impl Turn {
    async fn next(&mut self) -> Option<serde_json::Value> {
        loop {
            if let Some(event) = self.take() {
                return Some(event);
            }
            let frame = self.body.frame().await?.expect("the stream does not fault");
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(&String::from_utf8_lossy(&data));
            }
        }
    }

    async fn all(mut self) -> Vec<serde_json::Value> {
        let read = async {
            let mut events = Vec::new();
            while let Some(event) = self.next().await {
                events.push(event);
            }
            events
        };
        tokio::time::timeout(TURN_DEADLINE, read)
            .await
            .expect("the turn's stream ends")
    }

    fn take(&mut self) -> Option<serde_json::Value> {
        let cut = self.buffer.find("\n\n")?;
        let frame = self.buffer[..cut].to_string();
        self.buffer = self.buffer[cut + 2..].to_string();
        let payload: String = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .collect();
        match payload.is_empty() {
            true => None,
            false => Some(serde_json::from_str(&payload).expect("every frame carries one JSON event")),
        }
    }
}

fn tool_call(id: &str, tool: &str, arguments: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant",
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": { "name": tool, "arguments": arguments },
        }],
    })
}

fn offer_in(request: &serde_json::Value) -> String {
    request["messages"]
        .as_array()
        .expect("a request carries messages")
        .iter()
        .rev()
        .filter_map(|message| message["content"].as_str())
        .find_map(|content| {
            let after = content.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
        .expect("the feedback surfaced an offer")
}

fn of_kind<'a>(events: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    events.iter().filter(|event| event["type"] == kind).collect()
}

#[derive(Clone, Default)]
struct Provider {
    script: Arc<Mutex<VecDeque<Reply>>>,
}

enum Reply {
    Says {
        message: serde_json::Value,
        after: Duration,
    },
    PursuesTheOffer { call: String },
}

impl Provider {
    fn calls(&self, tool: &str, arguments: serde_json::Value) -> &Self {
        let id = self.next_id();
        self.push(tool_call(&id, tool, &arguments.to_string()), Duration::ZERO)
    }

    fn says(&self, text: &str) -> &Self {
        self.push(
            serde_json::json!({ "role": "assistant", "content": text }),
            Duration::ZERO,
        )
    }

    fn stalls_for(&self, how_long: Duration) -> &Self {
        self.push(serde_json::json!({ "role": "assistant", "content": "late" }), how_long)
    }

    fn pursues_the_offer(&self) -> &Self {
        let call = self.next_id();
        self.script
            .lock()
            .expect("not poisoned")
            .push_back(Reply::PursuesTheOffer { call });
        self
    }

    fn next_id(&self) -> String {
        format!("call_{}", self.script.lock().expect("not poisoned").len())
    }

    fn push(&self, message: serde_json::Value, after: Duration) -> &Self {
        self.script
            .lock()
            .expect("not poisoned")
            .push_back(Reply::Says { message, after });
        self
    }

    async fn serve(&self) -> String {
        let script = Arc::clone(&self.script);
        let app = Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
                let script = Arc::clone(&script);
                async move {
                    let reply = script.lock().expect("not poisoned").pop_front();
                    let (message, after) = match reply {
                        Some(Reply::Says { message, after }) => (message, after),
                        Some(Reply::PursuesTheOffer { call }) => (
                            tool_call(
                                &call,
                                "execute_remedy_plan",
                                &serde_json::json!({ "offer_id": offer_in(&request) }).to_string(),
                            ),
                            Duration::ZERO,
                        ),
                        None => (
                            serde_json::json!({ "role": "assistant", "content": "(nothing scripted)" }),
                            Duration::ZERO,
                        ),
                    };
                    tokio::time::sleep(after).await;
                    axum::Json(serde_json::json!({ "choices": [{ "message": message }] }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let address = listener.local_addr().expect("a bound address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }
}

#[tokio::test]
async fn the_preset_is_a_policy_the_service_itself_accepts() {
    let service = Service::new();

    let (status, preset) = service.get("/preset").await;
    assert_eq!(status, StatusCode::OK);
    let systems: Vec<&str> = preset["systems"]
        .as_array()
        .expect("the preset lists systems")
        .iter()
        .map(|system| system["id"].as_str().expect("a system id"))
        .collect();
    assert_eq!(systems, every_system());
    let advertised: usize = preset["systems"]
        .as_array()
        .expect("the preset lists systems")
        .iter()
        .map(|system| system["tools"].as_array().expect("a system's tools").len())
        .sum();

    let (status, checked) = service
        .post(
            "/policy/check",
            serde_json::json!({ "policy": preset["policy"], "systems": every_system() }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checked["ok"], true, "the shipped preset loads: {checked}");
    assert_eq!(checked["tools"], advertised, "every advertised tool is in the policy");
    assert_eq!(checked["boundary"]["trust"], "trusted");
    assert_eq!(checked["boundary"]["audience"], "public");
}

#[tokio::test]
async fn a_policy_that_does_not_load_is_a_finding_not_a_failure() {
    let service = Service::new();
    let (status, checked) = service
        .post(
            "/policy/check",
            serde_json::json!({ "policy": "version = 1\n[[tool]]\nname =", "systems": ["crm"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checked["ok"], false);
    assert!(checked["error"].as_str().is_some_and(|error| !error.is_empty()));
    assert!(checked["tools"].is_null(), "a policy that did not load has no tools");
}

#[tokio::test]
async fn a_check_reports_what_the_enabled_systems_did_to_the_policy() {
    let service = Service::new();
    let (status, checked) = service
        .post(
            "/policy/check",
            serde_json::json!({
                "policy": "version = 1\n[[tool]]\nname = \"list_issues\"\ndelta = {}\n",
                "systems": ["crm"],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(checked["ok"], true);
    assert_eq!(checked["ignored"], serde_json::json!(["list_issues"]));
    assert_eq!(
        checked["unconstrained"],
        serde_json::json!(["create_customer_data", "list_customers"])
    );
}

#[tokio::test]
async fn input_past_the_playground_s_size_is_refused() {
    let service = Service::new();
    let oversized = "#".repeat(33 * 1024);

    let (status, _) = service
        .post(
            "/policy/check",
            serde_json::json!({ "policy": oversized, "systems": [] }),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let (status, _) = service
        .post(
            "/session",
            serde_json::json!({ "policy": oversized, "systems": [], "model": MODEL }),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let session = service.open().await;
    let (status, _) = service
        .post(
            &format!("/session/{session}/message"),
            serde_json::json!({ "text": "x".repeat(3 * 1024) }),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_system_this_build_does_not_have_is_refused() {
    let service = Service::new();
    let (status, error) = service
        .post(
            "/policy/check",
            serde_json::json!({ "policy": "version = 1\n", "systems": ["payroll"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error["error"].as_str().is_some_and(|error| !error.is_empty()));
}

#[tokio::test]
async fn a_session_is_opened_once_and_closed_once() {
    let service = Service::new();
    let (status, opened) = service
        .post(
            "/session",
            serde_json::json!({ "policy": PRESET, "systems": every_system(), "model": MODEL }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(opened["tools"], 8);
    assert_eq!(opened["trust"], "trusted");
    assert_eq!(opened["audience"], "public");
    let session = opened["session"].as_str().expect("a session id");

    assert_eq!(
        service.delete(&format!("/session/{session}")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        service.delete(&format!("/session/{session}")).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_model_off_the_allowlist_never_opens_a_session() {
    let service = Service::new();
    let (status, error) = service
        .post(
            "/session",
            serde_json::json!({ "policy": PRESET, "systems": [], "model": "openai/o0-imaginary" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error["error"].as_str().is_some_and(|error| !error.is_empty()));
}

#[tokio::test]
async fn a_turn_is_refused_before_it_spends_anything() {
    let keyless = Service::with("http://127.0.0.1:1/v1", None, 30);
    let session = keyless.open().await;
    let (status, _) = keyless
        .post(
            &format!("/session/{session}/message"),
            serde_json::json!({ "text": "hi" }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "no key, no turn");

    let service = Service::new();
    let (status, _) = service
        .post("/session/nobody/message", serde_json::json!({ "text": "hi" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an expired session is gone, not silent");

    let spent = Service::with("http://127.0.0.1:1/v1", Some("sk-test"), 0);
    let session = spent.open().await;
    let (status, _) = spent
        .post(
            &format!("/session/{session}/message"),
            serde_json::json!({ "text": "hi" }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "a session's turns run out");
}

#[tokio::test]
async fn a_second_turn_while_one_is_streaming_is_refused() {
    let service = Service::new();
    let id = service.open().await;
    let session = service.sessions.get(&id).expect("the session is live");
    let held = Arc::clone(&session.turn_gate)
        .try_lock_owned()
        .expect("no turn is in flight yet");

    let (status, _) = service
        .post(&format!("/session/{id}/message"), serde_json::json!({ "text": "hi" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    drop(held);
}

#[tokio::test]
async fn only_an_allowlisted_origin_is_echoed() {
    let service = Service::new();
    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/session")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .expect("a well-formed request")
    };

    let response = service.send(preflight(ORIGIN)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some(ORIGIN)
    );

    let response = service.send(preflight("https://not-openappa.example")).await;
    assert!(
        response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
        "a foreign origin is answered without permission"
    );
}

#[tokio::test]
async fn a_turn_streams_both_what_the_model_did_and_what_the_policy_did() {
    let provider = Provider::default();
    provider
        .calls("list_customers", serde_json::json!({}))
        .calls(
            "create_issue",
            serde_json::json!({ "title": "Docs gap", "body": "The quickstart skips the boundary." }),
        )
        .says("I filed the issue; the customer records were not mine to read.");
    let service = Service::with(&provider.serve().await, Some("sk-test"), 30);
    let session = service.open().await;

    let events = service
        .turn(&session, "Read the customers, then file an issue about the docs")
        .await
        .all()
        .await;

    let proposed = of_kind(&events, "tool_proposed");
    assert_eq!(proposed.len(), 2, "both proposals are shown: {events:#?}");
    assert_eq!(proposed[0]["tool"], "list_customers");
    assert_eq!(proposed[1]["tool"], "create_issue");
    assert_eq!(
        proposed[1]["arguments"]["title"], "Docs gap",
        "the model's own arguments, as it spelled them"
    );

    let blocked = of_kind(&events, "blocked");
    assert_eq!(blocked.len(), 1, "one call was refused: {events:#?}");
    assert_eq!(
        blocked[0]["call_id"], proposed[0]["call_id"],
        "the block resolves the card its own proposal opened"
    );
    assert!(
        blocked[0]["text"].as_str().is_some_and(|text| !text.is_empty()),
        "the visitor is told what the model was told"
    );

    let results = of_kind(&events, "tool_result");
    assert_eq!(results.len(), 1, "only the released call has a result");
    assert!(results[0]["body"].as_str().is_some_and(|body| !body.is_empty()));
    let closed = of_kind(&events, "tool_closed");
    assert_eq!(closed.len(), 1, "the refused call never dispatched");
    assert_eq!(closed[0]["outcome"], "ran");

    let label = of_kind(&events, "label");
    let label = label.last().expect("an admitted flow labels the trajectory");
    assert_eq!(
        (&label["trust"], &label["audience"]),
        (&serde_json::json!("trusted"), &serde_json::json!("public")),
        "the refused read is exactly the narrowing that did not happen"
    );

    assert_eq!(
        events.last().expect("a turn ends")["type"],
        "answer",
        "the turn's outcome closes the stream"
    );
}

#[tokio::test]
async fn a_turn_that_runs_out_of_rounds_stops_rather_than_answers() {
    let provider = Provider::default();
    for _ in 0..20 {
        provider.calls("list_customers", serde_json::json!({}));
    }
    let service = Service::with(&provider.serve().await, Some("sk-test"), 30);
    let session = service.open().await;

    let events = service.turn(&session, "Keep reading").await.all().await;
    let last = events.last().expect("a turn ends");
    assert_eq!(last["type"], "stopped");
    assert!(last["text"].as_str().is_some_and(|text| !text.is_empty()));
}

#[tokio::test]
async fn a_closed_tab_ends_the_turn_it_was_watching() {
    let provider = Provider::default();
    provider
        .calls("list_customers", serde_json::json!({}))
        .stalls_for(Duration::from_secs(30));
    let service = Service::with(&provider.serve().await, Some("sk-test"), 30);
    let id = service.open().await;
    let session = service.sessions.get(&id).expect("the session is live");

    let mut turn = service.turn(&id, "Read the customers").await;
    turn.next().await.expect("the turn starts talking");
    drop(turn);

    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session.turn_gate.try_lock().is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "a hangup cancels the turn instead of waiting it out");
}

#[tokio::test]
async fn a_closed_tab_ends_a_turn_parked_on_a_human_ruling() {
    let provider = Provider::default();
    provider
        .calls(
            "make_transfer",
            serde_json::json!({ "amount_usd": 2500, "to_account": "acct-99", "memo": "invoice" }),
        )
        .pursues_the_offer();
    let service = Service::with(&provider.serve().await, Some("sk-test"), 30);
    let id = service.open().await;
    let session = service.sessions.get(&id).expect("the session is live");

    let mut turn = service.turn(&id, "Pay the invoice").await;
    let parked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = turn.next().await.expect("the turn talks before it parks");
            if event["type"] == "approval_requested" {
                return event;
            }
        }
    })
    .await;
    assert!(parked.is_ok(), "the transfer parks at the approval desk");
    drop(turn);

    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session.turn_gate.try_lock().is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "an abandoned ruling does not hold the chat");
}
