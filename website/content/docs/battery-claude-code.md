---
title: Claude Code tools battery
category: Batteries
order: 6.62
description: File labels for Claude Code's Read, Write and Edit, shell commands confined to what they declare, and rules for the tools only Claude Code offers.
sidebar: false
breadcrumb: Claude Code tools
---

The `claude-code` battery covers Claude Code's built-in tools. It maps the `coding` battery's canonical tools to Claude Code's `Bash`, `PowerShell`, `Read`, `Grep`, `Write`, `Edit`, `WebFetch` and `WebSearch`, and it maps the `file-tracker` battery's canonical tools to `Bash`, `Read`, `Write`, `Edit`, `PowerShell`, `Monitor`, `Grep` and `NotebookEdit`. It gives the tools only Claude Code offers static rules.

Every Claude Code install includes the `file-tracker` battery first, then `coding`, then `claude-code`. The first matching rule decides a call, so the `file-tracker` rules below decide the shell and file tools: `Read`, `Write`, and `Edit` go through the [file-tracker](/coding-agents#files-that-keep-their-labels), and every shell command is confined to the files and hosts it declares ([SafeBash](/coding-agents#safebash)).

To turn file tracking off, run `appa battery remove file-tracker`. The `coding` battery's rules then decide those tools: an Annotator classifies each shell command without a sandbox, `PowerShell`, `Monitor`, `Grep` and `NotebookEdit` are available, and files keep no label between sessions. To turn it on again, run `appa battery install file-tracker`.

View the source of the [file-tracker](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/file-tracker), [coding](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/coding) and [claude-code](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/claude-code) batteries.

## Covered tools

| Tool | Contract |
|---|---|
| `host/claude-code/Read` | The file-tracker returns the file's label. A file that a mediated call wrote carries the label that call published, in this session or another. A credential path such as `.env*`, `~/.ssh/*`, or a private key carries the audience `self` before any call writes it. |
| `host/claude-code/Write`, `host/claude-code/Edit` | The file-tracker records the label the call published on the file. Writing a credential path, an instruction file such as `CLAUDE.md`, a hook, or a shell profile requires a `trusted` session. Writing Claude Code settings, `.mcp.json`, or the deployment's policy asks the person running the session. |
| `host/claude-code/Bash`, `mcp/file-tracker/SafeBash` | The command declares its `input_files`, `output_files`, and `network_hosts`, and a sandbox confines it to exactly those. `clappa` adds the lists to `Bash` and confines each call in Claude Code's sandbox; where its self-test fails, it serves SafeBash in `srt` instead. A `Bash` call that declares no lists is refused. The call forwards its input files' label, and its result carries it. A call whose hosts are all one package registry's (npm, Yarn, the Go module proxy, crates.io, PyPI) runs as a call without hosts; any other call that declares a host requires public data, and its result is `suspicious`. Its output files take the label the call published. |
| `host/claude-code/PowerShell`, `Monitor`, `Grep`, `NotebookEdit` | Blocked. They read or write file content out of the file-tracker's sight. `clappa` starts Claude Code with them disabled. |
| `host/claude-code/WebFetch`, `host/claude-code/WebSearch` | The `coding` battery's web rules. Listed documentation sites keep the session's label, a search returns `suspicious` content, and an Annotator labels a fetch of any other URL per call. Where the root confines WebFetch's result, as the installed default root does with `confined_results = ["host/claude-code/WebFetch"]`, a fetch that lowers the session's trust also offers `coding.strip-instructions`: the model removes every instruction from the page, and the result keeps the session's trust. |

The default config `appa plugin install claude-code` writes binds the file-tracker to `appa file-tracker`, which the runtime executable serves. It also handles tools that the batteries do not name. The SafeBash fallback needs `srt` on `PATH`.

The model for the `coding` battery's sanitizers is configured in the deployment, not in the battery. The default config binds `coding.strip-instructions` to the Claude Code model (`builtin = "claude-code"` under `[externals.sanitizers]`), because a Claude Code session has the `claude` CLI. A root can bind it to `builtin = "llm"` instead. It also binds `coding.strip-secrets`, which a `coding` rule offers on a shell command that names a credential path. While the `file-tracker` battery is included, no `coding` rule decides a shell call, so it is not offered. APPA does not verify what a sanitizer's model removed: binding a sanitizer registers it as trusted.

## Files

```text
file-tracker/
|-- README.md
|-- appa-package.toml
`-- appa.toml
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
