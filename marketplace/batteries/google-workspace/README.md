# Google Workspace battery

The `google-workspace` audience source. This battery carries no tool
rules yet: it exists so a policy can build its audiences from the
Workspace directory.

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
