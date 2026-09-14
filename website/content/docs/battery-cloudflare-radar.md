---
title: Cloudflare Radar battery
category: Batteries
order: 6.69
description: Rules for the Cloudflare Radar MCP server's 66 tools: public Internet measurement reads, internal URL Scanner reads, and one reviewed scan submission.
sidebar: false
breadcrumb: Cloudflare Radar
---

The Cloudflare Radar battery covers Cloudflare's Radar MCP server at `https://radar.mcp.cloudflare.com/mcp`, all 66 tools it lists: 61 Radar reads and 5 URL Scanner tools.

Your root config must define an Authority permitting `cloudflare-radar-review` for the one write, and map the `internal` audience for the URL Scanner reads.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/cloudflare-radar).

## Tool behavior

- The 61 Radar tools read Cloudflare's public Internet measurement dataset: traffic, outages, BGP, DNS, attacks, bots, certificate transparency, netflows, speed and quality, rankings, and entity lookups. Radar publishes the same numbers to every caller, so results are public. The data is measured outside the trajectory, so it is untrusted.
- Radar arguments are filters — an ASN, a prefix, a date range, a location — sent to the account's own authenticated Cloudflare API, which only reads. No new reader sees them, so these reads carry no audience bound.
- `search_url_scans`, `get_url_scan`, `get_url_scan_screenshot`, and `get_url_scan_har` read scans the token's account owns. Their content is whatever page was scanned, published by a third party, so they return untrusted `internal` data.
- `create_url_scan` makes Cloudflare fetch the `url` the agent supplies, and the scan stays visible to everyone unless the call sets `visibility` to `Unlisted`. It requires trusted data that `public` may see and review, and records `cloudflare-radar.sensitive`.

## Limits

Cloudflare exposes no per-scan readers to a policy. No tool reports whether a stored scan is public or unlisted, so every URL Scanner read is `internal`. Map it to the people who may see everything this token can list, and narrow one account in the root config by argument. Root rules run before battery rules.

`create_url_scan` carries one contract for both visibilities, and it requires `public` for either, because the default is `Public`. A deployment that always scans unlisted can override the rule in its root config with an `internal` bound.

The pinned release marks this server deprecated in its own instructions and points callers at the unified `https://mcp.cloudflare.com/mcp`, whose two generic `search` and `execute` tools run arbitrary Cloudflare API code. That server is not covered here.
