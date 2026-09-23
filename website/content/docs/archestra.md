---
title: Archestra
nav_title: Archestra
category: Works with
order: 5
description: Deliver OpenAPPA to every developer machine through the Archestra platform.
---

[Archestra](https://archestra.ai) is an open source enterprise platform for AI agents: an LLM proxy, an MCP gateway, and skills catalog.

:::sponsor-note:::

## How it works

Archestra implements OpenAPPA at the LLM proxy level. Every agent that talks to a model through the proxy is covered by the same policy, whether it runs in a coding CLI, a SaaS app, or a service in production, and no per-agent integration work is needed. 

That makes Archestra a starting point for securing agents with OpenAPPA across an enterprise at scale and at low cost.

## Try it

Follow Archestra's [quickstart](https://archestra.ai/docs/platform-quickstart) or [deployment guide](https://archestra.ai/docs/platform-deployment). OpenAPPA support is in beta: without the beta switch, Archestra ships with its pre-OpenAPPA deterministic guardrails. Enable it with `ARCHESTRA_BETA=true`, as described in [Deployment](https://archestra.ai/docs/platform-deployment#skills-marketplace). Once enabled, OpenAPPA gets its own page in the Archestra sidebar:

![The Archestra sidebar with a red arrow pointing at the OpenAPPA entry](/images/archestra-openappa-sidebar.webp)
