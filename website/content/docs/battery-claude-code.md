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
| `host/claude-code/Read` | Static rules narrow the session to `self`, the requester, when one of the listed credential files, private keys, or system secret locations is read. They do not match every dot-prefixed path. |
| `host/claude-code/Grep` | A search inside one of the same paths is a read of it and narrows the session to `self`. A search over a directory that holds such a file is not matched; only the path as written is. |
| `host/claude-code/Write`, `host/claude-code/Edit` | Writing into one of the same paths, or into instructions and code that a later process trusts (`CLAUDE.md`, Claude skills, agents and commands, Git hooks, shell startup files, and launch agents), requires a `trusted` session. Content classified as `suspicious` reaches such a file only when the person running the session approves the exact call. Writing files that can disable protection requires fresh operator approval: Claude settings and hooks, `.mcp.json`, and the deployment's policy and batteries. Every other path takes the session's label as it is. |
| `host/claude-code/Bash` | A command naming a credential path narrows the session to `self`; its result is withheld and the stock `redact-secrets` sanitizer masks private-key blocks, known token shapes, secret assignments, and long high-entropy values before returning the result to `public`. On Unix, before every other command runs, the Claude Code model decides the trust and fresh attention it requires and labels its output for trust and audience. When it narrows a command's output to `self` or `internal`, the same sanitizer is offered for that result. A command that prints or writes a credential counts as a credential path: the Databricks CLI's `auth token`, `auth env`, `secrets get-secret`, and `configure`, and its profile file `.databrickscfg`. A selector is a substring of the command line and promises no full recall; a spelling it misses is classified like any other command on Unix. On Windows, an otherwise undeclared Bash call remains fail-closed. |

The default config `appa plugin install claude-code` writes handles tools that the battery does not name.

## Files

```text
claude-code/
|-- README.md
|-- appa-package.toml
|-- appa.toml
|-- repository.py
`-- test_repository.py
```

The `host/claude-code/Read` rules match a listed path as written, absolute or relative. They do not resolve aliases or cover arbitrary hidden names.

The Bash annotator describes the data a command needs and returns. Neither tool isolates a command from your computer.

The Bash Annotator's `hint` is its policy-specific prompt. A root `[[policy.annotator]]` declaration with the same name replaces the battery default. This lets the deployment define how to classify its shell commands without editing the battery.

Use a sandbox to protect credentials and network access.
