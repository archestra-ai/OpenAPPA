# Linear battery

Rules for 65 Linear MCP tools and the `linear` audience source that
reads who may see each issue, team, project, and document from Linear
itself. The manifest registers the battery with the marketplace.

## Files

**`appa.toml`** — the rules. Reads and mutation responses enter as
`suspicious`. A tool that names a resource by id is labelled with that
resource's readers through a selector placeholder:

- `get_issue`, `list_comments(issueId:*)`, `save_comment(issueId:*)`,
  `save_issue(id:*)`, `create_attachment`, and
  `create_attachment_from_upload` read or write
  `@linear:issue/$<argument>/readers`;
- `get_document` reads `@linear:document/$id/readers`;
- `list_cycles`, `list_issues(team:*)`, `list_documents(teamId:*)`, a
  bare `save_issue` (creating, so `team` is required), and
  `save_issue(id:*,team:*)` (moving: the destination team decides) read
  or write `@linear:team/$<argument>/readers`.

A write into a resource needs trusted data its readers may see, the
`linear-review` mark, and records `linear.changed` or `linear.sensitive`.
A summary of a private team's issue can be commented back onto that issue
and not onto a public team's. Tools that name no resource, or name one by
a free-text query (`get_project`, `list_milestones`, `save_project`, …),
keep the `internal` audience: map it in the root config to readers
authorized for everything this connection can list. Upload preparation
answers an `internal` signed URL, so the upload can then be attached to
any issue the workspace reads; image extraction can fetch
external URLs, so its input must be public and reviewed.

**`audience-source.py`** — the `linear` audience source, over the
GraphQL API. It answers these selectors:

- `linear:viewer` — the token's own reader. Feeds `self`.
- `linear:full-members` — every active member of the workspace, guests
  and app users excluded. Feeds `internal`.
- `linear:team/<key>/members` — one team's members. Feeds `group`
  entries.
- `linear:team/<key>/readers` — who can see the team's issues, projects,
  and cycles: every full member for a public team, its members for a
  private team, its members and the parent team's readers for a
  restricted sub-team.
- `linear:issue/<id>/readers` — the team's readers plus the people the
  issue is shared with individually.
- `linear:project/<id>/readers` — the readers of every team the project
  belongs to, plus the project's own members.
- `linear:document/<id>/readers` — the readers of the issue, project, or
  team the document belongs to; every full member for a document under
  an initiative.

A team is named by its UUID or key (`ENG`), an issue by its UUID or
identifier (`ENG-123`), a project and a document by their UUID or slug.
A name is never looked up: two things can share one, and a call that
spells a name gets no answer and is refused, so the agent names the
resource by id instead.

A roster of more than 5,000 accounts — the workspace, a team, a
project's members — is refused rather than paged: it cannot answer
inside the runtime's consult budget. Map such an audience in the root
config from a source that lists it in bulk.

A member is the account's email as Linear reports it, else
`linear:<id>`, which merges with no other provider's reader. The member
lookup resolves a `linear:<id>` member the same way and answers `null`
for an id Linear does not know.

The battery binds the source itself, under `[externals.audience.linear]`
in `appa.toml`, and declares the seven templates above as its
`selectors`. Audience mappings are root-only, so the root config maps
the chain onto them:

```toml
[policy.audience]
self = ["linear:viewer"]
internal = ["linear:full-members"]

[policy.audience.group.platform]
within = "internal"
from = ["linear:team/PLT/members"]
```

Every consult carries the declared templates, and the script refuses
one whose declaration differs from what it serves (exit status 2)
before it reads a token: a policy and a script of different versions
never answer each other. The runtime probes `viewer` and
`full-members` at startup, so the skew surfaces before a decision.

The script reads its token from `APPA_PROVIDER_LINEAR_TOKEN`, which the
binding's `token_env` forwards: a personal API key, or an OAuth access
token that can read users, teams, issues, projects, and documents. A
command inherits none of the runtime's `APPA_*` namespace — only the one
`APPA_PROVIDER_*` variable its own binding names. Any Linear error or
missing answer stops the operation without recording a decision; nothing
is guessed.

**`test_audience_source.py`** — tests without network: member and
person shaping, selector and member refusals, and the envelope and
declaration checks. Run with `python3 test_audience_source.py`.

## Configure the deployment

Install with `appa battery install linear --server <host-server-name>`, or include
`appa.toml` from the root policy. Installation adds policy, not an MCP connection
or Linear permissions. Map `self` and `internal` as above and pass the token.

Root rules take precedence over battery rules. They must state the complete
annotation, including write requirements and effects. Use `parameters` with
`additionalProperties = false` when an override assumes a fixed argument shape.

[The root example](../../../examples/linear-battery/appa.toml) combines the
Linear and GitHub batteries, maps the chain onto the Linear source, and keeps
one root override for a resource with readers Linear does not model. That
override and the review authority cannot widen an audience: reading a private
team's issue does not authorize publishing it to a public GitHub repository.

For read-only deployments, connect Linear's `/mcp/readonly` endpoint.

## Maintenance

Edit the TOML rules directly when tool behavior changes. Unknown tools receive no
permission from this battery. Review each tool's destination, returned content,
side effects and review requirements; tool descriptions and MCP annotations do
not grant permission.

```sh
cargo test --locked -p appa --test linear_policy --test marketplace
bash scripts/appa-marketplace.sh --check
python3 marketplace/batteries/linear/test_audience_source.py
```
