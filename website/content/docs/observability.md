---
title: Observability
category: Operations
order: 9
description: Investigate agent activity in your existing observability tools and use the evidence to improve appa.toml policies.
---

> **In progress:** The exporter is under development. The names, settings, and examples below define a proposed interface. They are not available in the current runtime.

OpenAPPA sends metrics, logs, and analytics to observability providers that accept OpenTelemetry (OTEL), including Grafana Cloud, Datadog, and New Relic. Use your existing dashboards and alerts to investigate agent activity, measure policy-check time, and find restrictions that interrupt legitimate work.

- **Tool calls:** what APPA allowed or blocked, and why.
- **Trajectories:** the decisions and tool calls associated with a trace ID.
- **Information flows:** which sources contributed to a value and where the agent tried to send it.
- **Audiences and trust:** who may receive the data, how APPA labels it, and which restriction prevents a flow.
- **Remedies:** which plans APPA offered, which the agent tried, and whether they succeeded.
- **Policy checks:** time spent evaluating policy or waiting for external services, including failures.
- **[`appa yell` reports](/yell):** feedback linked to the decisions that prompted it.

`appa yell` sends feedback and filtered policy records to your observability provider. Use the trace ID to find the related decisions and tool calls. The attached records exclude raw prompts, arguments, and tool results. The report message is sent as written.

## OpenTelemetry export

### Configuration

Set these variables on the process that runs APPA, then restart that process:

```sh
export OPENAPPA_OTEL_URL="http://localhost:4318"
export OTEL_SERVICE_NAME="appa-runtime"
export OTEL_RESOURCE_ATTRIBUTES=\
"deployment.environment.name=staging"
```

| Variable | Purpose |
|---|---|
| `OPENAPPA_OTEL_URL` | Collector base URL. Unset disables export. |
| `OTEL_SERVICE_NAME` | Service name shown in your backend. Defaults to `appa-runtime`. |
| `OTEL_RESOURCE_ATTRIBUTES` | Comma-separated attributes attached to exported records, such as deployment environment and version. |
| `OTEL_EXPORTER_OTLP_HEADERS` | Optional collector authentication headers. Supply through your secret manager. |

APPA uses the OpenTelemetry Protocol (OTLP) over HTTP with protobuf encoding. It appends `/v1/logs` and `/v1/metrics` to `OPENAPPA_OTEL_URL`. Use a collector address reachable from the runtime. Inside a container, `localhost` refers to that container.

### Collector setup

Enable an OTLP HTTP receiver and both log and metric pipelines in your OpenTelemetry Collector. For a local test, this configuration prints received telemetry to the Collector's output:

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
    logs:
      receivers: [otlp]
      exporters: [debug]
    metrics:
      receivers: [otlp]
      exporters: [debug]
```

The example accepts connections from the same host only. If APPA runs elsewhere, change the receiver address and allow connections only from APPA.

To send data to your provider, replace `debug` in both pipelines with its exporter and configure its credentials.

Run an agent tool call, then search your provider for `appa.policy.decision` under `appa-runtime`, or the service name you set.

Restrict who can read the telemetry and how long your provider keeps it. Tool names, audience identifiers, and report messages can contain sensitive information.

## Let your agent improve appa.toml

Your observability tools or an agent can flag unusual patterns, such as repeated attempts to send restricted data to new recipients. These patterns warrant investigation but do not, by themselves, prove an attack.

Your agent can periodically review decisions and [`appa yell` reports](/yell) in a “dream” cycle. For a task that repeatedly gets blocked, it can [propose a change to `appa.toml`](/self-improving-policies#improve-policies-from-yell-reports) and test it against allowed and prohibited calls.

Compare interruptions, successful remedies, and policy-check time before and after a change. This tells your team whether the change makes agents more useful. Tests of prohibited calls check that the policy still enforces the restrictions you intend to keep.

## Metrics

APPA exports the following metrics. Names below use the Prometheus format, including `_total` for counters and `_seconds` for durations.

| Metric | Type | Labels |
|---|---|---|
| `appa_policy_decisions_total` | Counter | `tool_name`, `outcome` |
| `appa_policy_check_duration_seconds` | Histogram | `outcome` |
| `appa_external_calls_total` | Counter | `external_name`, `role`, `outcome` |
| `appa_external_call_duration_seconds` | Histogram | `external_name`, `role`, `outcome` |
| `appa_remedy_attempts_total` | Counter | `outcome` |
| `appa_remedy_duration_seconds` | Histogram | `outcome` |
| `appa_yell_reports_total` | Counter | `source` |
| `appa_runtime_failures_total` | Counter | `component`, `error_type` |

Policy decisions use `outcome="allowed"` or `outcome="denied"`. Failed checks, such as an unreachable service, count as runtime failures rather than policy denials. Policy-check durations exclude model inference and agent tool execution. External-call and remedy durations include time spent waiting for their responses.

Individual trace, trajectory, call, and audience-member IDs stay in logs to avoid creating a metric series for every identifier.

### Example queries

Denied tool calls per second, grouped by tool, over the last five minutes:

```promql
sum by (tool_name) (
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

These queries run in a Prometheus-compatible backend that receives the exported metrics.

## Logs

Use `appa.event.name` to filter structured log records:

| Event | Fields specific to the event |
|---|---|
| `appa.policy.decision` | Tool name, outcome, reason, audience and trust restrictions |
| `appa.remedy.completed` | Offer ID, outcome, duration |
| `appa.external.completed` | Service name, role, outcome, duration, error category when a call fails |
| `appa.yell.report` | Report ID, source, message, related tool-call ID |
| `appa.runtime.failure` | Component and error category |

Records include a timestamp, `service.name`, and the relevant trace, trajectory, tool-call, and policy identifiers. The examples below show decoded records, not the OTLP wire format.

### Blocked tool call

```json
{
  "timestamp": "2026-09-24T10:15:30Z",
  "severity_text": "INFO",
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "resource": {
    "service.name": "appa-runtime"
  },
  "attributes": {
    "appa.event.name": "appa.policy.decision",
    "appa.trajectory.id": "trajectory-42",
    "appa.tool.call.id": "call-7",
    "appa.policy.id": "policy-12",
    "appa.tool.name": "slack.send_message",
    "appa.outcome": "denied",
    "appa.reason": "destination_outside_audience",
    "appa.audience": ["internal"],
    "appa.destination.audience": "public",
    "appa.trust": "trusted"
  }
}
```

### Related appa yell report

The report carries the same trace and tool-call IDs as the decision it concerns.

```json
{
  "timestamp": "2026-09-24T10:16:00Z",
  "severity_text": "INFO",
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "resource": {
    "service.name": "appa-runtime"
  },
  "attributes": {
    "appa.event.name": "appa.yell.report",
    "appa.trajectory.id": "trajectory-42",
    "appa.tool.call.id": "call-7",
    "appa.report.id": "report-3",
    "appa.report.source": "agent",
    "appa.report.message":
      "Public summary still contains customer data."
  }
}
```

## Trace correlation

Search for a call or report's trace ID to find related APPA records and application telemetry. Use the trajectory and tool-call IDs to locate the specific agent activity within those results.

## Start with one agent

Connect one agent's APPA runtime to the observability provider your team already uses. Measure blocked calls, successful remedies, and policy-check time before changing `appa.toml`. Use those results to choose a restriction to review, then test the proposed change against allowed and prohibited calls.
