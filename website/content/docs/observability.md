---
title: Observability
category: Operations
order: 9
description: What OpenAPPA emits in production and how to watch it.
---

`appa runtime` can export traces, logs, and metrics over OTLP. Exporting is
off until an OpenTelemetry endpoint is configured, so the default runtime
still writes only to stderr.

OpenAPPA's event log remains the authoritative security record. Operational
telemetry is a projection for finding slow decisions, refused hooks, and
unhealthy deployments. It also covers decisions that append no event-log
fact, such as a proposed call blocked before dispatch.

## Enable OTLP

Point the runtime at an OTLP/HTTP collector:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
OTEL_RESOURCE_ATTRIBUTES=deployment.environment.name=development \
appa runtime -v --config appa.toml --db appa.db
```

The exporter follows the standard signal-specific OpenTelemetry variables,
including `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`,
`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`,
`OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`, and
`OTEL_EXPORTER_OTLP_HEADERS`. Signal-specific settings override the common
endpoint. `OTEL_METRIC_EXPORT_INTERVAL` controls the metric export interval in
milliseconds.

Use `-v` to include the runtime's decision path at debug level and `-vv` to
include the engine algebra at trace level. Without either flag, the decision
summary is still emitted at info level. Network export happens off the hook's
request path; an unavailable collector does not allow, block, or otherwise
change a tool call.

On SIGTERM or Ctrl-C, the runtime stops accepting new requests and flushes all
three signals before exiting.

## Signals

Every parsed hook is represented by an `appa.hook` span and a structured
`hook decision` log inside that span. OTLP carries the trace and span IDs on
the log record, which lets a backend navigate from the decision trace to the
logs produced while that decision was evaluated.

The span and log attributes are:

| Attribute | Meaning |
|---|---|
| `appa.hook.event` | Bounded hook kind, such as `tool_call` or `tool_result` |
| `appa.decision` | Bounded outcome, such as `allow`, `deny`, `block`, or `refuse` |
| `appa.trajectory.id` | Root trajectory ID used to correlate separate hook requests |
| `appa.trajectory.child_id` | Child trajectory ID when the event belongs to one |
| `gen_ai.conversation.id` | The root trajectory under the OpenTelemetry GenAI convention |
| `gen_ai.tool.name` | Tool name on call and result hooks |
| `error.type` | Operational refusal family, when the span failed |

The custom `appa.*` namespace is used because the OpenTelemetry registry has
no policy-decision or agent-trajectory convention. Standard `service.*`,
`deployment.*`, `error.type`, and `gen_ai.*` attributes are used where their
meaning matches.

The runtime exports these low-cardinality metrics:

| Metric | Type | Dimensions |
|---|---|---|
| `appa.runtime.hook.requests` | Counter | `appa.hook.event`, `appa.decision` |
| `appa.runtime.hook.duration` | Histogram, seconds | `appa.hook.event`, `appa.decision` |
| `appa.runtime.uptime` | Gauge, seconds | none |

Trajectory IDs and tool names are intentionally absent from metric dimensions.
They remain available on spans and logs for drilldown without creating
unbounded time series.

## Data boundary

Telemetry never includes prompt text, tool arguments, tool results, policy
feedback, remedy text, or label values. Those values may contain the data the
policy exists to confine. A deployment may still treat trajectory IDs and tool
names as sensitive operational metadata and should scope access to its
observability backend accordingly.

## Local Grafana

Grafana's OpenTelemetry LGTM image provides an OTLP collector, Prometheus,
Loki, Tempo, and Grafana for local testing:

```sh
docker run --rm --name appa-lgtm \
  -p 3002:3000 -p 4317:4317 -p 4318:4318 \
  grafana/otel-lgtm
```

Open [Grafana](http://localhost:3002) with `admin` / `admin`, then import
[`observability/grafana/openappa-overview.json`](https://github.com/archestra-ai/OpenAPPA/blob/main/observability/grafana/openappa-overview.json).
For a provisioned stack, the repository also includes a starter
[`datasources.yml`](https://github.com/archestra-ai/OpenAPPA/blob/main/observability/grafana/datasources.yml)
with Tempo-to-Loki correlation. Adjust its URLs and UIDs for the deployment.
Click a decision's trace ID to keep the investigation inside the dashboard:
the selected trace renders as a waterfall and filters the log panel to records
with the same trace ID. The dashboard's trajectory field widens that view
across all hook traces belonging to one trajectory.

The top row separates expected policy stops (`deny` and `block`) from runtime
refusals, which indicate a hook OpenAPPA could not safely evaluate. Runtime
uptime supplies a heartbeat for deployment-specific missing-telemetry alerts.
Alert notification routes and thresholds belong to the deployment's Grafana
configuration; the starter dashboard does not choose recipients.
