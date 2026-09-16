# PostHog battery

Rules for PostHog's official MCP server (`@posthog/agent-toolkit`, hosted
at `https://mcp.posthog.com/mcp` or run from the
[PostHog/mcp](https://github.com/PostHog/mcp) repository). Plain TOML
rules, no helper process or provider credential. Add it to your root
config with `include`, or install it with
`appa battery install posthog --server <host-server-name>`.

## How the server exposes its tools

The server builds its tool list from one registry,
`schema/tool-definitions.json`, which holds 44 entries.
`typescript/src/tools/index.ts` maps each entry to its handler and
`getToolsForFeatures` filters the list.

The hosted endpoint reads a `features` query parameter and exposes one
subset per feature: `flags`, `insights`, `dashboards`, `experiments`,
`surveys`, `error-tracking`, `events`, `llm-analytics`, `workspace`, and
`docs`. Without the parameter it exposes every tool. The server then
drops any tool whose `required_scopes` the personal API key does not
carry. The battery names all 44 registry entries, so every subset is
covered.

## Rules

*Reads* — analytics queries, insights, dashboards, error tracking,
feature flag and experiment definitions, experiment results, survey
definitions and response statistics, LLM analytics, event and property
definitions, organizations, and projects. PostHog holds what the
customer's own end users produced, so these reads enter `suspicious`,
restricted to `internal`, and a query's input must be sharable with
`internal` too.

The query tools are read-only. `query-run` accepts only a trends,
funnel, or HogQL node (`InsightQuerySchema` in
`typescript/src/schema/query.ts`) and posts it to
`/api/environments/<id>/query/`, which reads
(`typescript/src/api/client.ts`). `insight-query` runs a stored
insight's own query through the same endpoint.
`query-generate-hogql-from-question` sends a natural-language question
to PostHog's `max_tools/create_and_query_insight/` endpoint and returns
the generated SQL and its results.

*Documentation* — `docs-search` sends its `query` to Inkeep
(`api.inkeep.com`) and returns public PostHog documentation, so its
input must be sharable with `public`.

*Reviewed writes* — creating or updating an insight, a dashboard, or a
survey, and adding an insight to a dashboard, need trusted data that
`internal` may see and the `posthog-review` mark, and record
`posthog.changed`.

*Reviewed sensitive writes* — feature flag writes record
`posthog.sensitive`, because a flag gates production behaviour. So do
experiment writes: `experiment-create` takes the
`feature_flag_key` of the flag it owns, and `experiment-update` carries
`launch`, which starts serving the variants. Every deletion —
`delete-feature-flag`, `insight-delete`, `dashboard-delete`,
`experiment-delete`, `survey-delete` — records `posthog.sensitive` too.

The Claude Code and kagent plugin defaults ship a human authority
permitting every mark (`attention = ["*"]`), so `posthog-review` needs no
wiring there. Another root config must permit it itself:

```toml
[[policy.authority]]
name = "posthog-operator"
hint = "Review the exact PostHog change."
permits = { trust_below = "trusted", attention = ["posthog-review"] }

[externals.authorities.posthog-operator]
builtin = "hitl"
```

The credential is the personal API key the server authenticates with
(`phx_…`), held by the host's MCP server entry, not by this battery.

## Limits

PostHog has per-project access that a policy cannot see. The server
reports no readers for a project, an organization, an insight, or a
dashboard, so every read is `internal`, the coarsest honest label: map
`internal` in the root config to the people who may see everything this
key can reach, through your organization's audience sources. Narrow one
project in a root rule by argument
(`get-llm-total-costs-for-project(projectId:12345)`); root rules run
first.

`switch-organization` and `switch-project` change which project the
server's later calls target. They change no PostHog data, so they carry
no effect, but the registry marks them `readOnlyHint: false`. They keep
the read contract, which holds while `internal` covers everyone the key
can reach. Pin one project in a root rule when it must not move.

`query-generate-hogql-from-question` sends its `question` to PostHog's
Max endpoint, which passes it to the model provider PostHog runs. The
question can carry trajectory data; the battery treats the call as an
`internal` read.

The registry entry `property-definitions` has no handler in
`typescript/src/tools/index.ts` at this commit, so the server cannot
expose it. The battery names it anyway, as a read, so wiring it changes
nothing here.

The tool list follows `PostHog/mcp` commit `13aaf2c`
(`@posthog/agent-toolkit` 0.2.2), read on 2026-09-14. The repository
publishes release tags only for its Python `posthog_agent_toolkit`
package, so the commit is the pin. Edit the TOML rules when the registry
changes; a tool the policy does not name is blocked.

```sh
cargo test --locked -p appa --test posthog_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
