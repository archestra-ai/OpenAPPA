---
title: Coding agents
category: Deep Dive
order: 4
description: Tool-flow enforcement, file labels, SafeBash, shell rules, and checked delegation for coding agents.
---

OpenAPPA checks coding-agent flows at the harness boundary. The [Claude Code](/claude-code) integration checks built-in and MCP tool calls and results. It adds three default boundaries: protected sessions, checked subagent returns, and contracts for native file and shell tools.

## Protected sessions

The Claude Code install protects only sessions started through `clappa`. Start `clappa` for a protected session and `claude` for an ordinary session. Protection is fixed when the process starts, so the agent cannot turn it off during that session.

Protected sessions fail closed: while the runtime is unavailable, every hooked action is blocked. The status line shows the trajectory's current trust and audience. The [Claude Code guide](/claude-code) covers installation, policy setup, and a first blocked flow.

## Subagent return checks

When a main agent delegates through Claude Code's `Agent` tool, OpenAPPA first checks that the subagent definition does not declare `maxTurns`. Claude Code bypasses return hooks when that setting is present. OpenAPPA therefore refuses the prompt if project, user, or plugin frontmatter declares it. The scan covers the first 64 KiB. Unclosed or unclassifiable frontmatter fails closed.

OpenAPPA then holds the spawn until the parent declares a return contract. The contract can accept the final message unchanged, bound it to a declared Label, or require a sanitizer. The agent declares the contract itself, right before the `Agent` call, with `execute_remedy_plan` and no `offer_id`. OpenAPPA asks a person only when the spawn has another gap that an authority must approve. OpenAPPA checks the child's tool calls normally, then validates its final message before the parent can receive it. A refused return keeps the child running until it produces an admissible message.

This lets a subagent read a private ticket or credential-adjacent log and return only a result the parent trajectory may hold. See [Subagent reads](/how-it-works#subagent-reads) and [Subagent Returns](/contracts#subagent-returns).

## Shell and native file tools

Every Claude Code install includes the [Claude Code tools battery](/battery-claude-code). It routes `Read`, `Write`, and `Edit` through the [file-tracker](#files-that-keep-their-labels), confines every shell command to the files and hosts it declares ([SafeBash](#safebash)), and blocks `PowerShell`, `Monitor`, `Grep`, and `NotebookEdit`.

The `coding` battery gives the canonical shell and file tools of other harnesses these contracts:

- **Reading credentials narrows the trajectory.** Read, search, or shell calls that contain configured credential-path spellings narrow the audience to `self`.
- **Writing where another process looks requires trust.** Writes to credential and control paths require a `trusted` trajectory. After suspicious input, the exact call requires human review.
- **Changing the controls requires review.** Writes to harness settings or OpenAPPA policy always ask the person running the session.
- **Shell commands are classified per call.** [Annotators](/how-it-works#annotators) determine the required trust and audience for each shell command. The GitHub battery can supply repository visibility and authorship context for `git push` and `gh` commands.

These contracts label what a command reads or returns; they do not sandbox the command. For confinement, see [SafeBash](#safebash).

## Files that keep their labels

A coding agent writes files that outlive its trajectory. The file-tracker plugin keeps a label on every file a mediated tool call writes, so a later read of that file carries the restriction the write carried. The read can happen in the same session, in a fresh session, or after a resume.

The plugin is a [recorder](/contracts#recorders) and an [annotator](/contracts#annotators) for the same tools:

- As a recorder, it receives the label each released call published: the trajectory's label at release, combined with the call's `forwards` and `delta`. It stores that label on each file the call wrote.
- As an annotator, it answers a `Read` with the stored label as `delta`. An `Edit` forwards the file's existing label, so the edited file keeps it. A `Write` replaces the file, so the new label replaces the old one.

The plugin's built-in path rules give credential paths such as `.env*` and `~/.ssh/*` the audience `self` before any call touches them. They also set the trust a write to a control file such as `CLAUDE.md` requires. A deployment adds rules in `~/.config/appa/file-tracker.toml`. The plugin never denies a call: the engine checks the annotation's `requires`.

### SafeBash

A shell command can read and write any file the process can reach, so a label on a file does not cover a native shell. Every shell command therefore declares three lists beside its `command`: `input_files`, `output_files`, and `network_hosts`. The command can read only its input files and system directories, write only its output files, and connect only to its hosts. Every other file in the home directory, the working directory, or a temporary directory is hidden from it, and a write to an undeclared path does not reach the disk. Each output file is emptied before the command runs.

APPA builds no sandbox of its own. It confines each command in the best sandbox the harness has:

| Where | Tool | Sandbox |
|---|---|---|
| Claude Code started by `clappa`, on a tested release | Claude Code's own `Bash`, with the three lists added | Claude Code's built-in sandbox (bubblewrap on Linux, Seatbelt on macOS) and its proxy, configured for each call |
| Claude Code otherwise | `mcp/file-tracker/SafeBash`, served by the file-tracker | Anthropic's sandbox runtime `srt`, the same runtime, started for each call |

Before each session, `clappa` loads a preload into Claude Code and runs its self-test. The self-test checks that the lists survive Claude Code's input handling, that an undeclared file is hidden and an undeclared write does not reach the disk, and that each command reaches only its own hosts. If the release is not on the tested list or any check fails, `clappa` disables `Bash`, registers SafeBash, and prints why. A `Bash` call that declares no lists, such as one from Claude Code started without `clappa`, is refused. Both tools return the exit code, standard output and standard error as a successful result, so the call's label applies whatever the exit code.

The annotator forwards the combined label of the input files, and the call's result carries it. The recorder applies the published label to every output file.

The label of a copied secret therefore survives across sessions. If the agent reads `.env` and writes its content into `/tmp/notes.txt`, that file now carries `self`. A later shell command that reads `/tmp/notes.txt` narrows its session to `self`, in this session or in any other, so a later public sink such as `WebFetch` is refused.

The hosts a call declares do not change its label. The sandbox confines the command to them, but APPA does not check what a host may receive.

### Per harness

| Harness | File tools | Shell |
|---|---|---|
| Claude Code | `Read`, `Write`, `Edit`, `Bash`, and `mcp/file-tracker/SafeBash` use the file-tracker annotator and recorder. | `clappa` confines `Bash` per call in Claude Code's sandbox, or serves SafeBash where it cannot. It starts Claude Code without `PowerShell`, `Monitor`, `Grep`, and `NotebookEdit`, and the battery blocks them. |
| OpenCode | `read`, `write`, `edit`, and `appa_safebash` use the file-tracker annotator and recorder. | AppenCode removes native `shell` and `patch`. `appa_safebash` runs in a sandbox extension with a proxy. |
| Amp in an orb | The orb's file system has the trajectory's lifetime, so it counts as part of the trajectory. No file-tracker runs. | A proxy in the orb posts each outbound connection as `host/amp/appa_network`. Sending a file to another thread is an outbound flow that `requires` checks. |

The `appa` executable serves the file-tracker as `appa file-tracker`. Claude Code's default policy binds it, and the Claude Code battery routes the tools. The SafeBash fallback needs `srt` on `PATH`.

### Limits

- A shell command can send its input files to any host it declares. Network destinations carry no policy yet.
- A file that changes outside a mediated call keeps its recorded label. A process outside the sandbox can still copy a file without its label.
- A read waits while a write of the same file has not yet reached the recorder. If the write does not settle within the wait, the read fails and the agent can propose it again.
- The stored audience must be one clause of audiences the policy declares. A file that two unrelated restricted audiences wrote fails its next annotation.
- Model-provider requests and assistant messages are not mediated.
