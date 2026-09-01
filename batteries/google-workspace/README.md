# Google Workspace battery

The `google-workspace` audience source. This battery carries no tool
rules yet: it exists so a policy can build its audiences from the
Workspace directory.

## Files

**`audience-source.py`** — answers the stock catalog's selectors over
Google's OpenID userinfo and Admin SDK Directory APIs:

- `google-workspace:viewer` — the token's own principal, with its
  email when the userinfo endpoint marks it verified. Feeds `self`.
- `google-workspace:full-members` — every active Workspace user;
  suspended and archived accounts are out. Feeds `internal`.
- `google-workspace:group/<group-address>` — one Workspace group.
  Nested groups are expanded, and a member outside the Workspace
  belongs to the group like any other: the group is the source of truth
  for its own membership, whatever the member's email domain. Being in
  a group proves membership, not identity, so a member is claimed with
  a verified address only where the directory administers their
  account; everyone else keeps a qualified identity and merges with no
  other provider's reader. Reading a group therefore also reads the
  directory once. Feeds `[[audience.group]]` entries.

It also answers the member lookup that canonicalizes a
`google-workspace:<address>` reader. A Workspace account's primary
email is administered by the Workspace itself, so members carry it as
their verified email.

The source is not wired by this file: audience mappings are root-only,
and the binding must sit beside them. In the root config:

```toml
[policy.audience.self]
from = ["google-workspace:viewer"]

[policy.audience.internal]
from = ["google-workspace:full-members"]

[[policy.audience.group]]
name = "finance"
within = "internal"
from = ["google-workspace:group/finance@corp.com"]

[externals.audience.google-workspace]
command = ["python3", "batteries/google-workspace/audience-source.py"]
token_env = "APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN"
```

A command path is resolved against the directory of the config file
that names it, so write the path as your root config sees the battery.

The script reads its token from `APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`,
which the binding's `token_env` forwards: an OAuth2 access token with the
`admin.directory.user.readonly` and
`admin.directory.group.member.readonly` scopes plus `openid email`.
A command inherits none of the runtime's `APPA_*` namespace — not its
wiring, not a bearer token it sends, not another command's credential —
only the one `APPA_PROVIDER_*` variable its own binding names. Any API error or missing answer stops the
operation without recording a decision; nothing is guessed.

Reads are directory-wide: `full-members` pages through every account, and
a non-empty `group/<address>` adds one directory pass after the traversal
to decide which member addresses this Workspace administers. Size
`externals.timeout_ms` and `externals.max_body_bytes` for your Workspace,
not for a single annotation.

**`test_audience_source.py`** — fixture tests over recorded Google API
payloads, no network. Run with `python3 test_audience_source.py`.
