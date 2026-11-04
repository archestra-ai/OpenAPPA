---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: File labels for Claude Code's Read, Write and Edit, shell commands confined by SafeBash, and rules for the tools only Claude Code offers.
sidebar: false
breadcrumb: Claude Code tools
---

The `claude-code` battery covers Claude Code's built-in tools. It routes `Read`, `Write`, and `Edit` through the [file-tracker](/coding-agents#files-that-keep-their-labels), runs every shell command as [SafeBash](/coding-agents#safebash), and gives the tools only Claude Code offers static rules. It maps the `coding` battery's canonical `fetch_url` and `web_search` tools to `WebFetch` and `WebSearch`, so a config includes both batteries; the `claude-code` battery fails to load without `coding`.

[View the coding battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/coding) and [the claude-code battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/claude-code).

## Covered tools

| Tool | Contract |
|---|---|
| `host/claude-code/Read` | The file-tracker returns the file's label. A file that a mediated call wrote carries the label that call published, in this session or another. A credential path such as `.env*`, `~/.ssh/*`, or a private key carries the audience `self` before any call writes it. |
| `host/claude-code/Write`, `host/claude-code/Edit` | The file-tracker records the label the call published on the file. Writing a credential path, an instruction file such as `CLAUDE.md`, a hook, or a shell profile requires a `trusted` session. Writing Claude Code settings, `.mcp.json`, or the deployment's policy asks the person running the session. |
| `mcp/file-tracker/SafeBash` | The command declares its `input_files`, `output_files`, and `network_hosts`, and the sandbox runtime `srt` confines it to exactly those. The call forwards its input files' label. A call that reaches the network lowers trust to the lowest rank and requires `public`. Its output files take the label the call published. |
| `host/claude-code/Bash`, `PowerShell`, `Monitor`, `Grep`, `NotebookEdit` | Blocked. They read or write file content out of the file-tracker's sight. `clappa` starts Claude Code with them disabled. |
| `host/claude-code/WebFetch`, `host/claude-code/WebSearch` | The `coding` battery's web rules. Listed documentation sites keep the session's label, a search returns `suspicious` content, and an Annotator labels a fetch of any other URL per call. |

The default config `appa plugin install claude-code` writes binds the file-tracker to `appa file-tracker`, which the runtime executable serves. It also handles tools that the batteries do not name. SafeBash needs `srt` on `PATH`.

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

The file-tracker's path rules are built into `appa`. Add rules for your own paths in `~/.config/appa/file-tracker.toml`; they combine with the built-in rules.
