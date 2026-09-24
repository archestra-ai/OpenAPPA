---
title: Reporting (appa yell)
category: Operations
order: 9.2
description: Agents report confusing blocks and suspected policy misconfigurations through the yell tool, giving policy maintainers evidence for improvements.
---

When APPA blocks expected work or offers a confusing remedy, the agent can call the `yell` tool. The report explains the problem and includes filtered policy diagnostics.

These reports can feed a [policy-improvement workflow](/self-improving-policies). A maintenance agent investigates the report and proposes tested changes for review. A report does not change the policy or grant permission.

## Enable agent reporting

Agent reporting is off by default. `appa init` asks whether to enable it. To enable it directly, set this in `appa.toml`:

```toml
[reporting]
agent_yell = true
```

The agent describes the problem in `message`. Set `with_trajectory = true` to include session decisions. Set it to `false` to include only policy information.

The normal tool proposal check still controls each agent report. An approved call sends directly and returns a receipt. It does not add a confirmation prompt or a local file.

To report manually, use the CLI:

```sh
appa yell "Cannot post a summary back to its source channel."
```

## What a report contains

Reports include filtered policy configuration. When requested and available, they also include decisions, remedies, and label changes. The diagnostics exclude prompts, tool arguments, tool outputs, and file paths. Report-local tokens replace raw trajectory IDs.

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

Release builds send reports to the OpenAPPA team receiver by default. To use your own receiver, set this in runtime and CLI environments:

```sh
export APPA_YELL_ENDPOINT="https://reports.example.com/report"
```

The URL must accept APPA's report protocol. Remote receivers require HTTPS. This setting does not configure OpenTelemetry export.

### Export a bounded report event to OpenTelemetry

Set the OTEL collector base URL on the APPA runtime process:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318"
```

The [Observability guide](/observability#configuration) describes exporter settings and collector setup. Do not point `APPA_YELL_ENDPOINT` at an OTEL collector.

After APPA prepares an approved agent report, it exports one `appa.yell.report` event. The event contains the report message, report ID, source, and root trajectory ID. APPA exports it before receiver delivery, so the event remains if delivery fails.

The full filtered report goes only to the existing `APPA_YELL_ENDPOINT` receiver. APPA does not send that report body through OTLP. CLI previews and CLI reports are not exported.

OTEL export does not change report authorization. It does not enable agent reporting, bypass the tool proposal check, or add a confirmation bypass.

Enabling OTEL export is operator consent to export the bounded event. The message, report ID, and root trajectory ID can still contain sensitive information.

### Keep a CLI report on disk

Decline the CLI send prompt. Copy the report from the printed temporary path to a permanent location if you must retain it.
