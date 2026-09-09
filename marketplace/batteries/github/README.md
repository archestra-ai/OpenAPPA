# GitHub battery

Rules for the GitHub MCP server (`https://api.githubcopilot.com/mcp/`)
with its default tool sets: your profile, repositories, issues, pull
requests, and user search. Written for public repositories, shared by the
Claude Code and kagent plugins. Installing the battery adds its policy include;
it does not create the MCP connection or enable tools in either host.

## Files

**`appa.toml`** — the rules, in three groups. Each rule names its tool by
the canonical tool id `mcp/github/<tool>`.

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

**`audience-source.py`** — the `github` audience source. It answers the
stock catalog's selectors over the GitHub REST API:

- `github:viewer` — the token's own reader: its primary verified email
  from `/user/emails`, else `github:<login>`. Feeds `self`.
- `github:org/<org>/members` — one organization's members, bots
  excluded. Feeds `internal`, and only for the organizations a policy
  names — membership in unrelated, open-source, or personal
  organizations never implies `internal`.
- `github:org/<org>/team/<team>` — one organization team, by slug.
  Feeds `group` entries.

A member is the email address GitHub verifies for the account, else
`github:<login>`, which merges with no other provider's reader. For an
organization or team member that is the email published on its
profile, read with one `/users/<login>` call per member, eight at a
time: GitHub lets an account publish only a verified address there. A
collection answer costs one API call per member inside one consult, so a
large organization needs `externals.timeout_ms` sized for it. The member lookup
resolves a `github:<login>` member the same way and answers `null`
for a login GitHub does not know.

The source is not wired by this file: audience mappings are root-only,
and the binding must sit beside them. In the root config:

```toml
[policy.audience]
self = ["github:viewer"]
internal = ["github:org/archestra-ai/members"]

[policy.audience.group.finance]
within = "internal"
from = ["github:org/archestra-ai/team/finance"]

[externals.audience.github]
command = ["python3", "batteries/github/audience-source.py"]
token_env = "APPA_PROVIDER_GITHUB_TOKEN"
```

A command path is resolved against the directory of the config file
that names it, so write the path as your root config sees the battery.

The script reads its token from `APPA_PROVIDER_GITHUB_TOKEN`, which the
binding's `token_env` forwards. The token needs the `read:org` and
`user:email` scopes. A command inherits none of the runtime's `APPA_*`
namespace — not its wiring, not a bearer token it sends, not another
command's credential — only the one `APPA_PROVIDER_*` variable its own
binding names. Any GitHub error or missing answer stops the operation without
recording a decision; nothing is guessed.

**`test_audience_source.py`** — fixture tests over recorded GitHub REST
payloads, no network. Run with `python3 test_audience_source.py`.

## Change the behaviour

The default assumes public repositories. For a private repository, add
root rules that name it (`repo:`), or its whole organisation (`owner:`),
so its reads come out `internal` and its writes accept `internal` data; the
comment at the top of `appa.toml` shows both.
To make a write ask a person first, add a root rule for that tool with
`attention = ["hitl"]` in its `requires`. Root rules run first. Nothing
in this file needs editing.
