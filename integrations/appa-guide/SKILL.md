---
name: appa-guide
description: Set up and tune OpenAPPA on the host you run in — Claude Code or a kagent cluster. Checks which tools and MCP servers the policy covers, includes the batteries that fit, writes rules for the rest, explains why a call was blocked, and makes the defaults stricter or looser on request.
argument-hint: "[init | adjust | explain | what you want]"
---

OpenAPPA configuration helper. Request: $ARGUMENTS

You run inside a host. Every host follows the same flow — inspect the
installed tools, propose contracts in plain English, wait for approval,
apply, reload — but the mechanics differ. The policy-writing rules every
host shares live in `references/core.md`. Detect the host, read the core
rules and then the matching host reference beside this one, and follow
both exactly. Do not guess their content.

## Detect the host

- **Claude Code**: this session provides the `/appa-guide` command and
  Claude Code's own tools. Claude packaging appends `references/core.md`
  and then `references/claude-code.md` to this `SKILL.md`; continue at
  the `# Policy-writing rules` section below, then its `# Claude Code`
  section. Do not call `Read` to load either reference.
- **kagent**: the tools `k8s_get_resources` and `k8s_get_resource_yaml`
  are available, and this session is a kagent agent chat. Before any
  cluster action, call `read_file` for
  `/skills/appa-guide/references/core.md` with `offset: 1` and
  `limit: 0`, then for `/skills/appa-guide/references/kagent.md` with
  `offset: 1` and `limit: 0`. Each exact call reads through end of file.
  Follow both complete results.
  The `skills` tool is used only for `command: appa-guide`. Runtime
  management uses only the direct `appa_*` tools named in the kagent
  reference, including `appa_update_policy`. Never invoke an
  `appa-guide-*` executable, `skills`, or `k8s_execute_command` for
  runtime policy or battery work.
- Neither: say that this skill supports Claude Code and kagent hosts,
  and stop.

## Mode

Use one mode:

- **`init`** — check what the host has connected and how the policy
  covers it, then propose a starting config: batteries to include, what
  they need set up, rules for tools nothing covers, and any host classifier
  guidance derived from those batteries. It is also the checkup to run after
  MCP servers change.
- **`adjust`** — change how OpenAPPA treats a tool, data source,
  destination, battery, or approval, including making the defaults
  stricter or looser.
- **`explain`** — say why a call was blocked, or what the current policy
  does. Read-only: it proposes nothing unless the operator then asks for
  a change, which continues as `adjust`.

With no request, run `init`. Otherwise start in the mode the request
makes clear, and ask only when two modes fit it equally. Do not run two
modes together. Treat an explicit maintenance or lifecycle request, such
as a battery refresh, health audit, Agent protection, or runtime upgrade,
as `adjust` with a clear goal. Treat "why was this blocked", "show
policy", or "what does the policy do" as `explain`, which only reads the
policy. If the operator chooses `adjust` without describing the
change, ask what they want OpenAPPA to do differently.

An explicit `init` authorizes the complete read-only inspection and the
proposal. Do not ask whether to continue before the proposal. Start with one
sentence saying what you will inspect.
Invoke only the `appa-guide` skill name; never invent a mode-specific skill name.
