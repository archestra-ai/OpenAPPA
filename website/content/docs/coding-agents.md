---
title: Coding agents
category: Deep Dive
order: 4
description: Tool-flow enforcement, tracked files, isolated processing, shell rules, and checked delegation for coding agents.
---

OpenAPPA checks coding-agent flows at the harness boundary. The exact coverage depends on the harness.

| Harness | Current coverage |
|---|---|
| [Claude Code](/claude-code) | Checks built-in and MCP tool calls and results. Its default battery covers native file and shell tools. It checks subagent returns. Experimental runtime-owned file tools can preserve Labels across file operations. |
| [Amp](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/amp) | The source-distributed `amppa` v1 plugin checks tool calls and results only. It does not track provenance across child threads, file transfers, or model prose. |

Amp users must apply policy to each hooked tool and account for those limits. Claude Code adds three default boundaries: protected sessions, checked subagent returns, and contracts for native file and shell tools. Its runtime-owned file tools are a separate experimental mode.

## Protected sessions

The Claude Code install protects only sessions started through `clappa`. Start `clappa` for a protected session and `claude` for an ordinary session. Protection is fixed when the process starts, so the agent cannot turn it off during that session.

Protected sessions fail closed: while the runtime is unavailable, every hooked action is blocked. The status line shows the trajectory's current trust and audience. The [Claude Code guide](/claude-code) covers installation, policy setup, and a first blocked flow.

## Subagent return checks

When a main agent delegates through Claude Code's `Agent` tool, OpenAPPA first checks that the subagent definition does not declare `maxTurns`. Claude Code bypasses return hooks when that setting is present. OpenAPPA therefore refuses the prompt if project, user, or plugin frontmatter declares it. The scan covers the first 64 KiB. Unclosed or unclassifiable frontmatter fails closed.

OpenAPPA then holds the spawn until the parent declares a return contract. The contract can accept the final message unchanged, bound it to a declared Label, or require a sanitizer. OpenAPPA checks the child's tool calls normally, then validates its final message before the parent can receive it. A refused return keeps the child running until it produces an admissible message.

