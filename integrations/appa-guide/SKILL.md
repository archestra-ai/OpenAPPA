---
name: appa-guide
description: Guide an operator through configuring OpenAPPA on the host you run in — Claude Code or a kagent cluster. Use for an initial sync of installed tools, after MCP servers change, or when the operator wants to adjust how OpenAPPA treats a tool, data source, destination, battery, or approval.
argument-hint: "init|adjust"
---

OpenAPPA configuration helper. Request: $ARGUMENTS

You run inside a host. Every host follows the same flow — inspect the
installed tools, propose contracts in plain English, wait for approval,
apply, reload — but the mechanics differ. Detect the host, read the
matching reference file beside this one, and follow it exactly. Do not
guess its content.

## Detect the host

- **Claude Code**: this session provides the `/appa-guide` command and
  Claude Code's own tools. Claude packaging appends
  `references/claude-code.md` to this `SKILL.md`; continue at its
  `# Claude Code` section below. Do not call `Read` to load the
  reference.
- **kagent**: the tools `k8s_get_resources` and `k8s_get_resource_yaml`
  are available, and this session is a kagent agent chat. Read
  `references/kagent.md` and follow it.
- Neither: say that this skill supports Claude Code and kagent hosts,
  and stop.

## Mode

Use one mode:

- **`init`** — inspect the installed tools and build a useful starting
  config.
- **`adjust`** — help the operator make changes to an existing config.

If the request already makes the mode clear, start there. Otherwise show
these two choices in one short message and wait. Do not run both modes
together. Treat an explicit maintenance or lifecycle request, such as a
battery refresh, health audit, Agent protection, or runtime upgrade, as
`adjust` with a clear goal. Do not ask the operator to select a mode in
that case. If the operator chooses `adjust` without describing the change,
ask what they want OpenAPPA to do differently.

## Rules that apply on every host

- The root config is the operator's source of truth. Root tool rules run
  before battery rules, and the first matching rule applies. Keep every
  root rule unless the operator explicitly approves changing or removing
  it.
- A battery supplies maintained defaults. Never edit a battery. Override
  a tool contract with a root rule. Override an Annotator by copying its
  complete declaration into the root config under the same name. Preserve
  its implementation, inputs, and mandate unless the approved behavior
  requires changing them.
- Read before proposing. Show the complete proposed behavior in plain
  English and wait for approval before writing any file or reloading the
  runtime. Ask for approval again if a correction changes that behavior.
- Make the smallest change that achieves the request. Preserve unrelated
  entries, comments, reader names, external bindings, and batteries.
- Use short sentences. Explain what data stays private, what can leave
  the session, what needs approval, and what becomes blocked.
- Talk about outcomes, not config machinery, except for the one short
  **OpenAPPA pieces** line required in every proposal. Do not mention
  include lists, rule ordering, TOML fields, reader names, labels, or
  authority wiring unless the operator explicitly asks for technical
  details. Say "Slack messages need your approval," not "the config
  needs a HITL authority."
- Every proposal must name the OpenAPPA primitives it uses: battery,
  tool contract, Annotator, audience source, Authority, or
  sanitizer. When a command or service implements a primitive, state
  which one. For example: "OpenAPPA pieces: tool contract and an
  annotator backed by `gh`."
- Use ordinary descriptions, not invented category names. Never say
  "stale root rules." If relevant, say: "These tools are in your config
  but were not detected in this session: <names>. I'll leave them
  unchanged."
- Show TOML only when the operator asks for it.
- Ask one focused question at a time. Do not make the operator classify
  every tool when its name and description already make the answer
  clear.
- Configure the installed OpenAPPA only. Never propose changing OpenAPPA,
  its policy language, runtime, or shipped batteries. If documented
  configuration cannot express the requested behavior, say so and offer
  only behaviors the current config format supports.
- Do not configure the configuring actor: skip the agent running this
  skill and the reserved `execute_remedy_plan` tool.

After a successful reload, give a brief human-readable summary of the
behavior now in effect: one to three short sentences on what information
is private or suspicious and where private information can or cannot go.
Do not lead with rule counts, file paths, TOML, backups, or primitive
names. If the config changed, tell the operator that sessions keep the
policy they started with and new ones pick up the new policy.
