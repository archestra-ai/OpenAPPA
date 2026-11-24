---
title: xmemory battery
category: Batteries
order: 6.76
description: Rules for the hosted xmemory instance and admin MCP servers' 41 tools, with internal reads and reviewed schema migrations, instance deletion, membership edits, and API key creation.
sidebar: false
breadcrumb: xmemory
---

The xmemory battery covers all 41 tools of the hosted xmemory MCP servers: 16 on the instance server, which reads and writes one memory instance, and 25 on the admin server. The admin server creates, describes, and deletes instances, edits their cluster memberships, lists API keys, and creates keys for the caller. One namespace, `xmemory`, is bound to each server you run.

Schema migrations, instance deletion, membership edits, and new API keys ask an Authority. The Claude Code and kagent plugin defaults ship one that the person running the session answers; another root config must define an Authority permitting `xmemory-review`.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/xmemory).

## Tool behavior

- Reads return `internal` data and keep the session's trust: memory reads, write status, the context pack, schema and migration reads, and schema suggestions. So do admin reads of clusters, instances, memberships, schemas, and key metadata. Trust follows who can write the text: the organisation's members and agents write memory, and only from trusted data.
- Writes, with `text` or with `structured_mutations`, need trusted input that `internal` may see, so suspicious text cannot enter memory and come back trusted. They record `xmemory.changed`.
- Recording schema decisions needs trusted input.
- Creating an instance and changing its metadata need trusted input; the returned instance keeps the session's trust.
- Applying a schema migration and deleting an instance need trusted input and review, and record `xmemory.schema` or `xmemory.deleted`.
- Editing an instance's cluster memberships changes who can read it, and creating an API key returns its secret once. Both need trusted input and review, and record `xmemory.admin`. The new key's result is `self`, so a memory write after it needs an Authority that may widen the audience.

## Limit

No xmemory tool reports who can read an instance, and a cluster's role assignments cover every instance in it. All reads default to `internal`; map it in your root `appa.toml` to your organisation's members. Map `self` to the person the session acts for, the only reader of a new API key. A write from an untrusted session needs an Authority that permits data below `trusted`. Every tool that reads or writes memory sends its input to the LLM provider behind xmemory's gateway.
