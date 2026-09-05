//! Operational telemetry for the runtime process.
//!
//! The engine stays unaware of exporters. This module projects the runtime's
//! existing `tracing` spans and events to OTLP when an OTLP endpoint is
//! configured, and leaves the ordinary stderr subscriber unchanged otherwise.

use std::sync::LazyLock;
use std::time::Instant;

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{Aggregation, Instrument, SdkMeterProvider, Stream};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::fmt::Layer as FmtLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt, util::SubscriberInitExt};

/// Providers kept alive for as long as the runtime serves. Dropping an SDK
/// provider does not flush it, so `shutdown` is explicit on an orderly exit.
pub(crate) struct Telemetry {
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Telemetry {
    /// Install stderr logging and, when an OTLP endpoint is configured, the
    /// non-blocking trace, log, and metric exporters.
    pub(crate) fn init(level: &str) -> Self {
        if !otlp_configured() {
            tracing_subscriber::registry().with(stderr_layer(level)).init();
            return Self::disabled();
        }

        match Self::init_otlp(level) {
            Ok(telemetry) => telemetry,
            Err(error) => {
                eprintln!("appa runtime: OTLP telemetry is disabled: {error}");
                tracing_subscriber::registry().with(stderr_layer(level)).init();
                Self::disabled()
            }
        }
    }

    /// Flush all three signals without allowing exporter failure to change the
    /// runtime's exit status.
    pub(crate) fn shutdown(self) {
        if let Some(provider) = self.meter {
            let _ = provider.shutdown();
        }
        if let Some(provider) = self.logger {
            let _ = provider.shutdown();
        }
        if let Some(provider) = self.tracer {
            let _ = provider.shutdown();
        }
    }

    fn init_otlp(level: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        use opentelemetry::trace::TracerProvider as _;

        crate::tls::install_crypto_provider();
        let resource = Resource::builder()
            .with_service_name("appa-runtime")
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();

        let span_exporter = opentelemetry_otlp::SpanExporter::builder().with_http().build()?;
        let log_exporter = opentelemetry_otlp::LogExporter::builder().with_http().build()?;
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder().with_http().build()?;

        let tracer = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .build();
        let logger = SdkLoggerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(log_exporter)
            .build();
        let meter = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_periodic_exporter(metric_exporter)
            .with_view(hook_duration_view)
            .build();

        global::set_meter_provider(meter.clone());
        register_runtime_uptime();
        let trace_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer.tracer("appa-runtime"))
            .with_filter(otel_filter(level));
        let log_layer = OpenTelemetryTracingBridge::new(&logger).with_filter(otel_filter(level));

        let subscriber_result = tracing_subscriber::registry()
            .with(stderr_layer(level))
            .with(trace_layer)
            .with(log_layer)
            .try_init();

        if let Err(error) = subscriber_result {
            let _ = meter.shutdown();
            let _ = logger.shutdown();
            let _ = tracer.shutdown();
            return Err(Box::new(error));
        }

        Ok(Self {
            tracer: Some(tracer),
            logger: Some(logger),
            meter: Some(meter),
        })
    }

    fn disabled() -> Self {
        Self {
            tracer: None,
            logger: None,
            meter: None,
        }
    }
}

/// Record only bounded labels. Trajectory and tool identity belong on spans
/// and logs, never metric dimensions.
pub(crate) fn record_hook(event: &'static str, decision: &'static str, elapsed_seconds: f64) {
    let attributes = [
        KeyValue::new("appa.hook.event", event),
        KeyValue::new("appa.decision", decision),
    ];
    HOOKS.add(1, &attributes);
    HOOK_DURATION.record(elapsed_seconds, &attributes);
}

fn otlp_configured() -> bool {
    otlp_configured_with(|name| std::env::var_os(name))
}

fn otlp_configured_with(mut value: impl FnMut(&str) -> Option<std::ffi::OsString>) -> bool {
    [
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    ]
    .into_iter()
    .any(|name| value(name).is_some_and(|value| !value.is_empty()))
}

fn stderr_layer(level: &str) -> Filtered<FmtLayer<Registry>, EnvFilter, Registry> {
    fmt::layer().with_filter(runtime_filter(level))
}

fn otel_filter(level: &str) -> EnvFilter {
    runtime_filter(level)
}

fn runtime_filter(level: &str) -> EnvFilter {
    EnvFilter::new(level)
        .add_directive("opentelemetry=off".parse().expect("the OpenTelemetry filter is valid"))
        .add_directive(
            "opentelemetry_sdk=off"
                .parse()
                .expect("the OpenTelemetry SDK filter is valid"),
        )
        .add_directive("opentelemetry_otlp=off".parse().expect("the OTLP filter is valid"))
        .add_directive("hyper=off".parse().expect("the Hyper filter is valid"))
        .add_directive("hyper_util=off".parse().expect("the Hyper utility filter is valid"))
        .add_directive("reqwest=off".parse().expect("the reqwest filter is valid"))
}

fn hook_duration_view(instrument: &Instrument) -> Option<Stream> {
    if instrument.name() != "appa.runtime.hook.duration" {
        return None;
    }
    Some(
        Stream::builder()
            .with_aggregation(Aggregation::ExplicitBucketHistogram {
                boundaries: vec![
                    0.000_1, 0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                ],
                record_min_max: true,
            })
            .build()
            .expect("hook latency histogram view is valid"),
    )
}

fn register_runtime_uptime() {
    let started_at = Instant::now();
    global::meter("appa-runtime")
        .u64_observable_gauge("appa.runtime.uptime")
        .with_description("Seconds since the OpenAPPA runtime started")
        .with_unit("s")
        .with_callback(move |observer| observer.observe(started_at.elapsed().as_secs(), &[]))
        .build();
}

static HOOKS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("appa-runtime")
        .u64_counter("appa.runtime.hook.requests")
        .with_description("Hook decisions made by the OpenAPPA runtime")
        .build()
});

static HOOK_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("appa-runtime")
        .f64_histogram("appa.runtime.hook.duration")
        .with_description("Elapsed time to answer an OpenAPPA hook")
        .with_unit("s")
        .build()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otlp_requires_a_nonempty_endpoint() {
        assert!(!otlp_configured_with(|_| None));
        assert!(otlp_configured_with(|name| {
            (name == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| "http://127.0.0.1:4318".into())
        }));
        assert!(!otlp_configured_with(|_| Some("".into())));
    }
}
