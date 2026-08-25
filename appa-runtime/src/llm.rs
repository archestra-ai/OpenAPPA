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

fn answer_budget(input: &str) -> u64 {
    (input.len() as u64 / 2 + ANSWER_OVERHEAD_TOKENS).clamp(MIN_ANSWER_TOKENS, MAX_ANSWER_TOKENS)
}

/// One provider client built from the `[externals.llm]` profile at open, shared by every
/// `builtin = "llm"` entry of the deployment together with the profile's permit pool.
#[derive(Clone)]
pub struct LlmBackend {
    client: LlmClient,
    model: String,
    timeout: Duration,
    permits: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for LlmBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmBackend")
            .field("provider", &self.client.provider())
            .field("model", &self.model)
            .field("timeout", &self.timeout)
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
    /// budget, used when the profile declares none of its own.
    pub fn new(profile: &LlmProfile, shared_timeout: Duration) -> Result<LlmBackend, LlmClientError> {
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
            permits: Arc::new(tokio::sync::Semaphore::new(profile.max_concurrent)),
        })
    }

    /// One consult. The deadline covers the permit wait and the request: queueing behind
    /// the pool spends the same budget the consult itself would.
    pub async fn consult(&self, prompt: &ModelPrompt) -> Result<serde_json::Value, NoAnswerReason> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let permit = match tokio::time::timeout_at(deadline, self.permits.acquire()).await {
            Ok(permit) => permit.expect("the llm consult gate is never closed"),
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
            Ok(Ok(text)) => serde_json::from_str(&text).map_err(|_| NoAnswerReason::Malformed),
        }
    }

    async fn prompt(&self, prompt: &ModelPrompt) -> Result<String, PromptError> {
        match &self.client {
            LlmClient::Anthropic(client) => run(client.completion_model(&self.model), prompt).await,
            LlmClient::OpenAi(client) => run(client.completion_model(&self.model), prompt).await,
            LlmClient::Gemini(client) => run(client.completion_model(&self.model), prompt).await,
            LlmClient::Ollama(client) => run(client.completion_model(&self.model), prompt).await,
        }
    }
}

/// One fresh, memoryless agent per consult: the preamble and declaration as the system
/// prompt, the artifact as the only user turn, the schema enforced natively.
async fn run<M: CompletionModel + 'static>(model: M, prompt: &ModelPrompt) -> Result<String, PromptError> {
    let schema = schemars::Schema::try_from(prompt.schema.clone()).expect("every consult schema is a JSON object");
    AgentBuilder::new(model)
        .preamble(&prompt.system)
        .output_schema_raw(schema)
        .output_mode(OutputMode::Native)
        .max_tokens(answer_budget(&prompt.input))
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
        LlmBackend::new(
            &profile(provider, Some(url), Some("sekret"), max_concurrent),
            Duration::from_secs(5),
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
        assert!(LlmBackend::new(&gemini, Duration::from_secs(1)).is_ok());
        let ollama = profile(LlmProvider::Ollama, None, None, 2);
        assert!(LlmBackend::new(&ollama, Duration::from_secs(1)).is_ok());
        let pinned = profile(LlmProvider::Ollama, Some("http://127.0.0.1:11434".to_string()), None, 2);
        assert!(LlmBackend::new(&pinned, Duration::from_secs(1)).is_ok());
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
    async fn the_profile_pool_bounds_concurrent_consults() {
        let (addr, stub) = serve("/v1/messages", anthropic_reply, Duration::from_millis(100)).await;
        stub.answering(StubAnswer::Text("{\"ruling\":\"approve\"}".to_string()));
        let backend = built(LlmProvider::Anthropic, format!("http://{addr}"), 1);

        let ask = prompt();
        let answers = futures_util::future::join_all((0..3).map(|_| backend.consult(&ask))).await;
        assert!(answers.iter().all(Result::is_ok), "{answers:?}");
        assert_eq!(
            stub.max_in_flight.load(Ordering::SeqCst),
            1,
            "one permit, one request at a time"
        );
        assert_eq!(stub.requests().len(), 3);
    }
}
