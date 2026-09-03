//! One OpenAI-compatible `/chat/completions` provider. It runs
//! inference and nothing else — it holds no trajectory, no transcript
//! and no policy, so it is a transport leaf.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::http::{HttpClient, read_body_capped};
use crate::wire::{ChatCompletionRequest, ChatCompletionResponse, WireMessage};

pub const DEFAULT_COMPLETION_BODY_CAP_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(4);
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint(String);

impl Endpoint {
    pub fn new(value: impl Into<String>) -> Self {
        Endpoint(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Endpoint {
    fn from(value: &str) -> Self {
        Endpoint::new(value)
    }
}

impl From<String> for Endpoint {
    fn from(value: String) -> Self {
        Endpoint::new(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        ModelId(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        ModelId::new(value)
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        ModelId::new(value)
    }
}

#[derive(Clone)]
struct ApiKey(String);

/// Configuration for one OpenAI-compatible chat-completions endpoint.
///
/// Deliberately without a `Debug` implementation: it owns the API key.
#[derive(Clone)]
pub struct OpenAiConfig {
    endpoint: Endpoint,
    model: ModelId,
    api_key: ApiKey,
    request_timeout: Duration,
    response_body_cap_bytes: usize,
    max_attempts: u32,
    retry_base_delay: Duration,
    retry_max_delay: Duration,
}

impl OpenAiConfig {
    pub fn new(endpoint: impl Into<Endpoint>, model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiConfig {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: ApiKey(api_key.into()),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            response_body_cap_bytes: DEFAULT_COMPLETION_BODY_CAP_BYTES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
        }
    }

    pub fn openrouter(model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiConfig::new(OPENROUTER_ENDPOINT, model, api_key)
    }

    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    #[cfg(test)]
    fn with_test_retry_policy(mut self, max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.retry_base_delay = base_delay;
        self.retry_max_delay = max_delay.max(base_delay);
        self
    }
}

/// One accepted completion and how many transport attempts it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCompletion {
    pub message: WireMessage,
    pub attempts: u32,
}

/// Why an upstream inference round failed after its bounded attempts.
/// Every variant is fail-closed: the agent stops rather than proceeding
/// on a guess.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("inference exceeded the {timeout:?} request deadline on the last of {attempts} attempt(s)")]
    Timeout { attempts: u32, timeout: Duration },
    #[error("inference transport fault after {attempts} attempt(s)")]
    Transport { attempts: u32 },
    /// The endpoint answered, but could not serve: a transient status (408, 429, 5xx) on
    /// the last of the bounded attempts.
    #[error("inference endpoint was unavailable (HTTP {code}) on the last of {attempts} attempt(s)")]
    Unavailable { code: u16, attempts: u32 },
    /// The endpoint answered with a status no retry changes: a rejected key, a bad request.
    #[error("inference endpoint returned HTTP {code}{detail} after {attempts} attempt(s)")]
    Status { code: u16, attempts: u32, detail: String },
    #[error("inference response was oversized or malformed after {attempts} attempt(s)")]
    Malformed { attempts: u32 },
    #[error("inference response carried no choices after {attempts} attempt(s)")]
    NoChoice { attempts: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AttemptFault {
    Timeout,
    Transport,
    Status {
        code: u16,
        retry_after: Option<Duration>,
        detail: String,
    },
    Malformed,
    NoChoice,
}

impl AttemptFault {
    /// A request that produced no usable response. An elapsed deadline and a
    /// failed connection are kept apart because they call for opposite
    /// remedies: a deadline that a long generation legitimately outgrows wants
    /// a longer deadline, since every retry then meets the same wall, while a
    /// flapping endpoint wants more attempts, spaced further apart. A
    /// connection that never came up is a transport fault even when it
    /// timed out: the request deadline never started.
    fn of(error: &reqwest::Error) -> Self {
        if error.is_timeout() && !error.is_connect() {
            AttemptFault::Timeout
        } else {
            AttemptFault::Transport
        }
    }

    fn retryable(&self) -> bool {
        match self {
            AttemptFault::Timeout | AttemptFault::Transport => true,
            AttemptFault::Status { code, .. } => matches!(code, 408 | 429 | 500 | 502 | 503 | 504),
            AttemptFault::Malformed | AttemptFault::NoChoice => false,
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            AttemptFault::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    fn finish(self, attempts: u32, request_timeout: Duration) -> ProviderError {
        match self {
            AttemptFault::Timeout => ProviderError::Timeout {
                attempts,
                timeout: request_timeout,
            },
            AttemptFault::Transport => ProviderError::Transport { attempts },
            AttemptFault::Status { code, .. } if self.retryable() => ProviderError::Unavailable { code, attempts },
            AttemptFault::Status { code, detail, .. } => ProviderError::Status { code, attempts, detail },
            AttemptFault::Malformed => ProviderError::Malformed { attempts },
            AttemptFault::NoChoice => ProviderError::NoChoice { attempts },
        }
    }
}

/// A non-streaming OpenAI/OpenRouter-compatible provider.
///
/// Deliberately without a `Debug` implementation: its configuration
/// owns the API key. The client never follows redirects. Only transient
/// failures of the identical inference request are retried; an agent action
/// is never replayed here.
#[derive(Clone)]
pub struct OpenAiCompatible {
    config: OpenAiConfig,
    client: HttpClient,
}

impl OpenAiCompatible {
    pub fn new(config: OpenAiConfig) -> Self {
        OpenAiCompatible::with_http_client(config, HttpClient::new())
    }

    pub fn with_http_client(config: OpenAiConfig, client: HttpClient) -> Self {
        OpenAiCompatible { config, client }
    }

    pub fn openrouter(model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiCompatible::new(OpenAiConfig::openrouter(model, api_key))
    }

    /// Run one provider call. The configured model replaces any model
    /// in `request`.
    pub async fn complete(&self, mut request: ChatCompletionRequest) -> Result<ProviderCompletion, ProviderError> {
        request.model = self.config.model.0.clone();
        let url = format!("{}/chat/completions", self.config.endpoint.0.trim_end_matches('/'));
        for attempt in 1..=self.config.max_attempts {
            match self.attempt(&url, &request).await {
                Ok(message) => {
                    return Ok(ProviderCompletion {
                        message,
                        attempts: attempt,
                    });
                }
                Err(fault) if fault.retryable() && attempt < self.config.max_attempts => {
                    tokio::time::sleep(self.retry_delay(attempt, fault.retry_after())).await;
                }
                Err(fault) => return Err(fault.finish(attempt, self.config.request_timeout)),
            }
        }
        unreachable!("the retry policy always permits at least one attempt")
    }

    async fn attempt(&self, url: &str, request: &ChatCompletionRequest) -> Result<WireMessage, AttemptFault> {
        let response = self
            .client
            .inner()
            .post(url)
            .bearer_auth(&self.config.api_key.0)
            .timeout(self.config.request_timeout)
            .json(request)
            .send()
            .await
            .map_err(|error| AttemptFault::of(&error))?;
        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut response = response;
            let detail = match read_body_capped(&mut response, 512).await {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).trim().to_string();
                    if text.is_empty() {
                        String::new()
                    } else {
                        format!(": {text}")
                    }
                }
                Err(_) => String::new(),
            };
            return Err(AttemptFault::Status {
                code: status.as_u16(),
                retry_after,
                detail,
            });
        }

        let mut response = response;
        let body = read_body_capped(&mut response, self.config.response_body_cap_bytes)
            .await
            .map_err(|error| AttemptFault::of(&error))?;
        if body.len() > self.config.response_body_cap_bytes {
            return Err(AttemptFault::Malformed);
        }
        let parsed: ChatCompletionResponse = serde_json::from_slice(&body).map_err(|_| AttemptFault::Malformed)?;
        let choice = parsed.choices.into_iter().next().ok_or(AttemptFault::NoChoice)?;
        Ok(choice.message)
    }

    fn retry_delay(&self, failed_attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(retry_after) = retry_after {
            return retry_after.min(self.config.retry_max_delay);
        }
        let multiplier = 1u32.checked_shl(failed_attempt.saturating_sub(1)).unwrap_or(u32::MAX);
        let cap = self
            .config
            .retry_base_delay
            .saturating_mul(multiplier)
            .min(self.config.retry_max_delay);
        let cap_nanos = cap.as_nanos();
        if cap_nanos == 0 {
            return Duration::ZERO;
        }
        let entropy = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Duration::from_nanos((entropy % (cap_nanos + 1)).min(u64::MAX as u128) as u64)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::post};

    use super::*;
    use crate::wire::WireMessage;

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: String::new(),
            messages: vec![WireMessage::user("hello")],
            tools: None,
        }
    }

    async fn provider(app: Router) -> OpenAiCompatible {
        provider_with_request_timeout(app, DEFAULT_REQUEST_TIMEOUT).await
    }

    async fn provider_with_request_timeout(app: Router, request_timeout: Duration) -> OpenAiCompatible {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let address = listener.local_addr().expect("the listener has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let config = OpenAiConfig::new(format!("http://{address}"), "fixture/model", "test-key")
            .with_request_timeout(request_timeout)
            .with_test_retry_policy(3, Duration::ZERO, Duration::ZERO);
        OpenAiCompatible::with_http_client(config, HttpClient::loopback())
    }

    #[tokio::test]
    async fn a_transient_status_retries_the_identical_logical_request() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let attempts = Arc::new(Mutex::new(0u32));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let seen = Arc::clone(&seen);
                let attempts = Arc::clone(&attempts);
                move |Json(body): Json<serde_json::Value>| {
                    let seen = Arc::clone(&seen);
                    let attempts = Arc::clone(&attempts);
                    async move {
                        seen.lock().expect("not poisoned").push(body);
                        let mut count = attempts.lock().expect("not poisoned");
                        *count += 1;
                        if *count == 1 {
                            (
                                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                [(reqwest::header::RETRY_AFTER.as_str(), "0")],
                                Json(serde_json::json!({})),
                            )
                                .into_response()
                        } else {
                            Json(serde_json::json!({
                                "choices": [{ "message": { "role": "assistant", "content": "done" } }]
                            }))
                            .into_response()
                        }
                    }
                }
            }),
        );
        let completion = provider(app)
            .await
            .complete(request())
            .await
            .expect("the retry recovers");

