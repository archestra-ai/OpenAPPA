---
title: monday battery
category: Batteries
order: 6.75
description: Conservative rules for the current monday Platform MCP server, with bounded board/item reads and public-input item creation.
sidebar: false
breadcrumb: monday
---

The monday battery supports four reads and item creation through the Platform
MCP server at `https://mcp.monday.com/mcp`. It blocks the other discovered tools.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/monday).

## Tool behavior

- `get_user_context`, `get_board_info`, `get_board_items_page`, and item-only
  `get_updates` are bounded suspicious reads. They require the root deployment
  to map `internal` to readers authorized for every board reachable by the
  connection. The battery does not infer board ACLs or install an audience
  helper.
- `create_item` admits only a board ID, a 1–255 character public name, and
  `columnValues = "{}"`. Groups, subitems, duplication, and label creation are
  outside the contract.
- `create_update` is explicitly blocked. Its structured response includes
  `item_name` read from the existing target item, so a public body and item ID
  do not make the returned content public.

The bounded `create_item` write requires trusted public input and records
`monday.changed`; it runs autonomously once that floor is satisfied.

The `create_item` write preserves trusted/public state only for the documented
argument subset and retained live response evidence: the observed returned
name matched the supplied name and the remaining fields were generated or
generic. This is manual response evidence, not a runtime-enforced output
whitelist. A restricted monday read cannot
flow into an unknown destination. If a response adds provider content or the
schema changes, remove the exact variant or reverify it before use. A root
deployment may replace an exact rule after independently attesting a complete
reader cohort, but this battery has no ACL resolver.

Provider failure text is forwarded by the current host/runtime outside this
battery's successful-output admission; the battery does not sanitize errors.
The observed invalid-board failure exposed only the attempted board ID and
generic request metadata. If a provider failure can return private board or
user-controlled text, disable this write contract until the host/runtime adds
error-output admission.

`create_update`, arbitrary GraphQL (`all_monday_api`, `all_api_read`, and
`all_api_write`), code and action execution, structural mutations, searches, uploads, workflows,
agents, Vibe tools, and the remaining discovered tools are refused. The six
known unsafe or bypass tools are among the 91 refused snapshot tools, all of
which carry exact `attention = ["blocked"]` rules. Future tool names are not
covered by this frozen inventory: a root `name = "*"` may cover them, so a new
discovery and disposition is required before extending the claim.

The policy replay in
[`examples/live-replays/monday`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/monday)
does not call Monday. A separate authenticated clappa smoke must use a
disposable fixture and verify provider state independently. Installation adds
the policy but does not create a connection or grant Monday permissions.
