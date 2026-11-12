# Claude Code battery

This battery applies the `coding` battery's rules to Claude Code's built-in
tools and covers the tools only Claude Code offers. `appa plugin install
claude-code` includes both batteries on a first install; `appa battery remove
claude-code` takes this one out, and a later plugin install does not bring it
back. A deployment that includes this battery without `coding` fails to load:
its map names the `coding` namespace, which no included battery declares.

Its `[aliases.tool.coding]` map binds the canonical web tools to the Claude
Code tools that serve them. Claude Code spells every argument the rules read
as the canonical tool does:

| Canonical tool | Claude Code tool |
| --- | --- |
| `tool/coding/fetch_url` | `host/claude-code/WebFetch` |
| `tool/coding/web_search` | `host/claude-code/WebSearch` |

The `coding` battery's README describes those rules.

## Files and shell commands

`Read`, `Write`, `Edit`, `Bash` and `mcp/file-tracker/SafeBash` name the
`file-tracker` as their Annotator and recorder. The file-tracker keeps a label
on every file a call writes, and a later read of that file, in this session or
another, carries that label. Its built-in path rules give credential paths
such as `.env*` and `~/.ssh/*` the audience `self`, and require a `trusted`
session to write instructions, hooks and shell profiles. Writes to Claude
Code settings, `.mcp.json` and the OpenAPPA policy ask the person running the
session. Add rules in `~/.config/appa/file-tracker.toml`.

A shell command declares its `input_files`, `output_files` and
`network_hosts`, and a sandbox confines it to exactly those. `clappa` adds
the three lists to Claude Code's own `Bash` and confines each call in Claude
Code's sandbox; where its self-test fails, it serves SafeBash in Anthropic's
sandbox runtime `srt` instead. A `Bash` call that declares no lists is
refused. A call whose hosts are all one package registry's (npm, Yarn, the Go
module proxy, crates.io, PyPI) runs as a call without hosts. Any other call that
declares a host requires public data, and its result is `suspicious`; a root rule such as `host/claude-code/Bash(network_hosts:*.corp.example)`
can give a known host its own readers. `PowerShell`, `Monitor`, `Grep` and `NotebookEdit` require the
`blocked` mark: they read or write file content out of the file-tracker's
sight.

The battery declares the `file-tracker` Annotator; the root binds it. The
default config `appa plugin install claude-code` writes binds it to
`appa file-tracker`, and `clappa` then starts Claude Code with the blocked
tools disabled and `Bash` confined, or SafeBash registered. A root that includes this battery
binds the file-tracker the same way; see the example below.

The [coding-agents guide](../../../website/content/docs/coding-agents.md#files-that-keep-their-labels)
describes the labels and the sandbox. The default config also adds the
wildcard fallback for tools no rule names.

## Other built-in tools

The other built-in tools get static rules:

- **Tools that stay inside the session** — `Glob`, `Skill`, `ToolSearch`,
  `TodoWrite`, the plan, worktree, task, cron and artifact tools, and the
  other session-local tools take the session's label as it is. The subagent
  tools `Agent`, `Task` and `Workflow` are the root's to declare: their rules
  rely on the deployment controlling a subagent's context.
- **Content from elsewhere** — `FetchInboxMessage`, `ReadMcpResourceTool`,
  `ReadNotifications`, and the run titles and logs of `RemoteTrigger` return
  `suspicious` content. Starting a remote routine and `DesignSync`
  require `internal`.
- **SendMessage** — A send is an `internal` sink. The default root precedes this
  rule with exceptions for protected sessions' socket and pipe addresses,
  which the runtime verifies.

## Add it to a deployment

```toml
include = ["batteries/coding/appa.toml", "batteries/claude-code/appa.toml"]

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

This small root includes every rule the two batteries declare and binds the
file-tracker. `appa` names the runtime's own executable. A tool no rule
names is refused; the installed default root adds a wildcard fallback for it.

Root rules take precedence over both batteries. Add a root rule when a
particular tool needs stricter, looser, or fully blocked behavior.

The map confines no result. A root that binds `coding.strip-instructions`
confines WebFetch itself with `confined_results = ["host/claude-code/WebFetch"]`
under `[policy.deployment]`; the installed default root already does.
WebSearch stays unconfined: Claude Code's replacement of its result is not
verified live.
