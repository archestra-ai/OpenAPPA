//! The `archestra` builtin: the rendered [`ModelPrompt`] posted to an endpoint the embedding
//! host supplies, which picks and calls the model itself. The host sees what an `llm`
//! provider would — the declaration in `system`, the redacted artifact in `input`, the
//! per-kind output schema in `schema` — plus, in `attribution`, the trajectory the consult
//! is made for, and answers with the schema object as the body.

use std::time::Duration;

use super::consult_with_retries;
use crate::config::{Endpoint, EndpointHost, EndpointToken, ModelLimits};
use crate::consult::ModelPrompt;
use crate::external::{ConsultGates, NoAnswerReason, RECORD_READ_GRACE, Transcript, error_line, read_body};
use appa_engine::value::TrajectoryId;
use appa_policy::AnnotatorBuiltin;

/// The trajectory a consult is made for, and the tool call it judges, if any. The host
/// files the model call it makes under the session and caller that own this trajectory.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct Attribution {
    pub(crate) root: TrajectoryId,
    pub(crate) trajectory: TrajectoryId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) call_id: Option<String>,
}

#[derive(Clone)]
struct Scope {
    context: Option<String>,
    attribution: Attribution,
}

// Scoped around the exact consult future, including retries and hedges; no spawned task
// or process-wide endpoint configuration carries another request's context or attribution.
tokio::task_local! { static SCOPE: Scope; }

pub(crate) async fn within<T>(context: Option<&str>, attribution: Attribution, future: impl Future<Output = T>) -> T {
    let scope = Scope {
        context: context.map(str::to_owned),
        attribution,
    };
    SCOPE.scope(scope, future).await
}

#[derive(serde::Serialize)]
struct Request<'a> {
    system: &'a str,
    input: &'a str,
    schema: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    attribution: Option<&'a Attribution>,
}

/// One client for the host's endpoint, shared by every `builtin = "archestra"` Annotator of
/// the deployment. Its permit pool is the runtime's `archestra` gate.
#[derive(Clone)]
pub struct ArchestraBackend {
    http: reqwest::Client,
    endpoint: Endpoint,
    timeout: Duration,
    max_body_bytes: usize,
    gates: ConsultGates,
}

impl ArchestraBackend {
    pub(crate) fn new(endpoint: Endpoint, max_body_bytes: usize, gates: &ConsultGates) -> ArchestraBackend {
        crate::tls::install_crypto_provider();
        let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        let builder = match endpoint.host() {
            EndpointHost::Loopback => builder.no_proxy(),
            EndpointHost::Remote => builder,
        };
        ArchestraBackend {
            http: builder
                .build()
                .expect("the reqwest client builds: the crypto provider is installed above"),
            endpoint,
            timeout: ModelLimits::MODEL_CALL.timeout,
            max_body_bytes,
            gates: gates.clone(),
        }
    }

    /// One consult, retried as [`consult_with_retries`] retries a transient failure.
    pub(crate) async fn consult(
        &self,
        prompt: &ModelPrompt,
        name: &str,
        seen: Option<&mut Transcript>,
    ) -> Result<serde_json::Value, NoAnswerReason> {
        consult_with_retries(
            &self.gates,
            AnnotatorBuiltin::Archestra,
            self.timeout,
            self.max_body_bytes,
            name,
            seen,
            || self.send(prompt),
        )
        .await
    }

