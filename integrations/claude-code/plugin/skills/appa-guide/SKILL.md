---
name: appa-guide
description: Guide a user through configuring OpenAPPA for Claude Code. Use for an initial sync of installed tools, after MCP servers change, or when the user wants to adjust how OpenAPPA treats a tool, data source, destination, battery, or approval.
argument-hint: "init|adjust"
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
- A battery supplies maintained defaults. Never edit a battery. Override a
  tool contract with a root rule. Override an Annotator by repeating its
  complete declaration in the root with the same name. Keep its implementation,
  inputs, and mandate unless the approved behavior requires changing them.
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
  contract, annotator, audience source, authority, or sanitizer. When a
  command or service implements a primitive, state which one. For example:
  "OpenAPPA pieces: tool contract and an annotator backed by `gh`."
- Use ordinary descriptions, not invented category names. Never say "stale
  root rules." If relevant, say: "These tools are in your config but were not
  detected in this session: <names>. I'll leave them unchanged."
- Show TOML only when the user asks for it.
- Ask one focused question at a time. Do not make the user classify every tool
  when its name and description already make the answer clear.
- Configure the installed OpenAPPA only. The marketplace's `installLocation`
  may be a local checkout; read only its installed battery files and contract
  guide named below. Never search that checkout, inspect source code, tests,
  Git history, or implementation details. Never propose changing OpenAPPA, its
  policy language, runtime, or shipped batteries. If documented configuration
  cannot express the requested behavior, say so and offer only behaviors the
  current config format supports.

For OpenAPPA configuration, read only:

- the output of `appa describe --config <live-path>`;
- the live root config and included files relevant to the request;
- a matched battery's `appa.toml` and README;
- the relevant section of the installed guide at
  `<marketplace-root>/website/content/docs/contracts.md`.

If the installed marketplace content is missing or these sources do not
establish the syntax or behavior, stop and report an incomplete installation.
Do not fetch a different OpenAPPA version or search the repository for an
answer.

## Find the live config

Read `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/plugins/known_marketplaces.json` and
take the `installLocation` for the `appa` marketplace as
`<marketplace-root>`. Local checkout installs use
the checkout itself; packaged and remote installs may use a Claude-managed
clone. If the entry or directory is missing, stop and report an incomplete
installation. Do not assume a fixed marketplace path.

Run `appa describe` first. Use the complete path on its `Config:` line, including
spaces. This is the installed deployment path and follows `APPA_CONFIG` and
`APPA_CONFIG_DIR` when either is set.

Then run:

```sh
ps ax -o command | grep '[a]ppa runtime'
```

If a running process visibly names a different `--config` path, stop and ask
the user which deployment to configure. Do not split an unquoted path on
spaces. If no runtime is running, continue with the path reported by `appa
describe`; initialization has already established it.

The runtime address is
`${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

## Initial tool sync

### Inspect

1. Run `appa describe --config <live-path>` before reading or changing
   the config. It is read-only and succeeds when the config is missing or
   invalid. Record its config state, effective policy tools, included battery
   names, audience sources, and named audiences. Treat its
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
<marketplace-root>/batteries/
```

Match a battery by the tool names in its `appa.toml`, not by its directory
name. For a matched battery, read only its `appa.toml` and README. Do not run
its scripts while inspecting it.

If that marketplace clone or its `batteries/` directory is missing, stop and
report an incomplete installation. Never configure one APPA build with
batteries fetched from another version.

When proposing a battery, give it exactly one short sentence that says what it
covers, what protection it adds, and any important assumption. Keep it under
20 words. Examples:

> Slack battery — Keeps Slack data inside your organization and asks before publishing it.
>
> GitHub battery — Assumes every repository is public and prevents private data from leaking to GitHub.

If the current config changes a battery's default behavior, describe the
resulting behavior in plain English. Do not explain the rule ordering unless
the user asks.

Check what each matched battery expects the root config to provide. Record
anything missing that the battery or complete config needs in order to work.
A proposal may name a group only when `appa describe` lists it under
`Named audiences:`, or when the user establishes it during this flow with a
configured audience source. A registered source does not prove that an
arbitrary plausible group name exists.

### Cover the remaining tools

Create root rules only for installed tools that neither the root config nor a
matched battery covers.

