---
title: Archestra battery
category: Batteries
order: 6.75
description: Checks every share made through Archestra's built-in tools against who it makes data readable by.
sidebar: false
breadcrumb: Archestra
---

The Archestra battery covers the sharing tools of Archestra's built-in MCP server, and builds audiences from Archestra's organization, teams, and members.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/archestra).

## Sharing

A share makes something readable by more people, so it needs trusted data those people may already see:

- sharing with the whole organization requires data sharable with `internal`;
- sharing with teams requires data sharable with each team, `@archestra:team/<team>`; and
- adding a member to a team requires data sharable with that member, `@archestra:user/<user>`.

This covers projects, apps, knowledge bases, knowledge connectors, plugins, agents, and MCP gateways. A connector that mirrors its source system's permissions has readers no source here can list, so it requires data sharable with `public`. Keeping a resource personal or private requires nothing. A call may name at most 32 teams.

A personal plugin shared with named members through `userIds` is not covered yet.

## Audiences

The source builds:

- `internal` from the organization's members;
- a team's members from `team/<team>`, by team id or name; and
- one member from `user/<user>`, by user id or email.

Every member is reported as their lowercased email, so Archestra readers compare with readers from any other email-based source, such as Google Workspace. Map `internal` in the root config:

```toml
[policy.audience]
internal = ["archestra:members"]
```

Set `ARCHESTRA_BASE_URL` to your Archestra API and pass an API key allowed to read organization membership through `APPA_PROVIDER_ARCHESTRA_TOKEN`.

Its tests serve recorded answers from a local HTTP server. They do not call Archestra.
