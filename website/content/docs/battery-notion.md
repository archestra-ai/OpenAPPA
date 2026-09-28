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

## Limit

The Notion connection inherits the authorizing user's full permissions without exposing granular page ACLs. All reads default to `internal`. To restrict an agent to specific pages or databases, scope them in your root `appa.toml` using argument selectors.
