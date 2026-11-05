---
title: monday battery
category: Batteries
order: 6.75
description: Rules for monday Platform MCP's 99 tools, with internal reads and reviewed writes and sensitive changes.
sidebar: false
breadcrumb: monday
---

The monday battery covers the 99 tools of monday.com's hosted Platform MCP server.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/monday)
and its [rules and covered tools](https://github.com/archestra-ai/OpenAPPA/blob/main/marketplace/batteries/monday/README.md#rules).

## Tool behavior

- **Internal reads:** boards, items, comments, searches, documents, people,
  schemas, assets, meetings, automation history, workflow inspection and Vibe
  inspection stay internal. Trust follows who can write the text: account
  members write boards, items, updates and docs, and a member published each
  form or installed each integration, so these reads keep the session's trust.
  Meeting notes and transcripts carry what outside participants said, so
  `explore_meetings`, `search_meetings_content` and `get_meetings_content`
  enter suspicious.
  Queries must be sharable with internal. Normal options such as search terms,
  descriptions, subitems and replies are available. `all_api_read` rejects
  mutations at the provider.
- **Public documentation:** `get_monday_knowledge` requires a public question
  and answers from monday's own documentation, so its result enters
  suspicious and public. `read_docs` instead reads internal
  workspace documents.
- **Reviewed writes:** items, comments, docs, folders, groups, dashboards,
  widgets, views and uploads require trusted internal input and `monday-review`,
  and record `monday.changed`. Results stay internal and keep the session's
  trust.
- **Reviewed sensitive changes:** structural changes, deletes, notifications,
  automations, workflows, agent management, code/actions, Vibe publication and
  general GraphQL operations record `monday.sensitive`. Results stay internal
  and keep the session's trust. Review includes the affected resources and any
  external destinations.
- **Notifications:** `create_notification` also checks its `user_id` recipient
  against the input audience. A monday audience source resolves that user to a
  confirmed email; absent, inactive, or unconfirmed users are refused. The
  single-user and multi-user targets both carry this check; the base policy
  alone accepts only public notification input.
- **External submissions:** WorkForm submissions and feedback to monday require
  public input and review. Their internal responses mean the exact
  call also needs an authority permitted to approve audience expansion.
- **Returned credentials:** `connect_external_agent` returns a signing secret
  and API token. Its reviewed contract keeps input and output within `self`.

The read actions of `manage_agent`, `manage_agent_triggers`, and
`manage_agent_knowledge` have separate contracts before their reviewed mutation
fallbacks.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of the three meeting reads, `get_monday_knowledge` and `connect_external_agent`. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `monday.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `monday.strip-secrets` | `audience`: `self` → `internal` | Replaces the signing secret and API token `connect_external_agent` returns with `[redacted-secret]`. The session narrows to `internal`, not `self`. The call still needs `monday-review`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The battery's sanitizers name no model. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

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
