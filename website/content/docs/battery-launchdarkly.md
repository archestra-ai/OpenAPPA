---
title: LaunchDarkly battery
category: Batteries
order: 6.71
description: Rules for LaunchDarkly's official MCP server, all 20 tools, with internal reads and every write reviewed.
sidebar: false
breadcrumb: LaunchDarkly
---

The LaunchDarkly battery covers all 20 tools that LaunchDarkly's official MCP server registers: feature flags, environments, AI Configs, code references, and the audit log.

Your root config must define an Authority permitting `launchdarkly-review` for writes.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/launchdarkly).

## Tool behavior

- Flags, AI Configs, environments, code references, and audit log entries return untrusted `internal` data. Engineers author the configuration, but the same reads carry context keys and attribute values that identify end users, source lines around each flag reference, and free-text audit comments.
- Creating or updating a flag or an AI Config changes what the application serves in production. It requires trusted internal data and review, and records `launchdarkly.changed`.
- Deleting a flag, an AI Config, or an AI Config variation records `launchdarkly.sensitive`.

## Limit

LaunchDarkly scopes API access by project, environment, and custom role, but no tool reports who may read a project or an environment. Every read is therefore `internal`. Map it to the people who may see everything the token can reach, and narrow one project in the root config by argument. Root rules run before battery rules.

This version exposes no project or environment write tool, so the battery names none.
