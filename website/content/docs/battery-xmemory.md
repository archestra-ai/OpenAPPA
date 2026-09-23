---
title: xmemory battery
category: Batteries
order: 6.76
description: Rules for the hosted xmemory instance and admin MCP servers' 32 tools, with internal reads and reviewed schema migrations and instance deletion.
sidebar: false
breadcrumb: xmemory
---

The xmemory battery covers all 32 tools of the hosted xmemory MCP servers: 15 on the instance server, which reads and writes one memory instance, and 17 on the admin server, which creates, describes, and deletes instances. One namespace, `xmemory`, is bound to each server you run.

Instance creation, metadata changes, schema migrations, and instance deletion ask an Authority. The Claude Code and kagent plugin defaults ship one that the person running the session answers; another root config must define an Authority permitting `xmemory-review` and data below `trusted`.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/xmemory).

## Tool behavior

- Reads, write status, schema and migration reads, schema suggestions, and admin reads of clusters and instances return untrusted `internal` data.
- Writes, with `text` or with `structured_mutations`, need input that `internal` may see, but not trusted input: everything a later read returns is untrusted, so a write cannot raise the trust of any data. They record `xmemory.changed`.
- Recording schema decisions needs trusted input.
- Creating an instance and changing its metadata need trusted input and return the whole instance as untrusted data, so each call asks the Authority.
- Applying a schema migration and deleting an instance need trusted input and review, and record `xmemory.schema` or `xmemory.deleted`.

## Limit

No xmemory tool reports who can read an instance, and a console user reaches every cluster of their organisation. All reads default to `internal`; map it in your root `appa.toml` to your organisation's members. A write from an untrusted session can change records other agents rely on; later reads of them stay untrusted. Every tool that reads or writes memory sends its input to the LLM provider behind xmemory's gateway.
