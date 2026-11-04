# Archestra battery

Rules for every tool of Archestra's built-in MCP server, plus the
`archestra` audience source.

## Files

**`appa.toml`** — one rule per tool, and one per sharing value of a
sharing tool, first match wins:

| Class | Tools | Adds | Requires |
| --- | --- | --- | --- |
| Own surfaces | `search_tools`, `search_files`, `load_skill`, `ask_user`, `todo_write`, `whoami`, `validate_guardrails_policy`, `resolve_openappa_yell`, credential setup cards, `cancel_agent_run` and other writes that carry no session data | nothing | nothing |
| Organization records | configuration, rosters, the guardrails policy and its previews, yells, consults, files, apps, run state | `internal` | nothing |
| Text strangers write | `query_knowledge_sources`, `get_mcp_server_logs`, `get_schedule_trigger_run_transcript`, upstream tool lists (`get_mcp_server_tools`, `inspect_guardrails_server`) | `internal`, `suspicious` | nothing |
| Control plane | deletes and unassigns, guardrails publishes, repository connection and sync, credential bindings, schedule controls, team changes, limits, tool assignments | nothing | `trusted` |
| Read or run by others | agents, gateways, MCP servers, hooks, skills, plugins, apps, scheduled tasks, run posts, credential transfers | nothing | `trusted`, `internal` |
| Shared files and runs | persistent file writes, `start_agent_run`, `steer_agent_run` (and their Archestra 1.4 names), workspace writes and uploads | nothing | `internal` |
| Shares | `publish_app` by `scope`, `update_knowledge_connector` by `visibility` or a sent `team_ids`, `add_team_member` | nothing | `trusted` and `internal` / `@archestra:team/<team>` / `@archestra:user/$user` |
| Sandbox | `run_command` | the `archestra.run-command` Annotator | |

The battery declares no rule for `yell` (ruled as `mcp/appa/yell`),
`run_tool` and `preview_app_tool` (ruled as the tool they run),
`execute_remedy_plan`, `get_remedy_plans`, `list_peer_messages`,
`read_peer_message`, and the app-runtime tools no agent lists.

The Archestra proxy adds string arrays no selector could read from the
original objects: `initialGrantPrincipals` (`<type>:<id>`, the
organization as `organization`, sent only with a non-empty
`initialGrants`) and `transferModes` (`value` or `link` per credential).
A create that sends grants requires `trusted` data sharable with
`internal`, which holds every principal a grant can name; without grants
it requires what the create alone needs. A credential transfer that only
returns links requires nothing; any other transfer requires `trusted` and
`internal`. An update that sends `team_ids` replaces who the
connector is shared with whatever visibility it already has, so the list
is checked on its own. Auto-synced connector permissions mirror the
connected system's own ACLs, which this source cannot read, so such a
connector requires `internal`. Every rule of a sharing tool declares its
lists as arrays of strings, so a malformed list is refused rather than
read as no list.

A run and its workspace are one container the organization may read,
since admins can open any run. What enters it must be sharable with
`internal`, and what leaves it adds `internal` and keeps its trust. This
holds on every host, including one whose calls carry no LLM Proxy proof
and so cross no label of their own. On such a host, data written into a run
is read back with the run's trust, so suspicious data steered into an
unbound run can return trusted; through the LLM Proxy, the run's return
crossing labels it.

`run_command` routes to the `archestra.run-command` Annotator, declared
here with `builtin = "claude-code"`. An Archestra root redeclares it with
`builtin = "archestra"`, so the annotator model answers it.

A team list names one collection per team, and OpenAPPA reads at most 100
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

`appa-runtime/tests/archestra_policy.rs` pins what a representative call
of each class adds and requires, and which rule each share selects.

**`test_audience_source.py`** — runs the script against a local HTTP
server that answers from recorded payloads:

```sh
python3 -m unittest test_audience_source.py
```
