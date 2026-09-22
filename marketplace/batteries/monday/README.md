# monday battery

Rules for the monday-hosted Platform MCP server
(`https://mcp.monday.com/mcp`), covering its 96 discovered tools. Plain
TOML rules, no helper process or provider credential. Add it to your root
config with `include`, or install it from a release containing this battery
with `appa battery install monday --server <host-server-name>`.

## Source

The tool list, descriptions and input schemas come from an authenticated
`tools/list` capture on 2026-09-22. `initialize` reported server version
1.0.0 and protocol `2025-11-25`; the capture returned 96 tools in one page.
Each rule links to the public implementation or the hosted MCP endpoint whose
`tools/list` declaration was inspected. Hosted discovery requires authentication;
it is not a browsable source-code page. The full discovery response is not part
of the battery or test suite.

The [official integration documentation](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp)
describes the connection. This battery targets Platform MCP, not Apps MCP.

The public server repository is [mondaycom/mcp](https://github.com/mondaycom/mcp).
The implementation references pin commit
[`8fdc0b4`](https://github.com/mondaycom/mcp/tree/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e)
(package `@mondaydotcomorg/monday-api-mcp` 3.3.1). It contains tool
implementations for 64 captured names; the other 32 use the hosted declarations
as evidence. We have no verified mapping from that public commit to the hosted
server's advertised 1.0.0. The dated capture, not the npm package version or an
assumed repository deployment, defines this battery's covered surface.

## How the server exposes its tools

Each operation has its own tool name and top-level arguments. The battery
classifies all 96 names. The server validates ordinary input parameters:
search, pagination, descriptions, subitems, replies, assets, column values,
and duplication remain available.

`manage_agent(action:get)`, `manage_agent_triggers(action:list)` and
`manage_agent_knowledge(action:list)` are read variants before their reviewed
mutation fallbacks. A new action on one of these mixed tools uses its
reviewed fallback. A future tool name follows the root deployment's
unknown-tool policy, including any `name = "*"` annotator.

## Rules

*Internal reads* — boards, items, updates, activity, users, teams, forms,
workspace documents, workspaces, schemas, insights, assets, automations,
sprints, workflow plans and validation, meeting content, agent catalogues,
display tools, saved actions and Vibe inspection. These results enter
`suspicious`, restricted to `internal`; input must be sharable with
`internal` too. `all_api_read` has this contract because the server describes
it as rejecting mutations before dispatch. `read_docs` reads workspace
documents. An asset's temporary download URL does not make its content public.

*Public documentation* — `get_monday_knowledge` answers from the official
knowledge base. Its question must be sharable with `public`, and its output
is suspicious/public.

*Reviewed ordinary writes* — item and comment creation, item updates, docs,
folders, groups, dashboards, widgets, views and asset uploads require trusted
internal input and `monday-review`, and record `monday.changed`. Like Linear
and PostHog, writes return suspicious/internal content: `create_update`, for
example, includes the existing item name. Internal information can flow back
into monday after approval of the exact write; it need not become public.

*Reviewed sensitive operations* — form/schema changes, workspace changes,
moves, board creation, deletes, notifications, automations, workflows, agents,
code and stored-action execution, Vibe generation/publication, and general
GraphQL operations require trusted internal input and `monday-review`, and
record `monday.sensitive`. Review must account for the resources and external
systems an operation can affect, including nested GraphQL mutations and
workflow/agent actions.

*External submissions* — `create_form_submission` can address another
workspace's form, and `submit_bug_or_feature_request` sends content to monday.
They require public input and review, record `monday.sensitive`, and keep
returned provider content suspicious/internal. Because the engine checks the
combined input/output label, these contracts require an authority permitted
to approve audience expansion even from a fresh trajectory. The shipped host
human authority has that permission; approval applies only to the exact call.

*Credentials* — `connect_external_agent` returns a signing secret and API
token. It requires trusted input sharable with `self` and `monday-review`,
records `monday.sensitive`, and returns suspicious/self content. Map `self`
to the credential owner. Credentials cannot flow into ordinary internal
writes without a separate audience-expansion approval.

The Claude Code and kagent defaults supply a human authority permitting
review marks, trust exceptions and audience expansion. Another root config
can use the same pattern:

```toml
[[policy.authority]]
name = "monday-operator"
hint = "Review the exact monday operation, affected resources, destinations and any audience expansion."
permits = { trust_below = "trusted", attention = ["monday-review"], audience_missing = ["public"] }

[externals.authorities.monday-operator]
builtin = "hitl"
```

Omit `audience_missing` when this authority must never approve a wider
recipient set. Internal read-to-write workflows still work; external
submissions and other audience expansions remain refused.

## Root configuration

The battery has no annotator or audience helper and no credential environment
variable. The host owns the authenticated MCP connection. The root must supply
an audience source that serves its real reader cohort and credential owner,
then map its selectors to `internal` and `self`. For example, **if the root
already binds** `directory:members` and `directory:viewer`:

```toml
[policy.audience]
internal = ["directory:members"]
self = ["directory:viewer"]
```

Use the selectors of the deployment's configured source. These names do not
install a source or discover monday permissions. Add the authority shown above
unless the host's existing human authority already covers these requirements.

## Limits

The tool set exposes no complete effective-reader resolver. As in the Notion
battery, map `internal` to people authorized for everything the connection can
reach. This is a deployment assumption, not inferred board ACLs. Root rules
can narrow specific resources; they run before battery rules. Installation
adds policy, not an MCP connection or board permissions.

Tool descriptions establish the classified operations; they do not prove
complete tenant permissions or every live response shape. Returned workspace
content stays suspicious/internal, including write responses. Provider error
text remains outside successful-output admission in the current host/runtime;
this battery does not sanitize it.

## Tests

[`monday_policy.rs`](../../../appa-runtime/tests/monday_policy.rs) exercises
reads, normal input options, reviewed internal writes, sensitive operations,
audience boundaries and wildcard composition. The examples use arguments checked
against the provider declarations; the tests assert policy decisions, result
labels and effects. Synthetic IDs do not prove
that a call would succeed against a real account.
The [`marketplace` suite](../../../appa-runtime/tests/marketplace.rs) composes
the package with both declared hosts, Claude Code and kagent, and checks its
committed digest.

The [offline replay](../../../examples/live-replays/monday) evaluates decisions
with a fictional audience and simulated authority. It does not execute monday
tools or contact the provider. The runtime tests separately probe trust and
public-audience restrictions. Live clappa checks need an authenticated
connection, disposable fixtures and independent verification of provider effects.
There are no battery helper scripts needing credential-free or live API tests.

```sh
cargo test --locked -p appa --test monday_policy --test marketplace
bash scripts/appa-marketplace.sh --check
appa replay --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```

## Questions still requiring evidence

- Which people may receive everything this particular connection can read?
  Board, guest, team and inherited permissions are not resolved by this battery;
  the deployment must establish its `internal` cohort and `self` owner.
- Which public source revision, if any, exactly matches the hosted build, and
  which tools vary by account or entitlement? Re-discover when the service
  changes; the advertised version alone does not pin an immutable deployment.
- What can each complete success and error response contain? Only 11 tools
  advertise output schemas. Provider-owned results remain suspicious/internal
  unless the public-documentation or credential contract applies; the current
  error-output boundary described above remains unresolved.
- Which readers and external systems can a particular workflow, agent, stored
  action, code execution, notification, form or published Vibe app reach?
  The static policy does not enumerate nested destinations or publication ACLs.
  Review must establish them for the proposed operation. Vibe's declaration
  describes availability on the caller's account, not unrestricted web access.
- Does the expanded policy pass real host/provider journeys with fresh fixtures?
  These checked-in tests and replay establish policy behavior, not live effects.

## Covered tools

All 96 captured names have a contract (99 rules including three read variants).
Public source links point to the implementation revision inspected above. For
hosted-only tools, use authenticated `tools/list` at the linked endpoint to
inspect the current declaration; the table records the dated inventory above.

| Tool | Contract | Source |
| --- | --- | --- |
| `agent_catalog` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/agent-catalog/agent-catalog-tool.ts#L26) |
| `all_api_read` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-api-read-tool.ts#L9) |
| `all_api_write` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-api-write-tool.ts#L9) |
| `all_monday_api` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-monday-api-tool.ts#L37) |
| `all_widgets_schema` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/all-widgets-schema-tool.ts#L11) |
| `board_insights` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/board-insights/board-insights-tool.ts#L56) |
| `change_item_column_values` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/change-item-column-values-tool.ts#L36) |
| `connect_external_agent` | Reviewed; credentials stay self | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `create_action` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `create_automation` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/create-automation/create-automation-tool.ts#L22) |
| `create_board` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-board-tool.ts#L22) |
| `create_column` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-column-tool.ts#L27) |
| `create_dashboard` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/create-dashboard-tool.ts#L30) |
| `create_doc` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-doc-tool/create-doc-tool.ts#L94) |
| `create_folder` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-folder-tool/create-folder-tool.ts#L25) |
| `create_form` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/create-form-tool/index.ts#L11) |
| `create_form_submission` | Reviewed external submission | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/create-submission-tool/index.ts#L13) |
| `create_group` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-group/create-group-tool.ts#L29) |
| `create_item` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-item-tool/create-item-tool.ts#L52) |
| `create_items` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-items-tool/create-items-tool.ts#L67) |
| `create_notification` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-notification-tool/create-notification-tool.ts#L22) |
| `create_update` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-update-tool/create-update-tool.ts#L40) |
| `create_view` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-view-tool/create-view-tool.ts#L59) |
| `create_view_table` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-view-table-tool/create-view-table-tool.ts#L103) |
| `create_widget` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/create-widget-tool.ts#L30) |
| `create_workflow` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `create_workspace` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-workspace-tool/create-workspace-tool.ts#L22) |
| `delete_action` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `delete_view` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `execute_code` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `explore_meetings` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `finalize_asset_upload` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/finalize-asset-upload-tool/finalize-asset-upload-tool.ts#L32) |
| `form_questions_editor` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/form-questions-editor-tool/index.ts#L7) |
| `get_action` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `get_asset_upload_url` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-asset-upload-url-tool/get-asset-upload-url-tool.ts#L27) |
| `get_assets` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-assets-tool/get-assets-tool.ts#L12) |
| `get_automation_runs` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/get-automation-runs/get-automation-runs-tool.ts#L73) |
| `get_automation_statistics` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/get-automation-statistics/get-automation-statistics-tool.ts#L40) |
| `get_board_activity` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-activity/get-board-activity-tool.ts#L36) |
| `get_board_info` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-info/get-board-info-tool.ts#L64) |
| `get_board_items_page` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-items-page-tool/get-board-items-page-tool.ts#L151) |
| `get_column_type_info` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-column-type-info/get-column-type-info-tool.ts#L28) |
| `get_form` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/get-form-tool/index.ts#L9) |
| `get_graphql_schema` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-graphql-schema-tool.ts#L16) |
| `get_meetings_content` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `get_monday_dev_sprints_boards` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprints-boards-tool/get-sprints-boards-tool.ts#L24) |
| `get_monday_knowledge` | Public documentation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-monday-knowledge/get-monday-knowledge.ts#L33) |
| `get_run_once_trigger_entities` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `get_sprint_summary` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprint-summary-tool/get-sprint-summary-tool.ts#L31) |
| `get_sprints_metadata` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprints-metadata-tool/get-sprints-metadata-tool.ts#L44) |
| `get_type_details` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-type-details-tool.ts#L14) |
| `get_updates` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-updates-tool/get-updates-tool.ts#L61) |
| `get_user_context` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/user-context-tool/user-context-tool.ts#L11) |
| `get_workflow_run_once_status` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `invoke_process_planner` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `invoke_workflow_expert` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `list_actions` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `list_automations` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/list-automations/list-automations-tool.ts#L60) |
| `list_users_and_teams` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/list-users-and-teams-tool/list-users-and-teams-tool.ts#L80) |
| `list_workspaces` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/list-workspace-tool/list-workspace-tool.ts#L26) |
| `manage_agent` | Read on `action:get`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent/manage-agent-tool.ts#L80) |
| `manage_agent_knowledge` | Read on `action:list`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-knowledge/manage-agent-knowledge-tool.ts#L48) |
| `manage_agent_skills` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-skills/manage-agent-skills-tool.ts#L48) |
| `manage_agent_triggers` | Read on `action:list`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-triggers/manage-agent-triggers-tool.ts#L47) |
| `manage_automations` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/manage-automations/manage-automations-tool.ts#L53) |
| `move_object` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/move-object-tool/move-object-tool.ts#L44) |
| `publish_workflow` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `read_docs` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/read-docs-tool/read-docs-tool.ts#L163) |
| `run_action` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `run_workflow_once` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `search` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/search-tool/search-tool.ts#L223) |
| `search_meetings_content` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `show-assign` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `show-battery` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `show-chart` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `show-table` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `stop_workflow_run_once` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `submit_bug_or_feature_request` | Reviewed external submission | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/submit-bug-or-feature-request-tool/submit-bug-or-feature-request-tool.ts#L25) |
| `update_action` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `update_column` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-column-tool.ts#L40) |
| `update_doc` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-doc-tool/update-doc-tool.ts#L67) |
| `update_folder` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-folder-tool/update-folder-tool.ts#L39) |
| `update_form` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/update-form-tool/index.ts#L7) |
| `update_items` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-items-tool/update-items-tool.ts#L58) |
| `update_view` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-view-tool/update-view-tool.ts#L63) |
| `update_view_table` | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-view-table-tool/update-view-table-tool.ts#L104) |
| `update_workspace` | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-workspace-tool/update-workspace-tool.ts#L22) |
| `validate_workflow` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_ask` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_create` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_delete` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_get` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_list` | Internal read | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_publication` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `vibe_update` | Reviewed sensitive operation | [Hosted tools/list](https://mcp.monday.com/mcp) |
| `workspace_info` | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workspace-info-tool/workspace-info-tool.ts#L14) |
