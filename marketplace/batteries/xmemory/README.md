# xmemory battery

Rules for the hosted [xmemory](https://xmemory.ai) MCP servers, all 41
tools: the instance server at `https://mcp.xmemory.ai` (or
`https://mcp.xmemory.ai/instance/<id>`, 16 tools), which reads and writes
one memory instance, and the admin server at
`https://mcp.xmemory.ai/admin` (25 tools), which creates, describes, and
deletes instances. The admin server also edits instances' cluster
memberships, lists API keys, and creates keys for the caller. Plain TOML
rules, no helper process or provider credential. One namespace,
`xmemory`, is bound to each server you run:

```sh
appa battery install xmemory --server <instance-server> --server <admin-server>
```

A deployment with one of the two servers binds only that one. The xmemory
Claude Code plugin names each instance server `xmemory-<first 8 characters
of the instance id>`; bind that name.

## Rules

Trust follows who can write the text. The organisation's members and
agents write memory, and every write needs trusted data, so reads and
write results keep the session's trust: suspicious text cannot enter
memory and come back trusted.

*Instance reads* — `read`, `write_status`, `get_instance_id`,
`get_instance_schema`, `get_setup_instructions`, `get_context_pack`, `review_suggestions`,
`list_schema_migrations`, `get_schema_migration`,
`dry_run_schema_migration`, `enhance_schema`. Results keep the session's
trust and are restricted to `internal`. A tool's input must be sharable with `internal` too, because
the instance stores it or its readers see it. `review_suggestions` also
merges duplicate feedback rows that rebuild the same proposal; it changes
no schema and no records. `get_context_pack` returns the owner
instructions and, when asked for, the live state that a read returns.

*Writes* — `write` and `write_async`, with `text` or with
`structured_mutations`. The input must be trusted and sharable with
`internal`. A `text` write can update and delete records through extraction,
so a `structured_mutations` write that names the records exactly, for
example
`[{"object_mutation": {"object_type": "Person", "delete": {"key": {"name": "Bob"}}}}]`,
gets the same contract. `write` returns the records it changed, with
their old values and the values of deleted records, so its result is
`internal`. `write_async` returns a queue id only, and
`write_status` returns the result. Both record `xmemory.changed`.

*Schema changes* — `decide_suggestions` records accept, reject, and defer
decisions and needs trusted data. Its result echoes the decisions and
keeps the trust, so it runs without an authority from a trusted
trajectory and records `xmemory.changed`. `apply_pending_decisions` and
`update_instance_schema` migrate the schema, which can drop fields and
their values. They need trusted data and the `xmemory-review` mark,
return `internal` content, and record `xmemory.schema`.

*Admin reads* — `admin_list_clusters`, `admin_get_cluster`,
`admin_list_instances`, `admin_list_own_instances`,
`admin_get_instance`, `admin_get_instance_by_id`,
`admin_get_instance_schema_by_id`, `admin_get_setup_instructions`,
`admin_list_instance_memberships`, `admin_generate_schema`,
`admin_enhance_schema`, `admin_validate_yaml_schema`,
`admin_convert_json_schema`, `admin_list_api_keys`, `admin_get_api_key`,
`admin_list_mintable_roles`. Instance names, descriptions, owner
instructions, schemas, and key names are written by the organisation's
members, so results keep the session's trust, restricted to `internal`.
The key tools return names, roles, permissions, and clusters, never a
secret; with `api_key.see_org_keys`, `admin_list_api_keys` also lists
other people's keys.

*Admin writes* — `admin_create_instance` and the four
`admin_update_instance_metadata*` and `admin_patch_instance_metadata*`
tools need trusted data and record `xmemory.admin`. Metadata includes the
owner instructions that every agent connected to the instance receives.
Each returns the whole instance, including fields the call did not set;
the result is `internal` and keeps the session's trust, so these calls
run without an authority from a trusted trajectory.

*Instance deletion* — `admin_delete_instance` and
`admin_delete_instance_by_id` delete an instance and its data. They need
trusted data and the `xmemory-review` mark, return the deleted instance
as `internal` content, and record `xmemory.deleted`.

*Access changes* — `admin_edit_instance_memberships` adds and removes an
instance's cluster memberships. A cluster's role assignments cover every
instance in it, so the change decides who can read the instance.
`admin_create_api_key` creates a key for the caller and returns its
secret once. Both need trusted data and the `xmemory-review` mark, and
record `xmemory.admin`. The membership result is `internal`. The new key
is a credential for the caller alone, so its result is `self`. After it,
a call whose input `internal` must see does not run without an authority
that may widen the audience. A memory write is such a call.

## Root config

The battery binds no audience source and names no credential variable.
A root config adds three things.

It maps `internal` onto the people who may read everything the
connection reaches: the organisation's members. For example, with the
Google Workspace battery's audience source:

```toml
[policy.audience]
internal = ["google-workspace:full-members"]
```

It maps `self` onto the person the session acts for, the only reader of
a new API key's secret:

```toml
[policy.audience]
self = ["google-workspace:viewer"]
```

A multi-user host that passes each session's principal resolves `self`
without it. With neither, a call after `admin_create_api_key` that needs
an audience's members is denied, because its readers cannot be
established.

It also permits `xmemory-review`, and data below `trusted` when a
person may approve a write from a trajectory that read untrusted text.
The Claude Code and kagent plugin defaults ship a human authority
permitting both (`trust_below = "trusted"`, `attention = ["*"]`), so the
person running the session approves those calls. Another root config declares one:

```toml
[[policy.authority]]
name = "xmemory-operator"
hint = "Review the exact xmemory change."
permits = { trust_below = "trusted", attention = ["xmemory-review"] }

[externals.authorities.xmemory-operator]
builtin = "hitl"
```

## Limits

xmemory keeps no per-record permissions, and no tool reports who can
read an instance. People and API keys reach the clusters and instances
their role assignments name. A cluster's assignments cover every instance
in it, and no tool lists users. So every read is `internal`, the
coarsest honest label. Map `internal` in the root
config to the organisation's members through your organisation's
audience sources. Per-cluster readers are not modeled.

The instance tools name no instance: the connection URL or the sign-in
binds it. A deployment that connects several instances binds each server
to this namespace, and they all share the `internal` label.

A write from an untrusted trajectory needs an authority that permits
data below `trusted`. Once permitted, it can change or delete records
that other agents and people rely on, and later reads of those records
keep the reader's trust.

Every tool that reads or writes memory, and the schema tools, send their
input to the LLM provider that xmemory's gateway routes to. That flow is
inside xmemory and outside the policy.

Tools that a connection does not advertise still have rules. An OAuth
connection chooses its tools on the sign-in page, and an API key session
gets the instance tools without the schema group
(`update_instance_schema`, `dry_run_schema_migration`,
`list_schema_migrations`, `get_schema_migration`, `enhance_schema`).
Only a connection that sends an API key itself is offered
`admin_create_api_key`, and every connection lists only the tools its
role assignments permit. A tool the policy does not name is blocked.

## Tests

`appa-runtime/tests/xmemory_policy.rs` loads the battery with a fixed
audience and checks that writes, schema decisions, and metadata changes
run after a read, and that none runs once outside text lowers the trust.
It also checks which effects each write records, and that a memory write
after a new API key is refused. The
offline replay `examples/live-replays/xmemory/` proposes one call of
each kind and checks each decision. The battery has no scripts.

```sh
cargo test --locked -p appa --test integration -- xmemory_policy:: marketplace::
appa replay --config examples/live-replays/xmemory/appa.toml \
  examples/live-replays/xmemory/xmemory-battery.appa
```
