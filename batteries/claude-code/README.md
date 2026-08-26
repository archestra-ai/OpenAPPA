# Claude Code battery

Default contracts for Claude Code's built-in tools. A root configuration can
include this battery and override any contract with an earlier root rule.

## Files

**`appa.toml`** names the Claude Code built-in tools. Most results keep their
current label. `WebFetch` and `WebSearch` produce suspicious results.

Every `Bash` call goes to the `claude-code` model builtin before dispatch. The
model classifies the trust and audience that the command requires. Bash output
is always suspicious and private; classifying the command does not certify its
result.

Every `Read` call goes to `read-sensitivity.py`. Hidden paths, credential and
private-key files, system-secret locations, and sensitive symlink targets are
private. Other paths are public. The resolver labels the returned value; it
does not block the read.

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