This lets a subagent read a private ticket or credential-adjacent log and return only a result the parent trajectory may hold. See [Subagent reads](/how-it-works#subagent-reads) and [Subagent Returns](/contracts#subagent-returns).

## Shell and native file tools

Every Claude Code install includes the [Claude Code tools battery](/battery-claude-code), which gives native tools these contracts:

- **Reading credentials narrows the trajectory.** Read, Grep, Bash, or PowerShell calls that contain configured credential-path spellings narrow the audience to `self`.
- **Writing where another process looks requires trust.** Writes to credential and control paths require a `trusted` trajectory. After suspicious input, the exact call requires human review.
- **Changing the controls requires review.** Writes to Claude Code settings or OpenAPPA policy always ask the person running the session.
- **Shell commands are classified per call.** [Annotators](/how-it-works#annotators) determine the required trust and audience for Bash, PowerShell, and Monitor calls. The GitHub battery can supply repository visibility and authorship context for `git push` and `gh` commands. The `redact-secrets` sanitizer can mask credential tokens before output reaches the model.

These contracts label what a command reads or returns; they do not sandbox the command. Use [isolated file processing](#isolated-file-processing) or an operating-system sandbox for confinement.

## File taint tracking

> **Experimental.** File taint tracking is off by default and is not yet a supported security boundary. See [Limits](#limits) before you rely on it.

A tracked file version has a [Label](/how-it-works#the-core-concepts): its trust and audience. On write, the new version inherits the trajectory's current Label combined with the tool's `delta`.

Without file tracking, content that the agent writes to disk loses its Label. A later native read does not recover provenance from the earlier write, although its tool contract can still apply a path-based Label. Runtime-owned file tools keep the Label with each recorded version. In this example, reading under `secrets/` narrows the trajectory to `self`. Writing under `public/` requires data that anyone may read. [Turn it on](#turn-it-on) has the complete policy:

```toml
[[policy.tool]]
name = "mcp/appa/appa_read_file(file_path:secrets/*)"
delta = { audience = ["self"] }

[[policy.tool]]
name = "mcp/appa/appa_write_file(file_path:public/*)"
requires = { audience = { contains = ["public"] } }

[[policy.tool]]
name = "mcp/appa/appa_copy_file(destination_path:public/*)"
requires = { audience = { contains = ["public"] } }
```

1. The subagent reads `secrets/deploy-notes.txt`. It executes the offered narrowing remedy, then retries with its trajectory narrowed to `self`.
2. The subagent writes `summary.md`. The new file version carries the `self` Label.
3. The main agent tries to copy `summary.md` into `public/`. OpenAPPA refuses the copy because `public/*` requires `public` data.
4. If the main agent reads `summary.md`, its trajectory also narrows to `self` before the content enters model context.

### Why hooks alone are not enough

For most tools, OpenAPPA checks the call when the harness proposes it and labels the result when the harness reports it. A native file tool breaks that model, because the value that flows is the file's content, and the hook never sees it:

- A native Read returns bytes nothing has labeled. By the time the result hook executes, the payload has entered model context.
- A native Write replaces content, but nothing in the call says which version of the file the model read.
- Claude Code validates some calls before the hook runs. A native Edit can report a failed match, which reveals file content, before OpenAPPA sees the call.
- A copy or move must carry the source's Label to the destination, even though the model never sees the bytes.

To track content propagation, the runtime supplies replacement MCP file tools: `appa_read_file`, `appa_write_file`, `appa_edit_file`, `appa_copy_file`, and `appa_move_file`. A sixth tool, `appa_process_files`, appears when a sandbox backend is configured. The model proposes paths and content. It never supplies a Label, file version, or trajectory identity.

The harness still registers its native tools. Complete mediation therefore requires the runtime to refuse native filesystem and shell calls after the first file call binds the workspace.

### Two Labels per call

A file call has two Label outcomes. The **trajectory Label** is the Label after the result enters model context. The **published file Label** is the Label recorded for new file content.

| Operation | The trajectory Label combines | The published file Label combines |
|---|---|---|
| Read | trajectory, file | — |
| Write, new file | trajectory, tool `delta` | trajectory, tool `delta` |
| Write, replacing a file | trajectory, previous version, tool `delta` | trajectory, tool `delta` |
| Edit | trajectory, previous version, tool `delta` | trajectory, previous version, tool `delta` |
| Copy or Move | trajectory, tool `delta` | trajectory, source, tool `delta` |
| Process | trajectory, tool `delta` | trajectory, every input, tool `delta` |

Label combination takes the minimum trust rank and intersects the audiences. The result is never less restrictive than any input.

Three rows need attention:

- **Write uses trajectory provenance.** The new content carries everything already in model context, through the trajectory Label. It does not add the replaced file as a content dependency. The old version stays in history.
- **Edit also uses file provenance.** The patch derives from the previous version, so the runtime records that version as a dependency and combines its Label.
- **Copy and Move keep the payload out of the trajectory.** They return a fixed confirmation, so the source Label does not enter model context. The destination still gets the source's Label, and `requires` is checked against it. A copy of a `self` file into a destination that only accepts `public` data is refused, even though the model never saw the bytes.

An error message gets the same Label a success would have had, so an error cannot reveal more than a success.

### One event stream per workspace

The runtime records file transitions in an append-only event stream for each canonical workspace. A root trajectory binds permanently to its working directory on its first file call. Its subagents inherit that binding. Separate roots bound to the same workspace share its versions, Labels, receipts, and one active reservation.

A path receives the configured initial Label only when a call first touches it and no history exists. The runtime records a content digest to verify the bytes. The digest does not classify them. Events retain version metadata, not historical file content.

Workspace events and root bindings use the configured SQLite, memory, or PostgreSQL event-log backend. Durable backends preserve them across runtime restarts. A restart does not clear an active quarantine.

The runtime requires exclusive ownership of a dedicated workspace. No other process may edit it. The workspace cannot contain symlinks or hard links. Keep policy, database, credentials, plugins, and sandbox backends outside it.

### Durability and repair

Only one file operation can hold the workspace reservation. The runtime records the proposed operation and its output Label before changing the filesystem. It then changes the file, records the new version, and admits the result.

The filesystem and event log cannot commit in one atomic transaction. If an outcome is missing after the writer stops, the runtime tries to repair the event stream:

- If the workspace still matches the pinned input, it releases the reservation.
- If the workspace matches an output that the recorded request could produce, it records that version under the request's bound Label. It does not claim that the call succeeded.
- If neither state matches, the workspace stays quarantined. Every later file call into that workspace is refused.

If a runtime crash prevents automatic repair, stop every workspace writer and run:

```sh
appa files repair --workspace /path/to/workspace
```

Repair never chooses a new Label. To assign the configured initial Label to selected current files, resolve any pending operation and run an explicit relabel:

```sh
appa files relabel --workspace /path/to/workspace path/to/file
```

### Turn it on

The `[file_tracking]` table enables the feature. Its settings define the initial Label for a path that has no recorded history. This complete policy enables file tracking with the `secrets/` and `public/` rules from the example:

The policy must explicitly declare all six file tools; undeclared tools are refused. Tool-input sanitizers and rewrite routes are disallowed because they can change paths after pinning. Output-only sanitizers remain available, but `confined_results` cannot name a runtime-owned file tool.

```toml
[policy]
version = 2

[file_tracking]
initial_trust = "suspicious"
initial_audience = "public"

# The first matching rule wins, so selected paths come first.
[[policy.tool]]
name = "mcp/appa/appa_read_file(file_path:secrets/*)"
delta = { audience = ["self"] }

[[policy.tool]]
name = "mcp/appa/appa_write_file(file_path:public/*)"
requires = { audience = { contains = ["public"] } }

[[policy.tool]]
name = "mcp/appa/appa_copy_file(destination_path:public/*)"
requires = { audience = { contains = ["public"] } }

# Declare every file tool. An empty delta leaves Labels unchanged.
[[policy.tool]]
name = "mcp/appa/appa_read_file"
delta = {}

[[policy.tool]]
name = "mcp/appa/appa_write_file"
delta = {}

[[policy.tool]]
name = "mcp/appa/appa_edit_file"
delta = {}

[[policy.tool]]
name = "mcp/appa/appa_copy_file"
delta = {}

[[policy.tool]]
name = "mcp/appa/appa_move_file"
delta = {}

[[policy.tool]]
name = "mcp/appa/appa_process_files"
delta = {}

# Subagent spawns run only when the policy declares them.
[[policy.tool]]
name = "host/claude-code/Agent"
delta = {}

[policy.deployment]
context_control = true

[externals]
timeout_ms = 5000
max_body_bytes = 65536
```

If the default Claude Code install already uses port 8787, run this experimental runtime on another port. `clappa` selects the installed default runtime, so use the explicit environment variables below to select this one:

```sh
appa runtime --config /host/file-policy.toml --db /host/runtime.db --listen 127.0.0.1:8788
cd /path/to/workspace
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude
```

The runtime warns that complete file mediation requires native tools and implicit reads to be disabled. The installed Claude Code plugin cannot enforce that condition. Native tools remain visible. After the first file call binds the workspace, the runtime refuses native filesystem and shell calls. Other policy-approved tools, including MCP tools, continue through ordinary admission.

The [file mediation design note](https://github.com/archestra-ai/OpenAPPA/blob/main/appa-runtime/FILE-MEDIATION.md) specifies the event stream, reservation lifecycle, repair rules, and test coverage.

### Limits

File taint tracking covers runtime-owned file calls and nothing outside them:

- **Claude Code is not confined.** Native tools stay installed. Native filesystem and shell calls are refused after binding, but Claude Code can perform native validation before a hook. It also reads project instructions and memory without a tool call.
- **Inference is not checked.** Requests to the model provider and final responses remain outside this boundary.
- **Only declared processing is isolated.** Before binding, a shell `cp` or `mv` does not carry a Label. After binding, native shell calls are refused.
- **The filesystem and event log are not one transaction.** Automatic or manual repair handles only states attributable to the recorded request. Incompatible states remain quarantined.
- **Relabelling is an operator action.** File operations and repair never lower a Label. Tool-input sanitizers and rewrites remain unsupported.
- **Outside writers break the model.** The runtime assumes exclusive ownership of the workspace.

## Isolated file processing

Some work needs a program rather than a file tool: a script that totals invoices, a formatter, or a build step. `appa_process_files(input_paths, output_path, command)` runs one shell command on snapshots of the declared input files. The command writes one output file.

The runtime does not mount the workspace. The command sees read-only copies of the inputs and a private output directory. A sandbox backend built from [agentsh](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/agentsh) runs it with Linux namespaces, Landlock, and seccomp. The command has no network, an empty environment, and resource limits. If the sandbox cannot start, the command does not run.

The output file, stdout, stderr, and failures receive the combined Label of every declared input. OpenAPPA cannot see which inputs the program used, so all declared inputs contribute.

Enable it by adding `--file-process-backend /host/backend` to the runtime command. It requires `[file_tracking]`.
