---
name: appa-guide
description: Guide a user through configuring OpenAPPA for Claude Code. Use for initial tool sync, MCP changes, or policy adjustments.
argument-hint: "init|adjust"
---

OpenAPPA configuration helper. Request: $ARGUMENTS

## Modes

Choose one mode. Ask the user if the request does not specify one:

- **`init`**: Scan installed tools and set up a starting configuration.
- **`adjust`**: Modify an existing configuration based on user needs.

If the user selects `adjust` without specifying details, ask what they want OpenAPPA to do differently.

## Core Rules

1. **Root config is source of truth**: Root tool rules take precedence over battery rules, and the first matching rule applies. Never alter or remove root rules without explicit user approval.
2. **Batteries are immutable defaults**: Never edit battery files directly. Override a tool contract using a root rule. Override an annotator by copying its complete declaration into the root configuration under the same name; preserve its implementation, inputs, and mandate unless the approved behavior requires changing them.
3. **Approval before changes**: Present the full proposed behavior in plain English. Wait for explicit user approval before modifying files or reloading the runtime. Request re-approval if a subsequent correction changes the proposed behavior.
4. **Minimal diffs**: Keep unrelated entries, comments, reader names, bindings, includes, and batteries intact.
5. **Focus on outcomes, not mechanics**: Explain privacy guarantees, blocked actions, and approval workflows plainly. Only display TOML when explicitly requested.
6. **"OpenAPPA pieces" line**: Every proposal must include a single line identifying the primitives used (e.g., `OpenAPPA pieces: tool contract, authority`).
7. **Scoped reads only**: Read only `appa describe` output, the live root config (with relevant includes), matched battery files (`appa.toml`, `README.md`), and `<marketplace-root>/website/content/docs/contracts.md`. Never search source code, Git history, or arbitrary repository files. If these sources do not clarify the syntax or behavior, report an incomplete installation rather than guessing or referencing external versions.

## Locate Runtime and Configuration

