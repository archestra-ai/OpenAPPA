# Coding battery

This battery gives a coding agent policy-aware shell commands and `self`
labels on the requester's own secrets. It writes its rules once, for
canonical tools under `tool/coding/`. A client battery binds each canonical
tool to the client's own tool with an `[aliases.tool.coding]` map; the
`claude-code` battery is that map for Claude Code, and `appa plugin install
claude-code` includes both. A rule no map binds is inactive.

| Canonical tool | What it is | Argument the rules read |
| --- | --- | --- |
| `tool/coding/shell` | a POSIX shell command | `command` |
| `tool/coding/powershell` | a PowerShell command | `command` |
| `tool/coding/read_file` | a file read | `file_path` |
| `tool/coding/search_files` | a content search under a path | `path` |
| `tool/coding/write_file` | a file write | `file_path` |
| `tool/coding/edit_file` | an in-place file edit | `file_path` |
| `tool/coding/fetch_url` | a web page fetch | `url` |
| `tool/coding/web_search` | a web search | none |

- **shell** — A command that names a credential path (`.env`, `.ssh/`,
  `.netrc`, `.claude.json`, `.aws/`, `.gnupg/`, a private key, ...) narrows
  the session to `self`, the requester. A client whose map entry sets
  `confined = true` withholds the command's result, and the battery offers the
  stock `redact-secrets` sanitizer, which masks private-key blocks, tokens of
  well-known shapes, the AWS secret access key, passwords inside URLs, the
  value of any assignment whose key names a secret, and long high-entropy
  runs, then returns the masked output to `internal`: the model reads the masked
  text and the session keeps its label.
  The sanitizer carries no tags: it is offered for any withheld shell result
  the session cannot read as it is, including one the Annotator narrowed to
  `self`, and for a subagent's return. Before any other command runs, the
  `claude-code.bash-requirements` Annotator decides what trust and fresh
  attention it requires and labels its output for trust and audience, inside
  the vocabulary static rules write: a command that visibly reads the
  requester's or the organization's data narrows to `self` or `internal`, and
  one that shares with a reader the policy names requires that reader.
- **shell, credential commands** — A command that prints or writes a
  credential is a credential path too: the Databricks CLI's `auth token`,
  `auth env`, `secrets get-secret`, and `configure`, with the CLI's global
  flags anywhere before the verb, and its profile file `.databrickscfg`. A
  selector is a substring of the command line, so a spelling these miss
  (the command inside `$(...)`, a heredoc, an alias) is classified by the
  Annotator like any other command; the rules narrow what they match and
  promise no full recall.
- **shell, `git push` and `gh`** — What a push, a pull request, an issue, a
  release, or a `gh api` call puts on GitHub is read by the repository's
  readers, which the command line does not say. These commands go to the
  `claude-code.bash-repository-requirements` Annotator. When the `github` battery
  is installed, its `github` context provider answers before the Annotator
  is asked: the repositories, pull requests and issues the command reaches,
  their visibility, and who wrote them. The Annotator requires audience
  `public` for a public repository and `internal` for a private or internal
  one. Without that answer, or when the provider fails, the Annotator treats
  the destination as public. The credential rules come first, so a push that
  also names a credential narrows without a consult.
- **powershell** — The shell rules: the same credential selectors, written
  for both path separators and for the Windows homes of the GitHub CLI and
  gcloud configs, plus `Get-StoredCredential` and `Get-Secret`; the same
  publishing selectors; the `claude-code.bash-requirements` Annotator
  for every other command; and the same masked results.
- **read_file** — Reading a hidden path, a credential file, a private key, or
  a system secret location narrows the session to `self`: nothing built from
  it reaches a sink that requires `internal` or `public`. The rules match the
  path as written, absolute or relative. Other paths keep the session's
  label. No rule blocks a read or lowers its trust.
- **search_files** — A search inside one of the same paths is a read of it
  and narrows the session to `self`. A search over a directory that holds
  such a file is not matched; only the path as written is.
- **write_file, edit_file** — Writing into one of the same paths, or into a
  file a later process reads as instructions or runs as code (`CLAUDE.md`,
  `.claude/skills/`, `.claude/agents/`, `.claude/commands/`, `.git/hooks/`,
  shell startup files, `Library/LaunchAgents/`), requires a `trusted`
  session: content that arrived at `suspicious` lands there only when the
  person running the session approves the exact call. Writing a file that can
  turn the protection off asks that person every time: the harness's settings
  (`.claude/settings*`) and hooks (`.claude/hooks/`), its MCP server list
  (`.mcp.json`), and the deployment's policy (`appa/appa.toml`,
  `appa/batteries/`). Every other path takes the session's label as it is.
- **fetch_url, web_search** — A page or search result is a `suspicious`
  source. A fetch URL is a `public` sink, except that a fetch from a listed
  documentation host keeps the session's label. A search, through
  `web_search` or a Google search URL, requires no audience: the query goes
  only to the search provider.

The Annotators run the local `claude` command (`builtin = "claude-code"`); on
Windows it must resolve to `claude.exe`. An Annotator reads the client's call
as sent, under the client's own argument names.

## Add it to a deployment

```toml
include = ["batteries/coding/appa.toml", "batteries/claude-code/appa.toml"]

[policy]
version = 2
```

The `coding` battery alone declares no client tool. Root rules take
precedence over both batteries, and a root rule names the client's tool
(`host/claude-code/Bash(command:kubectl *)`); the `claude-code` battery's
README shows overrides and Annotator hints.
