# monday battery

Rules for the monday-hosted Platform MCP server
(`https://mcp.monday.com/mcp`): four reads and item creation, with the
other discovered tools blocked. Plain TOML rules, no helper process or
provider credential. Add it to your root config with `include`, or
install it from a release containing this battery with
`appa battery install monday --server <host-server-name>`.

## Source

The tool list comes from an authenticated `tools/list` capture taken on
2026-09-22. `initialize` reported server version 1.0.0 and protocol
`2025-11-25`; the capture returned 96 tools with no further page. The
contracts use those input schemas and controlled live response checks.

## How the server exposes its tools

Each operation has its own tool name and top-level arguments. The
battery admits these five tools with the argument limits below:

| Tool | Arguments admitted by the contract |
| --- | --- |
| `get_user_context` | No arguments |
| `get_board_info` | `boardId` and view/column filters |
| `get_board_items_page` | `boardId`, up to 99 `itemIds`, `limit` from 1–500, `cursor`, `includeColumns`, `includeGroup` |
| `get_updates` | Item `objectId`, `objectType = "Item"`, `limit` from 1–100, `page`; replies and assets must be omitted or false |
| `create_item` | `boardId`, a 1–255 character `name`, and `columnValues = "{}"` |

Board filters only select returned views and columns. The item-page
contract omits search, description, subitem, and ordering arguments;
its 99-ID limit follows the server's requirement of fewer than 100 IDs.

Other arguments are refused. Item creation excludes groups, subitems,
duplication, non-empty column values, and label creation. The other 91
discovered tools have exact blocks in `appa.toml`, including `create_update`,
arbitrary GraphQL, code and action execution, structural mutations,
searches, uploads, workflows, agents, and Vibe publication.

## Rules

*Reads* — the four tools return connected-account and people metadata,
plus board, item, and update content authored by monday users. These
results enter `suspicious`, restricted to `internal`. A query's input
must be sharable with `internal` too.

*Writes* — `create_item` needs trusted data sharable with `public` and
records `monday.changed`. The destination board's readers are unknown,
so the public-input requirement prevents restricted read content from
flowing into it. The admitted response preserves trusted/public state.
The call runs autonomously once its trust and audience requirements are
satisfied; the battery requires no human-review mark.

*Blocked tools* — the reserved `attention = ["blocked"]` mark refuses
these calls without a remedy. No authority can permit this mark.

## Limits

The discovered tools do not report complete effective readers for a
board or item. Map `internal` in the root config to people authorized
to see everything the connection can reach, through your organization's
audience sources. Board metadata alone does not establish that cohort.
A narrower root rule needs independently verified readers and must
repeat the full contract; root rules run first. Installing the battery
does not create the MCP connection or grant monday permissions.

The observed `create_item` response contained a name matching the
supplied input, generated identifiers and URL, the requested board ID,
and a generic message. Its public classification depends on that
manually verified response shape. The server declares no output schema
for this tool, and the runtime does not enforce an output whitelist.
Reverify or remove this contract if the response changes.
`create_update` stays blocked because its response includes
`item_name` read from the existing target item.

Provider failure text is forwarded by the current host/runtime outside
successful-output admission; this battery does not sanitize errors.
The observed invalid-board failure contained only the attempted board
ID and generic request metadata. If a failure can return private or
user-controlled text, disable the write contract until the host/runtime
adds error-output admission.

The rules cover the 2026-09-22 tool inventory. Exact blocks keep those
91 names refused under a deployment's `name = "*"` fallback. A future
name has no battery rule and can match that fallback, so review new
tools and update the policy when the server changes.

The replay in `examples/live-replays/monday` checks policy decisions;
it does not call monday. Live clappa verification uses an authenticated
connection, disposable fixtures, and independent provider-state checks.

```sh
cargo test --locked -p appa --test monday_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
