---
title: GitHub battery
category: Batteries
order: 6.63
description: Rules for the GitHub MCP server's 44 default tools.
sidebar: false
breadcrumb: GitHub
---

The GitHub battery covers the MCP server's default profile, repository, issue, pull-request, and user-search tools. It assumes repositories are public.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/github).

## Tool behavior

- Profile and team lookups can run without extra restrictions.
- Repository, issue, pull-request, commit, search, and secret-scan results are untrusted.
- Writes accept only trusted public data because their results can be visible to anyone.

The battery does not include optional GitHub tools. Add rules to the root config before enabling Actions, Discussions, Gists, Projects, or security alerts.

## Audience source

The audience source can build `self`, organization member, and organization team audiences. Set it up in the root config and pass its token through `APPA_PROVIDER_GITHUB_TOKEN`.

Its tests use saved GitHub API responses. They do not call GitHub.
