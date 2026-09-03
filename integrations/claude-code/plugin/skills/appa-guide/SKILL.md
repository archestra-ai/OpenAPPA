---
name: appa-guide
description: Guide a user through configuring OpenAPPA for Claude Code. Use for initial tool sync, MCP changes, or adjusting policies.
argument-hint: "init|adjust"
---

OpenAPPA configuration helper. Request: $ARGUMENTS

## Modes

Choose one mode (ask user if not specified in request):
- **`init`**: Scan installed tools and set up a starting configuration.
- **`adjust`**: Modify an existing configuration based on user needs.

---

## Core Rules

1. **Root config is truth**: Root tool rules precede battery rules (first match wins). Never alter or remove root rules without explicit approval.
2. **Batteries are immutable defaults**: Never edit battery files directly. Override tool contracts with root rules, or override annotators by redeclaring them identically in root.
3. **Approval before changes**: Always present proposed behavior in plain English and wait for user approval before modifying files or reloading runtime.
4. **Minimal diffs**: Keep existing entries, comments, reader names, and bindings intact.
5. **Focus on outcomes, not mechanics**: Explain privacy, blocked actions, and approvals simply. Only show TOML when requested.
6. **"OpenAPPA pieces" line**: Every proposal must include a single line naming the primitives used (e.g. `OpenAPPA pieces: tool contract, hitl authority`).
7. **Scoped reads only**: Read only `appa describe` output, the live root config/includes, matched battery files (`appa.toml`, `README.md`), and `<marketplace-root>/website/content/docs/contracts.md`. Never search source code, git history, or arbitrary repo files.

---

## Locating Runtime & Config

1. **Marketplace Root**: Read `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/plugins/known_marketplaces.json` to find `installLocation` for the `appa` marketplace (`<marketplace-root>`). If missing, report an incomplete installation.
2. **Config Path**: Run `appa describe` and extract the path from the `Config:` line.
3. **Active Runtime Check**:
   ```sh
   ps ax -o command | grep '[a]ppa runtime'
   ```
   If a running process explicitly specifies a different `--config`, ask the user which to use. Runtime URL defaults to `${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

---

## Workflow: `init` (Initial Sync)

### 1. Inventory & Inspection
- Run `appa describe --config <live-path>` to inspect current config state, batteries, authorities, and audience sources.
- Read root config preserving comments and existing rules.
- Gather MCP tools via `claude mcp list` and tools active in session (`mcp__<server>__<tool>` and `mcp__plugin_<plugin>_<server>__<tool>`). Note any server whose tools cannot be inspected. Exclude APPA's own control tool (`*execute_remedy_plan`), which is handled internally by the runtime.

### 2. Match Batteries
- Check `<marketplace-root>/batteries/` matching batteries by tool names inside their `appa.toml` (not directory name).
- Summarize each battery in one concise sentence (<20 words), describing coverage, protection, and assumptions.

### 3. Policy Rules for Uncovered Tools
For tools not covered by root or a battery:
- **`self`**: User's private data (`delta = { audience = ["self"] }`).
- **`internal`**: Org-wide data (`delta = { audience = ["internal"] }`).
- **`public` sink**: Publishing/sharing (`requires = { audience = { contains = ["public"] } }`).
- **Neutral/public read**: `delta = {}`.
- Every tool entry must define `delta`. Use literal reader names; do not invent groups or audience labels.
- If a tool requires `public` and human review is appropriate, extend an existing `builtin hitl` authority permit rather than adding attention marks or inventing hard denials.

### 4. Proposal & Execution
- **Proposal structure**:
  - Treat this as a fresh setup: the starting configuration comes from default config, so present the proposed starting policy directly. **Do not frame this as a comparison against "current settings"** (the user has not configured any settings yet).
  - Batteries to add (one sentence each).
  - Proposed behavior for uncovered tools (data privacy, public sinks, human review).
  - Undetected/uninspected servers noted clearly without altering their rules.
  - Line: `OpenAPPA pieces: <primitives>`
  - Section `Needed for this to work` if authorities/prerequisites are missing.
  - Conclude: **Approve, or tell me what to change.**
- **Apply on approval**:
  1. Re-run `appa describe --config <live-path>` to ensure state has not drifted.
  2. Copy approved batteries to `batteries/<name>/` beside root config and add to root `include`.
  3. Add required authority permits and tool contracts to root config.
  4. Reload runtime.

---

## Workflow: `adjust` (Modify Config)

1. Run `appa describe --config <live-path>` and read only relevant root/included config sections.
2. If syntax is unclear, check `<marketplace-root>/website/content/docs/contracts.md`.
3. Distinguish Bash patterns:
   - Specific exact commands: Add ordered `Bash(command:...)` root contracts before fallbacks.
   - Command semantic interpretation: Override `claude-code.bash-requirements` annotator in root config and adjust `hint` (keeping `builtin`, `inputs`, and mandate intact).
4. Present proposal: Current behavior → Proposed change → Practical effect → `OpenAPPA pieces: ...`.
5. Conclude: **Approve, or tell me what to change.**
6. Apply changes to root config only (never edit imported batteries) and reload runtime.

---

## Reload & Verification

Trigger reload after saving config:
```sh
curl --fail-with-body -sS -X POST "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

- If reload fails, explain the error plainly, correct the file, and re-test.
- If successful, provide a 1–3 sentence plain-English summary of what is now private, allowed, or blocked.
- Remind user:
  > Start a new `clappa` session to use the updated policy; this session keeps the policy it started with.

