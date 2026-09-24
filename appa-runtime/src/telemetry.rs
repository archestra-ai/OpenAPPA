//! Value-safe operational telemetry. Exporters belong to the daemon, never the engine.
//! Only the `appa_telemetry` target leaves the process. Ordinary diagnostics can
//! contain values and remain on stderr, even when verbose logging is enabled.

use opentelemetry::{KeyValue, global};

use crate::api::{EventError, ToolCallDecision};
use crate::events::{ControlOutcome, ExternalOutcome, RuntimeEvent};

#[cfg(feature = "daemon")]
mod exporter;
#[cfg(feature = "daemon")]
pub(crate) use exporter::{Telemetry, shutdown_signal};

/// Bound caller-controlled identifiers without splitting UTF-8.
pub(crate) fn name(value: &str) -> &str {
    let mut end = value.len().min(256);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn count(metric: &'static str, attributes: &[KeyValue]) {
    global::meter("appa-runtime")
        .u64_counter(metric)
        .build()
        .add(1, attributes);
}

fn duration(metric: &'static str, seconds: f64, attributes: &[KeyValue]) {
    global::meter("appa-runtime")
        .f64_histogram(metric)
        .with_unit("s")
        .with_boundaries(vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 30.0])
        .build()
        .record(seconds, attributes);
}

pub(crate) fn policy(result: &Result<ToolCallDecision, EventError>, tool: &str, seconds: f64) {
    let outcome = match result {
        Ok(ToolCallDecision::Allow { .. }) => "allowed",
        Ok(ToolCallDecision::Deny { .. }) => "denied",
        Err(_) => "error",
    };
    let labels = [KeyValue::new("outcome", outcome)];
    duration("appa.policy.check.duration", seconds, &labels);
    tracing::Span::current().record("appa.outcome", outcome);
    match result {
        Ok(decision) => {
            count("appa.policy.decisions", &labels);
            let offers: Vec<&str> = match decision {
                ToolCallDecision::Deny { offers, .. } => {
                    offers.iter().take(32).map(|offer| offer.id.as_str()).collect()
                }
                ToolCallDecision::Allow { .. } => Vec::new(),
            };
            // Feedback, reviews, and display text can contain arguments. Never export them.
            tracing::info!(target: "appa_telemetry", {
                "appa.event.name" = "appa.policy.decision", "appa.tool.name" = name(tool),
                "appa.outcome" = outcome, "appa.offer.ids" = ?offers,
                "appa.duration.seconds" = seconds }, "policy check completed");
        }
        Err(error) => failure("policy_check", error_class(error)),
    }
}

fn error_class(error: &EventError) -> &'static str {
    match error {
        EventError::Storage(_) => "storage",
        EventError::UntrustedLog(_) => "untrusted_log",
        EventError::PolicyUnavailable(_) => "policy_unavailable",
        EventError::InventoryRefused(_) => "inventory_refused",
        EventError::EngineInvariant(_) | EventError::UnexpectedDecision => "engine_invariant",
        EventError::Contended { .. } => "contended",
        EventError::ResolutionDiverged { .. } => "resolution_diverged",
        EventError::AnnotationRefused { .. } => "annotation_refused",
        EventError::UndeclaredTool { .. } => "undeclared_tool",
        EventError::UndeclaredSpawn { .. } => "undeclared_spawn",
        EventError::MalformedPrincipal(_) | EventError::PrincipalMismatch => "principal",
        EventError::CallOutstanding | EventError::SpawnOutstanding | EventError::ChildDispatchOpen => {
            "outstanding_call"
        }
        EventError::CallIdReused => "call_id_reused",
        EventError::TrajectoryEnded => "trajectory_ended",
        EventError::UnknownTrajectory | EventError::TrajectoryExists => "trajectory",
        EventError::UnknownDispatch | EventError::OutcomeMismatch => "dispatch",
        EventError::UnknownOffer | EventError::RemedyArguments { .. } => "remedy",
        EventError::NotAChild
        | EventError::SpawnNotTaken
        | EventError::SpawnAmbiguous
        | EventError::BindingMismatch => "spawn",
    }
}

fn failure(component: &'static str, class: &str) {
    count(
        "appa.runtime.failures",
        &[
            KeyValue::new("component", component),
            KeyValue::new("error_type", class.to_owned()),
        ],
    );
    tracing::warn!(target: "appa_telemetry", { "appa.event.name" = "appa.runtime.failure",
        "appa.component" = component, "appa.error.type" = class }, "runtime flow refused");
}

