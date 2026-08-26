//! The `llm` builtin: the deployment's one API-key model profile, consulted in-process.
//! It receives the same [`ModelPrompt`] rendering the `claude-code` builtin does — the
//! declaration in the system prompt, the artifact as the user turn, the per-kind output
//! schema as the provider's native structured output — so a component switched between
//! the two transports sees the same question. Every failure the provider returns is no
//! answer; nothing about the trajectory reaches the model.

use std::sync::Arc;
use std::time::Duration;

use rig_agent::AgentBuilder;
use rig_agent::agent::OutputMode;
use rig_agent::completion::{Prompt, PromptError};
use rig_core::client::completion::CompletionClient;
use rig_core::completion::{CompletionError, CompletionModel};

use rig_core::providers::{anthropic, gemini, ollama, openai};

use crate::config::{LlmProfile, LlmProvider};
use crate::consult::ModelPrompt;
use crate::external::NoAnswerReason;

/// The answer budget: an answer restates at most the artifact (a sanitizer's rewritten
/// body) plus the schema's own overhead, and never less than a short ruling needs.
const MIN_ANSWER_TOKENS: u64 = 4096;
const MAX_ANSWER_TOKENS: u64 = 32_768;
const ANSWER_OVERHEAD_TOKENS: u64 = 1024;

/// The output tokens one consult may spend: sized from the input, and never more than
/// the deployment accepts as an answer body — a token is at least one byte, so an answer
/// within this budget always fits under `max_body_bytes`.
fn answer_budget(input: &str, max_body_bytes: usize) -> u64 {
    (input.len() as u64 / 2 + ANSWER_OVERHEAD_TOKENS)
        .clamp(MIN_ANSWER_TOKENS, MAX_ANSWER_TOKENS)
        .min(max_body_bytes as u64)
}

/// One provider client built from the `[externals.llm]` profile at open, shared by every
/// `builtin = "llm"` entry of the deployment, drawing on the runtime's one llm gate.
#[derive(Clone)]
pub struct LlmBackend {
    client: LlmClient,
    model: String,
    timeout: Duration,
    max_body_bytes: usize,
    gate: Arc<LlmGate>,
}

/// The permit pool every `llm` consult of a runtime draws on, bounded by `max_concurrent`
/// of the profile the runtime serves. A reload that raises the bound widens the pool at
/// once; one that lowers it reclaims permits as in-flight consults release them, so the
/// old and the new deployment snapshot never exceed the new bound together.
pub(crate) struct LlmGate {
    permits: Arc<tokio::sync::Semaphore>,
    shape: std::sync::Mutex<GateShape>,
}

/// The pool's bound and the permits a shrink still owes: the semaphore holds
/// `bound + owed` permits in total, available or in flight.
struct GateShape {
    bound: usize,
    owed: usize,
}

impl LlmGate {
    pub(crate) fn new(bound: usize) -> LlmGate {
        LlmGate {
            permits: Arc::new(tokio::sync::Semaphore::new(bound)),
            shape: std::sync::Mutex::new(GateShape { bound, owed: 0 }),
        }
    }

    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.permits.available_permits()
    }

    pub(crate) fn resize(&self, bound: usize) {
        let mut shape = self.shape.lock().expect("the llm gate mutex is never poisoned");
        let total = shape.bound + shape.owed;
        if bound >= total {
            self.permits.add_permits(bound - total);
            shape.owed = 0;
        } else {
            shape.owed = total - bound;
            shape.owed -= self.permits.forget_permits(shape.owed);
        }
        shape.bound = bound;
    }
}

/// One consult's permit. Dropping it — on an answer, a timeout, or a consult cancelled
/// mid-flight alike — settles a narrowed gate's debt before the permit can return.
struct LlmPermit {
    gate: Arc<LlmGate>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl LlmPermit {
    async fn acquire(gate: &Arc<LlmGate>) -> LlmPermit {
        let permit = gate
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("the llm consult gate is never closed");
        LlmPermit {
            gate: Arc::clone(gate),
            permit: Some(permit),
        }
    }
}

impl Drop for LlmPermit {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        let mut shape = self.gate.shape.lock().expect("the llm gate mutex is never poisoned");
        if shape.owed > 0 {
            shape.owed -= 1;
            permit.forget();
        }
    }
}

impl std::fmt::Debug for LlmBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmBackend")
            .field("provider", &self.client.provider())
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish()
    }
}

