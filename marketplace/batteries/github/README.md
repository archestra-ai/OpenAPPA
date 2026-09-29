# GitHub battery

Rules for the GitHub MCP server (`https://api.githubcopilot.com/mcp/`)
with its default tool sets: your profile, repositories, issues, pull
requests, and user search. Shared by the Claude Code and kagent plugins.
Installing the battery adds its policy include; it does not create the
MCP connection or enable tools in either host.

## Files

**`appa.toml`** — the rules, in six groups. Each rule names its tool by
the canonical tool id `mcp/github/<tool>`.

*Who am I* — `get_me` returns the viewer's own profile, nothing written
by a stranger. No restriction.

*Rosters and user search* — `get_teams`, `get_team_members`, and
`search_users` return profiles other people wrote, from every
organization the token sees: untrusted, and read by the viewer (`self`)
until a person widens it.

*Reads of one repository* — every tool that names a repository with
`owner` and `repo`: file contents, branches, commits, tags, releases,
collaborators, issues, labels, pull requests. The audience follows the
repository's visibility, which the `github.repository-visibility`
annotator asks GitHub for on each call: a public repository's content is
`public`; a private or internal one's is read by the collection
`@github:repo/<owner>/<repo>/collaborators`. For an Enterprise `internal`
repository every enterprise member may read it; its collaborators are the
bound this source can list. Content read from a non-public repository can
then go only where its collaborators read.

Trust follows who wrote the text, and visibility alone never grants it:

| Read | Keeps the session's trust when | Otherwise |
| --- | --- | --- |
| one pull request or issue (`pull_request_read`, `issue_read`) | the `github` context answer for that item shows every author, commenter, reviewer, editor, and commit author is an `OWNER`, `MEMBER`, or `COLLABORATOR` or a bot (an installed GitHub App), and `truncated` is `false` | `suspicious`, also when the provider answered an error or nothing |
| a listing of issues or pull requests (`list_issues`, `list_pull_requests`) | the `github` context answer for that repository holds the listing with `truncated` `false`, and every listed item's author, and its last editor when it was edited, is an `OWNER`, `MEMBER`, or `COLLABORATOR` or a bot | `suspicious`, also when the provider answered an error or nothing |
| other content (files, commits, branches, tags, releases) | the repository is private or internal and is no fork | `suspicious`: anyone may open the pull requests merged into a public repository, and a fork's content came from its parent |

*Reads across repositories* — code, commit, issue, pull-request and
repository searches, secret scanning, and org-level field listings name
no single repository and reach public repositories as well as every
private one the token sees. Their results enter `suspicious` and stay
with the viewer (`self`);
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
consult carries the call's `owner` and `repo` and the `github` context
entry; the script reads `GET /repos/{owner}/{repo}` for the visibility
and whether the repository is a fork, and answers the contract for that
repository. The policy's mandate for the call admits exactly
`@github:repo/<owner>/<repo>/collaborators`, and the script refuses a
consult whose mandate names anything else (exit status 2) before it
reads a token.

**`context.py`** — the `github` context provider, bound under
`[externals.context.github]`. The runtime asks it about every call owing
a fresh annotation, before the annotator. It recognizes two kinds of
call:

- `mcp/github/<tool>` with `owner` and `repo`, and `pullNumber` or
  `issue_number` when the tool names one;
- a `Bash` command running `gh` or `git push`. The repository comes from
  a GitHub URL, a `repos/OWNER/NAME` API path, a `gh repo` argument,
  `--repo`/`-R`, `GH_REPO`, a pushed remote, or the checkout's git
  config; `gh pr <verb> N` and `gh issue <verb> N` (or their URLs) name
  the item, and `gh issue list` and `gh pr list` a listing.

Any other call answers `null` without touching the network. A recognized
call costs one GraphQL query, and the answer carries facts only:

```json
{"viewer": "ana",
 "repository": {"name": "acme/widget", "visibility": "public",
                "viewer_permission": "ADMIN", "fork_of": null},
 "pull_request": {"number": 12,
                  "author": {"login": "ana", "association": "MEMBER"},
                  "locked": false, "cross_repository": false,
                  "participants": [{"login": "ana", "association": "MEMBER", "bot": false},
                                   {"login": "renovate", "association": "NONE", "bot": true}],
                  "commit_authors": ["ana"], "last_editors": ["ana"],
                  "truncated": false}}
```

An issue carries `issue` with the same fields, minus `cross_repository`
and `commit_authors`. `participants` are the author and every comment,
review, and review-comment author; a deleted account is `null`.
`truncated` is `true` when a list had more pages than the query reads
(100 comments, 50 reviews of 30 comments each, 100 commits). A command
the provider cannot follow — a shell, a subshell, a computed target, a
setting that moves git, several repositories or items at once — a
repository the token cannot see, or any GitHub error exits nonzero, and
the annotator receives an error entry instead of an answer.

A listing call (`list_issues`, `list_pull_requests`, `gh issue list`,
`gh pr list`) is repeated in the same query with the call's filters, and
the answer carries `issues` or `pull_requests`:

```json
{"items": [{"number": 41, "author": {"login": "ana", "association": "MEMBER"},
            "bot": false, "last_editor": "bo"},
           {"number": 40, "author": {"login": "dependabot", "association": "NONE"},
            "bot": true}],
 "truncated": false}
```

`items` hold every item the call returns, possibly more;
`last_editor` is present only for an edited item and is `null` for a
deleted account. The query repeats the call's order and reads as many
items as the call does (at most 100) when it can: `gh` without a search
filter, `list_issues`, and `list_pull_requests` sorted by `created` or
`updated`, `page` included. Otherwise it reads the whole set the call's
filters admit and drops what it cannot repeat — labels, milestone, type,
app, draft, `@me`, a `head` owner, `field_filters`, a sort by
popularity — and `truncated` is `true` when that set has more than 100
items. A `--search`, an unknown flag or argument, or a value the server
would refuse reads nothing and answers `truncated: true`.

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

**`github_token.py`** — where the scripts get their token. They read
`APPA_PROVIDER_GITHUB_TOKEN`, which each binding's `token_env` forwards;
when it is unset they ask the GitHub CLI with `gh auth token` for the
API host, so a machine where `gh auth login` has run needs no variable.
The install names the variable and this fallback after it includes the
battery. A token needs the `read:org` and `user:email` scopes, and
`repo` for the private repositories it reads and lists collaborators of;
a `gh` login's default scopes cover them, and `user:email` is optional
(the viewer then stays `github:<login>`). Neither present, the script
stops and names both fixes. The scripts call `https://api.github.com`
unless `GITHUB_API_URL` names another root, as it does for a GitHub
Enterprise Server (`https://<host>/api/v3`), and ask `gh` for that host.
A command inherits none of the runtime's `APPA_*` namespace — not its
wiring, not a bearer token it sends, not another command's credential —
only the one `APPA_PROVIDER_*` variable its own binding names, with the
rest of the environment `gh` needs. Any GitHub error or missing answer
stops the operation without recording a decision; nothing is guessed.

**`test_audience_source.py`**, **`test_context.py`**,
**`test_repository_visibility.py`**, **`test_github_token.py`** — tests
without network: recorded GitHub REST and GraphQL payloads for the
selectors and the context answer, command and tool target resolution,
trust by authorship and consult refusals for the annotators, the envelope
and declaration checks, and the token lookup against a stand-in `gh` on
its own `PATH`. Run with `python3 -m
unittest discover -s . -p 'test_*.py'`.

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
