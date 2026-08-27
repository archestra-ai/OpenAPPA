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
- Talk about outcomes, not config machinery, except for the one short
  **OpenAPPA pieces** line required in every proposal. Do not mention include
  lists, rule ordering, TOML fields, reader names, labels, or authority wiring
  unless the user explicitly asks for technical details. Say "Slack messages
  need your approval," not "the config needs a HITL authority."
- Every proposal must name the OpenAPPA primitives it uses: battery, tool
  contract, dynamic resolver, membership resolver, authority, sanitizer, or
  cast. When a command or service implements a primitive, state which one. For
  example: "OpenAPPA pieces: tool contract and a dynamic resolver backed by
  `gh`."
- Use ordinary descriptions, not invented category names. Never say "stale
  root rules." If relevant, say: "These tools are in your config but were not
  detected in this session: <names>. I'll leave them unchanged."
- Show TOML only when the user asks for it.
- Ask one focused question at a time. Do not make the user classify every tool
  when its name and description already make the answer clear.
- Configure the installed OpenAPPA only. Never inspect OpenAPPA source code,
  tests, Git history, local repository checkouts, or implementation details.
  Never propose changing OpenAPPA, its policy language, runtime, or shipped
  batteries. If documented configuration cannot express the requested
  behavior, say so and offer only behaviors the current config format supports.

For OpenAPPA configuration, read only:

- the output of `appa describe --config <live-path>`;
- the live root config and included files relevant to the request;
- a matched battery's `appa.toml` and README;
- the relevant section of the installed guide at
  `~/.claude/plugins/marketplaces/appa/website/content/docs/contracts.md`;
- if that guide is unavailable or does not answer the question, the relevant
  section of <https://www.openappa.com/contracts>.

If these sources do not establish the syntax or behavior, stop. Do not search
the OpenAPPA repository for an answer.

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

1. Run `appa describe --config <live-path>` before reading or changing
   the config. It is read-only and succeeds when the config is missing or
   invalid. Record its config state, effective policy tools, included battery
   names, referenced groups, and membership resolver/binding status. Treat its
   session integrations, tools, and accounts as unavailable when it says so;
   never turn an unavailable fact into an empty inventory.
2. Read the root config. Record its tool rules and included batteries, and
   preserve its comments. If `appa describe` and the file disagree, stop and
   report the mismatch instead of guessing.
3. Run `claude mcp list` for configured servers.
4. Add every MCP server visible in the current session, even when
   `claude mcp list` omits it. MCP tools use `mcp__<server>__<tool>` names;
   plugin-provided servers use `mcp__plugin_<plugin>_<server>__<tool>` names.
   Keep each exact full tool name and description.
5. Cross-check both sources. Record every configured MCP server whose tools
   could not be detected. Keep it separate from Claude Code's built-in tools.
   Do not invent its tool list.
6. Compare the installed tools with the root rules. Existing root rules stay
   in control, including rules for tools a battery also covers.

The command cannot see Claude's session tool catalogue or authenticated
connector accounts. The session supplies tool facts; the user supplies an
account identity when a connector does not expose one. Do not probe private
mail, messages, or files merely to infer an identity.

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

Check what each matched battery expects the root config to provide. Record
anything missing that the battery or complete config needs in order to work.
A proposal may mention only groups listed under `referenced_groups` by
`appa describe`, or a group the user explicitly establishes during this flow
with a concrete resolver. A registered membership resolver does not prove
that an arbitrary plausible group name exists.

### Cover the remaining tools

Create root rules only for installed tools that neither the root config nor a
matched battery covers.

- A tool that reads personal or authenticated data may return data for a
  configured `@self`. Organization-wide data may return data for a configured
  `@internal`. If the suitable group was not reported by `appa describe`,
  leave the tool blocked and explain the missing resolver. Never substitute
  `"private"`, `@company`, `@employees`, or another plausible reader or group.
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