/// The providers the builtin speaks to, closed: each is compiled in and dispatched by
/// match. OpenAI goes through the chat-completions API so an OpenAI-compatible `url`
/// works unchanged.
#[derive(Clone)]
enum LlmClient {
    Anthropic(anthropic::Client),
    OpenAi(openai::CompletionsClient),
    Gemini(gemini::Client),
    Ollama(ollama::Client),
}

impl LlmClient {
    fn provider(&self) -> LlmProvider {
        match self {
            LlmClient::Anthropic(_) => LlmProvider::Anthropic,
            LlmClient::OpenAi(_) => LlmProvider::OpenAi,
            LlmClient::Gemini(_) => LlmProvider::Gemini,
            LlmClient::Ollama(_) => LlmProvider::Ollama,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the [externals.llm] {provider} client cannot be built: {detail}")]
pub struct LlmClientError {
    provider: &'static str,
    detail: String,
}

impl LlmBackend {
    /// Build the provider client once. `shared_timeout` is the deployment's machine-consult
    /// budget, used when the profile declares none of its own; `max_body_bytes` is the
    /// deployment's cap on any answer, model answers included.
    pub(crate) fn new(
        profile: &LlmProfile,
        shared_timeout: Duration,
        max_body_bytes: usize,
        gate: Arc<LlmGate>,
    ) -> Result<LlmBackend, LlmClientError> {
        let token = profile.token.as_ref().map(|token| token.reveal()).unwrap_or("");
        let failed = |error: rig_core::http_client::Error| LlmClientError {
            provider: profile.provider.as_str(),
            detail: error.to_string(),
        };
        let client = match profile.provider {
            LlmProvider::Anthropic => {
                let mut builder = anthropic::Client::builder().api_key(token);
                if let Some(url) = &profile.url {
                    builder = builder.base_url(url);
                }
                LlmClient::Anthropic(builder.build().map_err(failed)?)
            }
            LlmProvider::OpenAi => {
                let mut builder = openai::Client::builder().api_key(token);
                if let Some(url) = &profile.url {
                    builder = builder.base_url(url);
                }
                LlmClient::OpenAi(builder.build().map_err(failed)?.completions_api())
            }
            LlmProvider::Gemini => {
                let mut builder = gemini::Client::builder().api_key(token);
                if let Some(url) = &profile.url {
                    builder = builder.base_url(url);
                }
                LlmClient::Gemini(builder.build().map_err(failed)?)
            }
            LlmProvider::Ollama => {
                let mut builder = ollama::Client::builder().api_key(token);
                if let Some(url) = &profile.url {
                    builder = builder.base_url(url);
                }
                LlmClient::Ollama(builder.build().map_err(failed)?)
            }
        };
        Ok(LlmBackend {
            client,
            model: profile.model.clone(),
            timeout: profile.timeout.unwrap_or(shared_timeout),
            max_body_bytes,
            gate,
        })
    }

    /// One consult. The deadline covers the permit wait and the request: queueing behind
    /// the pool spends the same budget the consult itself would.
    pub async fn consult(&self, prompt: &ModelPrompt) -> Result<serde_json::Value, NoAnswerReason> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let permit = match tokio::time::timeout_at(deadline, LlmPermit::acquire(&self.gate)).await {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!("the llm consult gate stayed saturated for the whole budget");
                return Err(NoAnswerReason::Timeout);
            }
        };
        let answered = tokio::time::timeout_at(deadline, self.prompt(prompt)).await;
        drop(permit);
        match answered {
            Err(_) => Err(NoAnswerReason::Timeout),
            Ok(Err(error)) => {
                tracing::debug!(%error, "the llm consult failed");
                Err(no_answer(error))
            }
            Ok(Ok(text)) if text.len() > self.max_body_bytes => Err(NoAnswerReason::Oversized),
            Ok(Ok(text)) => serde_json::from_str(&text).map_err(|_| NoAnswerReason::Malformed),
        }
    }

    async fn prompt(&self, prompt: &ModelPrompt) -> Result<String, PromptError> {
        let max_tokens = answer_budget(&prompt.input, self.max_body_bytes);
        match &self.client {
            LlmClient::Anthropic(client) => run(client.completion_model(&self.model), prompt, max_tokens).await,
            LlmClient::OpenAi(client) => run(client.completion_model(&self.model), prompt, max_tokens).await,
            LlmClient::Gemini(client) => run(client.completion_model(&self.model), prompt, max_tokens).await,
            LlmClient::Ollama(client) => run(client.completion_model(&self.model), prompt, max_tokens).await,
        }
    }
}

/// One fresh, memoryless agent per consult: the preamble and declaration as the system
/// prompt, the artifact as the only user turn, the schema enforced natively.
async fn run<M: CompletionModel + 'static>(
    model: M,
    prompt: &ModelPrompt,
    max_tokens: u64,
) -> Result<String, PromptError> {
    let schema = schemars::Schema::try_from(prompt.schema.clone()).expect("every consult schema is a JSON object");
    AgentBuilder::new(model)
        .preamble(&prompt.system)
        .output_schema_raw(schema)
        .output_mode(OutputMode::Native)
        .max_tokens(max_tokens)
        .temperature(0.0)
        .build()
        .prompt(prompt.input.as_str())
        .await
}

