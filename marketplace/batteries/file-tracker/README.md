# File-tracker battery

This battery routes a coding agent's shell and file tools through the
file-tracker. It writes its rules for canonical tools under
`tool/file-tracker/`, and a client battery's `[aliases.tool.file-tracker]`
map binds each to the client's own tool. The `claude-code` battery's map binds
them to `Bash`, `Read`, `Write`, `Edit`, `PowerShell`, `Monitor`, `Grep` and
`NotebookEdit`. The battery's own map binds `shell` to
`mcp/file-tracker/SafeBash`.

`appa plugin install claude-code` includes this battery on a first install.
`appa battery remove file-tracker` turns file tracking off, and
`appa battery install file-tracker` turns it on again.

## Rule order

Include this battery before `coding` and the client's battery. The first
matching rule decides a call, and this battery's bare `shell` rule matches
every shell call, so the `coding` battery's shell rules never decide one while
this battery is included. `appa battery install file-tracker` puts the include
before the first battery that follows it in the plugin's `batteries` list.

## Files

`read`, `write` and `edit` name the `file-tracker` as their Annotator and
recorder. The file-tracker keeps a label on every file a call writes, and a
later read of that file, in this session or another, carries that label. Its
built-in path rules give credential paths such as `.env*` and `~/.ssh/*` the
audience `self`, and require a `trusted` session to write instructions, hooks
and shell profiles. Writes to Claude Code settings, `.mcp.json` and the
OpenAPPA policy ask the person running the session. Add rules in
`~/.config/appa/file-tracker.toml`.

## Shell commands

A shell command declares its `input_files`, `output_files` and
`network_hosts`, and a sandbox confines it to exactly those. Under Claude Code,
`clappa` adds the three lists to Claude Code's own `Bash` and confines each
call in Claude Code's sandbox. Where its self-test fails, it serves SafeBash in
Anthropic's sandbox runtime `srt` instead. A shell call that declares no lists
is refused.

A call whose hosts are all one package registry's (npm, Yarn, the Go module
proxy, crates.io, PyPI) runs as a call without hosts. Any other call that
declares a host requires public data, and its result is `suspicious`. A root
rule such as `host/claude-code/Bash(network_hosts:*.corp.example)` can give a
known host its own readers.

`unmediated` requires the `blocked` mark. Its tools run commands, or read or
write file content, out of the file-tracker's sight.

## Add it to a deployment

```toml
include = [
  "batteries/file-tracker/appa.toml",
  "batteries/coding/appa.toml",
  "batteries/claude-code/appa.toml",
]

[policy]
version = 2

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.annotators.file-tracker]
command = ["appa", "file-tracker", "annotate"]

[externals.recorders.file-tracker]
command = ["appa", "file-tracker", "record"]
```

The battery declares the `file-tracker` Annotator, and the root binds it.
`appa` names the runtime's own executable. A root whose include list leaves
this battery out keeps the two bindings idle.

The [coding-agents guide](../../../website/content/docs/coding-agents.md#files-that-keep-their-labels)
describes the labels and the sandbox.
