# Google Workspace battery

Rules for the claude.ai Google Drive connector, all 11 of its tools,
the `google-workspace` audience source, which builds a policy's
audiences from the Workspace directory, and the
`google-workspace.drive-readers` annotator, which asks Drive who reads
the target of a write.

## Rules

*Reads* — `read_file_content`, `download_file_content`,
`get_file_metadata`, `get_file_permissions`, `search_files`,
`list_recent_files`. They return `self` data and enter `suspicious`.
The connector keeps its own Drive token, and no tool reports who can
see a file, so only the viewer is known to read every file the
connection reaches. Anyone, inside or outside the organization, can
share a file with the viewer or comment on one, so the text is
untrusted.

*Writes* — `create_file`, `copy_file`, `update_file` need trusted data
that the target's readers may see. Drive gives a file the sharing of
its folder, so the `google-workspace.drive-readers` annotator lists the
permissions of the folder a file is created or copied into, and of the
file that `update_file` renames or moves:

| Target readers | Requirement |
|---|---|
| Only the viewer, as in the viewer's My Drive root | `self` |
| Active Workspace users, the Workspace's own domain, or groups whose members all are | `internal` |
| Anyone with the link, a published file, another domain, an outside account, or readers the token may not list | `public` |

`copy_file` and a move by `update_file` also send the source file's
content on Google's side, so the answer forwards the audience that
already reads it: `public` when anyone may, `internal` when the
Workspace's domain may, else `self`. Copying a file only the viewer
reads into a folder the organization reads is refused.

`update_file` with `inheritedPermissionsDisabled: false` gives a folder
its parent's readers again, so it is checked as a move into that
parent; a parent the token cannot see needs `public`. With
`writersCanShare: true` any writer may share the file with anyone, so
it needs `public`, as `share_file` does. An `update_file` argument the
annotator does not know refuses the call.

`trash_file` needs trusted data that `internal` may see. A trashed file
can be restored, so trashing needs no review. Every write records
`google-workspace.changed`.

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
organization can see it: the read tools never report sharing. A
trajectory that reads a file can then write only into a Drive target
the viewer alone reads, and cannot send what it read to `internal`
without a remedy. A root rule can label one file by
argument, for a file the organization shares with `internal`; root
rules run first.

A write is checked against the target's sharing when the call is
annotated. Sharing that changes later, for example a folder someone
shares outside the organization after the write, is not seen.

The Gmail and Calendar connectors are not covered. A tool the policy
does not name is blocked, so add root rules for them.

```sh
cargo test --locked -p appa --test marketplace
python3 marketplace/batteries/google-workspace/test_audience_source.py
python3 marketplace/batteries/google-workspace/test_drive_readers.py
```

## Credentials

The audience source and the write annotator need a credential. The
`self`, `internal`, and group audiences come from the Admin SDK
Directory API, and a write target's readers from the Drive API, called
with the organization's own OAuth client.

A Workspace admin sets it up once per organization:

1. Create a Google Cloud project and enable the Admin SDK API and the
   Google Drive API.
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

**`drive-readers.py`** — the `google-workspace.drive-readers`
annotator. It reads the token from the same
`APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`, which also needs the
`drive.metadata.readonly` scope. It lists Drive permissions with
`supportsAllDrives` and published views, and checks each user and group
grantee against the directory. A 403 is an answer: a target needs
`public` data and a source's content stays with the viewer. Any other
API error stops the call. Its mandate admits `self` and `internal`;
the script refuses another one (exit status 2) before it reads the
token.

**`test_drive_readers.py`** — fixture tests over recorded Drive and
directory payloads, no network.

**`test_audience_source.py`** — fixture tests over recorded Google API
payloads, no network. Run with `python3 test_audience_source.py`.
