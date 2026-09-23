# Archestra battery

Rules for the sharing tools of Archestra's built-in MCP server, plus the
`archestra` audience source.

## Files

**`appa.toml`** — one rule per sharing value of each tool, first match
wins:

| Tool | Widens to | Requires |
| --- | --- | --- |
| `set_project_share` | `visibility: organization` / `team` or a sent `team_ids` | `internal` / `@archestra:team/$team_ids` |
| `publish_app` | `scope: org` / `team` | `internal` / `@archestra:team/$teams` |
| `create_knowledge_base` | default `org-wide` / `team-scoped` | `internal` / `@archestra:team/$teamIds` |
| `update_knowledge_base` | `org-wide` / `team-scoped` or a sent `teamIds` | `internal` / `@archestra:team/$teamIds` |
| `create_knowledge_connector` | default `org-wide` / `team-scoped` / `auto-sync-permissions` | `internal` / `@archestra:team/$team_ids` / `public` |
| `update_knowledge_connector` | `org-wide` / `team-scoped` or a sent `team_ids` / `auto-sync-permissions` | `internal` / `@archestra:team/$team_ids` / `public` |
| `create_plugin`, `update_plugin` | `scope: org` / `team` or a sent `teamIds` / a sent `userIds` / both lists | `internal` / `@archestra:team/$teamIds` / `@archestra:user/$userIds` / `internal` |
| `edit_agent`, `edit_mcp_gateway` | `scope: org` / `team` or a sent `teams` | `internal` / `@archestra:team/$teams` |
| `add_team_member` | the added member | `@archestra:user/$user` |

Every widening share also requires `trusted` data. Values that keep a
resource personal or private, and updates that send no scope and no
list, require nothing. An update that sends a team or member list
replaces who the resource is shared with whatever scope it already has,
so the list is checked on its own: an argument selector such as
`(teams:*)` matches a call that sent a non-empty list. A call that sends
both a team and a member list requires `internal`, which holds every
team and member. Auto-synced connector permissions mirror the connected
system's own ACLs, which this source cannot read, so such a connector
must be sharable with anyone. Every rule of a tool declares its lists as
arrays of strings, so a malformed list is refused rather than read as no
list.

A team list names one collection per team, and OpenAPPA reads at most 32
of them per call.

**`audience-source.py`** — answers these selectors through Archestra's
`GET /api/openappa/audience` endpoint:

- `archestra:members` — every member of the organization the token
  belongs to. Feeds `internal`.
- `archestra:team/<team>` — one team, by id or name.
- `archestra:user/<user>` — one member, by user id or email.

The member lookup resolves an `archestra:<user-id>` member to that user's
email; it answers `null` for a user Archestra does not know. Every member
is the account's email, as Archestra holds it, so Archestra readers compare with the
readers of any other email-keyed source, such as `google-workspace`.

The battery binds the source under `[externals.audience.archestra]` and
declares the three templates above. Audience mappings are root-only, so
the root config maps `internal` onto the source:

```toml
[policy.audience]
internal = ["archestra:members"]
```

Every consult carries the declared templates, and the script refuses one
whose declaration differs from what it serves (exit status 2).

The script reads the API base from `ARCHESTRA_BASE_URL` and its token from
`APPA_PROVIDER_ARCHESTRA_TOKEN`, which the binding's `token_env` forwards:
an Archestra API key allowed to read the organization's membership. It
sends the key as `Authorization: Bearer <key>`. An API error or a
malformed answer exits nonzero, which the runtime treats as no answer.

**`test_audience_source.py`** — runs the script against a local HTTP
server that answers from recorded payloads:

```sh
python3 -m unittest test_audience_source.py
```
