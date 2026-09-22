---
title: monday battery
category: Batteries
order: 6.75
description: Rules for monday Platform MCP's 96 tools, with internal reads and reviewed writes and sensitive changes.
sidebar: false
breadcrumb: monday
---

The monday battery covers the 96 tools discovered from the hosted Platform
MCP server at `https://mcp.monday.com/mcp`, version 1.0.0, on 2026-09-22.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/monday)
and its [rules and covered tools](https://github.com/archestra-ai/OpenAPPA/blob/main/marketplace/batteries/monday/README.md#rules).

## Tool behavior

- **Internal reads:** boards, items, comments, searches, documents, people,
  schemas, assets, meetings, automation history, workflow inspection and Vibe
  inspection enter suspicious/internal. Queries must be sharable with internal.
  Normal options such as search terms, descriptions, subitems and replies are
  available. `all_api_read` rejects mutations at the provider.
- **Public documentation:** `get_monday_knowledge` requires a public question
  and returns suspicious/public content. `read_docs` instead reads internal
  workspace documents.
- **Reviewed writes:** items, comments, docs, folders, groups, dashboards,
  widgets, views and uploads require trusted internal input and `monday-review`,
  and record `monday.changed`. Their results stay suspicious/internal because
  write responses can include existing provider content.
- **Reviewed sensitive changes:** structural changes, deletes, notifications,
  automations, workflows, agent management, code/actions, Vibe publication and
  general GraphQL operations record `monday.sensitive`. Review includes the
  affected resources and any external destinations.
- **Notifications:** `create_notification` also checks its `user_id` recipient
  against the input audience. A monday audience source resolves that user to a
  confirmed email; absent, inactive, or unconfirmed users are refused.
- **External submissions:** WorkForm submissions and feedback to monday require
  public input and review. Their suspicious/internal responses mean the exact
  call also needs an authority permitted to approve audience expansion.
- **Returned credentials:** `connect_external_agent` returns a signing secret
  and API token. Its reviewed contract keeps input and output within `self`.

The read actions of `manage_agent`, `manage_agent_triggers` and
`manage_agent_knowledge` have separate contracts before their reviewed mutation
fallbacks. There are no terminal blocks in the discovered inventory.

## Deployment

As with the Notion battery, the root must map `internal` to readers authorized
for all resources reachable through the connection. monday board ACLs are not
inferred. Map `self` to the credential owner for external-agent connections.
Root rules can narrow specific resources.
Notifications need `APPA_PROVIDER_MONDAY_TOKEN` with `users:read` for the
recipient lookup. The token runs beside the policy, separate from the host's
MCP credential. The root's reader identities must match confirmed emails.

The Claude Code and kagent defaults provide a human authority for
`monday-review`, trust exceptions and audience expansion. A custom authority
can omit audience expansion: reviewed internal read-to-write workflows still
work, while external submissions remain refused. See the battery README for
the authority configuration and complete limits.

Future tool names follow the deployment's fallback policy. Provider error text
is outside successful-output admission in the current host/runtime and is not
sanitized by this battery. Installing it adds policy, not a connection or board
permissions.

The [offline replay](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/monday)
checks decisions using a fictional audience and simulated approvals. It does
not call monday or verify provider effects.
