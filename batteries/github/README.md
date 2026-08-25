# GitHub battery

Rules for the GitHub MCP server (`https://api.githubcopilot.com/mcp/`)
with its default tool sets: your profile, repositories, issues, pull
requests, and user search. Written for public repositories. Add it to your root config with `include`.

## Files

**`appa.toml`** — the rules, in three groups.

*Who am I* — `get_me`, `get_teams`, `get_team_members`, `search_users`
return profile data, nothing written by strangers. No restriction.

*Reads* — every tool that returns repository content, issues, pull
requests, commits, search results, or a secret scan. The text was written by whoever
pushed it, so the result is treated as untrusted, the same way a
fetched web page is. That stops it from steering a later action.

*Writes* — every tool that creates, edits, comments, pushes, merges, or
deletes. Each one publishes to a place anyone can read, so it runs only
with trusted data that may be seen by everyone. Private data cannot be
written to GitHub under these rules. That includes
`issue_write`, `add_issue_comment`, `create_pull_request`,
`merge_pull_request`, `push_files`, and `delete_file`.

Tools from GitHub tool sets outside the default (Actions, Discussions,
Gists, Notifications, Projects, security alerts) are not listed here.
A tool the policy does not name is blocked; add rules for them in your
root config if you enable those sets.

## Change the behaviour

The default assumes public repositories. For a private repository, add
root rules that name it (`repo:`), or its whole organisation (`owner:`),
so its reads come out private and its writes accept private data; the
comment at the top of `appa.toml` shows both.
To make a write ask a person first, add a root rule for that tool with
`attention = ["hitl"]` in its `requires`. Root rules run first. Nothing
in this file needs editing.
