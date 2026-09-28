---
title: xmemory battery
category: Batteries
order: 6.76
description: Rules for the hosted xmemory instance and admin MCP servers' 32 tools, with internal reads and reviewed schema migrations and instance deletion.
sidebar: false
breadcrumb: xmemory
---

The xmemory battery covers all 32 tools of the hosted xmemory MCP servers: 15 on the instance server, which reads and writes one memory instance, and 17 on the admin server, which creates, describes, and deletes instances. One namespace, `xmemory`, is bound to each server you run.

Schema migrations and instance deletion ask an Authority. The Claude Code and kagent plugin defaults ship one that the person running the session answers; another root config must define an Authority permitting `xmemory-review`.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/xmemory).

## Tool behavior

- Reads, write status, schema and migration reads, schema suggestions, and admin reads of clusters and instances return `internal` data and keep the session's trust. Trust follows who can write the text: the organisation's members and agents write memory, and only from trusted data.
- Writes, with `text` or with `structured_mutations`, need trusted input that `internal` may see, so suspicious text cannot enter memory and come back trusted. They record `xmemory.changed`.
- Recording schema decisions needs trusted input.
- Creating an instance and changing its metadata need trusted input; the returned instance keeps the session's trust.
- Applying a schema migration and deleting an instance need trusted input and review, and record `xmemory.schema` or `xmemory.deleted`.

## Limit

No xmemory tool reports who can read an instance, and a console user reaches every cluster of their organisation. All reads default to `internal`; map it in your root `appa.toml` to your organisation's members. A write from an untrusted session needs an Authority that permits data below `trusted`. Every tool that reads or writes memory sends its input to the LLM provider behind xmemory's gateway.
