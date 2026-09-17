# Slack battery

Rules for the claude.ai Slack connector, all 19 of its tools. One rule
per tool; the rules that name a conversation read its id from the call.
Add it to your root config with `include`.

## Files

**`appa.toml`** — four groups.

*Reads of one conversation* — a channel's messages, a thread, a
channel's members, a message's reactions. The result is read by that
conversation's members: `delta = { audience =
["@slack:channel/$channel_id"] }` labels it with the collection the
call's `channel_id` spells, and the `slack` audience source answers who
is in it. A private channel's history stays with the people in that
channel; a DM's stays with its two ends. Trust is unchanged, so the
result can be summarised and posted back where those people read.

*Reads without a conversation* — canvases, files, profiles, and public
search. The result is `internal`: nothing built from it can go to a
public place. `slack_search_public_and_private` returns hits from every
private conversation the token can see, so its result stays with the
viewer (`self`) until a person widens it.

*Writes nobody else reads yet* — adding a reaction, saving a draft to
your own Drafts. Trusted data, no approval.

*Writes other people read* — sending or scheduling a message needs
trusted data that every member of the target conversation may see
(`audience = { contains = ["@slack:channel/$channel_id"] }`): a summary
of #eng can go back to #eng, or to a DM with someone in #eng, and not to
#all-hands. Creating or updating a canvas and creating a channel carry
no conversation id and need trusted data that reaches `internal`. Agents
post autonomously without human interruption, while requester secrets
(`self`) are strictly prevented from entering channels.

**`audience-source.py`** — the `slack` audience source. It answers
these selectors over the Slack Web API:

- `slack:viewer` — the token's own reader. Feeds `self`, so use a
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
  `group` entries.
- `slack:channel/<id>` — one conversation's readers. A public channel
  is read by every full member, since any of them may join it and read
  its history, plus the guests and Slack Connect participants already in
  it. A private channel, group DM, or DM is read by its members exactly
  as Slack reports them, deactivated accounts left out. A user id (`U…`
  or `W…`, which the connector accepts as a `channel_id` for a DM)
  names exactly the viewer and that user. A channel name, a `#` spelling,
  or a URL is refused, never guessed. The battery's contracts name this
  collection through the placeholder `@slack:channel/$channel_id`.

A member is the account's profile email where Slack marks the address
confirmed, else `slack:<id>`, which merges with no other provider's
reader. The member lookup resolves a `slack:U...` member the same way
and answers `null` for an id Slack does not know.

The battery binds the source itself, under `[externals.audience.slack]`
in `appa.toml`, and declares the four templates above as its
`selectors`. Audience mappings are root-only, so the root config maps
the chain onto them:

```toml
[policy.audience]
self = ["slack:viewer"]
internal = ["slack:full-members"]

[policy.audience.group.finance]
within = "internal"
from = ["slack:user-group/finance"]
```

Every consult carries the declared templates, and the script refuses
one whose declaration differs from what it serves (exit status 2), so a
policy and a script of different versions never answer each other.

A directory of more than 5,000 accounts is refused rather than paged:
Slack rate-limits `users.list` to tens of pages a minute, so a larger
workspace cannot answer `full-members`, a user group, or a public
channel inside the runtime's consult budget. Map such audiences in the
root config from a source that lists them in bulk.

The script reads its token from `APPA_PROVIDER_SLACK_TOKEN`, which the
binding's `token_env` forwards. The token needs the `users:read`,
`users:read.email`, and `usergroups:read` scopes, and for `channel/<id>`
the `channels:read`, `groups:read`, `im:read`, and `mpim:read` scopes. A command inherits
none of the runtime's `APPA_*` namespace — not its wiring, not a bearer
token it sends, not another command's credential — only the one
`APPA_PROVIDER_*` variable its own binding names. Any Slack error or missing answer stops the
operation without recording a decision; nothing is guessed.

Reads are workspace-wide: `full-members`, `user-group/<handle>`, and
`channel/<id>` page through the whole directory. Size `externals.timeout_ms` and
`externals.max_body_bytes` for your workspace, not for a single
annotation.

**`test_audience_source.py`** — tests without network: recorded Slack
Web API payloads for the selectors, id classification and refusals for
`channel/<id>`, and the envelope and declaration checks. Run with
`python3 test_audience_source.py`.

## Change the behaviour

Put a narrower rule in your root config; root rules run first. The
comment at the top of `appa.toml` shows one: let thread replies through
without a question. Nothing in this file needs editing.
