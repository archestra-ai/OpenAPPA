# Claude Code battery

This battery gives Claude Code sessions policy-aware shell commands and `self`
labels on the requester's own secrets. `appa plugin install claude-code`
includes it on a first install; `appa battery remove claude-code` takes it out,
and a later plugin install does not bring it back.

It covers five built-in tools, which the policy names `host/claude-code/Bash`,
`host/claude-code/Read`, `host/claude-code/Grep`, `host/claude-code/Write`
and `host/claude-code/Edit`:

- **Bash** — A command that names a credential path (`.env`, `.ssh/`, `.netrc`,
  `.claude.json`, `.aws/`, `.gnupg/`, a private key, ...) narrows the session
  to `self`, the requester. The battery withholds the command's result and
  offers the stock `redact-secrets` sanitizer, which masks private-key blocks,
  tokens of well-known shapes, the AWS secret access key, passwords inside
  URLs, the value of any assignment whose key names a secret, and long
  high-entropy runs, then
  returns the masked output to `public`: the model reads the masked text and
  the session keeps its label.
  The sanitizer carries no tags: it is offered for any withheld Bash result
  the session cannot read as it is, including one the Annotator narrowed to
  `self`, and for a subagent's return. On Unix, before any other command runs, the
  Claude Code model decides what trust and fresh attention it requires and
  labels its output for trust and audience, inside the vocabulary static rules
  write: a command that visibly reads the requester's or the organization's
  data narrows to `self` or `internal`, and one that shares with a reader the
  policy names requires that reader.
- **Bash, credential commands** — A command that prints or writes a
  credential is a credential path too: the Databricks CLI's `auth token`,
  `auth env`, `secrets get-secret`, and `configure`, with the CLI's global
  flags anywhere before the verb, and its profile file `.databrickscfg`. A
  selector is a substring of the command line, so a spelling these miss
  (the command inside `$(...)`, a heredoc, an alias) is classified by the
  Bash annotator like any other command; the rules narrow what they match
  and promise no full recall. Every other `databricks` command, the SQL of
  `databricks experimental aitools tools query` included, is classified by
  the Bash annotator under the root's hint; a deployment that wants a
  fixed contract for one command writes a root rule for it, as the
  `kubectl` example below does.
- **Bash, `git push` and `gh`** — What a push, a pull request, an issue, a
  release, or a `gh api` call puts on GitHub is read by the repository's
  readers, which the command line does not say. Before the Annotator is
  asked, `repository.py` establishes every repository the command reaches —
  each one a `gh` call names by `--repo`, `GH_REPO`, a `repos/OWNER/NAME`
  path, a URL, or an `OWNER/NAME` word of `gh repo`, and each one a push names by URL or remote (in its `-C` or `cd`
  directory), else the checkout's own in the directory Claude Code runs
  the command in — and their visibility, through the GitHub CLI's own login
  (`gh repo view`). Several repositories answer the most widely readable.
  The Annotator reads the finding as an established input and requires
  audience `public` for a public repository, `internal` for a private or
  internal one. A repository it cannot establish (no `gh`, no checkout, a
  shell, `eval`, `source` or subshell, `git -c`, an environment setting
  such as `GIT_DIR` or `HOME`, a program or target computed at run time) is
  answered as unknown, and the Annotator treats the destination as public.
  The finding covers the `git` and `gh` calls the command line shows; a
  script or another program that pushes on its own is the Annotator's to
  judge from the command. The finding informs the Annotator; the
  repository's collaborators are not resolved as readers. On Windows, the
  Unix-only publishing selectors are absent, so these otherwise undeclared
  calls are refused.
- **Read** — Reading a hidden path, a credential file, a private key, or a
  system secret location narrows the session to `self`, the requester: nothing
  built from it reaches a sink that requires `internal` or `public`. The rules
  match the path as written, absolute or relative. Other paths keep the
  session's label. No rule blocks a read or lowers its trust.
- **Grep** — A search inside one of the same paths is a read of it and
  narrows the session to `self`. A search over a directory that holds such
  a file is not matched; only the path as written is.
- **Write, Edit** — Writing into one of the same paths, or into a file a
  later process reads as instructions or runs as code (`CLAUDE.md`,
  `.claude/skills/`, `.claude/agents/`, `.claude/commands/`, `.git/hooks/`,
  shell startup files, `Library/LaunchAgents/`), requires a `trusted`
  session: content that arrived at `suspicious` lands there only when the
  person running the session approves the exact call. Writing a file that can
  turn the protection off asks that person every time: the harness's settings
  (`.claude/settings*`) and hooks (`.claude/hooks/`), its MCP server list
  (`.mcp.json`), and the deployment's policy (`appa/appa.toml`,
  `appa/batteries/`). Every other path takes the session's label as it is.

On Unix, the default config `appa plugin install claude-code` writes declares
the Bash Annotators, the GitHub publishing selectors, the Monitor rule, and the
wildcard fallback.
It resolves `repository.py` from the included battery's installed directory.
On Windows, the installer omits these Unix-only declarations. Bash calls that
have no static rule remain fail-closed. The battery still labels credential
paths and confines Bash results on both platforms.

## Add it to a deployment

```toml
include = ["batteries/claude-code/appa.toml"]

[policy]
version = 2
```

This small root only includes the battery's static rules. Use the installed
default root for Unix Bash Annotators and publishing selectors, or declare
those contracts in your own root policy.

Root rules take precedence over the battery. Add a root rule when a particular
Bash command or Read path needs stricter, looser, or fully blocked behavior.

The battery lists `host/claude-code/Bash` in `confined_results` itself, so the
masker can run on the command's output; a root needs no deployment setting for
it. `appa battery remove claude-code` takes the rules, the sanitizer and the
confinement out together.

## Customize Bash classification

How the model Annotator classifies a command — trust by who wrote the text it
returns, audience by its visible destination — is the runtime's and the same
for every model Annotator. A `hint` adds what only the deployment knows. To
give the Bash Annotator one, replace it in the root config instead of
modifying the battery:

```toml
[[policy.annotator]]
name = "claude-code.bash-requirements"
builtin = "claude-code"
hint = "Hosts under corp.example are the organization's own: what they return is internal. Require hitl attention before commands that publish releases or change production infrastructure."
```

On Unix, replace the default root declaration with this one. Preserve `builtin`
unless you intend to alter the implementation; write `audiences` only to narrow
the mandate below the policy's vocabulary. The battery continues to provide
ordered credential-path narrowing rules.

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
