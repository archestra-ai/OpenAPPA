# LaunchDarkly battery

Rules for LaunchDarkly's official MCP server
([`launchdarkly/mcp-server`](https://github.com/launchdarkly/mcp-server)),
version 0.6.2 (tag `v0.6.2`, commit
`5617287c35f0f5726a30bb7b125fd6fb2db90745`). Plain TOML rules, no helper
process or provider credential. Add it to your root config with
`include`, or install it with
`appa battery install launchdarkly --server <host-server-name>`.

## How the server exposes its tools

The server is generated from LaunchDarkly's OpenAPI spec by Speakeasy.
`src/mcp-server/server.ts` is the tool registry: it calls `tool(...)`
once per tool, and each `src/mcp-server/tools/<name>.ts` file declares
that tool's `name`, `scopes`, and `args`. The registry registers 20
tools, 10 with the `read` scope and 10 with the `write` scope:

| Tool | Scope | Contract |
| --- | --- | --- |
| `get-audit-log-entries` | read | internal read |
| `get-code-references` | read | internal read |
| `get-environments` | read | internal read |
| `get-flag-status-across-environments` | read | internal read |
| `list-feature-flags` | read | internal read |
| `get-feature-flag` | read | internal read |
| `list-ai-configs` | read | internal read |
| `get-ai-config` | read | internal read |
| `get-ai-config-variation` | read | internal read |
| `get-ai-config-targeting` | read | internal read |
| `create-feature-flag` | write | reviewed, `launchdarkly.changed` |
| `update-feature-flag` | write | reviewed, `launchdarkly.changed` |
| `create-ai-config` | write | reviewed, `launchdarkly.changed` |
| `update-ai-config` | write | reviewed, `launchdarkly.changed` |
| `update-ai-config-targeting` | write | reviewed, `launchdarkly.changed` |
| `create-ai-config-variation` | write | reviewed, `launchdarkly.changed` |
| `update-ai-config-variation` | write | reviewed, `launchdarkly.changed` |
| `delete-feature-flag` | write | reviewed, `launchdarkly.sensitive` |
| `delete-ai-config` | write | reviewed, `launchdarkly.sensitive` |
| `delete-ai-config-variation` | write | reviewed, `launchdarkly.sensitive` |

Every tool declares exactly one argument, `request`, which carries the
operation's own fields (`projectKey`, `featureFlagKey`, `env`, and so
on). The rules therefore name each tool by its id alone and need no
argument matching.

## Rules

*Reads* — flag and AI Config configuration is authored by the
organization's own engineers, but the same reads return data the
organization does not author. A flag's targeting carries context keys
and attribute values that identify the application's end users
(`Target.values` and `Clause.values`), `get-code-references` returns
source lines around each flag reference, and audit log entries carry
free-text `comment`, `description`, and `title` fields. All of it reaches
the agent as text, so every read enters `suspicious`, restricted to
`internal`. A query's input must be sharable with `internal` too.

*Writes* — every write changes a live flag or AI Config, which changes
what the application serves in production. Writes need trusted data that
`internal` may see and the `launchdarkly-review` mark, and record
`launchdarkly.changed`. The three deletes destroy configuration and
record `launchdarkly.sensitive`. A create or update returns the changed
object, so it labels the trajectory like a read; a delete returns no
content and changes no label.

The Claude Code and kagent plugin defaults ship a human authority
permitting every mark (`attention = ["*"]`), so `launchdarkly-review`
needs no wiring there. Another root config must permit it itself:

```toml
[[policy.authority]]
name = "launchdarkly-operator"
hint = "Review the exact LaunchDarkly change."
permits = { trust_below = "trusted", attention = ["launchdarkly-review"] }

[externals.authorities.launchdarkly-operator]
builtin = "hitl"
```

## Limits

LaunchDarkly scopes API access by project, environment, and custom role,
but no tool reports who may read a project or an environment, so
`internal` is the coarsest honest audience. Map it in the root config to
the people who may see everything this token can reach, through your
organization's audience sources, and narrow one project in a root rule by
argument; root rules run first.

This version exposes no tool that creates, changes, or deletes a project
or an environment, and no segment or metric tool, so the battery names
none. `get-environments` reads them. A future version that adds project
or environment writes needs new rules with the
`launchdarkly.sensitive` effect.

The tool list follows `launchdarkly/mcp-server` `v0.6.2`
(`src/mcp-server/server.ts`, 20 registrations). Edit the TOML rules when
it changes; a tool the policy does not name is blocked. The server also
filters its own tool list by `--tool` and by scope, which removes tools
but never renames them.

```sh
cargo test --locked -p appa --test launchdarkly_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of its 17 tools that return flag, environment, AI Config, code reference or audit log data. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `launchdarkly.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.
