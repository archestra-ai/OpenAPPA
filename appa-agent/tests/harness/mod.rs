
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use appa_agent::wire::{WireMessage, WireTool, WireToolCall};
use appa_runtime_v2::api::Runtime;
use appa_runtime_v2::config::Config;

#[derive(Clone, Default)]
pub struct Provider {
    script: Arc<Mutex<VecDeque<Step>>>,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

enum Step {
    Says(WireMessage),
    Remedy {
        id: String,
    },
}

impl Provider {
    pub fn says(&self, text: &str) -> &Self {
        self.push(Step::Says(WireMessage::assistant(text)))
    }

    pub fn says_nothing(&self) -> &Self {
        self.push(Step::Says(WireMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
        }))
    }

    pub fn calls(&self, tool: &str, arguments: serde_json::Value) -> &Self {
        self.calls_raw(tool, &arguments.to_string())
    }

    pub fn calls_raw(&self, tool: &str, arguments: &str) -> &Self {
        let id = self.next_id();
        self.push(Step::Says(WireMessage::assistant_tool_calls(
            None,
            vec![call_of(&id, tool, arguments)],
        )))
    }

    pub fn pursues_the_offer(&self) -> &Self {
        let id = self.next_id();
        self.push(Step::Remedy { id })
    }

    fn next_id(&self) -> String {
        format!(
            "call_{}",
            self.script.lock().expect("the script mutex is never poisoned").len()
        )
    }

    fn push(&self, step: Step) -> &Self {
        self.script
            .lock()
            .expect("the script mutex is never poisoned")
            .push_back(step);
        self
    }

    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.seen.lock().expect("the seen mutex is never poisoned").clone()
    }

    pub fn transcript(&self, nth: usize) -> Vec<serde_json::Value> {
        self.requests()[nth]["messages"]
            .as_array()
            .expect("a request carries messages")
            .clone()
    }

    pub fn tool_result(&self, nth: usize, id: &str) -> String {
        self.transcript(nth)
            .into_iter()
            .find(|message| message["tool_call_id"] == id)
            .unwrap_or_else(|| panic!("request {nth} answers {id}"))["content"]
            .as_str()
            .expect("a tool result carries text")
            .to_string()
    }

    pub async fn serve(self) -> String {
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let provider = self.clone();
                async move {
                    let step = provider
                        .script
                        .lock()
                        .expect("not poisoned")
                        .pop_front()
                        .expect("the script covers every round the agent runs");
                    let message = match step {
                        Step::Says(message) => message,
                        Step::Remedy { id } => {
                            let offer = surfaced_offer(&body)
                                .unwrap_or_else(|| panic!("the last feedback surfaces an offer id, in: {body}"));
                            WireMessage::assistant_tool_calls(
                                None,
                                vec![call_of(
                                    &id,
                                    "execute_remedy_plan",
                                    &serde_json::json!({ "offer_id": offer }).to_string(),
                                )],
                            )
                        }
                    };
                    provider.seen.lock().expect("not poisoned").push(body);
                    axum::Json(serde_json::json!({ "choices": [{ "message": message }] }))
                }
            }),
        );
        format!("http://{}", spawn(app).await)
    }
}