    /// One request; the answer text is read to one byte past `max_body_bytes`, enough for
    /// the caller to call it oversized.
    async fn send(&self, prompt: &ModelPrompt) -> Result<String, NoAnswerReason> {
        let scope = SCOPE.try_with(Clone::clone).ok();
        let body = Request {
            system: &prompt.system,
            input: &prompt.input,
            schema: &prompt.schema,
            attribution: scope.as_ref().map(|scope| &scope.attribution),
        };
        let mut request = self.http.post(&self.endpoint.url).json(&body);
        match &self.endpoint.token {
            Some(EndpointToken::Set(token)) => request = request.bearer_auth(token.reveal()),
            Some(EndpointToken::Deferred) => return Err(NoAnswerReason::Unregistered),
            None => {}
        }
        if let Some(context) = scope.and_then(|scope| scope.context) {
            let mut value = reqwest::header::HeaderValue::from_str(&context).map_err(|_| NoAnswerReason::Malformed)?;
            value.set_sensitive(true);
            request = request.header("x-archestra-openappa-context", value);
        }
        let mut response = request.send().await.map_err(|error| {
            tracing::debug!(%error, "the archestra consult failed");
            NoAnswerReason::Transport
        })?;
        let status = response.status();
        if !status.is_success() {
            // The host says why it could not answer; read it for the record alone.
            let mut body = Vec::new();
            let _ = tokio::time::timeout(
                RECORD_READ_GRACE,
                read_body(&mut response, self.max_body_bytes as u64, &mut body),
            )
            .await;
            return Err(NoAnswerReason::NonSuccess {
                status: status.as_u16(),
                detail: host_message(&body),
            });
        }
        let mut text = Vec::new();
        while text.len() <= self.max_body_bytes {
            match response.chunk().await {
                Ok(Some(chunk)) => text.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(_) => return Err(NoAnswerReason::Transport),
            }
        }
        if text.len() > self.max_body_bytes {
            return Ok(String::from_utf8_lossy(&text).into_owned());
        }
        String::from_utf8(text).map_err(|_| NoAnswerReason::Malformed)
    }
}

