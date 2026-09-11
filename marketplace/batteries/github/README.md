# GitHub battery

Rules for the GitHub MCP server (`https://api.githubcopilot.com/mcp/`)
with its default tool sets: your profile, repositories, issues, pull
requests, and user search. Shared by the Claude Code and kagent plugins.
Installing the battery adds its policy include; it does not create the
MCP connection or enable tools in either host.

## Files

**`appa.toml`** — the rules, in five groups. Each rule names its tool by
the canonical tool id `mcp/github/<tool>`.

*Who am I* — `get_me` returns the viewer's own profile, nothing written
by a stranger. No restriction.

*Rosters and user search* — `get_teams`, `get_team_members`, and
`search_users` return profiles other people wrote, from every
organization the token sees: untrusted, and read by the viewer (`self`)
until a person widens it.

*Reads of one repository* — every tool that names a repository with
`owner` and `repo`: file contents, branches, commits, tags, releases,
collaborators, issues, labels, pull requests. The text was written by
whoever pushed it, so the result is untrusted, the same way a fetched
web page is. Who may see it is the repository's visibility, which the
`github.repository-visibility` annotator asks GitHub for on each call:
`public` for a public repository, the collection
`@github:repo/<owner>/<repo>/collaborators` for a private one and for an
Enterprise `internal` one (every enterprise member may read it; its
collaborators are the bound this source can list). Content read from a
non-public repository can then go only where its collaborators read.

*Reads across repositories* — code, commit, issue, pull-request and
repository searches, secret scanning, and org-level field listings name
no single repository and reach every private repository the token
sees. Their results are untrusted and stay with the viewer (`self`);
a root rule can treat a search of public repositories as public by its
query.

*Writes into one repository* — every tool that creates, edits, comments,
pushes, merges, or deletes in a named repository. The
`github.repository-readers` annotator asks GitHub for the repository's
visibility: a write into a public repository runs only with trusted data
that may be seen by everyone, a write into a private repository with
trusted data its collaborators may see. An Enterprise `internal`
repository is read by every enterprise member, a collection this source
cannot list, so a write into it needs data everyone may see; a root rule
mapping the enterprise's members can relax that. A summary of a private
repository's issues can go back into that repository's issues and not
into a public one.

*Writes that name no existing repository* — `create_repository` runs
with trusted public data.

A repository the token cannot see gets no answer from either annotator,
and the call is refused; nothing is guessed public.

Tools from GitHub tool sets outside the default (Actions, Discussions,
Gists, Notifications, Projects, security alerts) are not listed here.
A tool the policy does not name is blocked; add rules for them in your
root config if you enable those sets.

**`repository-visibility.py`** — the two annotators, one script. A
consult carries the call's `owner` and `repo`; the script reads
`GET /repos/{owner}/{repo}` and answers the contract for that
repository. The policy's mandate for the call admits exactly
`@github:repo/<owner>/<repo>/collaborators`, and the script refuses a
consult whose mandate names anything else (exit status 2) before it
reads a token.

**`audience-source.py`** — the `github` audience source. It answers
these selectors over the GitHub REST API:

- `github:viewer` — the token's own reader: its primary verified email
  from `/user/emails`, else `github:<login>`. Feeds `self`.
- `github:org/<org>/members` — one organization's members, bots
  excluded. Feeds `internal`, and only for the organizations a policy
  names — membership in unrelated, open-source, or personal
  organizations never implies `internal`.
- `github:org/<org>/team/<team>` — one organization team, by slug.
  Feeds `group` entries.
- `github:repo/<owner>/<repo>/collaborators` — one repository's
  collaborators as GitHub lists them: direct and outside collaborators,
  and for an organization repository the members who reach it through a
  team or the organization's base permission. Named by the annotators'
  placeholder above; listing it needs push access to the repository.

A collection of more than 1,000 accounts is refused rather than resolved:
each member's profile is one request, and a larger roster cannot answer
inside the runtime's consult budget. Map such an audience in the root
config from a source that lists it in bulk.

A member is the email address GitHub verifies for the account, else
`github:<login>`, which merges with no other provider's reader. For an
organization, team, or repository member that is the email published on
its profile, read with one `/users/<login>` call per member, eight at a
time: GitHub lets an account publish only a verified address there. A
collection answer costs one API call per member inside one consult, so a
large organization needs `externals.timeout_ms` sized for it. The member
lookup resolves a `github:<login>` member the same way and answers
`null` for a login GitHub does not know.

The battery binds the source itself, under `[externals.audience.github]`
in `appa.toml`, and declares the four templates above as its
`selectors`. Audience mappings are root-only, so the root config maps
the chain onto them:

```toml
[policy.audience]
self = ["github:viewer"]
internal = ["github:org/archestra-ai/members"]

[policy.audience.group.finance]
within = "internal"
from = ["github:org/archestra-ai/team/finance"]
```

Every consult carries the declared templates, and the script refuses
one whose declaration differs from what it serves (exit status 2), so a
policy and a script of different versions never answer each other.

Both scripts read their token from `APPA_PROVIDER_GITHUB_TOKEN`, which
each binding's `token_env` forwards, and call `https://api.github.com`
unless `GITHUB_API_URL` names another root, as it does for a GitHub
Enterprise Server (`https://<host>/api/v3`). The token needs the `read:org` and
`user:email` scopes, and `repo` for the private repositories it reads
and lists collaborators of. A command inherits none of the runtime's
`APPA_*` namespace — not its wiring, not a bearer token it sends, not
another command's credential — only the one `APPA_PROVIDER_*` variable
its own binding names. Any GitHub error or missing answer stops the
operation without recording a decision; nothing is guessed.

**`test_audience_source.py`**, **`test_repository_visibility.py`** —
tests without network: recorded GitHub REST payloads for the selectors,
answer shaping and consult refusals for the annotators, and the envelope
and declaration checks. Run with `python3 -m unittest discover -s . -p
'test_*.py'`.

## Try it against GitHub

[`examples/live-replays/github`](../../../examples/live-replays/github) replays
one public and one private repository through the battery with a real
token: reads narrow to the private repository's collaborators, writes
into it accept what they may see.

## Change the behaviour

To make a write ask a person first, add a root rule for that tool with
`attention = ["hitl"]` in its `requires`. To treat a search of public
repositories as public by its query, or one repository's content as
`internal` instead of its collaborators, add a root rule naming it
(`query:`, `repo:`) or its whole organisation (`owner:`). Root rules run
first. Nothing in this file needs editing.
