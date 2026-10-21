---
title: Archestra battery
category: Batteries
order: 6.75
description: Checks every share made through Archestra's built-in tools against who it makes data readable by.
sidebar: false
breadcrumb: Archestra
---

The Archestra battery covers the sharing, discovery, reporting, and agent run tools of Archestra's built-in MCP server, and builds audiences from Archestra's organization, teams, and members.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/archestra).

## Sharing

A share makes something readable by more people, so it needs trusted data those people may already see:

- sharing with the whole organization requires data sharable with `internal`;
- sharing with teams requires data sharable with each team, `@archestra:team/<team>`; and
- adding a member to a team requires data sharable with that member, `@archestra:user/<user>`.

This covers projects, apps, knowledge bases, knowledge connectors, plugins, agents, and MCP gateways, including a personal plugin shared with named members. An update that sends a team or member list is checked against that list even when it names no scope, since it replaces who the resource is shared with. A connector that mirrors its source system's permissions has readers no source here can list; they are assumed to be within the organization, so it requires data sharable with `internal`. Keeping a resource personal or private requires nothing. A call may name at most 100 teams.

## Discovery

`search_tools` lists the tools an agent can call. `search_files` lists the files an agent can reach. `load_skill` loads a skill's instructions and bundled files. These tools add no label and require nothing. An agent can look up tools, files, and skills at any point in a session.

## Reporting

`yell` files a guardrails report with the organization's admins. It is how an agent escalates a block, so it adds no label and requires nothing: no label the session picked up can refuse it.

## Agent runs

`start_agent_run`, `get_agent_run`, `steer_agent_run`, and `cancel_agent_run` add no label and require nothing. Archestra crosses a run's input and output itself. A start or a steer joins the session's label into the run before the run reads the message. A run's result reaches the session only as a value the run returned, and that value already carries the run's label. `list_agent_runs` adds `internal`, because it names the organization's agents, requesters, and channels.

The workspace tools and the tools that post to a run's messaging thread have no rule in this battery. The deployment's annotator labels them.

## Audiences

The source builds:

- `internal` from the organization's members;
- a team's members from `team/<team>`, by team id or name; and
- one member from `user/<user>`, by user id or email.

Every member is reported as their email, so Archestra readers compare with readers from any other email-based source, such as Google Workspace. Map `internal` in the root config:

```toml
[policy.audience]
internal = ["archestra:members"]
```

Set `ARCHESTRA_BASE_URL` to your Archestra API and pass an API key allowed to read organization membership through `APPA_PROVIDER_ARCHESTRA_TOKEN`.

Its tests serve recorded answers from a local HTTP server. They do not call Archestra.
