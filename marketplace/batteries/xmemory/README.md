# xmemory battery

Rules for the hosted [xmemory](https://xmemory.ai) MCP servers, all 32
tools: the instance server at `https://mcp.xmemory.ai` (or
`https://mcp.xmemory.ai/instance/<id>`, 15 tools), which reads and writes
one memory instance, and the admin server at
`https://mcp.xmemory.ai/admin` (17 tools), which creates, describes, and
deletes instances. Plain TOML
rules, no helper process or provider credential. One namespace,
`xmemory`, is bound to each server you run:

```sh
appa battery install xmemory --server <instance-server> --server <admin-server>
```

A deployment with one of the two servers binds only that one. The xmemory
Claude Code plugin names each instance server `xmemory-<first 8 characters
of the instance id>`; bind that name.

## Server version

xmemory commit `b3aaaa6`, the production build on 2026-09-23. The
servers carry no version number. The instance and admin tool lists are
the servers' `tools/list` at that build; the five schema-management
tools are from the source at that commit.

## Rules

OpenAPPA checks `requires` against the label after the call's own
`delta`. A contract that needs trusted data and returns `suspicious`
content therefore asks an authority on every call.

*Instance reads* — `read`, `write_status`, `get_instance_id`,
`get_instance_schema`, `get_setup_instructions`, `review_suggestions`,
`list_schema_migrations`, `get_schema_migration`,
`dry_run_schema_migration`, `enhance_schema`. Memory content was written
by every agent and person with access to the instance, and an LLM
extracted it into records, so it enters `suspicious`, restricted to
`internal`. A tool's input must be sharable with `internal` too, because
the instance stores it or its readers see it. `review_suggestions` also
merges duplicate feedback rows that rebuild the same proposal; it changes
no schema and no records.

*Writes* — `write` and `write_async`, with `text` or with
`structured_mutations`. The input must be sharable with `internal`; it
does not need trusted data. Everything a later read returns enters
`suspicious`, whoever wrote it, so a write cannot raise the trust of any
data. A `text` write can update and delete records through extraction,
so a `structured_mutations` write that names the records exactly, for
example
`[{"object_mutation": {"object_type": "Person", "delete": {"key": {"name": "Bob"}}}}]`,
gets the same contract. `write` returns the records it changed, with
their old values and the values of deleted records, so its result enters
`suspicious` and `internal`. `write_async` returns a queue id only, and
`write_status` returns the result. Both record `xmemory.changed`.

*Schema changes* — `decide_suggestions` records accept, reject, and defer
decisions and needs trusted data. Its result echoes the decisions and
keeps the trust, so it runs without an authority from a trusted
trajectory and records `xmemory.changed`. `apply_pending_decisions` and
`update_instance_schema` migrate the schema, which can drop fields and
their values. They need trusted data and the `xmemory-review` mark,
return `suspicious` content, and record `xmemory.schema`.

*Admin reads* — `admin_list_clusters`, `admin_get_cluster`,
`admin_list_instances`, `admin_list_own_instances`,
`admin_get_instance`, `admin_get_instance_by_id`,
`admin_get_instance_schema_by_id`, `admin_get_setup_instructions`,
`admin_generate_schema`, `admin_enhance_schema`. Instance names,
descriptions, owner instructions, and schemas were written by the
organisation's members, and the two schema tools return LLM output, so
all enter `suspicious`, restricted to `internal`.

*Admin writes* — `admin_create_instance` and the four
`admin_update_instance_metadata*` and `admin_patch_instance_metadata*`
tools need trusted data and record `xmemory.admin`. Metadata includes the
owner instructions that every agent connected to the instance receives.
Each returns the whole instance, including fields the call did not set,
so its result enters `suspicious` and every call asks an authority that
permits data below `trusted`.

*Instance deletion* — `admin_delete_instance` and
`admin_delete_instance_by_id` delete an instance and its data. They need
trusted data and the `xmemory-review` mark, return the deleted instance
as `suspicious` content, and record `xmemory.deleted`.

## Root config

The battery binds no audience source and names no credential variable.
A root config adds two things.

It maps `internal` onto the people who may read everything the
connection reaches: the organisation's members. For example, with the
Google Workspace battery's audience source:

```toml
[policy.audience]
internal = ["google-workspace:full-members"]
```

It also permits `xmemory-review` and data below `trusted`. The Claude
Code and kagent plugin defaults ship a human authority permitting both
(`trust_below = "trusted"`, `attention = ["*"]`), so the person running
the session approves those calls. Another root config declares one:

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
read an instance. An API key reaches only the clusters linked to it, but
a console user reaches every cluster of their organisation, and an
xmemory API key cannot list users or other people's keys. So every read
is `internal`, the coarsest honest label. Map `internal` in the root
config to the organisation's members through your organisation's
audience sources. Per-cluster readers are not modeled.

The instance tools name no instance: the connection URL or the sign-in
binds it. A deployment that connects several instances binds each server
to this namespace, and they all share the `internal` label.

A write from an untrusted trajectory can change or delete records that
other agents and people rely on. The policy does not stop that; it keeps
every later read of those records `suspicious`.

Every tool that reads or writes memory, and the schema tools, send their
input to the LLM provider that xmemory's gateway routes to. That flow is
inside xmemory and outside the policy.

Tools that a connection does not advertise still have rules. An OAuth
connection chooses its tools on the sign-in page, and an API key session
gets the instance tools without the schema group
(`update_instance_schema`, `dry_run_schema_migration`,
`list_schema_migrations`, `get_schema_migration`, `enhance_schema`). A
tool the policy does not name is blocked.

## Tests

`appa-runtime/tests/xmemory_policy.rs` loads the battery with a fixed
audience and checks that writes run after a read, that schema decisions
and metadata changes do not, and which effects each write records. The
offline replay `examples/live-replays/xmemory/` proposes one call of
each kind and checks each decision. The battery has no scripts.

```sh
cargo test --locked -p appa --test xmemory_policy --test marketplace
bash scripts/appa-marketplace.sh --check
appa replay --config examples/live-replays/xmemory/appa.toml \
  examples/live-replays/xmemory/xmemory-battery.appa
```
