# Hugging Face battery

Rules for the hosted Hugging Face MCP server (`https://huggingface.co/mcp`,
[hf-mcp-server](https://github.com/huggingface/hf-mcp-server) 0.4.19):
your account, Hub search, repository reads and writes, Spaces, Jobs, and
the sandbox. Shared by the Claude Code and kagent plugins. Installing the
battery adds its policy include; it does not create the MCP connection or
enable tools in either host.

## Source

The tool list comes from a live `tools/list` capture of
`https://huggingface.co/mcp` taken on 2026-09-15 with a read token, and
from the server's source at that version. The capture returned
`hf_whoami`, `hub_repo_search`, `hub_repo_details`, `hf_fs`,
`dynamic_space`, and one `gr1_*` Space tool; the source adds
`hf_fs_write`, `create_repo`, `hf_jobs`, `hf_sandbox`,
`hf_sandbox_exec`, and `hf_sandbox_fs`, which the server lists only
when the account enables them. Every rule names its tool by the
canonical tool id `mcp/huggingface/<tool>`.

## Files

**`appa.toml`** — the rules, in six groups.

*Who am I* — `hf_whoami` returns the viewer's own account. Read by the
viewer (`self`).

*Search* — `hub_repo_search` sends the token, and the Hub then lists the
viewer's private repositories among the public ones, so the results are
untrusted and stay with the viewer (`self`). A root rule can treat a
search of public repositories as public by its arguments (`author:`,
`query:`); root rules run first.

*Reads of named repositories* — `hub_repo_details` and `hf_fs`. Model
cards, dataset files, and Space code were pushed by whoever owns the
repository, so the result is untrusted, the same way a fetched web page
is. Who may see it is each repository's visibility, which the
`huggingface.repo-visibility` annotator asks the Hub for on every call,
for every repository the call names: `public` for a public repository;
`self` for a gated repository's files (its metadata stays public), for
a private repository, and for a listing the token sees private names in
(`hf://models`, `hf://models/<owner>`, `hf://collections/<owner>`); the
collection `@huggingface:org/<org>/resource-group/<group>/members` for
a private organization repository that the Hub lists in a resource group
the token can see, once the root config admits that collection. A call
naming several repositories takes the narrowest audience it can name:
everyone when all are public, one collection when every non-public
entry is that collection, else the viewer. `hf://papers` and `hf://docs`
are public; `hf://buckets` is refused, since no visibility API is pinned
for it.

*Writes into a named repository* — `hf_fs_write` and `create_repo`. The
`huggingface.repo-readers` annotator asks the Hub for the target's
visibility: a write into a public repository runs only with trusted data
everyone may see, a write into the viewer's own private repository with
trusted data the viewer may see. A private organization repository is
read by the organization's admins whatever its resource group, a set no
collection lists, so a write into it needs data everyone may see. A new
repository's readers follow its `private` flag (omitted: public, the
Hub's default). `create_repo` with `source_uri` copies a repository
server-side, a flow the trajectory never carries, so the annotator
admits it only when the source's readers are inside the target's: a
public source into any target, or the viewer's own private repository
into another of the viewer's own; anything else is refused.

*Code run with the viewer's token* — `hf_jobs`, `hf_sandbox`,
`hf_sandbox_exec`, `hf_sandbox_fs`, and `dynamic_space` with
`operation` `invoke`, which posts the parameters to a Space's own code
with the token forwarded. They run only with trusted data the viewer may
see and a person permitting `huggingface-review`; what they return is
untrusted and stays with the viewer.

*Space discovery and schemas* — the other `dynamic_space` operations
search Spaces with the token and read a Space's parameter schema:
untrusted, and with the viewer.

A repository the token cannot see, a malformed id, or any Hub error gets
no answer from either annotator, and the call is refused; nothing is
guessed public.

The Claude Code and kagent plugin defaults ship a human authority
permitting every mark (`attention = ["*"]`), so `huggingface-review`
needs no wiring there. Another root config must permit it itself:

```toml
[[policy.authority]]
name = "huggingface-operator"
hint = "Review the exact job, sandbox command, or Space call."
permits = { trust_below = "trusted", attention = ["huggingface-review"] }

[externals.authorities.huggingface-operator]
builtin = "hitl"
```

Tools a Space adds to the server (`gr<N>_*`, `grp<N>_*`) are named after
each account's own Space list, so the battery cannot name them. A tool
the policy does not name is blocked; add a root rule with the exact name
for each one you enable, with the reviewed-write contract above when it
runs with your token.

**`repo-visibility.py`** — the two annotators, one script. A consult
carries the call's arguments; the script reads every repository they
name (`repo_ids`, the first argument of each `hf_fs` operation, `uri`
and `source_uri`), drops a revision (`@rev` changes content, never
readers), and asks `GET /api/{models|datasets|spaces}/{owner}/{name}`
for each. Without `repo_type` it asks every type the name exists as and
folds them. A private organization repository is looked up in
`GET /api/organizations/{org}/resource-groups`, which lists the groups
the token has access to with their repositories; a repository found in a
group takes that group's collection when the policy admits it, else the
viewer. The policy's mandate for every call admits `self`, and the
script refuses a consult whose mandate lacks it (exit status 2) before
it reads a token.

**`audience-source.py`** — the `huggingface` audience source. It answers
these selectors over the Hub API:

- `huggingface:viewer` — the token's own reader: its verified email from
  `/api/whoami-v2`, else `huggingface:<name>`. Feeds `self`.
- `huggingface:org/<org>/resource-group/<group>/members` — one resource
  group of one organization, by its id or its name: its users as
  `huggingface:<name>`, those with the `no_access` role excluded.
  Refused when the token cannot see the group.

There is no organization-wide members selector. An organization member
can hold the `no_access` role, which reads no private repository, and
the members endpoint reports no role to a read token, so the
organization's member list is wider than its private repositories'
readers.

A member other than the viewer stays `huggingface:<name>`: the Hub
attests no address for another account to a read token. The member
lookup answers the viewer's own name as the viewer, so the viewer seated
in a group and the viewer read as `self` are one reader, and answers
`null` for a name the Hub does not know.

The battery binds the source itself, under
`[externals.audience.huggingface]` in `appa.toml`, and declares the two
templates above as its `selectors`. Audience mappings are root-only, so
the root config maps `self` onto the viewer:

```toml
[policy.audience]
self = ["huggingface:viewer"]
```

A deployment whose organization uses resource groups admits each group's
collection by redeclaring the two annotators in the root config — a root
declaration replaces the battery's, fields included — with the group's
id (from the resource groups API) or its name:

```toml
[[policy.annotator]]
name = "huggingface.repo-visibility"
ranks = ["suspicious"]
audiences = ["self", "@huggingface:org/acme/resource-group/507f1f77bcf86cd799439011/members"]
marks = []

[[policy.annotator]]
name = "huggingface.repo-readers"
ranks = ["trusted"]
audiences = ["self", "@huggingface:org/acme/resource-group/507f1f77bcf86cd799439011/members"]
marks = []
```

The token is the viewer's own: the battery assumes one person behind it.
A shared service token makes `self` a service account, and what it reads
is then read by everyone who can act as that account.

**`hf_token.py`** — where both scripts get their token. They read
`APPA_PROVIDER_HUGGINGFACE_TOKEN`, which each binding's `token_env`
forwards; when it is unset they read the token the Hugging Face CLI
stored at `hf auth login` (`HF_TOKEN_PATH`, else `$HF_HOME/token`, else
`$XDG_CACHE_HOME/huggingface/token`, `~/.cache` when unset). The install names the variable and this
fallback after it includes the battery. A read token covers every read
and the audience source; `hf_fs_write` and `create_repo` need a write
token. Neither present, the script stops and names both fixes. The
scripts call `https://huggingface.co` unless `HF_ENDPOINT` names another
Hub. A command inherits none of the runtime's `APPA_*` namespace — only
the one `APPA_PROVIDER_*` variable its own binding names, with the rest
of the environment. Any Hub error or missing answer stops the operation
without recording a decision; nothing is guessed.

**`test_repo_visibility.py`**, **`test_audience_source.py`**,
**`test_hf_token.py`** — tests without network: recorded Hub payloads
for the public, gated, and missing cases, payloads following the Hub's
OpenAPI schema for the private and resource-group cases this token
cannot produce, every refusal, the envelope and declaration checks, and
the token lookup. Run with `python3 -m unittest discover -s . -p
'test_*.py'`.

## Try it against the Hub

[`examples/live-replays/huggingface`](../../../examples/live-replays/huggingface)
replays public, gated, and (optionally) private repositories through the
battery with a real token.

## Change the behaviour

To make a write ask a person first, add a root rule for that tool with
`attention = ["hitl"]` in its `requires`. To treat a search of public
repositories as public by its arguments, or one organization's content
as `internal` instead of its resource groups, add a root rule naming it.
Root rules run first. Nothing in this file needs editing.
