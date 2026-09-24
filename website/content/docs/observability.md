---
title: Observability
category: Operations
order: 9
description: Export bounded runtime telemetry to your existing observability tools and use it to investigate agent activity.
---

OpenAPPA exports traces, logs, and metrics through OpenTelemetry (OTEL). You can send this telemetry to an OTEL-compatible collector or provider.

The export covers tool proposal checks, external calls, store failures, remedies, hooks, and agent reports. Policy-check time includes external and storage time. It excludes agent model inference and tool execution. An Annotator's model call counts as external-call time.

The runtime exports only events with the `appa_telemetry` target. The `-v` and `-vv` options change stderr detail independently. The exporter does not capture function arguments.

## OpenTelemetry export

### Configuration

Set the collector base URL on the process that runs APPA. Then restart that process.

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318"
export OTEL_SERVICE_NAME="appa-runtime"
export OTEL_RESOURCE_ATTRIBUTES=\
"deployment.environment.name=staging"
```

| Variable | Purpose |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Required collector base URL. If unset or empty, APPA does not start the exporter. |
| `OTEL_SERVICE_NAME` | Service name shown in your backend. The default is `appa-runtime`. |
| `OTEL_RESOURCE_ATTRIBUTES` | Comma-separated resource attributes, such as the deployment environment. |
| `OTEL_EXPORTER_OTLP_HEADERS` | Optional collector authentication headers. Supply them through your secret manager. |
| `OTEL_SDK_DISABLED` | Set to `true` to disable export, even when an endpoint is set. |

APPA exports OTLP over HTTP with protobuf encoding. It uses `/v1/traces`, `/v1/logs`, and `/v1/metrics` under the base URL. Inside a container, `localhost` refers to that container.

Every record includes `service.name`. APPA also sets `service.version` to the runtime package version. The OTEL SDK adds configured resource attributes.

Enabling export is operator consent to send this bounded telemetry. Names, identifiers, and agent report messages can still contain sensitive information. Restrict access and retention in your collector and provider.

### Collector setup

Enable an OTLP HTTP receiver and trace, log, and metric pipelines. This local configuration prints received telemetry to the collector output:

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:4318

exporters:
  debug:
    verbosity: detailed

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
    logs:
      receivers: [otlp]
      exporters: [debug]
    metrics:
      receivers: [otlp]
      exporters: [debug]
```

This example accepts connections from the same host only. If APPA runs elsewhere, change the receiver address. Permit connections only from APPA.

To send data to a provider, replace `debug` in each pipeline with the provider exporter. Then configure its credentials.

Run an agent tool proposal. Search the provider for `appa.policy.decision` under the configured service name.

## Metrics

The table shows Prometheus mappings. OTLP uses the dotted names without Prometheus counter and unit suffixes.

| Prometheus mapping | OTLP name | Type | Labels |
|---|---|---|---|
| `appa_policy_decisions_total` | `appa.policy.decisions` | Counter | `outcome` |
| `appa_policy_check_duration_seconds` | `appa.policy.check.duration` | Histogram, seconds | `outcome` |
| `appa_external_calls_total` | `appa.external.calls` | Counter | `role`, `outcome` |
| `appa_external_call_duration_seconds` | `appa.external.call.duration` | Histogram, seconds | `role`, `outcome` |
| `appa_remedy_attempts_total` | `appa.remedy.attempts` | Counter | `outcome` |
| `appa_remedy_duration_seconds` | `appa.remedy.duration` | Histogram, seconds | `outcome` |
| `appa_yell_reports_total` | `appa.yell.reports` | Counter | `source` |
| `appa_runtime_failures_total` | `appa.runtime.failures` | Counter | `component`, `error_type` |
| `appa_runtime_uptime_seconds` | `appa.runtime.uptime` | Gauge, seconds | none |

The policy metrics measure tool proposal checks only. The decision counter uses `allowed` and `denied`. The duration histogram also uses `error` for failed checks. External outcomes are `answered` or `no_answer`. Remedy outcomes are `executed`, `declined`, `no_answer`, or `refused`. Agent reports use `source="agent"`.

Remedy metrics cover attempts with a valid, released offer. Requests refused before APPA identifies that offer do not count as remedy attempts.

The metrics do not use tool names, external names, or identifiers as labels. This design bounds metric cardinality.

