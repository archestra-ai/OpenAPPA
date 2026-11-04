---
title: Notion battery
category: Batteries
order: 6.67
description: Rules for the hosted Notion MCP server's 36 tools, with internal reads and reviewed structural changes.
sidebar: false
breadcrumb: Notion
---

The Notion battery covers all 36 tools on the hosted Notion MCP server's supported-tools page.

Structural and agent-session changes need `notion-review`. The Claude Code and kagent plugin defaults permit every mark, so the person running the session reviews them; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/notion).

## Tool behavior

- Fetch, data-source queries, comments, users, teams, Skills, and the read side of custom Agent sessions return `internal` data and keep the session's trust: workspace members write pages, databases, and comments, and a member published each form or connected each synced source.
- `notion-search`, `notion-ai-search`, and `notion-query-meeting-notes` return untrusted `internal` data: search also reaches connected sources such as mail, and AI meeting notes carry what outside participants said.
- Creating and updating pages, databases, folders, views, comments, and uploads require trusted internal data and record `notion.changed`.
- Moving pages, changing or trashing a data source, turning a page into a Skill, and driving Agent sessions require review and record `notion.sensitive`.
- Creating an attachment from a URL makes Notion fetch it, so its input must be public and reviewed.

## Sanitizers

The battery confines the results of the two searches and the meeting-note query. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `notion.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.

## Limit

The Notion connection inherits the authorizing user's full permissions without exposing granular page ACLs. All reads default to `internal`. To restrict an agent to specific pages or databases, scope them in your root `appa.toml` using argument selectors.
