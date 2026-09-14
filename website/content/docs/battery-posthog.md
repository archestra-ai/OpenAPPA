---
title: PostHog battery
category: Batteries
order: 6.72
description: Rules for PostHog's MCP server, all 44 registry tools, with internal analytics reads and reviewed writes.
sidebar: false
breadcrumb: PostHog
---

The PostHog battery covers PostHog's official MCP server: all 44 tools in its registry. The hosted endpoint exposes one feature subset per `features` query parameter and every tool without it, so the battery names the whole registry.

Your root config must define an Authority permitting `posthog-review` for writes.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/posthog).

## Tool behavior

- Analytics queries, insights, dashboards, error tracking, experiment results, survey statistics, LLM analytics, and event and property definitions hold what the customer's end users produced. They return untrusted `internal` data.
- The query tools read only: `query-run` accepts a trends, funnel, or HogQL node and posts it to PostHog's query endpoint.
- `docs-search` returns public PostHog documentation and sends its query to Inkeep, so its input must be public.
- Creating or updating an insight, a dashboard, or a survey requires trusted internal data and review, and records `posthog.changed`.
- Feature flag and experiment writes record `posthog.sensitive`: a flag gates production behaviour, and an experiment owns the flag it launches. Every deletion records `posthog.sensitive` too.

## Limit

PostHog has per-project access that a policy cannot see. No tool reports who can read a project, an insight, or a dashboard, so every read is `internal`. Map it to the people who may see everything the API key can reach, and narrow one project in the root config by argument. Root rules run before battery rules.

`switch-organization` and `switch-project` move which project later calls target. They change no PostHog data and carry no effect. Pin one project in a root rule when it must not move.
