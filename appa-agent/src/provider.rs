use std::time::Duration;

use appa_runtime::Completion;
use appa_runtime::tool::{HttpClient, read_body_capped};
use appa_runtime::wire::{ChatCompletionRequest, ChatCompletionResponse, WireMessage};
use thiserror::Error;

pub const DEFAULT_COMPLETION_BODY_CAP_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
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
/// This type deliberately has no `Debug` implementation because it owns the API key.
#[derive(Clone)]
pub struct OpenAiConfig {
    endpoint: Endpoint,
    model: ModelId,
    api_key: ApiKey,
    request_timeout: Duration,
    response_body_cap_bytes: usize,
}

impl OpenAiConfig {
    pub fn new(endpoint: impl Into<Endpoint>, model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiConfig {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: ApiKey(api_key.into()),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            response_body_cap_bytes: DEFAULT_COMPLETION_BODY_CAP_BYTES,
        }
    }

    pub fn openrouter(model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiConfig::new(OPENROUTER_ENDPOINT, model, api_key)
    }

    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub fn with_response_body_cap_bytes(mut self, response_body_cap_bytes: usize) -> Self {
        self.response_body_cap_bytes = response_body_cap_bytes;
        self
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("inference transport fault or timeout")]
    Transport,
    #[error("inference endpoint returned a non-success status")]
    Status,
    #[error("inference response was oversized or malformed")]
    Malformed,
    #[error("inference response carried no choices")]
    NoChoice,
}

/// A non-streaming OpenAI/OpenRouter-compatible `/chat/completions` provider.
///
/// This type deliberately has no `Debug` implementation because its configuration owns the API
/// key. Its HTTP client never follows redirects and no request is retried.
#[derive(Clone)]
pub struct OpenAiCompatible {
    config: OpenAiConfig,
    client: HttpClient,
}

impl OpenAiCompatible {
    pub fn new(config: OpenAiConfig) -> Self {
        Self::with_http_client(config, HttpClient::new())
    }

    pub fn with_http_client(config: OpenAiConfig, client: HttpClient) -> Self {
        OpenAiCompatible { config, client }
    }

    pub fn openrouter(model: impl Into<ModelId>, api_key: impl Into<String>) -> Self {
        OpenAiCompatible::new(OpenAiConfig::openrouter(model, api_key))
    }

    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    pub async fn complete(&self, mut request: ChatCompletionRequest) -> Result<Completion, ProviderError> {
        request.model = self.config.model.0.clone();
        request.stream = None;
        let url = format!("{}/chat/completions", self.config.endpoint.0.trim_end_matches('/'));
        let response = self
            .client
            .inner()
            .post(url)
            .bearer_auth(&self.config.api_key.0)
            .timeout(self.config.request_timeout)
            .json(&request)
            .send()
            .await
            .map_err(|_| ProviderError::Transport)?;
        if !response.status().is_success() {
            return Err(ProviderError::Status);
        }

        let mut response = response;
        let body = read_body_capped(&mut response, self.config.response_body_cap_bytes)
            .await
            .ok_or(ProviderError::Transport)?;
        if body.len() > self.config.response_body_cap_bytes {
            return Err(ProviderError::Malformed);
        }
        let parsed: ChatCompletionResponse = serde_json::from_slice(&body).map_err(|_| ProviderError::Malformed)?;
        let choice = parsed.choices.into_iter().next().ok_or(ProviderError::NoChoice)?;
        Ok(completion_of(choice.message))
    }
}

fn completion_of(message: WireMessage) -> Completion {
    Completion {
        content: message.content,
        tool_calls: message.tool_calls.unwrap_or_default(),
    }
}
