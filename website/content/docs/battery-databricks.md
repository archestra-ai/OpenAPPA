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

## Audience source

A reader is an active user's login address, which Databricks verifies at every sign-in; any other user is `databricks:<id>`. The source serves `viewer` for `self`, `members` for `internal`, `group/<name>` for groups, and `genie-space/<id>/readers` for root rules over a per-space Genie Agent server. Every read is one Databricks CLI command, so the CLI's own login names the workspace and the credential; a deployment that sets `APPA_PROVIDER_DATABRICKS_TOKEN` hands it to the CLI as its token.

Genie One holds no space id in any call, so every Genie read is `internal`; narrow a per-space Genie Agent server's tool in the root policy with `@databricks:genie-space/<id>/readers`.
