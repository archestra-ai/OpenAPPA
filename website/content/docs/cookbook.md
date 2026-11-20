---
title: Policy cookbook
category: Get started
order: 1.5
description: Copy-pasteable appa.toml configs organized by operator goal — allow/ask/deny rules, shadow rollout, shared team configs, battery overrides, and sensitive directories.
---

Each recipe solves one deployment goal with a minimal `appa.toml` you can copy and verify with `appa describe --config appa.toml --check`. None of these starting configs require an external user directory. See [How it works](/how-it-works) for information-flow control across tool calls and [Policy configuration](/contracts) for every schema field.

| Goal | Key in `appa.toml` |
|---|---|
| [Allow, ask, or deny without labels](#allow-ask-or-deny-tool-calls-without-labels) | `delta = {}`, `attention`, `blocked` |
| [Roll out a policy in shadow mode](#roll-out-a-policy-in-shadow-audit-mode) | `builtin = "approve"` and `except_tags` |
| [Share a base config across teams](#share-a-base-config-across-teams) | Top-level `include` and `mcp/*.toml` |
| [Override one rule from a battery](#override-one-noisy-rule-from-a-battery) | Root `[[policy.tool]]` before `include` |
| [Gate a sensitive directory](#gate-a-sensitive-directory) | `delta` audience + `requires` audience |

## Allow, ask, or deny tool calls without labels

OpenAPPA enforces stateless allow, human-approval, and deny rules on individual tool calls before you configure any `audience` or `trust` labels.

```toml
[policy]
version = 2

[[policy.tool]]
name = "mcp/deploy/status"
delta = {}

[[policy.tool]]
name = "mcp/deploy/release(env:prod|production)"
requires = { attention = ["human-approval"] }

[[policy.tool]]
name = "mcp/deploy/release"
delta = {}

[[policy.tool]]
name = "mcp/deploy/destroy"
requires = { attention = ["blocked"] }

[[policy.authority]]
name = "reviewer"
permits = { attention = ["human-approval"] }

[externals.authorities.reviewer]
builtin = "hitl"
```

OpenAPPA evaluates `[[policy.tool]]` rules from top to bottom and uses the first match. Selectors in parentheses (`arg:pattern`) match top-level string arguments, or non-empty string arrays where every element matches. The wildcard `*` matches any character sequence and `|` separates alternatives; missing arguments, null values, or non-string types do not match and fall through to the next rule.

- **Allow (`delta = {}`):** Runs the tool call immediately and leaves the `trajectory` (this session's tool-call history and security `label`) unchanged. A `release` call with `env = "staging"` skips `(env:prod|production)` and matches the bare `mcp/deploy/release` fallback below it.
- **Ask (`attention = ["human-approval"]`):** Suspends the call and prompts the person running the session via `builtin = "hitl"`. Approval applies to that single call; the next production release asks again.
- **Deny (`attention = ["blocked"]`):** Rejects the call unconditionally. `blocked` is a reserved mark that no `authority` can approve, even with `permits = { attention = ["*"] }`.
- **Default deny:** Any tool call that matches zero `[[policy.tool]]` rules fails closed before execution.

Because `attention` checks each call in isolation, it ignores what the agent read earlier. To let internal work run without prompts while blocking outbound leaks, pair `delta` on read tools with `requires` on outbound tools (see [Gate a sensitive directory](#gate-a-sensitive-directory)).

## Roll out a policy in shadow (audit) mode

To audit unmet policy requirements on live traffic without prompting a person or failing the tool call, bind a catch-all `authority` to `builtin = "approve"`.

```toml
[policy]
version = 2

[[policy.tool]]
name = "mcp/web/fetch"
delta = { trust = "suspicious" }

[[policy.tool]]
name = "mcp/slack/post_message"
requires = { trust = "trusted" }

[[policy.authority]]
name = "rollout"

[policy.authority.permits]
trust_below = "trusted"
audience_missing = ["public"]
attention = ["*"]

[externals.authorities.rollout]
builtin = "approve"
```

When `requires` blocks a call and `rollout` covers the unmet gap, OpenAPPA approves the call automatically at the block, records the `rollout` ruling in the trajectory `log` and [OpenTelemetry](/observability), and releases the call without interrupting the agent. Remove the `rollout` authority and its `[externals]` binding when you are ready to enforce the policy.

Boundaries during a shadow rollout:
- **Hard blocks remain active:** Calls to undeclared tools (with no matching `[[policy.tool]]` and no `"*"` wildcard) and calls requiring `attention = ["blocked"]` still fail closed.
- **Agent-accepted restrictions:** When a tool result narrows the trajectory's `label` (such as `delta = { trust = "suspicious" }` above) or spawns a subagent, no human is prompted, but the agent still confirms that tightened label programmatically before continuing.

### Enforce high-risk tools first

To enforce critical sinks immediately while keeping the rest of the policy in shadow mode, tag those tools and exclude the tag on `rollout` with `except_tags`:

```toml
[policy]
version = 2

[[policy.tool]]
name = "mcp/web/fetch"
delta = { trust = "suspicious" }

[[policy.tool]]
name = "mcp/slack/post_message"
requires = { trust = "trusted" }

[[policy.tool]]
name = "mcp/payments/send"
tags = ["enforced"]
requires = { trust = "trusted" }

[[policy.authority]]
name = "rollout"
except_tags = ["enforced"]

[policy.authority.permits]
trust_below = "trusted"
audience_missing = ["public"]
attention = ["*"]

[externals.authorities.rollout]
builtin = "approve"
```

After `mcp/web/fetch` lowers trust to `suspicious`, `mcp/slack/post_message` is auto-approved and logged, whereas `mcp/payments/send` stays blocked because no authority covers its `trust` gap. Every tag in `except_tags` must appear on at least one tool (or policy load fails). Note that `except_tags` excludes `audience`, `trust`, and prior-`effects` gaps only — `attention` marks always follow `permits.attention`, so if an enforced tool relies on an `attention` mark, list only the audited marks in `permits.attention` instead of `["*"]`.

## Share a base config across teams

Top-level `include` composes arbitrary local TOML files and single-directory globs — not only pre-built [batteries](/batteries) (reusable policy bundles for MCP servers). Place shared organization rules beside `appa.toml`, drop per-server files into a subdirectory, and keep team overrides in `appa.toml`:

```text
.
|-- appa.toml
|-- org-base.toml
`-- mcp/
    `-- github.toml
```

```toml
# appa.toml
include = [
  "org-base.toml",
  "mcp/*.toml",
]

[policy]
version = 2

[[policy.tool]]
name = "mcp/slack/post_message(channel:C0TEAMDEV)"
requires = { audience = { contains = ["internal"] } }

[[policy.authority]]
name = "reviewer"
permits = { attention = ["*"] }

[externals.authorities.reviewer]
builtin = "hitl"
```

```toml
# org-base.toml
[policy]
version = 2

[[policy.tool]]
name = "mcp/crm/get_customer"
delta = { audience = ["internal"] }

[[policy.tool]]
name = "mcp/slack/post_message"

[policy.tool.requires]
audience = { contains = ["internal"] }
attention = ["human-approval"]
```

**Evaluation order (first matching `[[policy.tool]]` rule wins across all files):**
1. Root `appa.toml` rules, from top to bottom.
2. Included files in `include` list order, relative to `appa.toml`.
3. Single-directory globs (`mcp/*.toml`) in alphabetical filename order. A missing `mcp/` directory matches zero files without error, so teams can add server files incrementally.

Above, posting internal CRM context to `C0TEAMDEV` matches the root rule and runs without a prompt; posting to any other channel falls through to `org-base.toml` and asks the root's `reviewer` authority, whose `permits = { attention = ["*"] }` covers `attention` marks declared in any included file.

**Composition constraints:**
- Every file (root and included) must declare `[policy]` with `version = 2`.
- Only the root `appa.toml` may declare `include` (no nested includes), the `[policy.audience]` directory-mapping table, `[aliases.mcp]`, or `[aliases.host]`.
- Globs support `*` in the filename only (`mcp/*.toml`, not `**/*.toml`), must stay inside the root config's directory, and apply to local file configs only (not hosted deployments).

## Override one noisy rule from a battery

After installing a [battery](/batteries) (for example, `appa battery install sentry`), add a narrower `[[policy.tool]]` rule in your root `appa.toml`. Root rules evaluate before `include`, so matching calls take your rule while everything else falls through to the battery unchanged.

```toml
include = ["batteries/sentry/appa.toml"]

[policy]
version = 2

# Keep delta, requires, and tags — overrides do not merge.
[[policy.tool]]
name = "mcp/sentry/update_issue(organizationSlug:staging)"
delta = { trust = "suspicious", audience = ["internal"] }
tags = ["sentry"]

[policy.tool.requires]
trust = "trusted"
audience = { contains = ["internal"] }

[[policy.authority]]
name = "reviewer"
permits = { attention = ["sentry-review"] }

[externals.authorities.reviewer]
builtin = "hitl"
```

> **Overrides do not merge fields.** A matching root rule replaces the battery's entire contract for that call. Copy the `delta`, `requires`, and `tags` fields you want to preserve: writing `delta = {}` alone above would drop the `trusted` and `internal` checks on staging updates and detach battery sanitizers scoped to `tags = ["sentry"]`.

The [Sentry battery](/battery-sentry) requires `attention = ["sentry-review"]` on every `mcp/sentry/update_issue` call and leaves the `authority` binding to your root config. With the root selector above, updates in `staging` run autonomously while production updates fall through to the battery's rule and ask `reviewer`.

## Gate a sensitive directory

Assign reads of a sensitive directory a `delta` audience instead of an `attention` prompt: the agent reads and computes over those files uninterrupted, and OpenAPPA blocks only outbound calls to unauthorized recipients.

```toml
[policy]
version = 2

[[policy.tool]]
name = "mcp/files/read(path:/srv/payroll/*)"
delta = { audience = ["payroll@corp.example"] }

[[policy.tool]]
name = "mcp/files/read"
delta = {}

[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["$to"] } }
```

In `contains = ["$to"]`, `$to` binds the required top-level `to` string argument of `mcp/mail/send` (calls omitting `to` or passing a non-string value are refused before execution; an empty or unrecognized string fails the audience check once narrowed):
- Reading `/srv/readme.md` matches the fallback rule and leaves the trajectory `public`.
- Reading `/srv/payroll/q4.csv` narrows the trajectory's `audience` to `payroll@corp.example`.
- Mailing `payroll@corp.example` passes without a prompt; mailing any other recipient is denied before `mcp/mail/send` executes.
- To forbid reading a directory outright instead of tracking its contents, replace `delta` on that selector rule with `requires = { attention = ["blocked"] }`.

> **Claude Code local files (`clappa`):** Because shell commands can also read and copy files on disk, `clappa` routes `Read`, `Write`, `Edit`, and `Bash` through `appa file-tracker`. Add local directory rules (`[[path]] pattern = "~/payroll/*"`) in `~/.config/appa/file-tracker.toml` so shell commands enforce the same label — see [Files that keep their labels](/coding-agents#files-that-keep-their-labels).

Test any of the configs above against scripted tool calls before rollout with [`appa replay`](/validation).
