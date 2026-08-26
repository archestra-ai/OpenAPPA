# Claude Code Bash and Read battery

Use this battery for Claude Code sessions that need policy-aware shell commands
and automatic privacy labels for local file reads.

It covers two built-in tools:

- **Bash** — Before a command runs, the Claude Code model decides what trust,
  audience, and fresh attention the command requires. It also labels the
  command's output for trust and audience. This lets later tools distinguish,
  for example, public build output from private or suspicious command output.
- **Read** — Before a file is read, a local Python resolver checks its path.
  Hidden paths, credential files, private keys, system-secret locations, and
  sensitive symlink targets produce private content. Other paths produce public
  content. The resolver does not block the read or lower its trust.

Choose it when Claude Code uses Bash for varied development work that cannot be
covered by a safe fixed command list, and when file confidentiality depends on
the path being read.

This is an information-flow policy, not a shell sandbox. Use an operating-system
sandbox when commands must be prevented from reaching files or the network.

## Add it to a deployment

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 1
```

Root rules take precedence over the battery. Add a root rule when a particular
Bash command or Read path needs stricter, looser, or fully blocked behavior.
