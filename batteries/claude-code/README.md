# Claude Code battery

Use this battery for Claude Code sessions that need policy-aware shell commands
and automatic privacy labels for local file reads.

It covers two built-in tools, and its wildcard covers every tool the policy
does not name:

- **Bash** — Before a command runs, the Claude Code model decides what trust,
  audience, and fresh attention the command requires. It also labels the
  command's output for trust and audience. This lets later tools distinguish,
  for example, public build output from private or suspicious command output.
- **Read** — Before a file is read, a local Python resolver checks its path.
  Hidden paths, credential files, private keys, system-secret locations, and
  sensitive symlink targets produce private content. Other paths produce public
  content. The resolver does not block the read or lower its trust.
- **Every other tool** — The wildcard rule sends a tool with no policy entry,
  such as an MCP server's tool, to the Claude Code model for a per-call
  contract before it runs. The annotator can hold an unknown tool's output to
  suspicious trust and a private audience, or clear one it recognizes. Name a tool exactly, or add a
  root rule, when it needs its own contract.

## Add it to a deployment

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 1
```

Root rules take precedence over the battery. Add a root rule when a particular
Bash command or Read path needs stricter, looser, or fully blocked behavior.

## Example override

If needed, the battery can be overridden from the root config. For example,
these root rules require fresh human approval for every `kubectl` command:

```toml
[[policy.tool]]
name = "Bash(command:kubectl)"
requires = { attention = ["hitl"] }
delta = { trust = "suspicious", audience = ["private"] }

[[policy.tool]]
name = "Bash(command:kubectl *)"
requires = { attention = ["hitl"] }
delta = { trust = "suspicious", audience = ["private"] }

[[policy.authority]]
name = "operator"
hint = "Ask the person running this Claude Code session."

[policy.authority.permits]
attention = ["hitl"]

[externals.authorities.operator]
builtin = "hitl"
```

The first rule matches bare `kubectl`; the second matches `kubectl` followed by
arguments. Other Bash commands continue to use the battery's model classifier.
