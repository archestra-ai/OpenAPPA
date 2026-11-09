---
title: Coding agents
category: Deep Dive
order: 4
description: Tool-flow enforcement, shell rules, and checked delegation for coding agents.
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

Every Claude Code install includes the [Claude Code tools battery](/battery-claude-code), which gives native tools these contracts:

- **Reading credentials narrows the trajectory.** Read, Grep, Bash, or PowerShell calls that contain configured credential-path spellings narrow the audience to `self`.
- **Writing where another process looks requires trust.** Writes to credential and control paths require a `trusted` trajectory. After suspicious input, the exact call requires human review.
- **Changing the controls requires review.** Writes to Claude Code settings or OpenAPPA policy always ask the person running the session.
- **Shell commands are classified per call.** [Annotators](/how-it-works#annotators) determine the required trust and audience for Bash, PowerShell, and Monitor calls. The GitHub battery can supply repository visibility and authorship context for `git push` and `gh` commands.

These contracts label what a command reads or returns; they do not sandbox the command. Use an operating-system sandbox for confinement.

### Per harness

| Harness | File tools | Shell |
|---|---|---|
| Claude Code | Static rules label `Read`, `Grep`, `Write`, and `Edit` calls by the path they name. | A model Annotator classifies each `Bash`, `PowerShell`, and `Monitor` call. |
| OpenCode | Static rules cover `read`, `write`, and `edit`. A read under `*/private/*` narrows the session to `internal`. | The shell permits only `pwd`. |
| Amp in an orb | The orb's file system has the trajectory's lifetime, so it counts as part of the trajectory. | A proxy in the orb posts each outbound connection as `host/amp/appa_network`. Sending a file to another thread is an outbound flow that `requires` checks. |

### Limits

- A model Annotator judges the visible text of a shell command. It can misjudge a command, so a flow it classifies has no deterministic guarantee.
- Credential rules match the spellings in a command line. A command that reaches a credential through another spelling depends on the Annotator.
- A Claude Code shell command can read and write any file its process can reach, and connect to any host.
- A `Grep` over a directory that holds a credential file does not narrow the trajectory. Only the path as written is matched.
- A failed shell command adds no label. A command that prints restricted data and exits non-zero leaves the trajectory's label unchanged.
- Model-provider requests and assistant messages are not mediated.
