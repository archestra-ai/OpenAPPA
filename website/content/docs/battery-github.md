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
- A pull request or issue keeps the session's trust only when the context provider shows that every author, commenter, reviewer, editor, and commit author is the repository's owner, member, or collaborator, or an installed GitHub App. An outsider, a list cut short, or a missing answer makes it untrusted, on public and private repositories alike. A listing of issues or pull requests keeps the session's trust only when the context provider repeats it in full and shows that every listed item's author and last editor is the repository's owner, member, or collaborator, or an installed GitHub App; otherwise it is untrusted. Other content of a public repository or a fork is untrusted; other content of a private or internal repository keeps the session's trust.
- Writes into a public repository accept only trusted public data; writes into a private repository accept trusted data its collaborators may see. An Enterprise-internal repository is read by every enterprise member, which the source cannot list, so writes into it accept only public data unless a root rule maps those members. A write's result enters the session unchanged, except where it returns stored text: the parent issue that `sub_issue_write` returns gets the label an issue read of it gets, and is untrusted when the context provider has no complete answer for it.
- Searches, secret scanning, and org-level listings name no single repository and reach public repositories as well as every private one the token sees, so their results are untrusted and stay with the viewer (`self`). A root rule can treat a search of public repositories as public by its query.

The battery does not include optional GitHub tools. Add rules to the root config before enabling Actions, Discussions, Gists, Projects, or security alerts.

## Sanitizers

The battery confines the results of the 27 reads that return text other people wrote; the target policies tag the same reads. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `github.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience does not change. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.

## Audience source

The audience source can build `self`, organization member, organization team, and repository collaborator audiences. The battery binds it, its two annotators, and its context provider; map `self` and `internal` onto it in the root config. The scripts read their token from `APPA_PROVIDER_GITHUB_TOKEN`, or from your GitHub CLI login (`gh auth login`) when the variable is unset; a token needs the `repo` and `read:org` scopes. `appa battery install github` names the variable and the fallback after it includes the battery. The scripts call `https://api.github.com`; set `GITHUB_API_URL` to a GitHub Enterprise Server's `/api/v3` root instead.

A first Claude Code install includes this battery when `gh` is on `PATH`.

The unit tests use saved GitHub API responses and do not call GitHub. [`examples/live-replays/github`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/github) replays the battery against real repositories with a token.
