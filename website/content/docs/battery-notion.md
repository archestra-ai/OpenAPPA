---
title: Notion battery
category: Batteries
order: 6.67
description: Rules for the hosted Notion MCP server's 36 tools, with internal reads and reviewed structural changes.
sidebar: false
breadcrumb: Notion
---

The Notion battery covers all 36 tools on the hosted Notion MCP server's supported-tools page.

Your root config must define an Authority permitting `notion-review` for structural and agent-session changes.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/notion).

## Tool behavior

- Search, fetch, data-source queries, meeting notes, comments, users, teams, Skills, and the read side of custom Agent sessions return untrusted `internal` data.
- Creating and updating pages, databases, folders, views, comments, and uploads require trusted internal data and record `notion.changed`.
- Moving pages, changing or trashing a data source, turning a page into a Skill, and driving Agent sessions require review and record `notion.sensitive`.
- Creating an attachment from a URL makes Notion fetch it, so its input must be public and reviewed.

## Limit

Notion exposes no page permissions to a policy: no tool reports who can see a page, a database, or a teamspace, and the connection acts with the full permissions of the person who authorized it. Every read is therefore `internal`, and per-page sharing is not modeled. Map `internal` to the people who may see everything the connection can reach, and narrow a page or database in the root config by argument. Root rules run before battery rules.
