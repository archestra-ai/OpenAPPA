# Microsoft Learn battery

Rules for the Microsoft Learn MCP Server, the public remote endpoint
`https://learn.microsoft.com/api/mcp` (Streamable HTTP, no
authentication). Its `initialize` response reports
`serverInfo.name = "Microsoft Learn MCP Server"` and
`serverInfo.version = "1.0.0"`. Plain TOML rules, no helper process and
no provider credential. Add it to your root config with `include`, or
install it with
`appa battery install microsoft-learn --server <host-server-name>`.

## How the server exposes its tools

The server is closed source, so the authoritative tool list is the live
`tools/list` response. This battery follows a capture from
`https://learn.microsoft.com/api/mcp` on **2026-09-14**
(`protocolVersion` `2025-06-18`). It lists three tools:

| Tool | Input schema | Annotations |
| --- | --- | --- |
| `microsoft_docs_search` | `query` (string, no `required` list) | `readOnlyHint = true`, `idempotentHint = true`, `destructiveHint = false` |
| `microsoft_code_sample_search` | `query` (string, required), `language` (string, optional) | `readOnlyHint = true`, `idempotentHint = true`, `destructiveHint = false` |
| `microsoft_docs_fetch` | `url` (string, required) | `readOnlyHint = true`, `idempotentHint = true`, `destructiveHint = false` |

The [official overview](https://learn.microsoft.com/en-us/training/support/mcp)
and the [MicrosoftDocs/mcp](https://github.com/MicrosoftDocs/mcp) README
document the same three tools and the same arguments, and state that the
endpoint needs no authentication.

## Rules

Results are published documentation and sample code, written by Microsoft
and its contributors rather than by this deployment. They enter
`suspicious`, because a page can carry text aimed at the agent. They are
already public, so no result restricts the audience.

Every call sends its own input outward to a public Microsoft endpoint —
the free-text `query`, or the `url` to fetch — so each call requires an
audience that contains `public`. A trajectory that has read restricted
data cannot search or fetch until that data leaves it.

```toml
[[policy.tool]]
name = "mcp/microsoft-learn/microsoft_docs_search"
delta = { trust = "suspicious" }
requires = { audience = { contains = ["public"] } }
```

The battery declares no write: all three tools are read-only, and the
server holds no account, profile, or training-record state a call could
change. It therefore defines **no review mark**, no effect, and no
attention requirement. Your root config adds no authority and no
`[policy.audience]` mapping for this battery, and the battery reads no
credential variable.

## Limits

The server is closed source. The rules follow the captured `tools/list`
response, not a released tag, so re-capture the list when the
[release notes](https://learn.microsoft.com/en-us/training/support/mcp-release-notes)
announce a change: a tool the policy does not name is blocked.

`microsoft_docs_search` declares `query` without a `required` list, so a
call with no `query` still matches the rule. The rule needs no argument,
so the missing constraint changes no decision.

The endpoint is unauthenticated and serves one public corpus, so there is
no per-resource reader to express and no narrower audience than `public`.
`microsoft_docs_fetch` accepts only microsoft.com HTML pages; the server
enforces that, and the policy does not restrict the `url` further.
Narrow it in a root rule by argument if you want a smaller set of pages
(`microsoft_docs_fetch(url:https://learn.microsoft.com/en-us/azure/*)`);
root rules run first.

```sh
cargo test --locked -p appa --test microsoft_learn_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```

## Sanitizers

The battery confines the results of its three tools. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `microsoft-learn.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.
