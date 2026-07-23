//! The upstream inference client: the runtime's call *to* the model (OpenRouter or any
//! OpenAI-compatible endpoint).

use std::time::Duration;

use thiserror::Error;

use crate::tool::{HttpClient, read_body_capped};
use crate::wire::{ChatCompletionRequest, ChatCompletionResponse, WireMessage, WireToolCall};

const COMPLETION_BODY_CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<WireToolCall>,
}

impl Completion {
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// Why an inference round failed. All are fail-closed: the drive never treats a fault as a model
/// answer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InferenceError {
    #[error("inference transport fault or timeout")]
    Transport,
    #[error("inference endpoint returned a non-success status")]
    Status,
    #[error("inference response was oversized or malformed")]
    Malformed,
    #[error("inference response carried no choices")]
    NoChoice,
}

#[derive(Clone, Debug)]
pub struct Inference {
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
    client: HttpClient,
}

impl Inference {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
        client: HttpClient,
    ) -> Self {
        Inference {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            timeout,
            client,
        }
    }

    /// Run one inference round. `messages` and `tools` are built by the caller from server-held facts;
    /// this sets the configured model and posts to `{base_url}/chat/completions`.
    pub async fn complete(&self, mut request: ChatCompletionRequest) -> Result<Completion, InferenceError> {
        request.model = self.model.clone();
        request.stream = None;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .inner()
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(self.timeout)
            .json(&request)
            .send()
            .await
            .map_err(|_| InferenceError::Transport)?;
        if !response.status().is_success() {
            return Err(InferenceError::Status);
        }
        let mut response = response;
        let body = read_body_capped(&mut response, COMPLETION_BODY_CAP)
            .await
            .ok_or(InferenceError::Transport)?;
        if body.len() > COMPLETION_BODY_CAP {
            return Err(InferenceError::Malformed);
        }
        let parsed: ChatCompletionResponse = serde_json::from_slice(&body).map_err(|_| InferenceError::Malformed)?;
        let choice = parsed.choices.into_iter().next().ok_or(InferenceError::NoChoice)?;
        Ok(completion_of(choice.message))
    }
}

fn completion_of(message: WireMessage) -> Completion {
    Completion {
        content: message.content,
        tool_calls: message.tool_calls.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ChatCompletionResponse, WireFunctionCall, WireMessage, WireToolCall};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_model(response_body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut received = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
                if let Some(pos) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&received[..pos]).to_lowercase();
                    let len: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if received.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{response_body}",
                response_body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    fn inference(base_url: String) -> Inference {
        Inference::new(
            base_url,
            "test-key",
            "test-model",
            Duration::from_secs(5),
            HttpClient::new(),
        )
    }

    #[tokio::test]
    async fn reads_back_a_final_answer_and_sends_the_configured_model() {
        let body = serde_json::to_string(&ChatCompletionResponse::single(
            "cmpl-1",
            WireMessage::assistant("the pod is crashlooping"),
            "stop",
        ))
        .unwrap();
        let (base, handle) = spawn_model(body).await;
        let completion = inference(base)
            .complete(ChatCompletionRequest {
                model: String::new(),
                messages: vec![WireMessage::user("what is wrong?")],
                tools: None,
                stream: None,
            })
            .await
            .unwrap();
        assert_eq!(completion.content.as_deref(), Some("the pod is crashlooping"));
        assert!(!completion.has_tool_calls());

        let request = handle.await.unwrap();
        assert!(
            request.contains("Authorization: Bearer test-key") || request.contains("authorization: Bearer test-key")
        );
        assert!(request.contains("\"model\":\"test-model\""));
    }

    #[tokio::test]
    async fn reads_back_tool_calls() {
        let body = serde_json::to_string(&ChatCompletionResponse::single(
            "cmpl-2",
            WireMessage::assistant_tool_calls(vec![WireToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireFunctionCall {
                    name: "k8s_get_pod_logs".to_string(),
                    arguments: r#"{"pod":"checkout"}"#.to_string(),
                },
            }]),
            "tool_calls",
        ))
        .unwrap();
        let (base, handle) = spawn_model(body).await;
        let completion = inference(base)
            .complete(ChatCompletionRequest {
                model: String::new(),
                messages: vec![WireMessage::user("investigate")],
                tools: None,
                stream: None,
            })
            .await
            .unwrap();
        assert!(completion.has_tool_calls());
        assert_eq!(completion.tool_calls[0].function.name, "k8s_get_pod_logs");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn transport_fault_fails_closed() {
        let err = inference("http://127.0.0.1:1".to_string())
            .complete(ChatCompletionRequest {
                model: String::new(),
                messages: vec![WireMessage::user("hi")],
                tools: None,
                stream: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err, InferenceError::Transport);
    }
}
