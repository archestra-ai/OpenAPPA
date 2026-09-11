# Notion battery

Rules for the hosted Notion MCP server (`https://mcp.notion.com/mcp`),
all 36 tools on its supported-tools page. Plain TOML rules, no helper
process or provider credential. Add it to your root config with
`include`, or install it with
`appa battery install notion --server <host-server-name>`.

## Rules

*Reads* — search, AI search, fetch, data-source queries, meeting notes,
comments, teams, users, Skills, attachments, and the read side of custom
Agent sessions. Page and database content was written by whoever edits
the workspace and by connected sources, so it enters `suspicious`,
restricted to `internal`. A query's input must be sharable with
`internal` too.

*Writes inside the workspace* — creating and updating pages, databases,
folders, views, comments, and file uploads need trusted data that
`internal` may see, and record `notion.changed`.

*Reviewed changes* — moving pages, changing or trashing a data source,
turning a page into an agent Skill, and spawning, stopping, or messaging
a custom Agent session need the `notion-review` mark and record
`notion.sensitive`. Creating an attachment from a URL makes Notion fetch
that URL, so its input must be public and reviewed.

Your root config must define an authority permitting `notion-review`:

```toml
[[policy.authority]]
name = "notion-operator"
hint = "Review the exact Notion change."
permits = { trust_below = "trusted", attention = ["notion-review"] }

[externals.authorities.notion-operator]
builtin = "hitl"
```

## Limits

Notion exposes no page permissions to a policy: no tool reports who can
see a page, a database, or a teamspace, and the connection acts with
the full permissions of the person who authorized it. So every read is
`internal`, the coarsest honest label, and per-page sharing is not
modeled. Map `internal` in the root config to the people who may see
everything this connection can reach, through your organization's
audience sources, and narrow a page or database in a root rule by
argument (`notion-fetch(id:<page id>)`); root rules run first. A
teamspace-private or privately shared page read through this battery is
labelled `internal` like every other.

The tool list follows Notion's supported-tools page as of 2026-09-10.
The server advertises tools per connection and plan; a tool the policy
does not name is blocked. Clients that see `fetch`, `search`, and
`ai-search` without the `notion-` prefix need root rules under those
names.

```sh
cargo test --locked -p appa --test notion_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