/// What the host said about a consult it refused: the `message` of its error envelope
/// (`{"error":{"message":…}}`), else the last line of whatever it wrote; none when it
/// wrote nothing.
fn host_message(body: &[u8]) -> Option<String> {
    let envelope: Option<String> = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.get("message")?.as_str().map(str::to_string));
    let line = error_line(envelope.as_deref().unwrap_or(&String::from_utf8_lossy(body)));
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;

    use super::*;
    use crate::config::Token;
    use crate::model::MAX_ATTEMPTS;
    use crate::recorder::ConsultBackend;

    type Requests = Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>;

    async fn serve(status: StatusCode, body: &'static str) -> (String, Requests) {
        let requests: Requests = Arc::default();
        let seen = Arc::clone(&requests);
        let router = Router::new().route(
            "/annotate",
            post(move |headers: HeaderMap, body_text: String| {
                let seen = Arc::clone(&seen);
                async move {
                    let request = serde_json::from_str(&body_text).expect("the request is JSON");
                    seen.lock().unwrap().push((headers, request));
                    (status, body)
                }
            }),
        );
        let addr = crate::test_support::serve(router).await;
        (format!("http://{addr}/annotate"), requests)
    }

    fn backend(url: String, max_body_bytes: usize) -> ArchestraBackend {
        let endpoint = Endpoint::new(url, Some(EndpointToken::Set(Token::new("sekret".to_string()))));
        ArchestraBackend::new(endpoint, max_body_bytes, &ConsultGates::per_runtime())
    }

    fn prompt() -> ModelPrompt {
        ModelPrompt {
            system: "You rule on one call.".to_string(),
            input: "{\"args\":{}}".to_string(),
            schema: serde_json::json!({ "type": "object" }),
        }
    }

    #[tokio::test]
    async fn a_consult_posts_the_prompt_with_the_bearer_and_reads_the_body_as_the_answer() {
        let (url, requests) = serve(StatusCode::OK, "{\"ruling\":\"approve\"}").await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);

        let answer = backend(url, 65_536).consult(&prompt(), "judge", Some(&mut seen)).await;

        assert_eq!(answer, Ok(serde_json::json!({ "ruling": "approve" })));
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(headers["authorization"], "Bearer sekret");
        assert_eq!(
            *body,
            serde_json::json!({
                "system": prompt().system,
                "input": prompt().input,
                "schema": prompt().schema,
            })
        );
        assert_eq!(seen.raw_response.as_deref(), Some(&b"{\"ruling\":\"approve\"}"[..]));
        assert_eq!(seen.http_status, None);
        assert!(!headers.contains_key("x-archestra-openappa-context"));
    }

    #[tokio::test]
    async fn a_scoped_consult_names_its_trajectory_and_call_and_carries_the_host_context() {
        let (url, requests) = serve(StatusCode::OK, "{\"ruling\":\"approve\"}").await;
        let backend = backend(url, 65_536);
        let attribution = |call_id: Option<&str>| Attribution {
            root: TrajectoryId::new("archestra:root"),
            trajectory: TrajectoryId::new("archestra:child"),
            call_id: call_id.map(str::to_owned),
        };

        within(
            Some("token-1"),
            attribution(Some("toolu_1")),
            backend.consult(&prompt(), "judge", None),
        )
        .await
        .unwrap();
        within(None, attribution(None), backend.consult(&prompt(), "judge", None))
            .await
            .unwrap();

        let requests = requests.lock().unwrap().clone();
        let [(headers, body), (bare_headers, bare_body)] = &requests[..] else {
            panic!("two requests: {requests:?}");
        };
        assert_eq!(headers["x-archestra-openappa-context"], "token-1");
        assert_eq!(
            body["attribution"],
            serde_json::json!({
                "root": "archestra:root",
                "trajectory": "archestra:child",
                "call_id": "toolu_1",
            })
        );
        assert!(!bare_headers.contains_key("x-archestra-openappa-context"));
        assert_eq!(
            bare_body["attribution"],
            serde_json::json!({ "root": "archestra:root", "trajectory": "archestra:child" })
        );
    }

    #[tokio::test]
    async fn a_refusing_host_s_message_is_the_detail_and_the_diagnostics() {
        let (url, _) = serve(
            StatusCode::BAD_GATEWAY,
            "{\"error\":{\"message\":\"The model call failed: anthropic/claude-sonnet-5 was rejected upstream with 401: API key is invalid.\",\"type\":\"api_internal_server_error\"}}",
        )
        .await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);

        let answer = backend(url, 65_536).consult(&prompt(), "judge", Some(&mut seen)).await;

        let message =
            "The model call failed: anthropic/claude-sonnet-5 was rejected upstream with 401: API key is invalid.";
        assert_eq!(
            answer,
            Err(NoAnswerReason::NonSuccess {
                status: 502,
                detail: Some(message.to_string())
            })
        );
        assert_eq!(seen.http_status, Some(502));
        assert_eq!(
            seen.diagnostics.map(|diagnostics| diagnostics.bytes),
            Some(message.as_bytes().to_vec())
        );

        let (url, _) = serve(StatusCode::BAD_GATEWAY, "").await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", Some(&mut seen)).await,
            Err(NoAnswerReason::NonSuccess {
                status: 502,
                detail: None
            })
        );
        assert_eq!(seen.diagnostics, None, "a silent host leaves no diagnostics");
    }

    #[tokio::test]
    async fn every_endpoint_failure_is_no_answer() {
        let (url, requests) = serve(StatusCode::INTERNAL_SERVER_ERROR, "boom").await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", Some(&mut seen)).await,
            Err(NoAnswerReason::NonSuccess {
                status: 500,
                detail: Some("boom".to_string())
            })
        );
        assert_eq!(requests.lock().unwrap().len(), MAX_ATTEMPTS, "a 5xx is retried");
        assert_eq!(seen.http_status, Some(500));

        let (url, requests) = serve(StatusCode::BAD_REQUEST, "no").await;
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", None).await,
            Err(NoAnswerReason::NonSuccess {
                status: 400,
                detail: Some("no".to_string())
            })
        );
        assert_eq!(requests.lock().unwrap().len(), 1, "a 4xx is not retried");

        let (url, _) = serve(StatusCode::OK, "not json").await;
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", None).await,
            Err(NoAnswerReason::Malformed)
        );

        let (url, _) = serve(StatusCode::OK, "{\"ruling\":\"approve\",\"reason\":\"long enough\"}").await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);
        assert_eq!(
            backend(url, 16).consult(&prompt(), "judge", Some(&mut seen)).await,
            Err(NoAnswerReason::Oversized)
        );
        assert_eq!(seen.raw_response.map(|raw| raw.len()), Some(16));
    }
}
