---
title: Coding agents
category: Deep Dive
order: 4
description: File taint tracking, isolated file processing, shell and credential rules, subagent return checks, and protected sessions for coding agents.
---

Coding agents move data through the filesystem, the shell, and subagents, not only through API tools. OpenAPPA tracks Labels through file reads, writes, copies, and moves, confines declared file processing, and checks shell commands and subagent returns. Claude Code is the first harness that uses these features.

| Feature | What it does | Status |
|---|---|---|
| [File taint tracking](#file-taint-tracking) | Gives every file version its own Label and carries it through Read, Write, Edit, Copy, and Move. | Experimental, opt-in |
| [Isolated file processing](#isolated-file-processing) | Runs a command on declared input files in a sandbox and labels its output with every input's Label. | Experimental, opt-in |
| [Shell and native file tools](#shell-and-native-file-tools) | Narrows the session when the agent touches credentials, and asks before it edits the harness settings or the policy. | Default in the Claude Code install |
| [Subagent return checks](#subagent-return-checks) | Checks what a subagent's final message can carry back to the main agent. | Default in the Claude Code install |
| [Protected sessions](#protected-sessions) | Protects the sessions you start with `clappa`, and fails closed when the runtime is down. | Default in the Claude Code install |

## File taint tracking

> **Experimental.** File taint tracking is off by default and is not yet a supported security boundary. See [Limits](#limits) before you rely on it.

Taint tracking follows where data came from as it moves through a program. In OpenAPPA, the taint is the file's [Label](/how-it-works#the-core-concepts): its trust and its audience. With file tracking on, every file in the workspace has a Label. When the agent writes a file, the file gets the session's current Label: the Label of everything the agent had read when it wrote the file.

This closes a gap. Without file tracking, content the agent writes to disk loses its Label, and a later read of that file starts clean. With file tracking, the Label stays with the file. Take a policy where reading under `secrets/` narrows the session to `self`, and writing under `public/` requires data that anyone may read. This excerpt shows only the rules that matter here; [Turn it on](#turn-it-on) has the complete policy:

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

1. The main agent starts a subagent to tidy the deploy notes and declares that the subagent may narrow to `self`. The subagent reads `secrets/deploy-notes.txt`. OpenAPPA asks it to accept the narrowing, and its session narrows to `self`.
2. The subagent writes a summary to `summary.md`. The runtime records the new version of `summary.md` with the `self` Label.
3. The main agent never opened the secrets file, so its session is still unrestricted. It can still write its own content into `public/`.
4. The main agent tries to copy `summary.md` into `public/` without reading it. OpenAPPA blocks the copy: the copied content carries the `self` Label.
5. The main agent reads `summary.md`. It must accept the narrowing to `self` first, and after that every write into `public/` is blocked.

### Why hooks alone are not enough

For most tools, OpenAPPA checks the call when the harness proposes it and labels the result when the harness reports it. A native file tool breaks that model, because the value that flows is the file's content, and the hook never sees it:

- A native Read returns bytes nothing has labeled. When the result hook fires, the bytes are already in the model's context.
- A native Write replaces content, but nothing in the call says which version of the file the model read.
- Claude Code validates some calls before the hook runs. A native Edit can report a failed match, which reveals file content, before OpenAPPA sees the call.
- A copy or move must carry the source's Label to the destination, even though the model never sees the bytes.

So the runtime runs the file operations itself. The session gets six MCP tools from the runtime's `appa` server: `appa_read_file`, `appa_write_file`, `appa_edit_file`, `appa_copy_file`, `appa_move_file`, and, with a sandbox backend configured, `appa_process_files`. The model proposes paths and content. It never supplies a Label, a file version, or a session identity.

### How one call is checked

Every file call follows the same steps:

1. **Pin.** The runtime reserves the workspace and records the current state of each path it touches: version, content hash, and Label, or "absent".
2. **Check.** OpenAPPA checks the call against the policy, using the pinned Labels. The same call over different file content is a different flow.
3. **Run.** The runtime runs the operation on the pinned path. A write goes to a staged file beside the target, which then replaces the target in one atomic step. An edit searches for its match string only after the check passes.
4. **Publish.** The runtime hashes the result and records a new version with its Label and the versions it was derived from. Only then does the result reach the model.

The reservation lets one file operation run at a time in a root session. If an operation does not finish cleanly, for example because the file changed under the runtime, the session fails closed: the runtime keeps the reservation and refuses every later file call in that session until it restarts. It does not guess a Label for bytes it cannot account for.

### Two Labels per call

A file call produces two Labels. The **session Label** is the session's Label after the call's result enters the model's context. The **file Label** is what the runtime records for the new file content.

| Operation | The session Label combines | The new file Label combines |
|---|---|---|
| Read | session, file | — |
| Write, new file | session, tool `delta` | session, tool `delta` |
| Write, replacing a file | session, previous version, tool `delta` | session, tool `delta` |
| Edit | session, previous version, tool `delta` | session, previous version, tool `delta` |
| Copy or Move | session, tool `delta` | session, source, tool `delta` |
| Process | session, every input, tool `delta` | session, every input, tool `delta` |

To combine Labels, OpenAPPA takes the lowest trust and only the audience that all of them share. The result is never less restrictive than any part.

Three rows need attention:

- **Write does not inherit the file it replaces.** The new content does not derive from the old content, so the old Label does not stay with the file. The old version stays in the history.
- **Edit does inherit.** The new content derives from the previous version, so the runtime records that version as a dependency and keeps its Label.
- **Copy and Move keep the payload out of the session.** They return a fixed confirmation, so the session Label does not change. The destination still gets the source's Label, and `requires` is checked against it. A copy of a `self` file into a destination that only accepts `public` data is refused, even though the model never saw the bytes.

An error message gets the same Label a success would have had, so an error cannot reveal more than a success.

### Sessions and subagents share one ledger

The runtime keeps a ledger for each root session. The ledger holds the versions, Labels, and dependencies of that session's files. Subagents use the root's ledger, so a file one of them writes has the same Label for all of them. Another root session has its own ledger, and can use another workspace.

On the first file call, the runtime binds the session to its working directory. It hashes every existing file and gives it the Label that `[file_tracking]` configures. A hash only verifies bytes. It does not classify them.

The ledger lives in memory. A runtime restart discards it, and the next session labels the workspace from the configured starting Label again.

### Turn it on

The `[file_tracking]` table turns the feature on. Its two settings give the starting Label of the files in the workspace. This complete policy enables file tracking with the `secrets/` and `public/` rules from the example above:

```toml
[policy]
version = 2

[file_tracking]
initial_trust = "suspicious"
initial_audience = "public"

# The example's rules. The first matching rule wins,
# so these come before the bare rules below.
[[policy.tool]]
name = "mcp/appa/appa_read_file(file_path:secrets/*)"
delta = { audience = ["self"] }

[[policy.tool]]
name = "mcp/appa/appa_write_file(file_path:public/*)"
requires = { audience = { contains = ["public"] } }

[[policy.tool]]
name = "mcp/appa/appa_copy_file(destination_path:public/*)"
requires = { audience = { contains = ["public"] } }

# Every file tool must be named.
# An empty delta leaves the Labels unchanged.
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

Run this runtime on its own port, so the runtime the Claude Code install started on port 8787 is untouched. Then start a protected session from the workspace directory and point it at the new runtime:

```sh
appa runtime --config /host/file-policy.toml --db /host/runtime.db --listen 127.0.0.1:8788
cd /path/to/workspace
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude
```

At startup, the runtime warns that the file tools need native tools and implicit reads disabled. Through the plugin, Claude Code keeps its native tools, and the runtime refuses their calls at the hook. The warning points at the gap listed under [Limits](#limits).

Three rules shape a file-tracking policy:

- **Name all six file tools.** A tool the policy does not name is refused. Add `requires` or a non-empty `delta` to restrict file flows further.
- **Declare no sanitizers or rewrites.** A rewritten call would carry arguments the ledger never pinned, so the runtime refuses to start when the policy declares one. The Claude Code starting policy declares sanitizers, so it cannot be used here.
- **Expect a file-only session.** The runtime allows declared subagent spawns. It refuses every other tool call that reaches it, including the native Read, Write, Edit, and Bash tools and OpenAPPA's own management tools. Run management commands from the `appa` command line.

Use a dedicated directory, not your working checkout. The runtime refuses a workspace that contains a symlink or a hard link. Keep the policy, the database, and any sandbox backend outside the workspace.

The [file mediation design note](https://github.com/archestra-ai/OpenAPPA/blob/main/appa-runtime/FILE-MEDIATION.md) has the reservation lifecycle and the list of tests that cover each part.

### Limits

File taint tracking covers the file calls that go through the runtime's tools, and nothing outside them:

- **Claude Code itself.** Native tools stay installed. OpenAPPA refuses their calls at the hook, but Claude Code can validate a call before the hook runs. It also reads project instructions and memory without a tool call.
- **Inference.** Requests to the model provider and the final response are not checked.
- **The shell.** Native Bash is refused, not tracked. `cp` or `mv` in a shell does not carry a Label.
- **Writers outside the session.** The runtime assumes that no other process edits the workspace.
- **Lowering a Label.** No file operation makes a Label less restrictive, and sanitizers are not available in this mode.

## Isolated file processing

Some work needs a program, not a file tool: a script that adds up invoices, a formatter, a build step. `appa_process_files(input_paths, output_path, command)` runs one shell command on copies of the declared input files. The command writes one output file.

The runtime does not mount the workspace. The command sees read-only copies of the inputs and a private output directory. A sandbox backend built from [agentsh](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/agentsh) runs it with Linux namespaces, Landlock, and seccomp. The command has no network, an empty environment, and resource limits. If the sandbox cannot start, the command does not run.

The output file, stdout, stderr, and any failure all get the combined Label of every declared input. OpenAPPA cannot see which inputs a program actually read, so it counts all of them. A command that ignores an input still carries that input's Label.

Enable it by adding `--file-process-backend /host/backend` to the runtime command. It requires `[file_tracking]`.

## Shell and native file tools

Outside the file runtime, Claude Code uses its native tools. The [Claude Code tools battery](/battery-claude-code), which every Claude Code install includes, gives them contracts:

- **Reading secrets narrows the session.** A Read or Grep of a hidden path, a credential file, a private key, or a system secret location narrows the session to `self`, the person running it.
- **Writing where the next process looks needs trust.** Writing to those paths requires a `trusted` session. After the agent reads suspicious content, such a write needs the person's approval for that exact call.
- **Changing the guardrails asks every time.** A write to Claude Code's own settings (`.claude/settings*`) or to the OpenAPPA policy asks the person running the session, however trusted the session is.
- **Shell commands are classified per call.** A Bash command that names a credential path narrows the session to `self`. The stock `redact-secrets` sanitizer can mask tokens in its output before the model sees it. For every other command, an [annotator](/how-it-works#annotators) decides the trust the command requires and labels its output.

These rules match paths as the command spells them. They label what a command reads and returns; they do not sandbox it. For confinement, use [isolated file processing](#isolated-file-processing) or an OS sandbox.

## Subagent return checks

Coding agents delegate. When the main agent starts a subagent with Claude Code's `Agent` tool, OpenAPPA holds the spawn until the main agent declares what the subagent's final message may carry: the message as it is, a message up to a declared Label (the floor, the most restrictive Label the main agent will accept), or the message after a sanitizer. The subagent's own tool calls are checked like any other. When it stops, its final message is checked. A message that may not cross is refused with the reason, and the subagent keeps working until it returns one that can.

This lets a subagent read something sensitive, such as a private ticket or a credential-adjacent log, and return only a result the main session may hold. See [Subagent reads](/how-it-works#subagent-reads) and [Subagent Returns](/contracts#subagent-returns).

Claude Code ends a subagent that declares `maxTurns` without the return check. So while a subagent definition in the project, the user's agent directory, or an installed plugin declares `maxTurns`, OpenAPPA refuses the session's prompts.

## Protected sessions

The Claude Code install protects only the sessions you ask it to protect. Start `clappa` for a protected session and `claude` for a normal one. Protection is fixed when the session starts, so the agent cannot turn it off during the session.

A protected session fails closed. While the runtime is down, every hooked action is blocked. The status line shows the session's current trust and audience, so you can see when a read has narrowed it. The [Claude Code guide](/claude-code) covers install, policy setup, and a first blocked flow.
