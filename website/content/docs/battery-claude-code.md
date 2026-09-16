---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: Rules and annotators for Claude Code's Read, Grep, Write, Edit and Bash tools.
sidebar: false
breadcrumb: Claude Code tools
---

This battery covers Claude Code's built-in `Read`, `Grep`, `Write`, `Edit` and `Bash` tools, which the policy names `host/claude-code/Read`, `host/claude-code/Grep`, `host/claude-code/Write`, `host/claude-code/Edit` and `host/claude-code/Bash`. Batteries can also cover tools that do not come from an MCP server.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/claude-code).

## Covered tools

| Tool | Contract |
|---|---|
| `host/claude-code/Read` | Static rules narrow the session to `self`, the requester, when a hidden path, a credential file, a private key, or a system secret location is read. |
| `host/claude-code/Grep` | A search inside one of the same paths is a read of it and narrows the session to `self`. A search over a directory that holds such a file is not matched; only the path as written is. |
| `host/claude-code/Write`, `host/claude-code/Edit` | Writing into one of the same paths requires a `trusted` session: content that arrived at `suspicious` reaches a file the next process trusts only when the person running the session approves the exact call. Writing the harness's own settings (`.claude/settings*`) or the deployment's policy (`appa/appa.toml`, `appa/batteries/`) asks that person every time. Every other path takes the session's label as it is. |
| `host/claude-code/Bash` | A command naming a credential path narrows the session to `self`; its result is withheld and the stock `redact-secrets` sanitizer masks every token before the output reaches the model, returning the value to `public`. Before every other command runs, the Claude Code model decides the trust and fresh attention it requires and labels its output for trust and audience. When it narrows a command's output to `self` or `internal`, the same sanitizer is offered for that result. A command that prints or writes a credential counts as a credential path: the Databricks CLI's `auth token`, `auth env`, `secrets get-secret`, and `configure`, and its profile file `.databrickscfg`. A selector is a substring of the command line and promises no full recall; a spelling it misses is classified like any other command. |

The default config `appa plugin install claude-code` writes handles tools that the battery does not name.

## Files

```text
claude-code/
|-- appa.toml
`-- README.md
```

The `host/claude-code/Read` rules match a path as written, absolute or relative, so a hidden name and its relative spelling are both covered.

The Bash annotator describes the data a command needs and returns. Neither tool isolates a command from your computer.

The Bash Annotator's `hint` is its policy-specific prompt. A root `[[policy.annotator]]` declaration with the same name replaces the battery default. This lets the deployment define how to classify its shell commands without editing the battery.

Use a sandbox to protect credentials and network access.
