---
title: Sentry battery
category: Batteries
order: 6.66
description: Rules for the Sentry MCP server's 9 listed tools and 55 catalog tools, with internal reads and reviewed writes.
sidebar: false
breadcrumb: Sentry
---

The Sentry battery covers the Sentry MCP server: the nine primary tools plus 55 catalog tools dispatched dynamically through `execute_sentry_tool`.

Writes need `sentry-review`. The Claude Code and kagent plugin defaults permit every mark, so the person running the session reviews them; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/sentry).

## Tool behavior

- Issues, events, stack traces, replays, profiles, releases, monitors, alert rules, and dashboards were produced by the monitored applications and their users. They return untrusted `internal` data.
- Documentation lookups fetch public pages, so their input must be public.
- Changing an issue, adding a note, starting a Seer run, and onboarding updates require trusted internal data and review, and record `sentry.changed`.
- Creating or changing a team, project, DSN, or uptime monitor records `sentry.sensitive`.
- A catalog name the policy does not know needs a person.

Sentry tokens do not expose per-project reader boundaries to policy, so all reads default to `internal`. To restrict an agent to specific projects, scope them in your root policy using argument selectors.
