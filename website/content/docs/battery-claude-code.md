---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: Rules and annotators for Claude Code's Read, Grep, Write, Edit and Bash tools, from the coding battery's canonical tools.
sidebar: false
breadcrumb: Claude Code tools
---

Two batteries cover Claude Code's built-in `Read`, `Grep`, `Write`, `Edit` and `Bash` tools, which the policy names `host/claude-code/Read`, `host/claude-code/Grep`, `host/claude-code/Write`, `host/claude-code/Edit` and `host/claude-code/Bash`. The `coding` battery writes the rules once for [canonical tools](/contracts#canonical-tools) such as `tool/coding/shell` and `tool/coding/read_file`. The `claude-code` battery maps each canonical tool to the Claude Code tool that serves it, and covers the tools only Claude Code offers. A config includes both; the `claude-code` battery fails to load without `coding`. Batteries can also cover tools that do not come from an MCP server.

[View the coding battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/coding) and [the claude-code battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/claude-code).

## Covered tools

| Tool | Contract |
|---|---|
| `host/claude-code/Read` | Static rules narrow the session to `self`, the requester, when a hidden path, a credential file, a private key, or a system secret location is read. |
| `host/claude-code/Grep` | A search inside one of the same paths is a read of it and narrows the session to `self`. A search over a directory that holds such a file is not matched; only the path as written is. |
| `host/claude-code/Write`, `host/claude-code/Edit` | Writing into one of the same paths requires a `trusted` session: content that arrived at `suspicious` reaches a file the next process trusts only when the person running the session approves the exact call. Writing the harness's own settings (`.claude/settings*`) or the deployment's policy (`appa/appa.toml`, `appa/batteries/`) asks that person every time, and so does writing `.mcp.json` or a `.claude/hooks/` file. Writing an instruction file such as `CLAUDE.md`, or a file under `.claude/skills/`, `.claude/agents/`, or `.claude/commands/`, requires a `trusted` session. Every other path takes the session's label as it is. |
| `host/claude-code/Bash` | A command naming a credential path narrows the session to `self`. Before every other command runs, the Claude Code model decides the trust and fresh attention it requires and labels its output for trust and audience. A command that prints or writes a credential counts as a credential path: the Databricks CLI's `auth token`, `auth env`, `secrets get-secret`, and `configure`, and its profile file `.databrickscfg`. A credential file name matches only as a path: after a space, `/` or `=`, at the end or before a space, or double-quoted. A dotenv file also matches with a mode suffix such as `.env.local` or `.env.production`. A selector promises no full recall; a spelling it misses, such as a name inside a string literal or a template such as `.env.example`, is classified like any other command. |

The default config `appa plugin install claude-code` writes handles tools that the batteries do not name.

The model for the `coding` battery's sanitizers is configured in the deployment, not in the battery. The default config binds both to the Claude Code model (`builtin = "claude-code"` under `[externals.sanitizers]`), because a Claude Code session has the `claude` CLI. A root can bind them to `builtin = "llm"` instead.

- `coding.strip-secrets` runs on a Bash or PowerShell command that names a credential path. The `claude-code` battery confines those results, so the output reaches the agent only after the sanitizer runs. The model replaces every secret value, and the output returns at `internal` instead of narrowing the session to `self`.
- `coding.strip-instructions` runs on a fetch that lowers the session's trust, where the root confines WebFetch's result, as the default config does with `confined_results = ["host/claude-code/WebFetch"]`. The model removes every instruction from the page, and the result keeps the session's trust.

APPA does not verify what a sanitizer's model removed: binding a sanitizer registers it as trusted.

## Files

```text
coding/
|-- README.md
|-- appa-package.toml
`-- appa.toml
claude-code/
|-- README.md
|-- appa-package.toml
`-- appa.toml
```

The `host/claude-code/Read` rules match a path as written, absolute or relative, so a hidden name and its relative spelling are both covered.

The Bash annotator describes the data a command needs and returns. Neither tool isolates a command from your computer.

The Bash Annotator's `hint` is its policy-specific prompt. A root `[[policy.annotator]]` declaration with the same name replaces the battery default. This lets the deployment define how to classify its shell commands without editing the battery. Facts about the whole deployment, such as which hosts are the organization's own, belong in the root's `[policy.deployment] hint`, which every annotator reads.

Use a sandbox to protect credentials and network access.
