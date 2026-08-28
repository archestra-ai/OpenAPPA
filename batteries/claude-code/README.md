# Claude Code battery

Use this battery for Claude Code sessions that need policy-aware shell commands
and automatic privacy labels for local file reads.

It covers two built-in tools and every tool the policy does not name:

- **Bash** — Before a command runs, the Claude Code model decides what trust,
  audience, and fresh attention the command requires. It also labels the
  command's output for trust and audience. This lets later tools distinguish,
  for example, public build output from private or suspicious command output.
- **Read** — Before a file is read, a local Python resolver checks its path.
  Hidden paths, credential files, private keys, system-secret locations, and
  sensitive symlink targets produce private content. Other paths produce public
  content. The resolver does not block the read or lower its trust.
- **Undeclared tools** — A tool with no policy entry, such as an MCP server's
  tool, is not blocked for being unnamed. Before it runs, the Claude Code model
  reads the call and answers the trust, audience, and fresh attention it
  requires; the session's label then decides the call exactly as it decides a
  declared tool's. A floor is checked against what the session already holds,
  so a fresh session meets any floor and the answer gains force as the session
  absorbs suspicious or private content. Attention marks come from the root
  policy's authorities: a root that registers `hitl` lets the model demand a
  person's sign-off for a call. Its results enter with an unknown label and are
  classified by the same cast when a later check needs them. Deployments that
  want undeclared tools blocked outright omit this battery's cast with a root
  rule of their own.

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
