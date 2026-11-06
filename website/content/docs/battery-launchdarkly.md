---
title: LaunchDarkly battery
category: Batteries
order: 6.71
description: Rules for LaunchDarkly's official MCP server, all 20 tools, with internal reads and every write reviewed.
sidebar: false
breadcrumb: LaunchDarkly
---

The LaunchDarkly battery covers all 20 tools that LaunchDarkly's official MCP server registers: feature flags, environments, AI Configs, code references, and the audit log.

Writes need `launchdarkly-review`. The Claude Code and kagent plugin defaults permit every mark, so the person running the session reviews them; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/launchdarkly).

## Tool behavior

- Flags, AI Configs, environments, code references, and audit log entries return untrusted `internal` data. Engineers author the configuration, but the same reads carry context keys and attribute values that identify end users, source lines around each flag reference, and free-text audit comments.
- Creating or updating a flag or an AI Config changes what the application serves in production. It requires trusted internal data and review, and records `launchdarkly.changed`.
- Deleting a flag, an AI Config, or an AI Config variation records `launchdarkly.sensitive`.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of its 17 tools that return flag, environment, AI Config, code reference or audit log data. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `launchdarkly.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

## Limit

LaunchDarkly tokens grant access across projects and environments without exposing per-resource ACLs. All reads default to `internal`. To confine an agent to specific projects, scope them in your root `appa.toml` using argument selectors.

This version exposes no project or environment write tool, so the battery names none.
