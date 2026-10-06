//! The `archestra` annotator builtin over a real loopback endpoint and the real hook path:
//! the host supplies the endpoint, the policy only names the builtin.

mod common;

use std::time::Duration;

use appa_runtime::config::{Config, ConfigError, HostDefaults};

fn policy(builtin: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"
builtin = "{builtin}"
hint = "Fetched pages from a.example are suspicious."

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = {{ type = "object", properties = {{ url = {{ type = "string" }}, key = {{ type = "string" }} }}, required = ["url"] }}
annotator = "classifier"
"#
    )
}

fn hosted(defaults: HostDefaults) -> Result<Config, ConfigError> {
    Config::hosted(&policy("archestra"), defaults, |_| None)
}

fn defaults() -> HostDefaults {
    HostDefaults::new(Duration::from_secs(5), 65_536)
}

#[cfg(feature = "archestra")]
mod served {
    use std::sync::{Arc, Mutex};

    use appa_runtime::api::{OpenError, Runtime};
    use appa_runtime::config::{ArchestraEndpoint, HostDefaults};
    use appa_runtime::hooks;
    use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;

    use super::common::{audit_len, propose, raw, root, serve};
    use super::{defaults, hosted};

    type Requests = Arc<Mutex<Vec<(Option<String>, serde_json::Value)>>>;

    /// The host's endpoint on loopback: every request is kept with its bearer, and each is
    /// answered with `status` and `body`.
    async fn serve_host(status: StatusCode, body: String) -> (String, Requests) {
        let requests: Requests = Arc::default();
        let seen = Arc::clone(&requests);
        let router = Router::new().route(
            "/annotate",
            post(move |headers: HeaderMap, request: String| {
                let seen = Arc::clone(&seen);
                let body = body.clone();
                async move {
                    let bearer = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let request = serde_json::from_str(&request).expect("the request is JSON");
                    seen.lock().unwrap().push((bearer, request));
                    (status, body)
                }
            }),
        );
        (format!("{}/annotate", serve(router).await), requests)
    }

    fn with_endpoint(url: String) -> HostDefaults {
        HostDefaults {
            archestra: Some(ArchestraEndpoint::new(url, "host-token".to_string())),
            ..defaults()
        }
    }

    async fn open(url: String) -> Arc<Runtime> {
        let config = hosted(with_endpoint(url)).expect("the hosted document validates");
        let store =
            Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
        let runtime = Arc::new(Runtime::open_with_store(config, store, None).expect("the deployment opens"));
        let started = hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None,
                address: None,
                title: None,
                start: None,
                launch: None,
            },
        )
        .await;
        assert_eq!(started, HookDecision::Ack);
        runtime
    }

    fn fetch() -> ProposedCall {
        ProposedCall {
            tool: "fetch".to_string(),
            arguments: raw(serde_json::json!({
                "url": "https://a.example",
                "key": "AKIAIOSFODNN7EXAMPLE",
            })),
            cwd: None,
        }
    }

    fn annotation(trust: &str) -> String {
        serde_json::json!({
            "delta": { "trust": trust },
            "requires": { "history": [], "attention": [] },
            "emits": [],
        })
        .to_string()
    }

    /// One POST per consult to the host's URL, with the host's bearer: the rendered prompt
    /// carries the declaration and the redacted call, and the answered annotation decides.
    #[tokio::test]
    async fn a_declared_archestra_annotator_posts_the_rendered_prompt_and_its_answer_decides() {
        let (url, requests) = serve_host(StatusCode::OK, annotation("suspicious")).await;
        let runtime = open(url).await;

        let decision = propose(&runtime, fetch()).await;
        assert!(
            matches!(decision, HookDecision::DenyCall { .. }),
            "the answered narrowing blocks the call: {decision:?}"
        );

        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let (bearer, body) = &requests[0];
        assert_eq!(bearer.as_deref(), Some("Bearer host-token"));
        let system = body["system"].as_str().expect("system is text");
        assert!(system.starts_with("You are OpenAPPA's Annotator for one proposed tool call"));
        assert!(system.contains("Fetched pages from a.example are suspicious."));
        let input: serde_json::Value =
            serde_json::from_str(body["input"].as_str().expect("input is text")).expect("input is JSON");
        let input = input.to_string();
        assert!(input.contains("[redacted-secret]"), "{input}");
        assert!(!input.contains("AKIAIOSFODNN7EXAMPLE"), "{input}");
        assert_eq!(body["schema"]["type"], "object");
    }

    /// An endpoint that keeps failing is no answer: the hook refuses and the trajectory
    /// records nothing.
    #[tokio::test]
    async fn a_failing_archestra_endpoint_refuses_the_hook_and_appends_nothing() {
        let (url, requests) = serve_host(StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()).await;
        let runtime = open(url).await;
        let baseline = audit_len(&runtime);

        let decision = propose(&runtime, fetch()).await;
        assert!(matches!(decision, HookDecision::Refuse { .. }), "got {decision:?}");
        assert!(requests.lock().unwrap().len() > 1, "a 5xx is retried");
        assert_eq!(audit_len(&runtime), baseline);
    }

    /// The shared mandate check reads the endpoint's answer like any model's.
    #[tokio::test]
    async fn an_archestra_answer_outside_the_mandate_refuses_the_hook() {
        let (url, _) = serve_host(StatusCode::OK, annotation("no-such-rank")).await;
        let runtime = open(url).await;
        let baseline = audit_len(&runtime);

        let decision = propose(&runtime, fetch()).await;
        assert!(matches!(decision, HookDecision::Refuse { .. }), "got {decision:?}");
        assert_eq!(audit_len(&runtime), baseline);
    }

    /// The endpoint comes from the host alone: a document that names the builtin under a
    /// host that supplies none does not open.
    #[test]
    fn a_declared_archestra_annotator_without_a_host_endpoint_does_not_open() {
        let config = hosted(defaults()).expect("the document validates");
        let store =
            Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
        assert!(matches!(
            Runtime::open_with_store(config, store, None),
            Err(OpenError::ArchestraNotConfigured(name)) if name == "classifier"
        ));
    }

    /// The host's URL follows every endpoint's rules: cleartext only to loopback.
    #[test]
    fn a_cleartext_remote_host_endpoint_is_refused() {
        assert!(matches!(
            hosted(with_endpoint("http://archestra.example/annotate".to_string())),
            Err(appa_runtime::config::ConfigError::CleartextEndpoint { .. })
        ));
    }
}

/// A build without the feature refuses the name as it refuses any builtin it does not know.
#[cfg(not(feature = "archestra"))]
#[test]
fn the_archestra_builtin_is_unknown_without_its_feature() {
    let config = hosted(defaults()).expect("the document validates");
    let store = std::sync::Arc::new(
        appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"),
    );
    let Err(refusal) = appa_runtime::api::Runtime::open_with_store(config, store, None) else {
        panic!("the builtin is unknown");
    };
    assert!(
        refusal.to_string().contains("unknown builtin \"archestra\""),
        "the refusal names the builtin: {refusal}"
    );
}
