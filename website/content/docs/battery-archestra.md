---
title: Archestra battery
category: Batteries
order: 6.75
description: Rules for every tool of Archestra's built-in MCP server, with shares checked against the teams and members they reach.
sidebar: false
breadcrumb: Archestra
---

The Archestra battery gives a rule to every tool of Archestra's built-in MCP server, and builds audiences from Archestra's organization, teams, and members.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/archestra).

## Tools Archestra rules itself

The battery declares no rule for these tools:

- `yell` is ruled as `mcp/appa/yell`. Archestra gives it an empty rule, so no label the session picked up can refuse a report.
- `run_tool` and `preview_app_tool` are ruled as the tool they run.
- `execute_remedy_plan`, `get_remedy_plans`, `list_peer_messages`, and `read_peer_message` carry the runtime's own recovery and peer messages.
- The app data and `llm_complete` tools exist only inside a running app. No agent lists them.

## Reads

- Discovery and the session's own surfaces add no label: `search_tools`, `search_files`, `load_skill`, `ask_user`, `todo_write`, and `whoami`. A validation or a yell resolve adds no label either.
- A read of Archestra's configuration, rosters, guardrails policy, yells, helper consults, files, apps, and run state adds `internal`.
- A read that returns text that strangers can write adds `internal` and `suspicious`: `query_knowledge_sources` (synced mail and tickets) and `get_schedule_trigger_run_transcript` (every tool result the run read).

## Writes

- A write that removes or controls a shared resource requires `trusted` data. This covers deletes and unassigns, guardrails policy and validation publishes, policy repository connection and sync, credential bindings, schedule controls, team changes, limits, and tool assignments.
- A write that other members read or run also requires data sharable with `internal`. This covers agents, MCP gateways and servers, hooks, skills, plugins, apps, scheduled tasks, posts to a run's messaging thread, and credential transfers.
- A write to persistent files, which a project or app can share and a later read returns as trusted, requires `trusted` data sharable with `internal`. So does an app spec or a knowledge base update; a personal agent, plugin or knowledge base create requires `trusted`.
- `run_command` routes to the `archestra.run-command` Annotator. An Archestra root answers it with its own model (`builtin = "archestra"`).

## Sharing

A share makes something readable by more people, so it needs trusted data those people may already see:

- sharing with the whole organization requires data sharable with `internal`;
- sharing with teams requires data sharable with each team, `@archestra:team/<team>`; and
- adding a member to a team requires data sharable with that member, `@archestra:user/<user>`.

`publish_app` and `update_knowledge_connector` select their rule by scope or visibility. An update that sends a `team_ids` list is checked against that list even when it names no visibility, since it replaces who the connector is shared with. A connector that mirrors its source system's permissions has readers no source here can list, so it requires data sharable with `internal`. A call may name at most 100 teams.

The Archestra proxy adds string arrays no selector could read from the original objects: `initialGrantPrincipals` (`<type>:<id>`, the organization as `organization`, sent only with a non-empty `initialGrants`) and `transferModes` (`value` or `link` per credential). A create that sends grants requires `trusted` data sharable with `internal`, which holds every principal a grant can name; without grants it requires what the create alone needs. A credential transfer that only returns links declares a `key` and `label` every reader of the agent sees, so it requires `internal`; any other transfer requires `trusted` and `internal`.

## Agent runs

A run and its workspace are one container the organization may read, because admins can open any run. The same rules apply on every host, including a host whose calls carry no LLM Proxy proof. On such a host, data written into a run is read back with the run's trust, so suspicious data steered into an unbound run can return trusted. Through the LLM Proxy, the run's return crossing labels it.

- `start_agent_run`, `steer_agent_run`, `write_workspace_file`, and a `transfer_workspace_file` upload put session data into the run. They require data sharable with `internal`.
- `get_agent_run`, `read_workspace_file`, and a `transfer_workspace_file` download bring the run's data into the session. They add `internal` and keep the session's trust.
- `cancel_agent_run` returns only the run's state. It adds no label and requires nothing.
- `list_agent_runs` adds `internal`, because it names the organization's agents, requesters, and channels.
- `post_agent_run_message` and `post_agent_run_file` post to the run's messaging thread. They require `trusted` data sharable with `internal`.

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