        assert_eq!(completion.attempts, 2);
        assert_eq!(completion.message.content.as_deref(), Some("done"));
        let seen = seen.lock().expect("not poisoned");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1], "a retry changes no model input");
    }

    #[tokio::test]
    async fn a_permanent_client_status_is_not_retried() {
        let attempts = Arc::new(Mutex::new(0u32));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let attempts = Arc::clone(&attempts);
                move || {
                    let attempts = Arc::clone(&attempts);
                    async move {
                        *attempts.lock().expect("not poisoned") += 1;
                        axum::http::StatusCode::UNAUTHORIZED
                    }
                }
            }),
        );
        let error = provider(app)
            .await
            .complete(request())
            .await
            .expect_err("401 is permanent");

        assert_eq!(
            error,
            ProviderError::Status {
                code: 401,
                attempts: 1,
                detail: String::new()
            }
        );
        assert_eq!(*attempts.lock().expect("not poisoned"), 1);
    }

    #[tokio::test]
    async fn a_status_error_carries_response_body_detail() {
        let app = Router::new().route(
            "/chat/completions",
            post(|| async { (axum::http::StatusCode::PAYMENT_REQUIRED, "Key credit limit exceeded") }),
        );
        let error = provider(app)
            .await
            .complete(request())
            .await
            .expect_err("402 is permanent");

        assert_eq!(
            error,
            ProviderError::Status {
                code: 402,
                attempts: 1,
                detail: ": Key credit limit exceeded".to_string()
            }
        );
        assert_eq!(
            error.to_string(),
            "inference endpoint returned HTTP 402: Key credit limit exceeded after 1 attempt(s)"
        );
    }

    #[tokio::test]
    async fn a_transient_failure_stops_at_the_attempt_ceiling() {
        let attempts = Arc::new(Mutex::new(0u32));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let attempts = Arc::clone(&attempts);
                move || {
                    let attempts = Arc::clone(&attempts);
                    async move {
                        *attempts.lock().expect("not poisoned") += 1;
                        axum::http::StatusCode::TOO_MANY_REQUESTS
                    }
                }
            }),
        );
        let error = provider(app)
            .await
            .complete(request())
            .await
            .expect_err("the outage persists");

        assert_eq!(error, ProviderError::Unavailable { code: 429, attempts: 3 });
        assert_eq!(*attempts.lock().expect("not poisoned"), 3);
    }

    #[tokio::test]
    async fn a_malformed_success_is_not_retried() {
        let attempts = Arc::new(Mutex::new(0u32));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let attempts = Arc::clone(&attempts);
                move || {
                    let attempts = Arc::clone(&attempts);
                    async move {
                        *attempts.lock().expect("not poisoned") += 1;
                        (axum::http::StatusCode::OK, "not json")
                    }
                }
            }),
        );
        let error = provider(app)
            .await
            .complete(request())
            .await
            .expect_err("the response is invalid");

        assert_eq!(error, ProviderError::Malformed { attempts: 1 });
        assert_eq!(*attempts.lock().expect("not poisoned"), 1);
    }

    #[tokio::test]
    async fn an_endpoint_slower_than_the_deadline_reports_the_deadline() {
        let attempts = Arc::new(Mutex::new(0u32));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let attempts = Arc::clone(&attempts);
                move || {
                    let attempts = Arc::clone(&attempts);
                    async move {
                        *attempts.lock().expect("not poisoned") += 1;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        axum::http::StatusCode::OK
                    }
                }
            }),
        );
        let timeout = Duration::from_millis(50);
        let error = provider_with_request_timeout(app, timeout)
            .await
            .complete(request())
            .await
            .expect_err("the endpoint never answers in time");

        assert_eq!(error, ProviderError::Timeout { attempts: 3, timeout });
        assert_eq!(*attempts.lock().expect("not poisoned"), 3);
    }

    #[tokio::test]
    async fn a_connection_the_endpoint_drops_is_a_transport_fault_not_a_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let address = listener.local_addr().expect("the listener has an address");
        let attempts = Arc::new(Mutex::new(0u32));
        tokio::spawn({
            let attempts = Arc::clone(&attempts);
            async move {
                while let Ok((socket, _)) = listener.accept().await {
                    *attempts.lock().expect("not poisoned") += 1;
                    drop(socket);
                }
            }
        });
        let config = OpenAiConfig::new(format!("http://{address}"), "fixture/model", "test-key")
            .with_request_timeout(Duration::from_secs(5))
            .with_test_retry_policy(3, Duration::ZERO, Duration::ZERO);
        let error = OpenAiCompatible::with_http_client(config, HttpClient::loopback())
            .complete(request())
            .await
            .expect_err("the endpoint never answers");

        assert_eq!(error, ProviderError::Transport { attempts: 3 });
        assert_eq!(*attempts.lock().expect("not poisoned"), 3);
    }
}
