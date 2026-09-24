//! OTLP setup adapted from Joey Orlando's exporter in PR #168.

use opentelemetry::{KeyValue, global, trace::TracerProvider};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, metrics::SdkMeterProvider, trace::SdkTracerProvider};
use tracing_subscriber::{EnvFilter, Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt};

pub(crate) struct Telemetry {
    providers: Option<(SdkTracerProvider, SdkLoggerProvider, SdkMeterProvider)>,
}

impl Telemetry {
    pub(crate) fn init(level: &str) -> Self {
        let stderr = tracing_subscriber::fmt::layer()
            .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)));
        let providers = if configured() {
            match Self::providers() {
                Ok(providers) => Some(providers),
                Err(_) => {
                    // Exporter errors can include credentials or endpoint query strings.
                    eprintln!("appa runtime: cannot initialize OTLP export; telemetry is disabled");
                    None
                }
            }
        } else {
            None
        };
        // Allowlist the target independently of RUST_LOG and -vv. Exporting the
        // ordinary subscriber would disclose values in diagnostic messages.
        let spans = providers.as_ref().map(|(tracer, _, _)| {
            tracing_opentelemetry::layer()
                .with_tracer(tracer.tracer("appa-runtime"))
                .with_filter(filter_fn(|metadata| metadata.target() == "appa_telemetry"))
        });
        let logs = providers.as_ref().map(|(_, logger, _)| {
            OpenTelemetryTracingBridge::new(logger)
                .with_filter(filter_fn(|metadata| metadata.target() == "appa_telemetry"))
        });
        tracing_subscriber::registry()
            .with(stderr)
            .with(spans)
            .with(logs)
            .init();
        if let Some((_, _, meter)) = &providers {
            global::set_meter_provider(meter.clone());
            let started = std::time::Instant::now();
            global::meter("appa-runtime")
                .u64_observable_gauge("appa.runtime.uptime")
                .with_unit("s")
                .with_callback(move |observer| observer.observe(started.elapsed().as_secs(), &[]))
                .build();
        }
        Self { providers }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.providers.is_some()
    }

    fn providers()
    -> Result<(SdkTracerProvider, SdkLoggerProvider, SdkMeterProvider), Box<dyn std::error::Error + Send + Sync>> {
        crate::tls::install_crypto_provider();
        let resource = Resource::builder()
            .with_service_name(std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "appa-runtime".into()))
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();
        // Construct all exporters before starting any background workers.
        let spans = opentelemetry_otlp::SpanExporter::builder().with_http().build()?;
        let logs = opentelemetry_otlp::LogExporter::builder().with_http().build()?;
        let metrics = opentelemetry_otlp::MetricExporter::builder().with_http().build()?;
        Ok((
            SdkTracerProvider::builder()
                .with_resource(resource.clone())
                .with_batch_exporter(spans)
                .build(),
            SdkLoggerProvider::builder()
                .with_resource(resource.clone())
                .with_batch_exporter(logs)
                .build(),
            SdkMeterProvider::builder()
                .with_resource(resource)
                .with_periodic_exporter(metrics)
                .build(),
        ))
    }

    /// Run on a blocking worker while the Tokio runtime still exists.
    pub(crate) fn shutdown(self) {
        if let Some((tracer, logger, meter)) = self.providers {
            let _ = meter.shutdown();
            let _ = logger.shutdown();
            let _ = tracer.shutdown();
        }
    }
}

fn configured() -> bool {
    !std::env::var("OTEL_SDK_DISABLED").is_ok_and(|value| value.eq_ignore_ascii_case("true"))
        && std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok_and(|value| !value.trim().is_empty())
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {},
                    _ = tokio::signal::ctrl_c() => {},
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
