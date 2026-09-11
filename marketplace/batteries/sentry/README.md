# Sentry battery

Rules for the Sentry MCP server (`@sentry/mcp-server`, hosted at
`https://mcp.sentry.dev/mcp` or run locally), version 0.39. Plain TOML
rules, no helper process or provider credential. Add it to your root
config with `include`, or install it with
`appa battery install sentry --server <host-server-name>`.

## How the server exposes its tools

`tools/list` shows nine tools. Seven read or change Sentry directly:
`find_organizations`, `find_projects`, `search_events`, `search_issues`,
`get_sentry_resource`, `update_issue`, `analyze_issue_with_seer`. The
other two are a catalog: `search_sentry_tools` finds a tool by name, and
`execute_sentry_tool` runs any of the 55 catalog tools by its `name`
argument. The battery names each catalog tool through that argument —
`mcp/sentry/execute_sentry_tool(name:get_issue_details)` — so the inner
tool decides what the call is. A `name` outside the catalog matches the
bare `execute_sentry_tool` rule and needs a person.

## Rules

Reads return what the monitored applications and their users produced,
so they enter `suspicious`, restricted to `internal`, and a query's
input must be sharable with `internal`. `search_docs` and `get_doc`
fetch public pages, so their input must be public. Writes need trusted
data that `internal` may see and the `sentry-review` mark, and record
`sentry.changed`; creating or changing a team, project, DSN, or uptime
monitor records `sentry.sensitive` instead.

Your root config must define an authority permitting `sentry-review`:

```toml
[[policy.authority]]
name = "sentry-operator"
hint = "Review the exact Sentry change."
permits = { trust_below = "trusted", attention = ["sentry-review"] }

[externals.authorities.sentry-operator]
builtin = "hitl"
```

## Limits

Sentry exposes no per-project or per-team readers a policy could name,
so every read is `internal`: map it in the root config to the people who
may see everything this token can list, through your organization's
audience sources. Narrow one project in a root rule by argument
(`search_issues(projectSlugOrId:payments)`); root rules run first.

`search_events`, `search_issues`, `search_issue_events`, and
`get_trace_details` may send their free-text query to the LLM provider
the server is configured with (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or
`OPENROUTER_API_KEY`); that provider is the deployment's own choice.

The tool list follows `@sentry/mcp-core` 0.39.0 (`toolDefinitions.json`,
57 entries). Edit the TOML rules when it changes; a tool the policy does
not name is blocked.

```sh
cargo test --locked -p appa --test sentry_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
