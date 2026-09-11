---
title: Sentry battery
category: Batteries
order: 6.66
description: Rules for the Sentry MCP server's 9 listed tools and 55 catalog tools, with internal reads and reviewed writes.
sidebar: false
breadcrumb: Sentry
---

The Sentry battery covers the Sentry MCP server: the nine tools it lists and the 55 catalog tools behind `execute_sentry_tool`, each named through the `name` argument, so the inner tool decides what a call is.

Your root config must define an Authority permitting `sentry-review` for writes.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/sentry).

## Tool behavior

- Issues, events, stack traces, replays, profiles, releases, monitors, alert rules, and dashboards were produced by the monitored applications and their users. They return untrusted `internal` data.
- Documentation lookups fetch public pages, so their input must be public.
- Changing an issue, adding a note, starting a Seer run, and onboarding updates require trusted internal data and review, and record `sentry.changed`.
- Creating or changing a team, project, DSN, or uptime monitor records `sentry.sensitive`.
- A catalog name the policy does not know needs a person.

Sentry exposes no per-project readers to a policy, so every read is `internal`. Map it to the people who may see everything the token can list, and narrow one project in the root config by argument. Root rules run before battery rules.
