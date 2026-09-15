---
title: PagerDuty battery
category: Batteries
order: 6.73
description: Rules for the PagerDuty-hosted MCP server's 18 tools, with internal reads and every write reviewed.
sidebar: false
breadcrumb: PagerDuty
---

The PagerDuty battery covers all 18 tools of the PagerDuty-hosted MCP server: eleven `browse_*` reads and seven `manage_*` writes.

Your root config must define an Authority permitting `pagerduty-review` for writes.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/pagerduty).

## Tool behavior

- Incidents, alerts, notes, log entries, change events, and status page posts were written by monitoring integrations, responders, and status page authors. They return untrusted `internal` data, and a query's input must be sharable with `internal`.
- `manage_incidents` updates an incident, appends a note, pages responders, or starts a workflow. It requires trusted internal data and review, and records `pagerduty.changed`.
- The other six writes change configuration or publish outward — services, schedules, team membership, event orchestration routers, alert grouping, and status page posts — and record `pagerduty.sensitive`.

## Limit

Each tool takes one `request` object, and the operation is a `const` `action` inside it. A selector checks top-level string arguments, so neither a battery rule nor a root rule can separate `add_note` from `create` on `manage_incidents`. A deployment that needs one action reviewed and another not needs an annotator that reads `request.action`.

PagerDuty scopes visibility by team, and no tool reports which teams may see an incident, a service, or a schedule. All reads therefore default to `internal`. Map it in your root `appa.toml` to the people who may see everything this token can list.
