//! The `archestra` builtin: the rendered [`ModelPrompt`] posted to an endpoint the embedding
//! host supplies, which picks and calls the model itself. The host sees what an `llm`
//! provider would — the declaration in `system`, the redacted artifact in `input`, the
//! per-kind output schema in `schema` — and answers with the schema object as the body.

use std::time::Duration;

use super::consult_with_retries;
use crate::config::{Endpoint, EndpointHost, EndpointToken, ModelLimits};
use crate::consult::ModelPrompt;
use crate::external::{ConsultGates, NoAnswerReason, Transcript};
use appa_policy::AnnotatorBuiltin;

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
        let body = serde_json::json!({
            "system": prompt.system,
            "input": prompt.input,
            "schema": prompt.schema,
        });
        let mut request = self.http.post(&self.endpoint.url).json(&body);
        match &self.endpoint.token {
            Some(EndpointToken::Set(token)) => request = request.bearer_auth(token.reveal()),
            Some(EndpointToken::Deferred) => return Err(NoAnswerReason::Unregistered),
            None => {}
        }
        let mut response = request.send().await.map_err(|error| {
            tracing::debug!(%error, "the archestra consult failed");
            NoAnswerReason::Transport
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(NoAnswerReason::NonSuccess {
                status: status.as_u16(),
                detail: None,
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
    }

    #[tokio::test]
    async fn every_endpoint_failure_is_no_answer() {
        let (url, requests) = serve(StatusCode::INTERNAL_SERVER_ERROR, "boom").await;
        let mut seen = Transcript::of(ConsultBackend::Archestra);
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", Some(&mut seen)).await,
            Err(NoAnswerReason::NonSuccess {
                status: 500,
                detail: None
            })
        );
        assert_eq!(requests.lock().unwrap().len(), MAX_ATTEMPTS, "a 5xx is retried");
        assert_eq!(seen.http_status, Some(500));

        let (url, requests) = serve(StatusCode::BAD_REQUEST, "no").await;
        assert_eq!(
            backend(url, 65_536).consult(&prompt(), "judge", None).await,
            Err(NoAnswerReason::NonSuccess {
                status: 400,
                detail: None
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
