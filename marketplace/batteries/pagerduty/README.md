# PagerDuty battery

Rules for the PagerDuty-hosted MCP server
(`https://mcp.pagerduty.com/mcp`), all 18 tools. Plain TOML rules, no
helper process or provider credential. Add it to your root config with
`include`, or install it with
`appa battery install pagerduty --server <host-server-name>`.

## Source

The server is closed source and hosted by PagerDuty, so the tool list
comes from a live `tools/list` capture taken on 2026-09-15 with a
PagerDuty API token. `initialize` reported `serverInfo.name` "PagerDuty
MCP Server", version 3.3.1, protocol `2025-06-18`. The capture returned
18 tools with their full `inputSchema` and `annotations`. Every tool
name and action below is from that capture. PagerDuty's
[MCP server page](https://support.pagerduty.com/main/docs/pagerduty-mcp-server)
lists the same 18 tools with the same actions; it prints the actions of
`browse_users` as "list, get" where the capture declares the variants in
the order `get`, `list`, and it writes the `context` action of
`browse_incidents` as "context (related|past|outlier)", which is the one
action with a required `context_type` enum. No other discrepancy.

## How the server exposes its tools

Every tool takes one required argument, `request`, an object whose
`oneOf` variants each carry the operation as a `const` `action`. Eleven
`browse_*` tools carry `readOnlyHint: true`; seven `manage_*` tools
carry `readOnlyHint: false` and `destructiveHint: true`.

| Tool | `request.action` values |
| --- | --- |
| `browse_incidents` | `list`, `get`, `list_alerts`, `get_alert`, `list_notes`, `context`, `list_change_events`, `list_workflows`, `get_workflow` |
| `browse_services` | `list`, `get` |
| `browse_schedules` | `list`, `get`, `list_users`, `list_oncalls` |
| `browse_teams` | `list`, `get`, `list_members` |
| `browse_event_orchestrations` | `list`, `get`, `get_router`, `get_service`, `get_global` |
| `browse_alert_grouping` | `list`, `get` |
| `browse_status_pages` | `list`, `list_severities`, `list_impacts`, `list_statuses`, `get_post`, `list_post_updates` |
| `browse_escalation_policies` | `list`, `get` |
| `browse_users` | `get`, `list` |
| `browse_change_events` | `list`, `get`, `list_service` |
| `browse_activity` | `list_log_entries`, `get_log_entry` |
| `manage_incidents` | `create`, `update`, `add_note`, `add_responders`, `start_workflow` |
| `manage_services` | `create`, `update` |
| `manage_schedules` | `create`, `update`, `create_override` |
| `manage_teams` | `create`, `update`, `delete`, `add_member`, `remove_member` |
| `manage_event_orchestrations` | `update_router`, `append_router_rule` |
| `manage_alert_grouping` | `create`, `update`, `delete` |
| `manage_status_pages` | `create_post`, `create_post_update` |

## Rules

*Reads* — the eleven `browse_*` tools. Incidents, alerts, notes, log
entries, change events, and status page posts were written by monitoring
integrations, responders, and whoever posts to a status page, so reads
enter `suspicious`, restricted to `internal`. A query's input must be
sharable with `internal` too.

*Writes* — the seven `manage_*` tools. Each one needs trusted data that
`internal` may see and the `pagerduty-review` mark.
`manage_incidents` is the operational write path — update an incident,
append a note, page responders, start a workflow — and records
`pagerduty.changed`. The other six change configuration or publish
outward, so they record `pagerduty.sensitive`: `manage_services` and
`manage_schedules` change who gets paged and when, `manage_teams`
changes team membership, `manage_event_orchestrations` rewrites the
router that routes every incoming event, `manage_alert_grouping` changes
how alerts collapse into incidents, and `manage_status_pages` publishes
a post readers outside the account see.

Your root config must define an authority permitting `pagerduty-review`:

```toml
[[policy.authority]]
name = "pagerduty-operator"
hint = "Review the exact PagerDuty change."
permits = { trust_below = "trusted", attention = ["pagerduty-review"] }

[externals.authorities.pagerduty-operator]
builtin = "hitl"
```

## Limits

Every contract covers a whole tool, because the operation lives in
`request.action`. A selector checks top-level string arguments, and
`request` is an object, so no static rule and no root rule can separate
`add_note` from `create` on `manage_incidents`, or `list` from `context`
on `browse_incidents`. A deployment that needs `add_note` without review
while `create` still waits for a person needs an annotator that reads
`request.action`; this battery does not ship one.

PagerDuty scopes visibility by team, and no tool reports which teams may
see an incident, a service, or a schedule. The account-level and
user-level credentials also differ in what they can list. So every read
is `internal`, the coarsest honest label: map it in the root config to
the people who may see everything this token can list, through your
organization's audience sources. Tool filtering is not available on the
hosted server; a Scoped OAuth client is PagerDuty's own way to limit
what a token can do, and it is independent of this battery.

The tool list follows the 2026-09-15 capture of "PagerDuty MCP Server"
3.3.1. Edit the TOML rules when the server changes; a tool the policy
does not name is blocked.

```sh
cargo test --locked -p appa --test pagerduty_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
