---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: Rules and annotators for Claude Code Read and Bash tools.
sidebar: false
---

This battery covers Claude Code's built-in `Read` and `Bash` tools. Batteries can also cover tools that do not come from an MCP server.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/claude-code).

## Covered tools

| Tool | Contract |
|---|---|
| `Read` | A local Python annotator marks sensitive paths private and other paths public. |
| `Bash` | Before a command runs, the Claude Code model records what data it needs and what its output can contain. |

The default config created by `appa init claude-code` handles tools that the battery does not name.

## Files

```text
claude-code/
|-- appa.toml
|-- README.md
|-- read-sensitivity.py
`-- test_read_sensitivity.py
```

The path annotator checks hidden paths, credential files, private keys, system-secret locations, and sensitive symbolic-link targets.

The Bash annotator describes the data a command needs and returns. It does not isolate the command from your computer.

Use a sandbox to protect credentials and network access.