/// A provider's non-success status is a non-success, however rig carried it; a
/// response rig could not read is malformed; everything else is a transport failure.
fn no_answer(error: PromptError) -> NoAnswerReason {
    let PromptError::CompletionError(error) = error else {
        return NoAnswerReason::Transport;
    };
    if let Some(status) = error.provider_response_status() {
        return NoAnswerReason::NonSuccess {
            status: status.as_u16(),
        };
    }
    match error {
        CompletionError::JsonError(_) | CompletionError::ResponseError(_) => NoAnswerReason::Malformed,
        _ => NoAnswerReason::Transport,
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;

    use super::*;
    use crate::config::Token;

    #[derive(Clone)]
    enum StubAnswer {
        Text(String),
        Status(u16),
        Stall,
    }

    #[derive(Clone)]
    struct Stub {
        answer: Arc<Mutex<StubAnswer>>,
        requests: Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl Stub {
        fn answering(&self, answer: StubAnswer) {
            *self.answer.lock().unwrap() = answer;
        }

        fn requests(&self) -> Vec<(HeaderMap, serde_json::Value)> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn anthropic_reply(text: &str) -> String {
        serde_json::json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "test-model",
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": { "input_tokens": 1, "output_tokens": 1 },
        })
        .to_string()
    }

    fn openai_reply(text: &str) -> String {
        serde_json::json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "test-model",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "logprobs": null, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
        })
        .to_string()
    }

    /// One provider stub on a loopback port: `reply` renders the answer text in the
    /// provider's own response shape.
    async fn serve(path: &'static str, reply: fn(&str) -> String, delay: Duration) -> (SocketAddr, Stub) {
        let stub = Stub {
            answer: Arc::new(Mutex::new(StubAnswer::Text(String::new()))),
            requests: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
            delay,
        };
        let router = Router::new()
            .route(
                path,
                post(
                    move |State(stub): State<Stub>, headers: HeaderMap, body: String| async move {
                        let flying = stub.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        stub.max_in_flight.fetch_max(flying, Ordering::SeqCst);
                        let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                        stub.requests.lock().unwrap().push((headers, request));
                        tokio::time::sleep(stub.delay).await;
                        let answer = stub.answer.lock().unwrap().clone();
                        let response = match answer {
                            StubAnswer::Text(text) => (axum::http::StatusCode::OK, reply(&text)),
                            StubAnswer::Status(status) => (
                                axum::http::StatusCode::from_u16(status).expect("a valid status"),
                                "boom".to_string(),
                            ),
                            StubAnswer::Stall => {
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                (axum::http::StatusCode::OK, reply("{}"))
                            }
                        };
                        stub.in_flight.fetch_sub(1, Ordering::SeqCst);
                        response
                    },
                ),
            )
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let addr = listener.local_addr().expect("the bound address is readable");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("the stub serves");
        });
        (addr, stub)
    }

    fn profile(provider: LlmProvider, url: Option<String>, token: Option<&str>, max_concurrent: usize) -> LlmProfile {
        LlmProfile {
            provider,
            model: "test-model".to_string(),
            url,
            token: token.map(|token| Token::new(token.to_string())),
            timeout: Some(Duration::from_millis(1500)),
            max_concurrent,
        }
    }

    fn built(provider: LlmProvider, url: String, max_concurrent: usize) -> LlmBackend {
        built_under(provider, url, max_concurrent, 65_536)
    }

    fn built_under(provider: LlmProvider, url: String, max_concurrent: usize, max_body_bytes: usize) -> LlmBackend {
        built_over(provider, url, max_concurrent, max_body_bytes, Arc::new(LlmGate::new(0)))
    }

    fn built_over(
        provider: LlmProvider,
        url: String,
        max_concurrent: usize,
        max_body_bytes: usize,
        gate: Arc<LlmGate>,
    ) -> LlmBackend {
        gate.resize(max_concurrent);
        LlmBackend::new(
            &profile(provider, Some(url), Some("sekret"), max_concurrent),
            Duration::from_secs(5),
            max_body_bytes,
            gate,
        )
        .expect("the backend builds")
    }

    fn prompt() -> ModelPrompt {
        ModelPrompt {
            system: "You rule on one call.\n{\"hint\":\"only reads\",\"permits\":{}}".to_string(),
            input: "{\"tool\":\"read\",\"arguments\":{\"path\":\"a\"},\"requirements\":[]}".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": { "ruling": { "type": "string", "enum": ["approve", "deny"] } },
                "required": ["ruling"],
                "additionalProperties": false,
            }),
        }
    }

    /// A message body as the provider spells it: a string, or text blocks.
    fn text_of(content: &serde_json::Value) -> String {
        match content {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .map(|block| block["text"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(""),
            other => panic!("content is neither text nor blocks: {other}"),
        }
    }

    fn property_names(schema: &serde_json::Value) -> Vec<String> {
        schema["properties"]
            .as_object()
            .expect("the schema has properties")
            .keys()
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn an_anthropic_consult_carries_the_prompt_the_schema_and_the_key() {
        let (addr, stub) = serve("/v1/messages", anthropic_reply, Duration::ZERO).await;
        stub.answering(StubAnswer::Text("{\"ruling\":\"approve\"}".to_string()));
        let backend = built(LlmProvider::Anthropic, format!("http://{addr}"), 4);

        let answer = backend.consult(&prompt()).await;
        assert_eq!(answer, Ok(serde_json::json!({ "ruling": "approve" })));

        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(headers["x-api-key"], "sekret");
        assert_eq!(body["model"], "test-model");
        assert_eq!(text_of(&body["system"]), prompt().system);
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(text_of(&body["messages"][0]["content"]), prompt().input);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            property_names(&body["output_config"]["format"]["schema"]),
            property_names(&prompt().schema)
        );
        assert!(body["max_tokens"].as_u64().is_some_and(|n| n >= MIN_ANSWER_TOKENS));
        assert!(
            body.get("tools")
                .is_none_or(|tools| tools.as_array().is_none_or(Vec::is_empty))
        );
    }

    #[tokio::test]
    async fn an_openai_consult_uses_chat_completions_with_a_response_format() {
        let (addr, stub) = serve("/v1/chat/completions", openai_reply, Duration::ZERO).await;
        stub.answering(StubAnswer::Text("{\"ruling\":\"deny\"}".to_string()));
        let backend = built(LlmProvider::OpenAi, format!("http://{addr}/v1"), 4);

        let answer = backend.consult(&prompt()).await;
        assert_eq!(answer, Ok(serde_json::json!({ "ruling": "deny" })));

        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(headers["authorization"], "Bearer sekret");
        assert_eq!(body["model"], "test-model");
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(text_of(&messages[0]["content"]), prompt().system);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(text_of(&messages[1]["content"]), prompt().input);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            property_names(&body["response_format"]["json_schema"]["schema"]),
            property_names(&prompt().schema)
        );
    }

    #[test]
    fn gemini_and_ollama_profiles_build_without_a_network() {
        let gemini = profile(LlmProvider::Gemini, None, Some("sekret"), 2);
        let gate = || Arc::new(LlmGate::new(0));
        assert!(LlmBackend::new(&gemini, Duration::from_secs(1), 65_536, gate()).is_ok());
        let ollama = profile(LlmProvider::Ollama, None, None, 2);
        assert!(LlmBackend::new(&ollama, Duration::from_secs(1), 65_536, gate()).is_ok());
        let pinned = profile(LlmProvider::Ollama, Some("http://127.0.0.1:11434".to_string()), None, 2);
        assert!(LlmBackend::new(&pinned, Duration::from_secs(1), 65_536, gate()).is_ok());
    }

    #[tokio::test]
    async fn every_provider_failure_is_no_answer() {
        let (addr, stub) = serve("/v1/messages", anthropic_reply, Duration::ZERO).await;
        let backend = built(LlmProvider::Anthropic, format!("http://{addr}"), 4);

        stub.answering(StubAnswer::Status(500));
        assert_eq!(
            backend.consult(&prompt()).await,
            Err(NoAnswerReason::NonSuccess { status: 500 })
        );

        stub.answering(StubAnswer::Text("not json".to_string()));
        assert_eq!(backend.consult(&prompt()).await, Err(NoAnswerReason::Malformed));

        stub.answering(StubAnswer::Stall);
        let started = std::time::Instant::now();
        assert_eq!(backend.consult(&prompt()).await, Err(NoAnswerReason::Timeout));
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the profile's own budget bounds the consult"
        );

        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port binds");
        let unreachable = format!("http://{}", closed.local_addr().expect("the address reads"));
        drop(closed);
        let backend = built(LlmProvider::Anthropic, unreachable, 4);
        assert!(backend.consult(&prompt()).await.is_err());
    }

    #[tokio::test]
    async fn an_answer_past_max_body_bytes_is_oversized_and_the_budget_never_asks_for_one() {
        let (addr, stub) = serve("/v1/messages", anthropic_reply, Duration::ZERO).await;
        let backend = built_under(LlmProvider::Anthropic, format!("http://{addr}"), 4, 48);

        stub.answering(StubAnswer::Text("{\"ruling\":\"approve\"}".to_string()));
        assert_eq!(
            backend.consult(&prompt()).await,
            Ok(serde_json::json!({ "ruling": "approve" }))
        );
        assert_eq!(stub.requests()[0].1["max_tokens"], 48, "a token is at least a byte");

        let long = format!("{{\"ruling\":\"approve\",\"reason\":\"{}\"}}", "x".repeat(64));
        stub.answering(StubAnswer::Text(long));
        assert_eq!(backend.consult(&prompt()).await, Err(NoAnswerReason::Oversized));
    }

    #[tokio::test]
    async fn the_runtime_gate_bounds_concurrent_consults_across_deployment_snapshots() {
        let (addr, stub) = serve("/v1/messages", anthropic_reply, Duration::from_millis(100)).await;
        stub.answering(StubAnswer::Text("{\"ruling\":\"approve\"}".to_string()));
        let gate = Arc::new(LlmGate::new(0));
        // The snapshot before a reload and the one after it: the same runtime gate, the
        // profile's bound applied by whichever loaded last.
        let before = built_over(
            LlmProvider::Anthropic,
            format!("http://{addr}"),
            4,
            65_536,
            gate.clone(),
        );
        let after = built_over(LlmProvider::Anthropic, format!("http://{addr}"), 1, 65_536, gate);

        let ask = prompt();
        let answers = futures_util::future::join_all([
            before.consult(&ask),
            after.consult(&ask),
            before.consult(&ask),
            after.consult(&ask),
        ])
        .await;
        assert!(answers.iter().all(Result::is_ok), "{answers:?}");
        assert_eq!(
            stub.max_in_flight.load(Ordering::SeqCst),
            1,
            "one permit, one request at a time, whichever snapshot asks"
        );
        assert_eq!(stub.requests().len(), 4);
    }

    #[tokio::test]
    async fn a_narrowed_gate_reclaims_permits_as_consults_in_flight_let_go_of_them() {
        let gate = Arc::new(LlmGate::new(2));
        gate.resize(3);
        assert_eq!(
            gate.permits.available_permits(),
            3,
            "a wider bound is available at once"
        );

        let first = LlmPermit::acquire(&gate).await;
        let second = LlmPermit::acquire(&gate).await;
        gate.resize(1);
        assert_eq!(
            gate.permits.available_permits(),
            0,
            "the one permit not in flight is forgotten; the bound is owed one more"
        );
        // A consult cancelled mid-flight drops its permit the same way an answered one does.
        drop(first);
        assert_eq!(
            gate.permits.available_permits(),
            0,
            "the first permit let go settles the debt"
        );
        drop(second);
        assert_eq!(gate.permits.available_permits(), 1, "the pool is the new bound");

        gate.resize(2);
        assert_eq!(gate.permits.available_permits(), 2);
    }
}
