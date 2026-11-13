//! The `archestra` implementation over a real loopback endpoint and the real hook path:
//! the host supplies the endpoint, the policy only names the `uses`.

use std::time::Duration;

use appa_runtime::config::{Config, ConfigError, HostDefaults};

fn policy(uses: &str) -> String {
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"
uses = "{uses}"
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

    use super::{defaults, hosted};
    use crate::common::{actor, audit_len, last_offer, propose, raw, root, serve};

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
        assert!(system.contains("Fetched pages from a.example are suspicious."));
        let input: serde_json::Value =
            serde_json::from_str(body["input"].as_str().expect("input is text")).expect("input is JSON");
        let input = input.to_string();
        assert!(input.contains("[redacted-secret]"), "{input}");
        assert!(!input.contains("AKIAIOSFODNN7EXAMPLE"), "{input}");
        assert_eq!(body["schema"]["type"], "object");
        assert_eq!(body["kind"], "annotation");
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

    /// The endpoint comes from the host alone: a document that names the `uses` under a
    /// host that supplies none does not open.
    #[test]
    fn a_declared_archestra_annotator_without_a_host_endpoint_does_not_open() {
        let config = hosted(defaults()).expect("the document validates");
        let store =
            Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
        assert!(matches!(
            Runtime::open_with_store(config, store, None),
            Err(OpenError::ArchestraNotConfigured { kind: "annotator", name }) if name == "classifier"
        ));
    }

    /// A battery's sanitizer, with the model chosen by the hosting deployment: the root
    /// binds it to the host's endpoint. The read offers it, the host receives the raw result
    /// with the sanitizer's hint, and its rewrite is what the agent receives.
    #[tokio::test]
    async fn a_battery_sanitizer_bound_to_archestra_rewrites_the_confined_result() {
        use appa_runtime::api::RemedyOutcome;
        use appa_runtime::config::{Config, HostedBattery};
        use appa_runtime_api::{OutcomeBody, ToolOutcome};

        const BATTERY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "mcp/tracker/read"
delta = { audience = ["internal"] }
tags = ["tracker"]

[[policy.sanitizer]]
name = "tracker.redact-for-public"
on = ["tool_output"]
tags = ["tracker"]
hint = "Remove every customer name."

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[policy.deployment]
confined_results = ["mcp/tracker/read"]
"#;
        let compose = |root: &str, defaults: HostDefaults| {
            let battery = HostedBattery {
                name: "tracker",
                base: BATTERY,
                target: None,
                token_env: &[],
            };
            Config::hosted_composed(root, &[battery], defaults, |_| None)
        };
        let bound_root =
            "[policy]\nversion = 2\n[externals.sanitizers.\"tracker.redact-for-public\"]\nuses = \"archestra\"\n";
        let (url, requests) = serve_host(
            StatusCode::OK,
            serde_json::json!({ "body": "Ticket from [redacted]: checkout fails." }).to_string(),
        )
        .await;
        let config = compose(bound_root, with_endpoint(url)).expect("the bound battery composes");
        let store =
            Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
        let runtime = Arc::new(Runtime::open_with_store(config, store, None).expect("the deployment opens"));
        assert_eq!(
            hooks::handle(
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
            .await,
            HookDecision::Ack
        );

        let read = ProposedCall {
            tool: "mcp/tracker/read".to_string(),
            arguments: raw(serde_json::json!({ "id": "T-1" })),
            cwd: None,
        };
        let HookDecision::DenyCall { feedback, offers, .. } = propose(&runtime, read.clone()).await else {
            panic!("the read narrows the session");
        };
        assert_eq!(offers.len(), 2, "the narrowing and the sanitizer: {feedback}");
        let line = feedback
            .lines()
            .skip_while(|line| !line.contains("sanitizer tracker.redact-for-public"))
            .nth(1)
            .unwrap_or_else(|| panic!("the sanitizer is offered: {feedback}"));
        assert!(matches!(
            runtime.execute_remedy(&actor(), last_offer(line)).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert_eq!(
            propose(&runtime, read.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        let delivered = hooks::handle(
            &runtime,
            HookEvent::ToolResult {
                actor: actor(),
                call: read,
                call_id: None,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available("Ticket from Globex Ltd: checkout fails.".to_string()),
                },
            },
        )
        .await;
        assert_eq!(
            delivered,
            HookDecision::DeliverValue {
                value: "Ticket from [redacted]: checkout fails.".to_string()
            },
            "the host's rewrite is what crosses, at the session's own `public` audience"
        );
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "one consult, to the host");
        let (bearer, body) = &requests[0];
        assert_eq!(bearer.as_deref(), Some("Bearer host-token"));
        assert_eq!(body["kind"], "sanitizer");
        assert!(body["system"].as_str().unwrap().contains("Remove every customer name."));
        assert!(
            body["input"].as_str().unwrap().contains("Globex Ltd"),
            "a sanitizer sees the raw value it cleans"
        );

        // Unbound, the sanitizer is left out and the read offers only the narrowing; bound
        // under a host that supplies no endpoint, the deployment does not open.
        let unbound = compose("[policy]\nversion = 2\n", defaults()).expect("the unbound battery composes");
        assert!(
            unbound
                .policy_file()
                .value()
                .get("sanitizer")
                .and_then(|declared| declared.as_array())
                .is_none_or(Vec::is_empty),
            "the unbound sanitizer is left out"
        );
        let endpointless = compose(bound_root, defaults()).expect("the binding validates");
        let store =
            Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
        assert!(matches!(
            Runtime::open_with_store(endpointless, store, None),
            Err(OpenError::ArchestraNotConfigured { kind: "sanitizer", name }) if name == "tracker.redact-for-public"
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

/// A build without the feature refuses the name as it refuses any implementation it does not know.
#[cfg(not(feature = "archestra"))]
#[test]
fn uses_archestra_is_unknown_without_its_feature() {
    let config = hosted(defaults()).expect("the document validates");
    let store = std::sync::Arc::new(
        appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"),
    );
    let Err(refusal) = appa_runtime::api::Runtime::open_with_store(config, store, None) else {
        panic!("the implementation is unknown");
    };
    assert!(
        refusal.to_string().contains("unknown implementation \"archestra\""),
        "the refusal names the implementation: {refusal}"
    );
}