- Who may see a value is the built-in audience chain `self` ⊆ `internal` ⊆
  `public`: `self` is the person running the session, `internal` their
  organization. A tool that reads the requester's own data — credentials,
  personal mail, private messages — labels it `self`:
  `delta = { audience = ["self"] }`. A tool that reads organization-wide data
  labels it `internal`: `delta = { audience = ["internal"] }`. Both work
  without an audience source; a source is needed only when a rule names an
  individual recipient that must be checked against them. Never substitute
  an invented reader such as `"private"`, `@company`, or `@employees`, and
  never use a group `appa describe` does not list.
- An annotator's answer may name literal readers only, never `self` or
  `internal`. A tool whose result is the requester's or the organization's
  data therefore gets a static rule, not an annotator.
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
with `mcp__claude_ai_Gmail__`; do not assume a fixed connector tool list. Mail
the requester reads is `self` data; a rule that names a recipient
(`contains = ["$to"]`) can be checked against `self` or `internal` only through
a configured audience source. An email domain is never a source: `internal`
is fed by a directory (`google-workspace:full-members`, `slack:full-members`,
`github:org/<org>/members`), and Gmail by itself feeds nothing. Without a
source, say that recipient-checked sends are refused as unanswerable and
leave them so; do not invent a group. This boundary answer is separate from
approval to write or install anything.

### Propose, then apply

Group the proposal by server. Show:

- batteries to add, each with its one-sentence explanation;
- existing behavior that stays unchanged, but only when it affects the result;
- how the remaining installed tools will behave;
- installed tools the proposal leaves undeclared: annotated call by call by a
  wildcard tool rule (`name = "*"`) when the config has one, refused otherwise;
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
   batteries, audience sources, or named audiences changed since the
   proposal, revise the proposal and ask for approval again.
2. Copy each approved battery directory beside the root config under
   `batteries/<name>/` and add its `appa.toml` to the root `include` list. Use
   the installed marketplace clone so supporting scripts stay on the same
   APPA version. Leave an existing copied battery unchanged unless the user
   asked to refresh it.
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

1. Run `appa describe --config <live-path>`. Record the config state, batteries,
   policy tools, audience sources, and named audiences. Keep session tools
   and accounts unavailable when the command says they are unavailable.
2. Read the root config and only the included files relevant to the requested
   changes.
3. For policy syntax or behavior that the current config does not demonstrate,
   first consult the relevant section of
   `<marketplace-root>/website/content/docs/contracts.md`.
   If it is unavailable or does not answer the question, stop and report an
   incomplete installation. Do not guess syntax, fetch another version, search
   for an OpenAPPA checkout, or inspect source code.
4. Explain three things: what happens now, what you propose, and the practical
   effect. Add one short **OpenAPPA pieces** line naming every primitive used.
   Ask only for a decision that changes the result.
5. If a battery would help, propose it with the same one-sentence rule used in
   `init` mode. Existing root rules still take priority.
6. End with: **Approve, or tell me what to change.** Wait for the reply.
7. Run `appa describe --config <live-path>` again. If the config, batteries,
   audience sources, or named audiences changed since the proposal, revise
   the proposal and ask for approval again.
8. Copy each newly approved battery directory from the installed marketplace
   beside the root config under `batteries/<name>/`, add its `appa.toml` to the
   root `include` list, and add any root support it requires. Leave an existing
   copied battery unchanged unless the user asked to refresh it.
9. Apply only the approved root changes. To change battery behavior, add or
   edit a root declaration; never modify the battery.
10. Reload and report the result as described below.

For several root rules with the same tool name, order matters. Put a narrow
argument-specific rule before its general fallback. Do not reorder unrelated
rules.

For Bash changes, distinguish exact command patterns from contextual
classifications:

- Use ordered root `Bash(command:...)` contracts for exact patterns.
- Use the `hint` on the root `claude-code.bash-requirements` Annotator when the
  behavior requires interpreting a command.

If that Annotator exists only in the Claude Code battery, copy its complete
declaration into the root before modifying the hint. Do not add a broad root
`Bash` contract because it would run before the battery's credential-path
refusals. Keep `builtin`, `inputs`, and mandate bounds unchanged unless the user
approves changing them. A hint can select only values that the mandate admits.
Add any Authority or other support those values need, and include it in the
proposal.

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
