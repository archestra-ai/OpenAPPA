---
title: Cloudflare docs battery
category: Batteries
order: 6.69
description: "Rules for Cloudflare's documentation MCP server: two public reads whose query must be sharable with public."
sidebar: false
breadcrumb: Cloudflare docs
---

The Cloudflare docs battery covers Cloudflare's documentation MCP server at `https://docs.mcp.cloudflare.com/mcp`, both of the tools it lists.

Your root config needs no Authority for it. The server needs no credential and reaches no Cloudflare account.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/cloudflare-docs).

## Tool behavior

- `search_cloudflare_documentation` and `migrate_pages_to_workers_guide` return pages of the public Cloudflare developer documentation. That text was written outside the trajectory, so both reads are untrusted, and neither narrows the audience.
- `search_cloudflare_documentation` sends the agent's `query` to Cloudflare's public documentation index, so the query must be sharable with `public`. An agent that has read internal data cannot search the documentation until that data is released.
- `migrate_pages_to_workers_guide` takes no argument and fetches one fixed URL, so it carries nothing out and needs no audience bound.
- Nothing on this server writes. The battery declares no effect and no review mark.

## Limit

The battery treats every result as public because the corpus behind the tool, developers.cloudflare.com, is public. A private corpus behind the same tool name would need a different contract.
