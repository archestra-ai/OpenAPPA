---
title: Cloudflare Observability battery
category: Batteries
order: 6.70
description: "Rules for the Cloudflare Workers Observability MCP server's 8 tools: internal logs, telemetry and Worker reads, plus two public documentation reads."
sidebar: false
breadcrumb: Cloudflare Observability
---

The Cloudflare Observability battery covers Cloudflare's Workers Observability MCP server at `https://observability.mcp.cloudflare.com/mcp`, all eight tools it lists.

Your root config needs no Authority for it: every tool on this server reads, so the battery declares no write, no effect, and no review mark. It must map the `internal` audience, because the account-scoped reads narrow onto it.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/cloudflare-observability).

## Tool behavior

- `query_worker_observability`, `observability_keys`, and `observability_values` read Workers logs, metrics, and invocations. Those logs carry whatever the account's Workers wrote and whatever their callers sent — request paths, headers, bodies, error strings — so they return untrusted `internal` data.
- `workers_list`, `workers_get_worker`, and `workers_get_worker_code` read the account's own Workers. The code download is the deployed bundle, which carries third-party dependencies the policy cannot vouch for, so it is untrusted too.
- `search_cloudflare_documentation` and `migrate_pages_to_workers_guide` return public pages of developers.cloudflare.com and leave the audience alone. The search sends the agent's `query` to Cloudflare's public documentation index, so that query must be sharable with `public`.

## Limits

Cloudflare API tokens grant account-wide log access without exposing per-Worker reader boundaries. All reads default to `internal`. To restrict an agent to specific Workers, scope them in your root `appa.toml` using argument selectors.

Log content is unsanitized. An attacker who can reach one of the account's Workers can write text into the logs the agent reads. The battery labels that text untrusted, which stops it from reaching a tool that needs trusted input, but no Sanitizer strips it.
