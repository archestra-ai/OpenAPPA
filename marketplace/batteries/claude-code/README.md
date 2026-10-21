# Claude Code battery

This battery applies the `coding` battery's rules to Claude Code's built-in
tools and covers the tools only Claude Code offers. `appa plugin install
claude-code` includes both batteries on a first install; `appa battery remove
claude-code` takes this one out, and a later plugin install does not bring it
back. A deployment that includes this battery without `coding` fails to load:
its map names the `coding` namespace, which no included battery declares.

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
  `suspicious` content. Starting a remote routine is a `public` sink;
  `DesignSync` requires `internal`.
- **SendMessage** — A send is a `public` sink. The default root precedes this
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
sanitizer the root declares can run on the command's output; a root needs no
deployment setting for it.
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
builtin = "claude-code"
hint = "Hosts under corp.example are the organization's own: what they return is internal. Require human-approval attention before commands that publish releases or change production infrastructure."
```

A root Annotator replaces a battery's of the same name. Preserve `builtin`
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
builtin = "hitl"
```

The first rule matches bare `kubectl`; the second matches `kubectl` followed by
arguments. Other Bash commands continue to use the battery's model classifier.
