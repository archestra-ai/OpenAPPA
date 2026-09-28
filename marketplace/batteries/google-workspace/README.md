# Google Workspace battery

Rules for the claude.ai Google Drive connector, all 11 of its tools,
and the `google-workspace` audience source, which builds a policy's
audiences from the Workspace directory.

## Rules

*Reads* — `read_file_content`, `download_file_content`,
`get_file_metadata`, `get_file_permissions`, `search_files`,
`list_recent_files`. They return `self` data and enter `suspicious`.
The connector keeps its own Drive token, and no tool reports who can
see a file, so only the viewer is known to read every file the
connection reaches. Anyone, inside or outside the organization, can
share a file with the viewer or comment on one, so the text is
untrusted.

*Writes* — `create_file`, `copy_file`, `update_file`, `trash_file` need
trusted data that `internal` may see, and record
`google-workspace.changed`. A trashed file can be restored, so trashing
needs no review.

*Sharing* — `share_file` exposes the whole file, which is not a value
the policy tracks. It needs public input and the
`google-workspace-review` mark, and records `google-workspace.sensitive`.

The Claude Code plugin default ships a human authority permitting every
mark (`attention = ["*"]`), so `google-workspace-review` needs no wiring
there. Another root config must permit it itself:

```toml
[[policy.authority]]
name = "google-workspace-operator"
hint = "Review who gets access to the file."
permits = { trust_below = "trusted", attention = ["google-workspace-review"] }

[externals.authorities.google-workspace-operator]
builtin = "hitl"
```

## Limits

A doc read through this battery is `self` data, even when the whole
organization can see it: the file's sharing never reaches the policy. A
trajectory that reads a file cannot then write to Drive or send what it
read to `internal` without a remedy. A root rule can label one file by
argument, for a file the organization shares with `internal`; root
rules run first.

The Gmail and Calendar connectors are not covered. A tool the policy
does not name is blocked, so add root rules for them.

```sh
cargo test --locked -p appa --test google_workspace_policy --test marketplace
python3 marketplace/batteries/google-workspace/test_audience_source.py
```

## Credentials

The tool rules need no credential. The audience source does: the
`self`, `internal`, and group audiences come from the Admin SDK
Directory API, called with the organization's own OAuth client.

A Workspace admin sets it up once per organization:

1. Create a Google Cloud project and enable the Admin SDK API.
2. Configure the OAuth consent screen as *Internal*. Only accounts in
   the organization can use the client, and Google does not review it.
3. Create an OAuth client of type *Desktop app* and share its client
   JSON with the people who run the agent. A desktop client's secret is
   not confidential.

Each user then signs in once in a browser with that client, granting
the scopes below. Google access tokens expire after about an hour, so
the token must be refreshed from the stored refresh token before it is
passed through `APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`. Reading the
directory may need a user with admin rights.

## Files

**`audience-source.py`** — answers these selectors over Google's
OpenID userinfo and Admin SDK Directory APIs:

- `google-workspace:viewer` — the token's own reader: its email when
  the userinfo endpoint marks it verified, else
  `google-workspace:<address>`. Feeds `self`.
- `google-workspace:full-members` — every active Workspace user, each
  as its primary email; suspended and archived accounts are out. Feeds
  `internal`.
- `google-workspace:group/<group-address>` — one Workspace group.
  Nested groups are expanded, and a member outside the Workspace
  belongs to the group like any other: the group is the source of truth
  for its own membership, whatever the member's email domain. Each
  member is the address the group lists. Feeds `group` entries.

The member lookup resolves a `google-workspace:<address>` member to
the account's primary email, so an alias resolves to the same reader
as the account; it answers `null` for an address the directory does
not know.

The battery binds the source itself, under
`[externals.audience.google-workspace]` in `appa.toml`, and declares
the three templates above as its `selectors`. Audience mappings are
root-only, so the root config maps the chain onto them:

```toml
[policy.audience]
self = ["google-workspace:viewer"]
internal = ["google-workspace:full-members"]

[policy.audience.group.finance]
within = "internal"
from = ["google-workspace:group/finance@corp.com"]
```

Every consult carries the declared templates, and the script refuses
one whose declaration differs from what it serves (exit status 2), so a
policy and a script of different versions never answer each other.

The script reads its token from `APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`,
which the binding's `token_env` forwards: an OAuth2 access token with the
`admin.directory.user.readonly` and
`admin.directory.group.member.readonly` scopes plus `openid email`.
A command inherits none of the runtime's `APPA_*` namespace — not its
wiring, not a bearer token it sends, not another command's credential —
only the one `APPA_PROVIDER_*` variable its own binding names. Any API error or missing answer stops the
operation without recording a decision; nothing is guessed.

Reads are directory-wide: `full-members` pages through every account.
Size `externals.timeout_ms` and `externals.max_body_bytes` for your
Workspace, not for a single annotation.

**`test_audience_source.py`** — fixture tests over recorded Google API
payloads, no network. Run with `python3 test_audience_source.py`.
