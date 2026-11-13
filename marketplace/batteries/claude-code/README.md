# Claude Code battery

This battery applies the `coding` battery's rules to Claude Code's built-in
tools and covers the tools only Claude Code offers. `appa plugin install
claude-code` includes both batteries on a first install; `appa battery remove
claude-code` takes this one out, and a later plugin install does not bring it
back. A deployment that includes this battery without `coding` gates none of the
tools its `coding` map binds: a map for a namespace no included battery
declares binds nothing.

Its `[aliases.tool.coding]` map binds each canonical tool to the Claude Code
tool that serves it. Claude Code spells every argument the rules read as the
canonical tool does:

| Canonical tool | Claude Code tool | Result |
| --- | --- | --- |
| `tool/coding/shell` | `host/claude-code/Bash` | confined |
| `tool/coding/powershell` | `host/claude-code/PowerShell` | confined |
| `tool/coding/read_file` | `host/claude-code/Read` | |
| `tool/coding/search_files` | `host/claude-code/Grep` | |
| `tool/coding/write_file` | `host/claude-code/Write` | |
| `tool/coding/edit_file` | `host/claude-code/Edit` | |
| `tool/coding/fetch_url` | `host/claude-code/WebFetch` | |
| `tool/coding/web_search` | `host/claude-code/WebSearch` | |

The `coding` battery's README describes those rules.
PowerShell is the tool Claude Code offers on Windows beside Bash.

## File tracking

Its `[aliases.tool.file-tracker]` map binds the `file-tracker` battery's
canonical tools to the same Claude Code tools:

| Canonical tool | Claude Code tool |
| --- | --- |
| `tool/file-tracker/shell` | `host/claude-code/Bash` |
| `tool/file-tracker/read` | `host/claude-code/Read` |
| `tool/file-tracker/write` | `host/claude-code/Write` |
| `tool/file-tracker/edit` | `host/claude-code/Edit` |
| `tool/file-tracker/unmediated` | `host/claude-code/PowerShell`, `Monitor`, `Grep`, `NotebookEdit` |

`appa plugin install claude-code` includes `file-tracker` before `coding`.
The first matching rule decides a call, so the file-tracker's rules decide
these tools, and `clappa` confines each `Bash` call to the files and hosts it
declares. The `file-tracker` battery's README describes those rules.

`appa battery remove file-tracker` turns file tracking off. The map then binds
nothing, and the `coding` rules decide the tools above. `clappa` starts Claude
Code's own `Bash` unconfined, with `PowerShell`, `Monitor`, `Grep` and
`NotebookEdit` available. `appa battery install file-tracker` turns it on
again.

`host/claude-code/Monitor` runs a shell command or opens a WebSocket, and its
output reaches the model as later notifications. The
`claude-code.monitor-requirements` Annotator labels each call before it runs.
The default config `appa plugin install claude-code` writes adds the wildcard
fallback for tools no rule names.

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
```

This small root includes every rule the two batteries declare. A tool no rule
names is refused; the installed default root adds a wildcard fallback for it.

Root rules take precedence over both batteries. Add a root rule when a
particular Bash command or Read path needs stricter, looser, or fully blocked
behavior. A root Bash rule also precedes the `coding` battery's credential
rules, so a command it matches is not narrowed for a credential path it names.

The map's `confined = true` confines Bash and PowerShell results, so a
sanitizer can run on the command's output; a root needs no deployment setting
for it. Once a root binds the `coding` battery's `coding.strip-secrets`, as the
installed default root does, a command that names a credential path offers it:
the model replaces every secret value in the output, which returns at
`internal` instead of narrowing the session to `self`. A sanitizer the root
declares can run there too.

The map confines no WebFetch result. A root that binds
`coding.strip-instructions` confines WebFetch itself with
`confined_results = ["host/claude-code/WebFetch"]` under `[policy.deployment]`;
the installed default root already does. WebSearch stays unconfined: Claude
Code's replacement of its result is not verified live.
`appa battery remove claude-code` takes the map, and with it the confinement,
out.

## Customize Bash classification

How the model Annotator classifies a command — trust by who wrote the text it
returns, audience by its visible destination — is the runtime's and the same
for every model Annotator. A `hint` adds what only the deployment knows. To
give the Bash Annotator one, replace it in the root config instead of
modifying the `coding` battery:

```toml
[[policy.annotator]]
name = "claude-code.bash-requirements"
uses = "claude-code"
hint = "Hosts under corp.example are the organization's own: what they return is internal. Require human-approval attention before commands that publish releases or change production infrastructure."
```

A root Annotator replaces a battery's of the same name. Preserve `uses`
unless you intend to alter the implementation; write `audiences` only to narrow
the mandate below the policy's vocabulary. The `coding` battery continues to provide
ordered credential-path narrowing rules.

## Example override

If needed, the battery can be overridden from the root config. For example,
these root rules require fresh human approval for every `kubectl` command:

```toml
[[policy.tool]]
name = "host/claude-code/Bash(command:kubectl)"
requires = { attention = ["human-approval"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "host/claude-code/Bash(command:kubectl *)"
requires = { attention = ["human-approval"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.authority]]
name = "operator"
hint = "Ask the person running this Claude Code session."

[policy.authority.permits]
attention = ["human-approval"]

[externals.authorities.operator]
uses = "hitl"
```

The first rule matches bare `kubectl`; the second matches `kubectl` followed by
arguments. Other Bash commands continue to use the battery's model classifier.
