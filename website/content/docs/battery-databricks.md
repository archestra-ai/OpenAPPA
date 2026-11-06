---
title: Databricks battery
category: Batteries
order: 6.74
description: Rules for Databricks' Genie One and Databricks SQL managed MCP servers, with internal reads, every SQL statement classified before it runs, and the databricks audience source.
sidebar: false
breadcrumb: Databricks
---

The Databricks battery covers the two managed MCP servers with fixed tool names: Genie One (`/api/2.0/mcp/genie`, five tools) and Databricks SQL (`/api/2.0/mcp/sql`, one tool). One namespace is bound to both host servers:

```sh
appa battery install databricks --server <genie-server> --server <sql-server>
```

The battery also ships the `databricks` audience source, which reads the workspace's users, groups, and Genie space permissions.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/databricks).

## Tool behavior

- Genie answers a question with SQL it generates and the rows that SQL returns from the workspace's tables. The four Genie reads return untrusted `internal` data, and the question must be sharable with `internal`.
- `execute_sql` runs any statement. The Claude Code model classifies each one before it runs, inside a fixed mandate: a read-only statement returns untrusted `internal` data; `INSERT`, `UPDATE`, `DELETE`, `MERGE`, and `COPY INTO` need trusted internal input and record `databricks.changed`; DDL, grants, several statements, or anything unclear need the `databricks-review` mark and record `databricks.sensitive`.
- An answer outside that mandate is refused with the call. The answer inside it is the model's reading of the statement, not a parse: prose hidden in a SQL comment can steer it, so a workspace that must not take that risk sets `disallow_writes` on the warehouse or overrides the rule to send every statement to a person.

The Claude Code plugin default permits every mark, so the person running the session reviews `databricks-review`; another root config must define an Authority permitting it.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of the four Genie reads and `execute_sql`. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `databricks.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

## Audience source

A reader is an active user's login address, which Databricks verifies at every sign-in; any other user is `databricks:<id>`. The source serves `viewer` for `self`, `members` for `internal`, `group/<name>` for groups, and `genie-space/<id>/readers` for root rules over a per-space Genie Agent server. Every read is one Databricks CLI command, so the CLI's own login names the workspace and the credential; a deployment that sets `APPA_PROVIDER_DATABRICKS_TOKEN` hands it to the CLI as its token.

Genie One holds no space id in any call, so every Genie read is `internal`; narrow a per-space Genie Agent server's tool in the root policy with `@databricks:genie-space/<id>/readers`.
