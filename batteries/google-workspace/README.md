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
  for its own membership, whatever the member's email domain. That
  member keeps its qualified identity, since the Workspace administers
  no account for the address and so attests nothing about it. Feeds
  `[[audience.group]]` entries.

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
```

A command path is resolved against the directory of the config file
that names it, so write the path as your root config sees the battery.

The script reads its token from `OPENAPPA_GOOGLE_WORKSPACE_TOKEN`: an
OAuth2 access token with the `admin.directory.user.readonly` and
`admin.directory.group.member.readonly` scopes plus `openid email`.
The runtime strips every `APPA_*` variable from a command it runs — its
own credentials never reach an external — so a source's token must be
named outside that prefix. Any API error or missing answer stops the
operation without recording a decision; nothing is guessed.

Reads are directory-wide: `full-members` pages through every account.
Size `externals.timeout_ms` and `externals.max_body_bytes` for your
Workspace, not for a single annotation.

**`test_audience_source.py`** — fixture tests over recorded Google API
payloads, no network. Run with `python3 test_audience_source.py`.
