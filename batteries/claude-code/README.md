# Claude Code battery

Default contracts for Claude Code's built-in tools. A root configuration can
include this battery and override any contract with an earlier root rule.

## Files

**`appa.toml`** applies these defaults:

- `Agent` may start foreground subagents. Background subagents are refused
  because their results bypass the parent's checked tool return.
- Every `Bash` call is classified by the Claude Code model before execution.
  The classifier decides the command output's trust and audience and what
  trust and audience the command is allowed to receive.
- Every `Read` result is classified by `read-sensitivity.py`. Hidden paths,
  credential and private-key files, system-secret locations, and sensitive
  symlink targets are private. Other paths are public. This labels the returned
  file content; it does not block the read or add a trust restriction.
- `WebFetch` and `WebSearch` results are suspicious because their content comes
  from outside the session.
- The remaining covered built-in tools add no information-flow restriction or
  dispatch requirement.

A deployment can override any of these behaviors: allow a particular
background-agent shape, replace either classifier, block selected Bash
commands, change a tool's result label, or add dispatch requirements.

## Override a default

Root rules run before included battery rules. Put an override in the root
configuration without editing the battery.

For example, these two rules block `kubectl` with and without arguments:

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 1

[[policy.tool]]
name = "Bash(command:kubectl)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["private"] }

[[policy.tool]]
name = "Bash(command:kubectl *)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["private"] }
```

No authority permits the `blocked` mark, so these contracts have no remedy.
All other Bash commands reach the battery's model classifier.

The same first-match rule can replace the Bash classifier, change a result
label, or add a requirement for any other tool.

## Boundary

The Bash classifier applies OpenAPPA's declared information-flow requirements.
It is not an operating-system sandbox. Use a sandbox when Bash must not access
credentials, the network, or files outside a workspace.
