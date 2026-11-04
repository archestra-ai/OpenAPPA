---
title: Cloudflare battery
category: Batteries
order: 6.69
description: "Rules for Cloudflare's documentation and Workers Observability MCP servers: public documentation reads, internal logs, telemetry and Worker reads, no writes."
sidebar: false
breadcrumb: Cloudflare
---

The Cloudflare battery covers two of Cloudflare's hosted MCP servers: documentation (`https://docs.mcp.cloudflare.com/mcp`, two tools) and Workers Observability (`https://observability.mcp.cloudflare.com/mcp`, eight tools, the two documentation tools among them). One namespace is bound to each server you run:

```sh
appa battery install cloudflare --server <docs-server> --server <observability-server>
```

A deployment with one of the two servers binds only that one.

Your root config needs no Authority for it: every tool on these servers reads, so the battery declares no write, no effect, and no review mark. A deployment that runs the Workers Observability server must map the `internal` audience, because the account-scoped reads narrow onto it. The documentation server needs no credential and reaches no Cloudflare account.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/cloudflare).

## Tool behavior

- `query_worker_observability`, `observability_keys`, and `observability_values` read Workers logs, metrics, and invocations. Those logs carry whatever the account's Workers wrote and whatever their callers sent — request paths, headers, bodies, error strings — so they return untrusted `internal` data.
- `workers_list`, `workers_get_worker`, and `workers_get_worker_code` read the account's own Workers. The code download is the deployed bundle, which carries third-party dependencies the policy cannot vouch for, so it is untrusted too.
- `search_cloudflare_documentation` and `migrate_pages_to_workers_guide` return public pages of developers.cloudflare.com and leave the audience alone, on either server. The search sends the agent's `query` to Cloudflare's public documentation index, so that query must be sharable with `public`. An agent that has read internal data cannot search the documentation until that data is released.

## Sanitizers

The battery confines the results of its eight Workers Observability and documentation tools. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `cloudflare.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `cloudflare.redact-for-public` | `audience`: `internal` → `public` | Removes account, zone and Worker identifiers, hostnames, request paths, IP addresses, headers, bodies, user identifiers, credentials and code from Workers logs and telemetry, and keeps errors, status codes and timings. The read does not narrow the session's audience, so it adds no audience gap to a later call bound for `public` readers. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts or settles with the other sanitizer. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.

## Limits

Cloudflare API tokens grant account-wide log access without exposing per-Worker reader boundaries. All reads default to `internal`. To restrict an agent to specific Workers, scope them in your root `appa.toml` using argument selectors.

Log content is unsanitized. An attacker who can reach one of the account's Workers can write text into the logs the agent reads. The battery labels that text untrusted, which stops it from reaching a tool that needs trusted input, but no Sanitizer strips it.

The battery treats every documentation result as public because the corpus behind it is public. A private corpus behind the same tool name would need a different contract.