For Gmail, match only exact tools visible in this session whose names start
with `mcp__claude_ai_Gmail__`; do not assume a fixed connector tool list. If
the connected account is not exposed, include the account and intended data
boundary in the one grouped ambiguity question. A non-consumer email domain
is only a candidate boundary and still needs confirmation. Never suggest
`gmail.com` or another consumer-mail domain as `@internal`.

Confirmation alone does not create a working domain-backed group. The current
membership resolver must expand a group to concrete reader IDs, so Gmail by
itself cannot implement `@internal` from a domain. Require a real directory
resolver that can enumerate those readers; otherwise leave internal-dependent
tools blocked and say why. This boundary answer is separate from approval to
write or install anything.

### Propose, then apply

Group the proposal by server. Show:

- batteries to add, each with its one-sentence explanation;
- existing behavior that stays unchanged, but only when it affects the result;
- how the remaining installed tools will behave;
- installed tools that will remain blocked;
- every configured MCP server whose tools could not be detected.

Add one short **OpenAPPA pieces** line that names every primitive the proposal
uses. Do not list file plumbing such as include paths.

Do not mention config entries that were not detected unless they affect the
user's requested outcome.

Name each configured MCP server that could not be inspected and say: "<server>
is configured, but I could not inspect its tools in this session." Do not omit
the server or fold it into a list of individual tools.

When tools named in the config were not detected, use their exact names and
say only that they were not detected in this session and will be left
unchanged. Do not call them stale, removed, obsolete, or uninstalled.

At the end of the proposal, add **Needed for this to work** when any required
support is missing. Group every missing requirement there and propose the
concrete fix. For example: "Slack needs your approval before publishing, but
approval is not set up yet. I'll add it." Do not merely report "no HITL
authority," and do not mix missing requirements with unchanged rules.

End with: **Approve, or tell me what to change.** Wait for the reply.

After approval:

1. Run `appa describe --config <live-path>` again. If the config,
   batteries, referenced groups, or membership wiring changed since the
   proposal, revise the proposal and ask for approval again.
2. Copy each approved battery directory beside the root config under
   `batteries/<name>/` and add its `appa.toml` to the root `include` list. Use
   the marketplace clone when present; otherwise fetch every file in that
   battery directory from GitHub so its supporting scripts are included. Leave
   an existing copied battery unchanged unless the user asked to refresh it.
3. Add any root support the battery requires, such as its human-approval
   authority. This is part of making the approved behavior work; describe the
   behavior to the user, not this wiring.
4. Add the approved uncovered-tool rules to the root config. Do not remove
   overlapping root rules; they intentionally override batteries.
5. Reload and report the result as described below.

## Adjust the current config

Start from the user's requested outcome, not from a full tool rescan.

If the requested outcome is ambiguous, ask one focused question and wait. Do
not guess.

1. Read the root config and only the included files relevant to the requested
   changes.
2. For policy syntax or behavior that the current config does not demonstrate,
   first consult the relevant section of
   `~/.claude/plugins/marketplaces/appa/website/content/docs/contracts.md`.
   If it is unavailable or does not answer the question, consult the relevant
   section of <https://www.openappa.com/contracts>. Do not guess syntax, search
   for an OpenAPPA checkout, or inspect source code.
3. Explain three things: what happens now, what you propose, and the practical
   effect. Add one short **OpenAPPA pieces** line naming every primitive used.
   Ask only for a decision that changes the result.
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

After a successful reload, give a brief human-readable summary of the behavior
now in effect. Use one to three short sentences or bullets. Say what information
is now treated as private or suspicious and where private information can or
cannot go. For example:

> Information from shared Slack channels is now treated as suspicious.
>
> Private information cannot be sent to public GitHub repositories.

Do not lead with rule counts, file paths, TOML, backups, or primitive names.
Mention an important remaining limitation in one short sentence when needed.

If the config changed, add:

> Start a new `clappa` session to use the updated policy; this session keeps
> the policy it started with.
