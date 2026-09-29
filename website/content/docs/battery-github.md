---
title: GitHub battery
category: Batteries
order: 6.63
description: Rules for the GitHub MCP server's 44 default tools; each repository's visibility decides its readers.
sidebar: false
breadcrumb: GitHub
---

The GitHub battery covers the MCP server's default profile, repository, issue, pull-request, and user-search tools. For every tool that names a repository, an annotator asks GitHub whether it is private and labels the call's audience accordingly. Its `github` context provider tells every annotator who wrote a pull request or issue, for these tools and for `gh` and `git push` commands in Claude Code's Bash tool.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/github).

## Tool behavior

- The viewer's own profile can be read without extra restrictions; team rosters and user search return untrusted profiles that stay with the viewer (`self`).
- Content read from a public repository is `public`. Content read from a private or Enterprise-internal repository is read by the collection `@github:repo/<owner>/<repo>/collaborators`, which the battery's audience source resolves.
- A pull request or issue keeps the session's trust only when the context provider shows that every author, commenter, reviewer, editor, and commit author is the repository's owner, member, or collaborator, or an installed GitHub App. An outsider, a list cut short, or a missing answer makes it untrusted, on public and private repositories alike. Listings of issues and pull requests are untrusted. Other content of a public repository or a fork is untrusted; other content of a private or internal repository keeps the session's trust.
- Writes into a public repository accept only trusted public data; writes into a private repository accept trusted data its collaborators may see. An Enterprise-internal repository is read by every enterprise member, which the source cannot list, so writes into it accept only public data unless a root rule maps those members.
- Searches, secret scanning, and org-level listings name no single repository and reach public repositories as well as every private one the token sees, so their results are untrusted and stay with the viewer (`self`). A root rule can treat a search of public repositories as public by its query.

The battery does not include optional GitHub tools. Add rules to the root config before enabling Actions, Discussions, Gists, Projects, or security alerts.

## Audience source

The audience source can build `self`, organization member, organization team, and repository collaborator audiences. The battery binds it, its two annotators, and its context provider; map `self` and `internal` onto it in the root config. The scripts read their token from `APPA_PROVIDER_GITHUB_TOKEN`, or from your GitHub CLI login (`gh auth login`) when the variable is unset; a token needs the `repo` and `read:org` scopes. `appa battery install github` names the variable and the fallback after it includes the battery. The scripts call `https://api.github.com`; set `GITHUB_API_URL` to a GitHub Enterprise Server's `/api/v3` root instead.

A first Claude Code install includes this battery when `gh` is on `PATH`.

The unit tests use saved GitHub API responses and do not call GitHub. [`examples/live-replays/github`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/github) replays the battery against real repositories with a token.
