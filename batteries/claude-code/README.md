# Claude Code battery

Use this battery for Claude Code sessions that need policy-aware shell commands
and `self` labels on the requester's own secrets.

It covers two built-in tools:

- **Bash** — A command that names a credential path (`.ssh/`, `.netrc`,
  `.claude.json`, `.aws/credentials`, a private key, ...) is refused outright:
  one contract judges one call, and a compound command could read and send in
  the same call. Before any other command runs, the Claude Code model decides
  what trust and fresh attention it requires and labels its output for trust.
  An annotation names no reader (`audiences = []`), so the model never decides
  who may see a command's output; static rules do.
- **Read** — Reading a hidden path, a credential file, a private key, or a
  system secret location narrows the session to `self`, the requester: nothing
  built from it reaches a sink that requires `internal` or `public`. The rules
  match the path as written, absolute or relative. Other paths keep the
  session's label. No rule blocks a read or lowers its trust.

The default config created by `appa init claude-code` separately provides the
wildcard fallback for tools it does not name. Keeping that fallback in the root
lets this battery compose without declaring a second wildcard or annotator.

## Add it to a deployment

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 2
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
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "Bash(command:kubectl *)"
requires = { attention = ["hitl"] }
delta = { trust = "suspicious", audience = ["internal"] }

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
