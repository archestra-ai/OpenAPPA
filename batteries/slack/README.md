# Slack battery

Rules for the claude.ai Slack connector, all 19 of its tools. No argument
patterns, no channel ids. Add it to your root config with `include`.

## Files

**`appa.toml`** — three groups.

*Reads* — channels, threads, canvases, files, profiles, members,
reactions, and every search. The result is `internal`: nothing built from
it can go to a public place. Its trust is unchanged, so it can be
summarised and posted back to Slack.

*Writes nobody else reads yet* — adding a reaction, saving a draft to
your own Drafts. Trusted data, no approval.

*Writes other people read* — sending or scheduling a message, creating
or updating a canvas, creating a channel. Trusted data that reaches
`internal` (`audience = { contains = ["internal"] }`): agents post
autonomously without human interruption, while requester secrets
(`self`) are strictly prevented from entering channels.

**`audience-source.py`** — the `slack` audience source. It answers the
stock catalog's selectors over the Slack Web API:

- `slack:viewer` — the token's own principal. Feeds `self`, so use a
  user token when the session acts for a person; a bot token makes the
  bot the viewer.
- `slack:full-members` — every full member of the token's own
  workspace. Guests (multi- and single-channel) and Slack Connect
  participants are in the workspace but are not full members; bots and
  deactivated accounts are out too. On an Enterprise Grid an account
  Slack places in another workspace of the org is not a member here.
  Feeds `internal`.
- `slack:user-group/<handle>` — one user group, exactly as Slack
  reports it — a guest in the group is in the audience. Feeds
  `[[audience.group]]` entries.

It also answers the member lookup that canonicalizes a `slack:U...`
reader. A verified email comes from a Slack profile only where Slack
marks the address confirmed; every other member keeps the qualified
`slack:` identity.

The source is not wired by this file: audience mappings are root-only,
and the binding must sit beside them. In the root config:

```toml
[policy.audience.self]
from = ["slack:viewer"]

[policy.audience.internal]
from = ["slack:full-members"]

[[policy.audience.group]]
name = "finance"
within = "internal"
from = ["slack:user-group/finance"]

[externals.audience.slack]
command = ["python3", "batteries/slack/audience-source.py"]
token_env = "APPA_PROVIDER_SLACK_TOKEN"
```

A command path is resolved against the directory of the config file
that names it, so write the path as your root config sees the battery.

The script reads its token from `APPA_PROVIDER_SLACK_TOKEN`, which the
binding's `token_env` forwards. The token needs the `users:read`,
`users:read.email`, and `usergroups:read` scopes. A command inherits
none of the runtime's `APPA_*` namespace — not its wiring, not a bearer
token it sends, not another command's credential — only the one
`APPA_PROVIDER_*` variable its own binding names. Any Slack error or missing answer stops the
operation without recording a decision; nothing is guessed.

Reads are workspace-wide: `full-members` and `user-group/<handle>` page
through the whole directory. Size `externals.timeout_ms` and
`externals.max_body_bytes` for your workspace, not for a single
annotation.

**`test_audience_source.py`** — fixture tests over recorded Slack Web
API payloads, no network. Run with `python3 test_audience_source.py`.

## Change the behaviour

Put a narrower rule in your root config; root rules run first. The
comment at the top of `appa.toml` shows two: let thread replies through
without a question, or let one channel through by its id. Nothing in
this file needs editing.
