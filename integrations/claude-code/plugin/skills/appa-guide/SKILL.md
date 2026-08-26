---
name: appa-guide
description: Guide a user through configuring OpenAPPA for Claude Code. Use for an initial sync of installed tools, after MCP servers change, or when the user wants to adjust how OpenAPPA treats a tool, data source, destination, battery, or approval.
---

OpenAPPA configuration helper. Request: $ARGUMENTS

## Mode

Use one mode:

- **`init`** — inspect the installed tools and build a useful starting config.
- **`adjust`** — help the user make changes to an existing config.

If the request already makes the mode clear, start there. Otherwise show these
two choices in one short message and wait. Do not run both modes together.
If the user chooses `adjust` without describing the change, ask what they want
OpenAPPA to do differently.

## Rules that apply in both modes

- The root config is the user's source of truth. Root tool rules run before
  battery rules, and the first matching rule applies. Keep every root rule
  unless the user explicitly approves changing or removing it.
- A battery supplies maintained defaults. Never edit a battery. Override it
  with a root rule.
- Read before proposing. Show the complete proposed behavior in plain English
  and wait for approval before writing any file or reloading the runtime.
- Make the smallest change that achieves the request. Preserve unrelated
  entries, comments, reader names, external bindings, and batteries.
- Use short sentences. Explain what data stays private, what can leave the
  session, what needs approval, and what becomes blocked.
- Talk about outcomes, not config machinery. Do not mention include lists,
  rule ordering, TOML fields, reader names, labels, or authority wiring unless
  the user explicitly asks for technical details. Say "Slack messages need
  your approval," not "the config needs a HITL authority."
- Show TOML only when the user asks for it.
- Ask one focused question at a time. Do not make the user classify every tool
  when its name and description already make the answer clear.

## Find the live config

Run:

```sh
ps ax -o command | grep appa-runtime | grep -v grep
```

Use the path after `--config`. If the process has no visible config path, or
is not running, ask the user for the path. Do not guess it.

The runtime address is
`${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

## Initial tool sync

### Inspect

1. Read the root config first. Record its tool rules and included batteries.
2. Run `claude mcp list` for configured servers. Use the current session's
   tool surface for the exact tool names and descriptions. A configured server
   that is not visible is unprobed; report it and do not invent its tools.
3. Compare the installed tools with the root rules. Existing root rules stay
   in control, including rules for tools a battery also covers.

### Find useful batteries

Look under:

```sh
~/.claude/plugins/marketplaces/appa/batteries/
```

Match a battery by the tool names in its `appa.toml`, not by its directory
name. For a matched battery, read only its `appa.toml` and README. Do not run
its scripts while inspecting it.

If that marketplace clone or its `batteries/` directory is missing, list the
batteries from GitHub:

```sh
gh api repos/archestra-ai/OpenAPPA/contents/batteries --jq '.[].name'
```

Use the GitHub contents API to read only a matched battery's `appa.toml` and
README. Do not search other repositories.

When proposing a battery, give it exactly one short sentence that says what it
covers, what protection it adds, and any important assumption. Keep it under
20 words. Examples:

> Slack battery — Keeps Slack data private and asks before publishing it.
>
> GitHub battery — Assumes every repository is public and prevents private data from leaking to GitHub.

If the current config changes a battery's default behavior, describe the
resulting behavior in plain English. Do not explain the rule ordering unless
the user asks.

### Cover the remaining tools

Create root rules only for installed tools that neither the root config nor a
matched battery covers.

- A tool that reads user, company, local, or authenticated data returns
  private data: `delta = { audience = ["private"] }`. Reuse the config's
  existing private reader name when it has one.
- A tool that publishes, posts, sends, shares, or uploads requires data that
  may be public: `requires = { audience = { contains = ["public"] } }`.
- A clearly public read or a tool whose result carries no data uses
  `delta = {}`.
- Every new tool entry needs `delta`, including entries with `requires`.

### Ask about ambiguity

Use tool names and descriptions when their behavior is clear. If you still
cannot tell which servers can return data that should stay private, ask the
user once. Put every unclear server in one grouped question. Do not guess and
do not ask about each tool separately.

Wait for the answer before showing the proposal. This answer does not replace
the approval required below. If nothing is unclear, do not ask.

### Propose, then apply

Group the proposal by server. Show:

- batteries to add, each with its one-sentence explanation;
- existing behavior that stays unchanged, but only when it affects the result;
- how the remaining installed tools will behave;
- installed tools that will remain blocked.

Do not mention unchanged or stale config entries unless they affect the user's
requested outcome.

End with: **Approve, or tell me what to change.** Wait for the reply.

After approval:

1. Copy each approved battery directory beside the root config under
   `batteries/<name>/` and add its `appa.toml` to the root `include` list. Use
   the marketplace clone when present; otherwise fetch every file in that
   battery directory from GitHub so its supporting scripts are included. Leave
   an existing copied battery unchanged unless the user asked to refresh it.
2. Add any root support the battery requires, such as its human-approval
   authority. This is part of making the approved behavior work; describe the
   behavior to the user, not this wiring.
3. Add the approved uncovered-tool rules to the root config. Do not remove
   overlapping root rules; they intentionally override batteries.
4. Reload and report the result as described below.

## Adjust the current config

Start from the user's requested outcome, not from a full tool rescan.

If the requested outcome is ambiguous, ask one focused question and wait. Do
not guess.

1. Read the root config and only the included files relevant to the requested
   changes.
2. For policy syntax or behavior that the current config does not demonstrate,
   consult the relevant section of
   `~/.claude/plugins/marketplaces/appa/website/content/docs/contracts.md`.
   Do not guess syntax.
3. Explain three things: what happens now, what you propose, and the practical
   effect. Ask only for a decision that changes the result.
4. If a battery would help, propose it with the same one-sentence rule used in
   `init` mode. Existing root rules still take priority.
5. End with: **Approve, or tell me what to change.** Wait for the reply.
6. Apply only the approved changes in the root config. To change battery
   behavior, add or edit a root rule; never modify the battery.
7. Reload and report the result as described below.

For several root rules with the same tool name, order matters. Put a narrow
argument-specific rule before its general fallback. Do not reorder unrelated
rules.

## Reload and finish

Reload only after an approved write:

```sh
curl --fail-with-body -sS -X POST \
  "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

The runtime checks the whole config before installing it. If reload is
refused, the previous config keeps serving. Explain the error plainly and fix
it. Ask for approval again if the fix changes the behavior the user approved.

Finish with what changed and what it protects. If the config changed, add:

> Start a new `clappa` session to use the updated policy; this session keeps
> the policy it started with.
