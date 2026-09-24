---
title: Reporting (appa yell) · v2
category: Operations
order: 9.2
description: Agents report confusing blocks and suspected policy misconfigurations through the yell tool, giving policy maintainers evidence for improvements.
---

When APPA blocks work the agent expected to complete, or offers a confusing remedy, the agent can call the `yell` tool to report it. The report explains where the agent got stuck and includes policy diagnostics to help investigate the problem.

These reports can feed a [policy-improvement workflow](/self-improving-policies-v2): a maintenance agent investigates suspected misconfigurations and proposes tested changes for review. A report does not change the policy or grant permission.

## Enable agent reporting

Agent reporting is off by default. `appa init` will ask whether to enable it. To enable it directly, set this in `appa.toml`:

```toml
[reporting]
agent_yell = true
```

The agent describes the problem in `message`. Set `with_trajectory = true` to include session decisions, or `false` for policy only. Permitted calls send directly and return a receipt, without a confirmation prompt or local file.

To report manually, use the CLI:

```sh
appa yell "Cannot post a summary back to its source channel."
```

## What a report contains

Reports include filtered policy configuration and, when requested and available, decision records such as rulings, remedies, and label changes. The diagnostics exclude prompts, tool arguments, tool outputs, and file paths. Report-local tokens replace raw trajectory IDs.

The report message is included as written. Agents and users must not put secrets, customer data, or conversation content in it.

Example report excerpt, with build details, policy configuration, and decision records omitted:

```json
{
  "schema": "openappa.yell.v1",
  "report_id": "d6b5d455-b72c-487b-98ef-d52e47c41971",
  "created_at": "2026-09-24T10:00:00Z",
  "origin": { "kind": "agent", "pseudonymized": false },
  "message": "Cannot post a summary back to its source channel.",
  "trajectory": {
    "branches": [
      { "id": "trajectory-1", "parent": null, "yelling": true }
    ]
  }
}
```

## Choose where reports go

### Send to your own report receiver

Release builds send reports to the OpenAPPA team's receiver by default. To use your own receiver, set this in both the runtime and CLI environments:

```sh
export APPA_YELL_ENDPOINT="https://reports.example.com/report"
```

The URL must accept APPA's report protocol. Remote receivers require HTTPS. This setting does not configure OpenTelemetry export.

### Send to OpenTelemetry

The [Observability configuration](/observability-v2#configuration) defines a separate collector setting on the APPA process:

```sh
export OPENAPPA_OTEL_URL="http://localhost:4318"
```

See that guide for exporter availability, authentication, and [Collector setup](/observability-v2#collector-setup). Use [trace correlation](/observability-v2#trace-correlation) to find the decisions related to a report. Do not point `APPA_YELL_ENDPOINT` at an OTEL collector.

### Keep a CLI report on disk

Decline the CLI send prompt. Copy the report from the printed temporary path somewhere permanent if you need to retain it.
