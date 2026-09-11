---
title: GitHub battery
category: Batteries
order: 6.63
description: Rules for the GitHub MCP server's 44 default tools; each repository's visibility decides its readers.
sidebar: false
breadcrumb: GitHub
---

The GitHub battery covers the MCP server's default profile, repository, issue, pull-request, and user-search tools. For every tool that names a repository, an annotator asks GitHub whether it is private and labels the call accordingly.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/github).

## Tool behavior

- The viewer's own profile can be read without extra restrictions; team rosters and user search return untrusted profiles that stay with the viewer (`self`).
- Repository, issue, pull-request, commit, search, and secret-scan results are untrusted.
- Content read from a public repository is `public`; content read from a private or Enterprise-internal repository is read by the collection `@github:repo/<owner>/<repo>/collaborators`, which the battery's audience source resolves.
- Writes into a public repository accept only trusted public data; writes into a private repository accept trusted data its collaborators may see. An Enterprise-internal repository is read by every enterprise member, which the source cannot list, so writes into it accept only public data unless a root rule maps those members.
- Searches name no single repository and reach every private repository the token sees, so their results stay with the viewer (`self`). A root rule can treat a search of public repositories as public by its query.

The battery does not include optional GitHub tools. Add rules to the root config before enabling Actions, Discussions, Gists, Projects, or security alerts.

## Audience source

The audience source can build `self`, organization member, organization team, and repository collaborator audiences. The battery binds it and its two annotators; map `self` and `internal` onto it in the root config and pass the token through `APPA_PROVIDER_GITHUB_TOKEN`.

The unit tests use saved GitHub API responses and do not call GitHub. [`examples/github-battery`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/github-battery) replays the battery against real repositories with a token.
