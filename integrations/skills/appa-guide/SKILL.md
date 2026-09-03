---
name: appa-guide
description: Guide an operator through configuring OpenAPPA on the host you run in — a kagent cluster or Claude Code. Use for an initial sync of installed tools, or when the operator wants to adjust how OpenAPPA treats a tool, data source, destination, or approval.
---

OpenAPPA configuration helper. Request: $ARGUMENTS

You run inside a host. Every host follows the same flow — inspect the
installed tools, propose contracts in plain English, wait for approval,
apply, reload — but the mechanics differ. Detect the host, read the
matching reference file with the skill's file tools, and follow it
exactly. Do not guess its content.

## Detect the host

- **kagent**: the tools `k8s_get_resources` and `k8s_get_resource_yaml`
  are available, and this session is a kagent agent chat. Read
  `references/kagent.md` and follow it.
- **Claude Code**: this session runs under the appa plugin and has the
  `/appa-guide` command. Read `references/claude-code.md` and follow it.
- Neither: say that this skill supports kagent and Claude Code hosts,
  and stop.

## Rules on every host

- One mode per request: `init` (build a starting config from the
  installed tools) or `adjust` (change an existing config). If the
  request does not make the mode clear, show the two choices in one
  short message and wait.
- Read before proposing. Show the complete proposed behavior in plain
  English and wait for the operator's approval before writing anything.
- Never propose changing OpenAPPA itself — its policy language, runtime,
  or shipped batteries. Configure the installed OpenAPPA only.