1. Read `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/plugins/known_marketplaces.json`. Find `installLocation` for the `appa` marketplace and assign it to `<marketplace-root>`. If the entry or directory is missing, report an incomplete installation.
2. Run `appa describe`. Extract the full path from the `Config:` line (including any spaces) and assign it to `<live-path>`.
3. Check for an active runtime:

   ```sh
   ps ax -o command | grep '[a]ppa runtime'
   ```

   - If a running process explicitly sets a different `--config`, ask the user which deployment to configure.
   - If no runtime is running, proceed with `<live-path>`.
   - The runtime URL defaults to `${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

## Workflow: `init`

### 1. Inspect Current State

1. Run `appa describe --config <live-path>`. Record:
   - Configuration state
   - Effective policy tools
   - Included batteries
   - Authority implementations and permits
   - Audience sources and named audiences
   *(Note: Treat session integrations, tools, or accounts reported as unavailable as unknown rather than empty.)*
2. Read the root config, preserving its comments and existing rules. If it conflicts with `appa describe`, stop and report the mismatch.
3. Gather MCP servers using `claude mcp list` along with tools active in the session. Preserve exact full tool names and descriptions:
   - `mcp__<server>__<tool>`
   - `mcp__plugin_<plugin>_<server>__<tool>`
4. Cross-check both sources. Note any configured server whose tools cannot be inspected, but do not invent tool definitions. Exclude APPA's internal control tool (`*execute_remedy_plan`).
5. Do not inspect private mail, messages, files, or other sensitive data to infer an account identity. Ask the user if identity affects the proposed behavior.

### 2. Match Batteries

- Match batteries under `<marketplace-root>/batteries/` by tool names listed in each `appa.toml` (not by directory name).
- Read only a matched battery's `appa.toml` and `README.md`. Do not execute its scripts during inspection.
- If the batteries directory is missing, report an incomplete installation. Never mix OpenAPPA versions with batteries from another version.
- Summarize each proposed battery in a single sentence under 20 words covering its scope, protection, and key assumptions.
- Verify root dependencies required by each battery. Only name a group if `appa describe` lists it as a named audience or the proposal configures an audience source for it.

### 3. Cover Unmatched Tools

Create root rules only for installed tools that are not covered by root or a matched battery.

- **Audience hierarchy**: The built-in chain is `self` ⊆ `internal` ⊆ `public`.
- **Private reads**: Use `delta = { audience = ["self"] }` for tools reading the requester's private data.
- **Internal reads**: Use `delta = { audience = ["internal"] }` for tools reading organization-wide data.
- **Audience sources**: Static contracts can reference `self` and `internal` without an audience source. However, checking a literal recipient against either audience requires an explicit audience source.
- **Annotators**: Annotator outputs can only specify literal readers; they cannot return `self` or `internal`. Use a static contract when tool output belongs to a built-in audience.
- **Sinks**: Use `requires = { audience = { contains = ["public"] } }` for any publishing, posting, sending, sharing, or uploading tool.
- **Public reads / No data**: Use `delta = {}` for public reads or tools returning no data.
- **Completeness**: Every tool entry must define `delta`. Never fabricate reader names, groups, or audiences.

For public-audience requirements, reuse an appropriate `builtin hitl` authority and extend its audience permit instead of adding attention solely for routing reviews. Preserve hard denials when requested by the user, declared in a root rule/comment, or when a mark is intentionally unserved. If multiple authorities can review a disclosure and the choice determines who reviews it, prompt the user.

### 4. Propose and Apply

The proposal must include:

- The proposed starting policy (without comparing it to "current settings").
- Batteries to add (each summarized in one sentence).
- Proposed privacy, public-sink, and review behavior for unmatched tools.
- Any configured servers that could not be inspected.
- Undeclared tools (clarifying whether the wildcard rule annotates them or the runtime refuses them).
- A line specifying: `OpenAPPA pieces: <primitives>`.
- A `Needed for this to work` section listing missing authorities, audience sources, or other prerequisites.
- The closing call to action: **Approve, or tell me what to change.**

After user approval:

1. Run `appa describe --config <live-path>` again. Compare state, batteries, authorities, audience sources, and named audiences against the proposal. If relevant changes occurred, revise the proposal and request re-approval.
2. Copy each approved battery's full directory from the installed marketplace to `batteries/<name>/` adjacent to the root config. Add its `appa.toml` to the root `include`. Leave existing copied batteries untouched unless an update was approved.
3. Add approved authority permits, audience sources, and tool contracts to the root config.
4. Reload and verify.

## Workflow: `adjust`

Focus on the requested outcome rather than running a full tool rescan. If the goal is unclear, ask one targeted question.

1. Run `appa describe --config <live-path>`. Record config state, batteries, effective tools, authorities, audience sources, and named audiences (treating unavailable session details as unknown).
2. Read the root config and only the included sections relevant to the change. If root conflicts with `appa describe`, stop and report the mismatch.
3. Refer to `<marketplace-root>/website/content/docs/contracts.md` if the existing configuration lacks required syntax or behavior patterns.
4. If an existing battery satisfies the requirement, propose it using the `init` battery rules.
5. Present the change as: Current behavior → Proposed behavior → Practical effect → `OpenAPPA pieces: ...`.
6. Conclude with: **Approve, or tell me what to change.**
7. After approval, rerun `appa describe --config <live-path>`. If the environment changed, revise the proposal and request re-approval.
8. Copy newly approved batteries following the `init` steps. Apply only approved root changes; never modify imported batteries directly.
9. Reload and verify.

### Bash Adjustments

- For exact command patterns, insert a narrow, ordered `Bash(command:...)` root contract before its fallback. Do not reorder unrelated rules.
- For semantic command interpretation, copy the full `claude-code.bash-requirements` annotator declaration into the root configuration and modify its `hint`. Preserve its `builtin`, `inputs`, and mandate unless user approval specifies otherwise.
- Avoid broad root `Bash` contracts that could bypass the battery's credential-path protections.

When making an audience mismatch reviewable, grant the authority the required audience permit. Do not add attention purely to route the review. Retain existing attention requirements when they represent an independent per-call review.

## Reload and Verification

Reload only after an approved write:

```sh
curl --fail-with-body -sS -X POST "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

- **On reload failure**: Explain the error, fix syntax or wiring issues, and request re-approval if the fix changes approved behavior. Then reload and retest. (A failed reload keeps the previous configuration active.)
- **On reload success**:
  - Summarize the effective policy in 1–3 plain-English sentences (what is private, permitted, reviewable, or blocked).
  - Note any critical remaining limitations.
  - If configuration changes were made, remind the user:
    > Start a new `clappa` session to use the updated policy; this session keeps the policy it started with.