#[derive(Clone, Default)]
pub struct ToolHost {
    bodies: Arc<Mutex<Vec<(String, String)>>>,
    sanitized: Arc<Mutex<Option<String>>>,
    sanitizer_consults: Arc<Mutex<usize>>,
    ruling: Arc<Mutex<Option<String>>>,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl ToolHost {
    pub fn answers(&self, tool: &str, body: &str) -> &Self {
        self.bodies
            .lock()
            .expect("not poisoned")
            .push((tool.to_string(), body.to_string()));
        self
    }

    pub fn sanitizes_to(&self, body: &str) -> &Self {
        *self.sanitized.lock().expect("not poisoned") = Some(body.to_string());
        self
    }

    pub fn rules(&self, ruling: &str) -> &Self {
        *self.ruling.lock().expect("not poisoned") = Some(ruling.to_string());
        self
    }

    pub fn calls(&self) -> Vec<serde_json::Value> {
        self.calls.lock().expect("not poisoned").clone()
    }

    pub fn sanitizer_consults(&self) -> usize {
        *self.sanitizer_consults.lock().expect("not poisoned")
    }

    pub async fn serve(self) -> String {
        let tools = self.clone();
        let sanitizer = self.clone();
        let authority = self.clone();
        let app = axum::Router::new()
            .route(
                "/tools",
                axum::routing::post(move |body: String| {
                    let host = tools.clone();
                    async move {
                        let call: serde_json::Value =
                            serde_json::from_str(&body).expect("the agent dispatches a JSON body");
                        let tool = call["tool"].as_str().expect("a dispatch names its tool").to_string();
                        host.calls.lock().expect("not poisoned").push(call);
                        let answer = host
                            .bodies
                            .lock()
                            .expect("not poisoned")
                            .iter()
                            .find(|(name, _)| *name == tool)
                            .map(|(_, body)| body.clone());
                        match answer {
                            Some(body) => (axum::http::StatusCode::OK, body),
                            None => (axum::http::StatusCode::NOT_FOUND, format!("no such tool: {tool}")),
                        }
                    }
                }),
            )
            .route(
                "/sanitizer",
                axum::routing::post(move |axum::Json(_): axum::Json<serde_json::Value>| {
                    let body = sanitizer.sanitized.lock().expect("not poisoned").clone();
                    *sanitizer.sanitizer_consults.lock().expect("not poisoned") += 1;
                    async move {
                        axum::Json(serde_json::json!({
                            "version": 1,
                            "answer": { "body": body.expect("the fixture binds a sanitizer answer") },
                        }))
                    }
                }),
            )
            .route(
                "/authority",
                axum::routing::post(move |axum::Json(_): axum::Json<serde_json::Value>| {
                    let ruling = authority.ruling.lock().expect("not poisoned").clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "version": 1,
                            "answer": { "ruling": ruling.expect("the fixture binds a ruling") },
                        }))
                    }
                }),
            );
        format!("http://{}", spawn(app).await)
    }
}

fn call_of(id: &str, tool: &str, arguments: &str) -> WireToolCall {
    WireToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: appa_agent::wire::WireFunctionCall {
            name: tool.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn surfaced_offer(request: &serde_json::Value) -> Option<String> {
    let messages = request["messages"].as_array()?;
    messages
        .iter()
        .rev()
        .find_map(|message| offer_id(message["content"].as_str()?))
}

pub fn offer_id(text: &str) -> Option<String> {
    let after = text.split("offer_id:").nth(1)?;
    let rest = after.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

async fn spawn(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

pub fn runtime(dir: &tempfile::TempDir, policy: &str, externals: &str) -> Runtime {
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{externals}"),
    )
    .expect("the deployment file writes");
    let config = Config::load(&path).expect("the deployment validates");
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens")
}

pub fn tool(name: &str) -> WireTool {
    WireTool::new(name, format!("the {name} tool"), serde_json::json!({"type": "object"}))
}

std::thread_local! {
    static RECORDED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub struct Decisions;

impl Decisions {
    pub fn recording() -> Decisions {
        use tracing_subscriber::layer::SubscriberExt;
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Recorder));
        });
        RECORDED.with(|recorded| recorded.borrow_mut().clear());
        Decisions
    }

    pub fn recorded(&self) -> Vec<String> {
        RECORDED.with(|recorded| recorded.borrow().clone())
    }
}

struct Recorder;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Recorder {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "appa::decision" {
            return;
        }
        let mut message = Message(None);
        event.record(&mut message);
        if let Some(text) = message.0 {
            RECORDED.with(|recorded| recorded.borrow_mut().push(text));
        }
    }
}

struct Message(Option<String>);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}
