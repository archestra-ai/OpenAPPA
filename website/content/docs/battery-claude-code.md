---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: Rules and annotators for Claude Code Read and Bash tools.
sidebar: false
breadcrumb: Claude Code tools
---

This battery covers Claude Code's built-in `Read` and `Bash` tools. Batteries can also cover tools that do not come from an MCP server.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/claude-code).

## Covered tools

| Tool | Contract |
|---|---|
| `Read` | Static rules narrow the session to `self`, the requester, when a hidden path, a credential file, a private key, or a system secret location is read. |
| `Bash` | A command naming a credential path requires the `token-exposed` mark and is refused before secrets reach the model. Before every other command runs, the Claude Code model decides the trust and fresh attention it requires and labels its output for trust. Its mandate names no reader, so who may see a command's output is the session's label, not the model's choice. |

The default config created by `appa init claude-code` handles tools that the battery does not name.

## Files

```text
claude-code/
|-- appa.toml
`-- README.md
```

The `Read` rules match a path as written, absolute or relative, so a hidden name and its relative spelling are both covered.

The Bash annotator describes the data a command needs and returns. Neither tool isolates a command from your computer.

The Bash Annotator's `hint` is its policy-specific prompt. A root `[[policy.annotator]]` declaration with the same name replaces the battery default. This lets the deployment define how to classify its shell commands without editing the battery.

Use a sandbox to protect credentials and network access.
