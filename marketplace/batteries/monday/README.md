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
The names are retained independently in
[`monday-tools.json`](../../../appa-runtime/tests/fixtures/monday-tools.json).
The [official integration documentation](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp)
describes the connection. This battery targets Platform MCP, not Apps MCP.

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

The offline replay and focused runtime tests exercise reads, normal input
options, reviewed internal writes, sensitive operations, audience boundaries
and wildcard composition. They do not execute monday tools. Live clappa checks need an authenticated
connection, disposable fixtures and independent verification of provider effects.

```sh
cargo test --locked -p appa --test monday_policy --test marketplace
bash scripts/appa-marketplace.sh --check
appa replay --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```
