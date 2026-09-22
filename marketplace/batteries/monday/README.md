# monday battery

Rules for the hosted Platform MCP at `https://mcp.monday.com/mcp`: 96 tools,
server version 1.0.0, discovered on 2026-09-22. Plain TOML, no helper or provider
credential. Include `appa.toml` in the root config, or install from a release
containing it with `appa battery install monday --server <host-server-name>`.

The [official connection guide](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp)
covers authentication. [appa.toml](appa.toml) links each rule to the inspected
[mondaycom/mcp source](https://github.com/mondaycom/mcp/tree/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e)
or authenticated hosted `tools/list`. The hosted build is not pinned to that
public source revision.

## Rules

*Internal reads* — queries and results stay within `internal`; results enter
`suspicious`. `all_api_read` rejects mutations at the provider, and `read_docs`
reads workspace documents. This group covers:

`agent_catalog`, `all_api_read`, `all_widgets_schema`, `board_insights`,
`explore_meetings`, `get_action`, `get_assets`, `get_automation_runs`,
`get_automation_statistics`, `get_board_activity`, `get_board_info`,
`get_board_items_page`, `get_column_type_info`, `get_form`, `get_graphql_schema`,
`get_meetings_content`, `get_monday_dev_sprints_boards`,
`get_run_once_trigger_entities`, `get_sprint_summary`, `get_sprints_metadata`,
`get_type_details`, `get_updates`, `get_user_context`, `get_workflow_run_once_status`,
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
suspicious/internal because they can include existing content, such as the item
name returned by `create_update`. These tools record `monday.changed`:

`change_item_column_values`, `create_dashboard`, `create_doc`, `create_folder`,
`create_group`, `create_item`, `create_items`, `create_update`, `create_view`,
`create_view_table`, `create_widget`, `finalize_asset_upload`, `get_asset_upload_url`,
`update_doc`, `update_items`, `update_view`, `update_view_table`.

*Sensitive writes* — the same reviewed internal contract, recording
`monday.sensitive`. Review must account for affected resources and destinations,
including nested operations in code, GraphQL, workflows and agents:

`all_api_write`, `all_monday_api`, `create_action`, `create_automation`, `create_board`,
`create_column`, `create_form`, `create_notification`, `create_workflow`,
`create_workspace`, `delete_action`, `delete_view`, `execute_code`,
`form_questions_editor`, `invoke_workflow_expert`, `manage_agent_skills`,
`manage_automations`, `move_object`, `publish_workflow`, `run_action`,
`run_workflow_once`, `stop_workflow_run_once`, `update_action`, `update_column`,
`update_folder`, `update_form`, `update_workspace`, `vibe_create`, `vibe_delete`,
`vibe_publication`, `vibe_update`.

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
Unknown tools follow the root's fallback policy. Provider error text remains
outside successful-output admission and is not sanitized by this battery.

[Policy tests](../../../appa-runtime/tests/monday_policy.rs) check decisions,
labels and effects; marketplace tests check host composition. The
[offline replay](../../../examples/live-replays/monday) uses fictional readers
and simulated approval, and does not execute monday tools.

```sh
cargo test --locked -p appa --test monday_policy --test marketplace
bash scripts/appa-marketplace.sh --check
appa replay --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```
