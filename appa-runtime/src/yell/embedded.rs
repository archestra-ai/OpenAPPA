//! Reporting for an authenticated host that embeds the runtime in its own process.
use std::sync::Arc;

use super::{
    Mode, Selection, YellArgs, client,
    report::{Author, Harness, ReportRequest, YellMessage},
};
use crate::api::{Actor, Runtime};

/// Host-supplied context and tool arguments. The receiver and actor must come
/// from the embedding host, never from model-controlled tool arguments.
pub struct Request {
    pub actor: Actor,
    /// The name the receiver files this host's reports under: lowercase letters, digits
    /// and hyphens, as an adapter name is spelled.
    pub harness: &'static str,
    pub endpoint: String,
    /// Public deployment hostname. Do not supply a machine name or a full URL.
    pub hostname: Option<String>,
    pub message: String,
    pub with_trajectory: bool,
}

/// Send a report using the same field classification, size limits, signature,
/// and transport as the standalone runtime. Requires a released yell call from
/// this actor's hook and the deployment's explicit reporting opt-in.
pub async fn send(runtime: &Arc<Runtime>, request: Request) -> Result<String, String> {
    if !runtime.agent_yell() {
        return Err("Agent reporting is disabled".into());
    }
    if request.hostname.as_ref().is_some_and(|host| {
        host.is_empty()
            || host.len() > 253
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
    }) {
        return Err("Invalid reporting hostname".into());
    }
    let args = YellArgs {
        message: request.message,
        with_trajectory: request.with_trajectory,
    };
    let (acting, _) = runtime
        .take_vouched(&args.ticket())
        .map_err(|_| "No unambiguous released yell call exists for this request".to_string())?;
    if acting != request.actor {
        return Err("The yell call belongs to a different session".into());
    }
    let message = YellMessage::new(&args.message).map_err(|error| error.to_string())?;
    let receiver = client::Receiver::parse(&request.endpoint)
        .ok_or_else(|| "The reporting receiver must use HTTPS".to_string())?;
    let report = ReportRequest {
        message,
        author: Author::Agent,
        mode: Mode::Baseline,
        selection: if args.with_trajectory {
            Selection::Vouched(acting.root)
        } else {
            Selection::RulesOnly
        },
        harness: Harness::Embedded(request.harness),
        hostname: request.hostname,
    };
    let finished = runtime
        .report_off_thread(report)
        .await
        .map_err(|_| "The report is too large to send".to_string())?;
    let receipt = client::send(&finished, &receiver)
        .await
        .map_err(|error| error.to_string())?;
    Ok(receipt.receipt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, HostDefaults},
        hooks,
    };
    use appa_eventlog::{Backend, LogStore};
    use appa_runtime_api::{HookDecision, HookEvent, ProposedCall, TrajectoryId};
    use std::time::Duration;

    fn runtime(enabled: bool) -> Arc<Runtime> {
        let mut config = Config::hosted(
            "[policy]\nversion=2\n[[policy.tool]]\nname='yell'\ndelta={}\n",
            HostDefaults {
                consult_timeout: Duration::from_secs(1),
                max_body_bytes: 4096,
            },
        )
        .unwrap();
        config.reporting.agent_yell = enabled;
        Arc::new(Runtime::open_with_store(config, Arc::new(LogStore::open(Backend::Memory).unwrap()), None).unwrap())
    }

    fn request() -> Request {
        Request {
            actor: Actor {
                root: TrajectoryId("private-session-id".into()),
                child: None,
            },
            harness: "test-platform",
            endpoint: "http://127.0.0.1:1".into(),
            hostname: Some("platform.example.com".into()),
            message: "The feedback is confusing".into(),
            with_trajectory: true,
        }
    }

    async fn release(runtime: &Runtime, request: &Request) {
        runtime.create_session(request.actor.root.clone()).unwrap();
        let call = ProposedCall {
            tool: "yell".into(),
            arguments: serde_json::value::to_raw_value(
                &serde_json::json!({ "message": request.message, "with_trajectory": request.with_trajectory }),
            )
            .unwrap(),
        };
        assert!(matches!(
            hooks::handle(
                runtime,
                HookEvent::ToolCall {
                    actor: request.actor.clone(),
                    call,
                    call_id: None,
                    spawn: false,
                    ruling: None
                }
            )
            .await,
            HookDecision::AllowCall { .. }
        ));
    }

    #[tokio::test]
    async fn refuses_disabled_unvouched_and_cross_session_reports() {
        assert!(send(&runtime(false), request()).await.unwrap_err().contains("disabled"));
        let runtime = runtime(true);
        let mut invalid = request();
        invalid.hostname = Some("https://user:secret@example.com/path".into());
        assert!(send(&runtime, invalid).await.unwrap_err().contains("hostname"));
        assert!(send(&runtime, request()).await.unwrap_err().contains("released"));
        let mut request = request();
        release(&runtime, &request).await;
        request.actor.root = TrajectoryId("other-session".into());
        assert!(send(&runtime, request).await.unwrap_err().contains("different session"));
    }

    #[tokio::test]
    async fn sends_a_filtered_signed_report_and_consumes_the_release() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                    tx.send((headers, body)).await.unwrap();
                    axum::Json(serde_json::json!({"receipt_id":"test-receipt","duplicate":false}))
                },
            ),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let runtime = runtime(true);
        let mut request = request();
        request.endpoint = format!("http://{address}/");
        release(&runtime, &request).await;
        assert_eq!(send(&runtime, request).await.unwrap(), "test-receipt");
        let (headers, body) = rx.recv().await.unwrap();
        assert_eq!(headers["content-encoding"], "gzip");
        assert!(headers["x-appa-signature"].to_str().unwrap().starts_with("v1="));
        let mut plain = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(body.as_ref()), &mut plain).unwrap();
        let document: serde_json::Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(document["runtime"]["serving"]["harness"], "test-platform");
        assert_eq!(document["schema"], "openappa.yell.v1");
        assert_eq!(document["runtime"]["serving"]["hostname"], "platform.example.com");
        assert!(!plain.contains("private-session-id"));
        assert!(
            send(&runtime, super::tests::request())
                .await
                .unwrap_err()
                .contains("released")
        );
        server.abort();
    }
}
