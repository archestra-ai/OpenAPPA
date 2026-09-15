# Claude Code battery

This battery gives Claude Code sessions policy-aware shell commands and `self`
labels on the requester's own secrets. `appa plugin install claude-code`
includes it on a first install; `appa battery remove claude-code` takes it out,
and a later plugin install does not bring it back.

It covers two built-in tools, which the policy names `host/claude-code/Bash`
and `host/claude-code/Read`:

- **Bash** — A command that names a credential path (`.env`, `.ssh/`, `.netrc`,
  `.claude.json`, `.aws/credentials`, a private key, ...) narrows the session
  to `self`, the requester. The battery withholds the command's result and
  offers the stock `redact-secrets` sanitizer, which masks private-key blocks,
  tokens of well-known shapes, passwords inside URLs, the value of any
  assignment whose key names a secret, and long high-entropy runs, then
  returns the masked output to `public`: the model reads the masked text and
  the session keeps its label.
  The sanitizer carries no tags: it is offered for any withheld Bash result
  the session cannot read as it is, including one the Annotator narrowed to
  `self`, and for a subagent's return. Before any other command runs, the
  Claude Code model decides what trust and fresh attention it requires and
  labels its output for trust and audience, inside the vocabulary static rules
  write: a command that visibly reads the requester's or the organization's
  data narrows to `self` or `internal`, and one that shares with a reader the
  policy names requires that reader.
- **Read** — Reading a hidden path, a credential file, a private key, or a
  system secret location narrows the session to `self`, the requester: nothing
  built from it reaches a sink that requires `internal` or `public`. The rules
  match the path as written, absolute or relative. Other paths keep the
  session's label. No rule blocks a read or lowers its trust.

The default config `appa plugin install claude-code` writes provides the
wildcard fallback for tools it does not name and the deployment-specific Bash
Annotator hint. The root Annotator replaces this battery's default declaration.

## Add it to a deployment

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 2
```

Root rules take precedence over the battery. Add a root rule when a particular
Bash command or Read path needs stricter, looser, or fully blocked behavior.

The battery lists `host/claude-code/Bash` in `confined_results` itself, so the
masker can run on the command's output; a root needs no deployment setting for
it. `appa battery remove claude-code` takes the rules, the sanitizer and the
confinement out together.

## Customize Bash classification

The battery's `hint` instructs the model how to classify shell commands. To
customize it, replace the Annotator in the root config instead of modifying the
battery:

```toml
[[policy.annotator]]
name = "claude-code.bash-requirements"
builtin = "claude-code"
hint = "Treat network output as suspicious. Require hitl attention before commands that publish releases or change production infrastructure."
```

The root declaration replaces the battery's Annotator with the same name.
Preserve `builtin` unless you intend to alter the implementation; write
`audiences` only to narrow the mandate below the policy's vocabulary. The battery continues to provide ordered Bash rules, including its
credential-path refusals.

## Example override

If needed, the battery can be overridden from the root config. For example,
these root rules require fresh human approval for every `kubectl` command:

```toml
[[policy.tool]]
name = "host/claude-code/Bash(command:kubectl)"
requires = { attention = ["hitl"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "host/claude-code/Bash(command:kubectl *)"
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
