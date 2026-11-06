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

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of all 18 tools. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `pagerduty.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `pagerduty.redact-for-public` | `audience`: `internal` → `public` | Removes responder and customer contact details, customer names, internal service, host and team names, internal URLs, IP addresses, credentials and alert payloads, and keeps what failed, the timeline and the resolution. The read does not narrow the session's audience, so it adds no audience gap to a later call bound for `public` readers. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts or settles with the other sanitizer. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

## Limit

Each tool takes one `request` object, and the operation is a `const` `action` inside it. A selector checks top-level string arguments, so neither a battery rule nor a root rule can separate `add_note` from `create` on `manage_incidents`. A deployment that needs one action reviewed and another not needs an annotator that reads `request.action`.

PagerDuty scopes visibility by team, and no tool reports which teams may see an incident, a service, or a schedule. All reads therefore default to `internal`. Map it in your root `appa.toml` to the people who may see everything this token can list.
