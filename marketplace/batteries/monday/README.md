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
The complete tool descriptions, input schemas, 11 advertised output schemas and
annotations are retained in
[`monday-tools.json`](../../../appa-runtime/tests/fixtures/monday-tools.json),
with capture metadata and the original capture's SHA-256. This is declaration
evidence, without credentials, tenant responses or UI metadata. Each policy
rule links to its declaration at an immutable repository revision.
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
audience boundaries and wildcard composition. It checks every named tool
against the captured inventory, checks action selectors against provider enums,
and validates test and replay arguments against the captured input schemas.
Descriptions can impose further requirements beyond JSON Schema; schema-valid
synthetic IDs are not proof that a call would succeed against a real account.
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
Tool links open the exact provider declaration with its description and schemas;
implementation links are the public-source cross-check described above.

| Tool | Contract | Implementation reference |
| --- | --- | --- |
| [`agent_catalog`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5976) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/agent-catalog/agent-catalog-tool.ts#L26) |
| [`all_api_read`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2123) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-api-read-tool.ts#L9) |
| [`all_api_write`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2153) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-api-write-tool.ts#L9) |
| [`all_monday_api`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2093) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/all-monday-api-tool.ts#L37) |
| [`all_widgets_schema`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3764) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/all-widgets-schema-tool.ts#L11) |
| [`board_insights`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3841) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/board-insights/board-insights-tool.ts#L56) |
| [`change_item_column_values`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L606) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/change-item-column-values-tool.ts#L36) |
| [`connect_external_agent`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6126) | Reviewed; credentials stay self | Hosted declaration only |
| [`create_action`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7140) | Reviewed sensitive operation | Hosted declaration only |
| [`create_automation`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4309) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/create-automation/create-automation-tool.ts#L22) |
| [`create_board`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5907) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-board-tool.ts#L22) |
| [`create_column`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L1842) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-column-tool.ts#L27) |
| [`create_dashboard`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3710) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/create-dashboard-tool.ts#L30) |
| [`create_doc`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2515) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-doc-tool/create-doc-tool.ts#L94) |
| [`create_folder`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3568) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-folder-tool/create-folder-tool.ts#L25) |
| [`create_form`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L702) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/create-form-tool/index.ts#L11) |
| [`create_form_submission`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L1489) | Reviewed external submission | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/create-submission-tool/index.ts#L13) |
| [`create_group`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2025) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-group/create-group-tool.ts#L29) |
| [`create_item`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L196) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-item-tool/create-item-tool.ts#L52) |
| [`create_items`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L249) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-items-tool/create-items-tool.ts#L67) |
| [`create_notification`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2316) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-notification-tool/create-notification-tool.ts#L22) |
| [`create_update`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L317) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-update-tool/create-update-tool.ts#L40) |
| [`create_view`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5119) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-view-tool/create-view-tool.ts#L59) |
| [`create_view_table`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5229) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-view-table-tool/create-view-table-tool.ts#L103) |
| [`create_widget`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3781) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/create-widget-tool.ts#L30) |
| [`create_workflow`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4731) | Reviewed sensitive operation | Hosted declaration only |
| [`create_workspace`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3525) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/create-workspace-tool/create-workspace-tool.ts#L22) |
| [`delete_action`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7852) | Reviewed sensitive operation | Hosted declaration only |
| [`delete_view`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8434) | Reviewed sensitive operation | Hosted declaration only |
| [`execute_code`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6955) | Reviewed sensitive operation | Hosted declaration only |
| [`explore_meetings`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4946) | Internal read | Hosted declaration only |
| [`finalize_asset_upload`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4192) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/finalize-asset-upload-tool/finalize-asset-upload-tool.ts#L32) |
| [`form_questions_editor`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L1191) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/form-questions-editor-tool/index.ts#L7) |
| [`get_action`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7355) | Internal read | Hosted declaration only |
| [`get_asset_upload_url`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4155) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-asset-upload-url-tool/get-asset-upload-url-tool.ts#L27) |
| [`get_assets`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4126) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-assets-tool/get-assets-tool.ts#L12) |
| [`get_automation_runs`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4341) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/get-automation-runs/get-automation-runs-tool.ts#L73) |
| [`get_automation_statistics`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4463) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/get-automation-statistics/get-automation-statistics-tool.ts#L40) |
| [`get_board_activity`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L425) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-activity/get-board-activity-tool.ts#L36) |
| [`get_board_info`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L477) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-info/get-board-info-tool.ts#L64) |
| [`get_board_items_page`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L12) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-board-items-page-tool/get-board-items-page-tool.ts#L151) |
| [`get_column_type_info`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2213) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-column-type-info/get-column-type-info-tool.ts#L28) |
| [`get_form`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L1167) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/get-form-tool/index.ts#L9) |
| [`get_graphql_schema`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2183) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-graphql-schema-tool.ts#L16) |
| [`get_meetings_content`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5060) | Internal read | Hosted declaration only |
| [`get_monday_dev_sprints_boards`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4568) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprints-boards-tool/get-sprints-boards-tool.ts#L24) |
| [`get_monday_knowledge`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8061) | Public documentation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-monday-knowledge/get-monday-knowledge.ts#L33) |
| [`get_run_once_trigger_entities`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4818) | Internal read | Hosted declaration only |
| [`get_sprint_summary`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4617) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprint-summary-tool/get-sprint-summary-tool.ts#L31) |
| [`get_sprints_metadata`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4585) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/monday-dev-tools/get-sprints-metadata-tool/get-sprints-metadata-tool.ts#L44) |
| [`get_type_details`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2291) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-type-details-tool.ts#L14) |
| [`get_updates`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L355) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-updates-tool/get-updates-tool.ts#L61) |
| [`get_user_context`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4112) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/user-context-tool/user-context-tool.ts#L11) |
| [`get_workflow_run_once_status`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4916) | Internal read | Hosted declaration only |
| [`invoke_process_planner`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4642) | Internal read | Hosted declaration only |
| [`invoke_workflow_expert`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4666) | Reviewed sensitive operation | Hosted declaration only |
| [`list_actions`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7496) | Internal read | Hosted declaration only |
| [`list_automations`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4237) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/list-automations/list-automations-tool.ts#L60) |
| [`list_users_and_teams`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L552) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/list-users-and-teams-tool/list-users-and-teams-tool.ts#L80) |
| [`list_workspaces`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2484) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/list-workspace-tool/list-workspace-tool.ts#L26) |
| [`manage_agent`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6013) | Read on `action:get`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent/manage-agent-tool.ts#L80) |
| [`manage_agent_knowledge`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6312) | Read on `action:list`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-knowledge/manage-agent-knowledge-tool.ts#L48) |
| [`manage_agent_skills`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6257) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-skills/manage-agent-skills-tool.ts#L48) |
| [`manage_agent_triggers`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6168) | Read on `action:list`; reviewed sensitive otherwise | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/agents-tools/manage-agent-triggers/manage-agent-triggers-tool.ts#L47) |
| [`manage_automations`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4273) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/manage-automations/manage-automations-tool.ts#L53) |
| [`move_object`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3646) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/move-object-tool/move-object-tool.ts#L44) |
| [`publish_workflow`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4783) | Reviewed sensitive operation | Hosted declaration only |
| [`read_docs`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2360) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/read-docs-tool/read-docs-tool.ts#L163) |
| [`run_action`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7912) | Reviewed sensitive operation | Hosted declaration only |
| [`run_workflow_once`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4848) | Reviewed sensitive operation | Hosted declaration only |
| [`search`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4039) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/search-tool/search-tool.ts#L223) |
| [`search_meetings_content`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4995) | Internal read | Hosted declaration only |
| [`show-assign`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6673) | Internal read | Hosted declaration only |
| [`show-battery`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6843) | Internal read | Hosted declaration only |
| [`show-chart`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6370) | Internal read | Hosted declaration only |
| [`show-table`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L6504) | Internal read | Hosted declaration only |
| [`stop_workflow_run_once`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4886) | Reviewed sensitive operation | Hosted declaration only |
| [`submit_bug_or_feature_request`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4524) | Reviewed external submission | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/submit-bug-or-feature-request-tool/submit-bug-or-feature-request-tool.ts#L25) |
| [`update_action`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L7637) | Reviewed sensitive operation | Hosted declaration only |
| [`update_column`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L1929) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-column-tool.ts#L40) |
| [`update_doc`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2587) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-doc-tool/update-doc-tool.ts#L67) |
| [`update_folder`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3423) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-folder-tool/update-folder-tool.ts#L39) |
| [`update_form`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L770) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/update-form-tool/index.ts#L7) |
| [`update_items`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L645) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-items-tool/update-items-tool.ts#L58) |
| [`update_view`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5507) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-view-tool/update-view-tool.ts#L63) |
| [`update_view_table`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L5623) | Reviewed ordinary write | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-view-table-tool/update-view-table-tool.ts#L104) |
| [`update_workspace`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L3377) | Reviewed sensitive operation | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/update-workspace-tool/update-workspace-tool.ts#L22) |
| [`validate_workflow`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L4701) | Internal read | Hosted declaration only |
| [`vibe_ask`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8326) | Internal read | Hosted declaration only |
| [`vibe_create`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8095) | Reviewed sensitive operation | Hosted declaration only |
| [`vibe_delete`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8409) | Reviewed sensitive operation | Hosted declaration only |
| [`vibe_get`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8206) | Internal read | Hosted declaration only |
| [`vibe_list`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8277) | Internal read | Hosted declaration only |
| [`vibe_publication`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8375) | Reviewed sensitive operation | Hosted declaration only |
| [`vibe_update`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L8163) | Reviewed sensitive operation | Hosted declaration only |
| [`workspace_info`](https://github.com/archestra-ai/OpenAPPA/blob/5eeb69bcb8a7223ddc603e095c65508b231268dd/appa-runtime/tests/fixtures/monday-tools.json#L2459) | Internal read | [Source](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workspace-info-tool/workspace-info-tool.ts#L14) |