A runtime refusal is not a policy denial. The runtime records it as an error or a hook outcome. A store failure can also cause a policy failure. Thus, the sum of failure metrics does not identify a unique count of failed calls.

### Example queries

Denied tool proposal checks per second over the last five minutes:

```promql
sum(
  rate(appa_policy_decisions_total{outcome="denied"}[5m])
)
```

The 95th-percentile policy-check duration in seconds:

```promql
histogram_quantile(
  0.95,
  sum by (le) (
    rate(appa_policy_check_duration_seconds_bucket[5m])
  )
)
```

These queries require a Prometheus-compatible backend that receives the exported metrics.

## Logs

Filter structured logs with `appa.event.name`:

| Event | Exported event fields |
|---|---|
| `appa.policy.decision` | `appa.tool.name`, `appa.outcome`, `appa.offer.ids`, `appa.duration.seconds` |
| `appa.external.completed` | `appa.trajectory.root`, `appa.external.name`, `appa.external.role`, `appa.outcome`, `appa.error.type`, `appa.duration.ms` |
| `appa.remedy.completed` | `appa.trajectory.root`, `appa.offer.id`, `appa.outcome`, `appa.duration.ms` |
| `appa.hook.completed` | `appa.trajectory.root`, `appa.hook.event`, `appa.outcome` |
| `appa.runtime.failure` | `appa.component`, `appa.error.type`, plus `appa.trajectory.root` and `appa.operation` for store failures |
| `appa.yell.report` | `appa.trajectory.root`, `appa.report.source`, `appa.report.id`, `appa.report.message` |

The exporter does not send raw prompts, tool arguments, tool results, provenance, audience sets, or trust claims. It also does not export policy feedback, review text, or remedy display text.

APPA bounds caller-controlled exported identifiers and names to 256 UTF-8 bytes. A policy decision exports at most 32 offer IDs.

## Traces and correlation

OTEL logs correlate with spans through their native `trace_id` and `span_id`. The `appa.policy.check` span carries `appa.trajectory.root`, `appa.trajectory.id`, `appa.tool.call.id`, the tool name, and outcome. `appa.policy.id` identifies the trajectory's pinned policy when the check reaches policy evaluation. A host that supplies no call ID leaves that field empty.

A blocked check records `appa.policy.gaps` and `appa.policy.narrowing` on its span. Gap classes are `trust_floor`, `includes`, `cap`, `prior`, `no_prior`, and `attention`. These explain the restriction without exporting audience members or call values. A marked spawn can need a return declaration without a gap or narrowing.

Each served hook, remedy, and report starts its own trace. External calls create child spans within the operation that invokes them. APPA does not extract an incoming `traceparent`, and it does not create one trace for a full trajectory.

Correlate separate events with `appa.trajectory.root` or `appa.offer.id`. Use native trace correlation for events within one hook, remedy, or report operation.

The standalone `appa runtime` process owns this exporter. An embedding host owns its tracing subscriber and metric provider. The library emits instrumentation but does not configure an exporter from environment variables.

Export runs on background workers with bounded queues. A failed export cannot change a policy decision. Queue overflow, process termination, or collector failure can lose telemetry. With export enabled, SIGINT and SIGTERM flush completed records before exit. In-flight operations are not guaranteed to finish. Telemetry is not a durable audit log.

## Agent reports

An approved agent report exports `appa.yell.report` after APPA prepares the filtered report. The event contains the message, report ID, source, and root trajectory ID. APPA exports this event even if delivery to the report receiver later fails.

The full filtered report goes only to the receiver configured by `APPA_YELL_ENDPOINT`. APPA does not send the report body through OTLP. CLI report previews and CLI reports do not enter OTLP export.

OpenTelemetry does not change report approval or delivery rules. It does not bypass the agent-reporting opt-in or add a confirmation bypass. See [`appa yell`](/yell) for report controls.

## Improve policies from evidence

Observability tools can flag repeated denials, slow external calls, or unsuccessful remedies. These patterns need investigation. They do not prove an attack or a policy defect.

An agent can review decisions and [`appa yell` reports](/yell) in a maintenance cycle. It can then [propose a tested `appa.toml` change](/self-improving-policies#improve-policies-from-yell-reports).

Compare denials, remedy outcomes, runtime failures, and policy-check time before and after a change. Tests of prohibited calls make sure the policy keeps the intended restrictions.
