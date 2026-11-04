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

## Sanitizers

The battery confines the results of every read and write whose result enters `suspicious`. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `sentry.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `sentry.redact-for-public` | `audience`: `internal` → `public` | Removes user identifiers, emails, IP addresses, request data, breadcrumbs, user reports, project and host names, frames of private code and credentials, and keeps the error type, message, open-source frames, counts and times. The read does not narrow the session's audience, so it adds no audience gap to a later call bound for `public` readers. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts or settles with the other sanitizer. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.
