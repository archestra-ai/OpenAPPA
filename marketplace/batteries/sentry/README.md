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

The Claude Code and kagent plugin defaults ship a human authority
permitting every mark (`attention = ["*"]`), so `sentry-review` needs no
wiring there. Another root config must permit it itself:

```toml
[[policy.authority]]
name = "sentry-operator"
hint = "Review the exact Sentry change."
permits = { trust_below = "trusted", attention = ["sentry-review"] }

[externals.authorities.sentry-operator]
uses = "hitl"
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
cargo test --locked -p appa --test integration -- sentry_policy:: marketplace::
```

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of every read and write whose result enters `suspicious`. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `sentry.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `sentry.redact-for-public` | `audience`: `internal` → `public` | Runs only on the issue and event reads: `search_issues` and `search_events`, and the catalog's `get_issue_details`, `get_event_stacktrace`, `search_issues`, `search_events` and `search_issue_events` (tag `sentry-records`). DSNs, attachments, writes and a catalog name outside the battery get no public redaction. Removes user identifiers, emails, IP addresses, request data, breadcrumbs, user reports, project and host names, frames of private code and credentials, and keeps the error type, message, open-source frames, counts and times. The read does not narrow the session's audience, so it adds no audience gap to a later call bound for `public` readers. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts or settles with the other sanitizer. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `uses = "llm"` with an `[externals.llm]` profile, `uses = "claude-code"` where the `claude` CLI is installed, or a `url`. The default config of the Claude Code plugin binds every battery sanitizer to `claude-code`, so in `clappa` sessions it is active once the battery is installed. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tags offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.
