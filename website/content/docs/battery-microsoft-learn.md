---
title: Microsoft Learn battery
category: Batteries
order: 6.68
description: Rules for the Microsoft Learn MCP Server's 3 read-only documentation tools, with public queries and untrusted results.
sidebar: false
breadcrumb: Microsoft Learn
---

The Microsoft Learn battery covers all three tools on the public Microsoft Learn MCP Server, `https://learn.microsoft.com/api/mcp`. The server needs no authentication and declares no write, so your root config adds no Authority and no audience mapping for it.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/microsoft-learn).

## Tool behavior

- `microsoft_docs_search` searches Microsoft Learn, `microsoft_code_sample_search` searches the code samples in those pages, and `microsoft_docs_fetch` converts one microsoft.com page to markdown.
- Results are published documentation and sample code that Microsoft and its contributors wrote, so they enter `suspicious`: a page can carry text aimed at the agent. They are already public, so no result restricts the audience.
- Every call sends its own `query` or `url` outward to a public Microsoft endpoint, so each call requires an audience that contains `public`. A trajectory that has read restricted data cannot search or fetch until that data leaves it.
- The battery declares no write, no effect, and no review mark.

## Limit

The server is closed source, so the rules follow a `tools/list` capture from the live endpoint rather than a released tag. Re-capture the list when the release notes announce a change; a tool the policy does not name is blocked.

The endpoint serves one public corpus and exposes no per-resource reader, so `public` is the honest audience for every result. Narrow the pages a trajectory may fetch in the root config by argument. Root rules run before battery rules.
