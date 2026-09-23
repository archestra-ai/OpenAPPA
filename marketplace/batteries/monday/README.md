# monday battery

Rules for the hosted Platform MCP at `https://mcp.monday.com/mcp`: 99 tools,
server version 1.0.0, discovered on 2026-09-22. Static contracts cover the
inventory; `audience-source.py` resolves notification recipients. Include
`appa.toml` in the root config, or install from a release
containing it with `appa battery install monday --server <host-server-name>`.

The [official connection guide](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp)
covers authentication. The rules use the inspected
[mondaycom/mcp source](https://github.com/mondaycom/mcp/tree/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e)
and authenticated hosted `tools/list`. The hosted build is not pinned to that
public source revision.

## Rules

*Internal reads* — queries and results stay within `internal`. Most results enter
`suspicious`. `get_graphql_schema`, `get_column_type_info`, and
`get_automation_statistics(breakdown:totals)` return bounded provider metadata
and preserve trust (a fresh trajectory stays `trusted`). The statistics tool's `by_entity`
response remains suspicious. `all_api_read` rejects mutations at the provider,
and `read_docs` reads workspace documents. This group covers:

`agent_catalog`, `all_api_read`, `all_widgets_schema`, `board_insights`,
`explore_meetings`, `get_action`, `get_assets`, `get_assigned_items`, `get_automation_runs`,
`get_automation_statistics`, `get_board_activity`, `get_board_info`,
`get_board_items_page`, `get_column_type_info`, `get_form`, `get_graphql_schema`,
`get_meetings_content`, `get_monday_dev_sprints_boards`,
`get_run_once_trigger_entities`, `get_sprint_summary`, `get_sprints_metadata`,
`get_type_details`, `get_updates`, `get_user_context`, `get_user_mentions`,
`get_user_recent_activity`, `get_workflow_run_once_status`,
`invoke_process_planner`, `list_actions`, `list_automations`, `list_users_and_teams`,
`list_workspaces`, `read_docs`, `search`, `search_meetings_content`, `show-assign`,
`show-battery`, `show-chart`, `show-table`, `validate_workflow`, `vibe_ask`, `vibe_get`,
`vibe_list`, `workspace_info`.

*Mixed tools* — `manage_agent(action:get)`, `manage_agent_triggers(action:list)`
and `manage_agent_knowledge(action:list)` use the read contract. Other actions
use the sensitive-write contract below.

*Public documentation* — `get_monday_knowledge` requires a public question and
returns suspicious/public content.

*Reviewed writes* — trusted internal input and `monday-review`; results remain
suspicious/internal when they can include existing content, such as the item
name returned by `create_update`, or API error details from `create_items` and
`create_doc`. Creation of folders, groups, dashboards and widgets
returns only new object identifiers, trusted input or provider defaults, so
those results preserve trust. `get_asset_upload_url` returns a provider issued
upload ID, URL and expiration and also preserves trust.
These tools record `monday.changed`:

`change_item_column_values`, `create_dashboard`, `create_doc`, `create_folder`,
`create_group`, `create_item`, `create_items`, `create_update`, `create_view`,
`create_view_table`, `create_widget`, `finalize_asset_upload`, `get_asset_upload_url`,
`update_doc`, `update_items`, `update_view`, `update_view_table`.

*Sensitive writes* — the same reviewed internal contract except the
recipient-bound notification below, recording `monday.sensitive`. Review must
account for affected resources and destinations, including nested operations in
code, GraphQL, workflows and agents. `create_board`, `create_column`,
`create_workspace` and `create_form` return bounded new object confirmations and
preserve trust. `move_object(objectType:Folder)` returns a fixed confirmation and
object ID with the same result rule; the other object types remain suspicious.
These tools are:

`all_api_write`, `all_monday_api`, `create_action`, `create_automation`, `create_board`,
`create_column`, `create_form`, `create_notification`, `create_workflow`,
`create_workspace`, `delete_action`, `delete_view`, `execute_code`,
`form_questions_editor`, `invoke_workflow_expert`, `manage_agent_skills`,
`manage_automations`, `move_object`, `publish_workflow`, `run_action`,
`run_workflow_once`, `stop_workflow_run_once`, `update_action`, `update_column`,
`update_folder`, `update_form`, `update_workspace`, `vibe_create`, `vibe_delete`,
`vibe_publication`, `vibe_update`.

`create_notification` additionally requires that its `user_id` recipient can
read the input. The `monday` audience source looks up that user's confirmed
email through the monday Users API. It refuses absent, inactive, or
unconfirmed users and lookup errors. The contract requires trusted input and
`monday-review`; its result remains suspicious/internal.

*External submissions* — `create_form_submission` and
`submit_bug_or_feature_request` require trusted public input and review, and
record `monday.sensitive`. Results remain suspicious/internal, so the combined
input/output check also requires explicit audience-expansion authority, even
from a fresh trajectory.

*Credentials* — `connect_external_agent` requires trusted input sharable with
`self` and review, records `monday.sensitive`, and returns suspicious/self
content containing a signing secret and API token.

## Root configuration

The host owns the authenticated MCP connection. Map `internal` to people
allowed to see everything it can reach, and `self` to the credential owner.
Use your configured audience source's selectors; for example, if it already
serves `directory:members` and `directory:viewer`:

```toml
[policy.audience]
internal = ["directory:members"]
self = ["directory:viewer"]
```

Set `APPA_PROVIDER_MONDAY_TOKEN` where the runtime runs. Its token needs
`users:read` to resolve `@monday:user/$user_id`; this is separate from the
host's MCP connection credential. The source uses API version `2026-07` and
requires a confirmed email so it can compare the recipient with the root's
reader identities. If the root uses different reader identifiers, the source
must be adapted to that mapping before notifications can flow.

Claude Code and kagent defaults provide the human authority. A custom root can
use:

```toml
[[policy.authority]]
name = "monday-operator"
hint = "Review the exact operation, affected resources, destinations and audience expansion."
permits = { trust_below = "trusted", attention = ["monday-review"], audience_missing = ["public"] }

[externals.authorities.monday-operator]
builtin = "hitl"
```

Omit `audience_missing` to forbid audience expansion. Reviewed internal writes
still work; external submissions remain refused. Installation adds policy,
not a connection or provider permissions.

## Limits

The battery does not resolve board ACLs or nested destinations and publication
readers. Root rules can narrow known resources and run before battery rules.
Only the explicit `create_notification` recipient is checked against the
input audience; other nested destinations still require review.
Unknown tools follow the root's fallback policy. Provider error text remains
outside successful-output admission and is not sanitized by this battery.

[Policy tests](../../../appa-runtime/tests/monday_policy.rs) check decisions,
labels and effects; [audience-source tests](test_audience_source.py) check
recipient identity and refusal paths; marketplace tests check host composition. The
[offline replay](../../../examples/live-replays/monday) uses fictional readers
and simulated approval, and does not execute monday tools.

```sh
cargo test --locked -p appa --test monday_policy --test marketplace
python3 -m unittest discover -s marketplace/batteries/monday -p 'test_*.py'
bash scripts/appa-marketplace.sh --check
appa replay --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```
