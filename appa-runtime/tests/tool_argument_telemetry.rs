use std::ffi::OsString;

use appa_runtime::api::Runtime;
use appa_runtime::config::Config;
use appa_runtime::hooks::answer;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::layer::SubscriberExt as _;

#[test]
fn opted_in_arguments_leave_on_spans_but_not_logs() {
    let _capture = ScopedCaptureOptIn::new();
    let span_exporter = InMemorySpanExporter::default();
    let tracer_provider = SdkTracerProvider::builder()
        .with_simple_exporter(span_exporter.clone())
        .build();
    let log_exporter = InMemoryLogExporter::default();
    let logger_provider = SdkLoggerProvider::builder()
        .with_simple_exporter(log_exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer("appa-runtime-test")))
        .with(OpenTelemetryTracingBridge::new(&logger_provider));
    let _subscriber = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config_path = dir.path().join("appa.toml");
    std::fs::write(
        &config_path,
        r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "Bash"

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#,
    )
    .expect("the fixture config writes");
    let config = Config::load(&config_path).expect("the fixture config loads");
    let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the fixture runtime opens");
    let codec = appa_adapter_claude_code::codec();
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the test runtime builds");
    tokio.block_on(async {
        let start = br#"{"hook_event_name":"SessionStart","session_id":"telemetry-test"}"#;
        assert_eq!(answer(&runtime, &codec, start).await.0, 200);
        let call = br#"{"hook_event_name":"PreToolUse","session_id":"telemetry-test","tool_name":"Bash","tool_input":{"command":"git status","secret":"span-only"}}"#;
        assert_eq!(answer(&runtime, &codec, call).await.0, 200);
    });
    drop(_subscriber);
    tracer_provider.force_flush().expect("the spans flush");
    logger_provider.force_flush().expect("the logs flush");

    let spans = span_exporter
        .get_finished_spans()
        .expect("the exported spans are readable");
    let tool_call = spans
        .iter()
        .find(|span| span_attribute(span, "appa.hook.event").as_deref() == Some("tool_call"))
        .expect("the tool-call span exported");
    assert_eq!(
        span_attribute(tool_call, "gen_ai.tool.call.arguments").as_deref(),
        Some(r#"{"command":"git status","secret":"span-only"}"#)
    );

    let logs = log_exporter.get_emitted_logs().expect("the exported logs are readable");
    assert!(logs.iter().any(|log| log.record.trace_context().is_some()));
    for log in logs {
        assert!(
            log.record
                .attributes_iter()
                .all(|(key, _)| key.as_str() != "gen_ai.tool.call.arguments")
        );
        assert!(!format!("{:?}", log.record.body()).contains("span-only"));
    }
}

fn span_attribute(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.as_str().into_owned())
}

struct ScopedCaptureOptIn {
    previous: Option<OsString>,
}

impl ScopedCaptureOptIn {
    fn new() -> Self {
        let previous = std::env::var_os(CAPTURE_TOOL_ARGUMENTS_ENV);
        // This integration-test executable contains one synchronous test. It
        // changes the variable before it creates a runtime or telemetry task.
        unsafe { std::env::set_var(CAPTURE_TOOL_ARGUMENTS_ENV, "true") };
        Self { previous }
    }
}

impl Drop for ScopedCaptureOptIn {
    fn drop(&mut self) {
        // The test remains single-threaded until this guard restores the value.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(CAPTURE_TOOL_ARGUMENTS_ENV, previous),
                None => std::env::remove_var(CAPTURE_TOOL_ARGUMENTS_ENV),
            }
        }
    }
}

const CAPTURE_TOOL_ARGUMENTS_ENV: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";