/// Closed event classes only. No debug rendering of errors or external responses.
pub(crate) fn runtime_event(root: Option<&appa_runtime_api::TrajectoryId>, event: &RuntimeEvent) {
    let root = root.map_or("", |root| name(&root.0));
    match event {
        RuntimeEvent::External {
            role,
            name: external,
            outcome,
            duration_ms,
            ..
        } => {
            let role = spelling(role);
            let (outcome, error) = match outcome {
                ExternalOutcome::Answered => ("answered", String::new()),
                ExternalOutcome::NoAnswer(class) => ("no_answer", spelling(class)),
            };
            let labels = [KeyValue::new("role", role.clone()), KeyValue::new("outcome", outcome)];
            count("appa.external.calls", &labels);
            duration("appa.external.call.duration", *duration_ms as f64 / 1000.0, &labels);
            tracing::info!(target: "appa_telemetry", { "appa.event.name" = "appa.external.completed",
                "appa.trajectory.root" = root, "appa.external.name" = name(external), "appa.external.role" = role,
                "appa.outcome" = outcome, "appa.error.type" = error, "appa.duration.ms" = duration_ms },
                "external call completed");
        }
        RuntimeEvent::Control {
            call,
            outcome,
            duration_ms,
        } => {
            let outcome = match outcome {
                ControlOutcome::Executed => "executed",
                ControlOutcome::Declined => "declined",
                ControlOutcome::NoAnswer => "no_answer",
                ControlOutcome::Refused => "refused",
            };
            let crate::events::ControlCall::Remedy { offer, .. } = call;
            let labels = [KeyValue::new("outcome", outcome)];
            count("appa.remedy.attempts", &labels);
            duration("appa.remedy.duration", *duration_ms as f64 / 1000.0, &labels);
            tracing::info!(target: "appa_telemetry", { "appa.event.name" = "appa.remedy.completed",
                "appa.trajectory.root" = root, "appa.offer.id" = name(offer), "appa.outcome" = outcome,
                "appa.duration.ms" = duration_ms }, "remedy completed");
        }
        RuntimeEvent::StoreError { operation, class } => {
            tracing::warn!(target: "appa_telemetry", { "appa.event.name" = "appa.runtime.failure",
                "appa.trajectory.root" = root, "appa.component" = "store", "appa.operation" = spelling(operation),
                "appa.error.type" = spelling(class) }, "store operation failed");
            count(
                "appa.runtime.failures",
                &[
                    KeyValue::new("component", "store"),
                    KeyValue::new("error_type", spelling(class)),
                ],
            );
        }
        RuntimeEvent::Hook { event, outcome, .. } => {
            // Policy counters come from the session, not the hook's presentation.
            // A runtime refusal can appear as DenyCall on the wire.
            tracing::info!(target: "appa_telemetry", { "appa.event.name" = "appa.hook.completed",
                "appa.trajectory.root" = root, "appa.hook.event" = spelling(event),
                "appa.outcome" = spelling(outcome) }, "hook completed");
        }
        RuntimeEvent::Reload { .. } => {}
    }
}

fn spelling(value: &impl serde::Serialize) -> String {
    match serde_json::to_value(value).expect("closed diagnostic enums serialize") {
        serde_json::Value::String(value) => value,
        // Exclude the status code from NonSuccess metric labels.
        serde_json::Value::Object(value) => value.keys().next().cloned().unwrap_or_default(),
        _ => unreachable!("diagnostic classes serialize as names or tagged objects"),
    }
}

#[cfg(feature = "daemon")]
pub(crate) fn yell(report: &crate::yell::Finished, root: &appa_runtime_api::TrajectoryId) {
    // Finished contains only the existing deny-by-default report projection.
    // Agent reports are already opted in. CLI previews must never reach here.
    let Ok(report) = serde_json::from_slice::<serde_json::Value>(&report.plain) else {
        return;
    };
    count("appa.yell.reports", &[KeyValue::new("source", "agent")]);
    tracing::info!(target: "appa_telemetry", { "appa.event.name" = "appa.yell.report",
        "appa.trajectory.root" = name(&root.0), "appa.report.source" = "agent",
        "appa.report.id" = report["report_id"].as_str().unwrap_or_default(),
        "appa.report.message" = report["message"].as_str().unwrap_or_default() },
        "agent report prepared");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_error_classes_do_not_export_unbounded_details() {
        assert_eq!(name(&"é".repeat(200)).len(), 256);
        assert_eq!(name(&format!("{}é", "x".repeat(255))).len(), 255);
        assert_eq!(error_class(&EventError::Storage("private path".into())), "storage");
        assert_eq!(
            spelling(&crate::events::NoAnswerClass::NonSuccess { status: 503 }),
            "non_success"
        );
    }
}
