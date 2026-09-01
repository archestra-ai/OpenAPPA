# Batteries

A battery is an OpenAPPA config for one tool set. It ships its tool rules,
its annotators and sanitizers, and the scripts those run. A deployment
adds a battery with `include` in its root `appa.toml`; root rules run
before battery rules, so a deployment overrides a battery without editing
it.

| Battery | Covers | Externals |
| --- | --- | --- |
| `claude-code/` | `Bash` and `Read` in a Claude Code session | The Claude Code model annotates Bash calls; `read-sensitivity.py` labels file contents |
| `slack/` | the claude.ai Slack connector, all 19 tools: read, search, send, canvases | none |
| `github/` | the GitHub MCP server's default tool sets: profile, repositories, issues, pull requests, users (44 tools) | none |
| `grain/` | the Grain MCP server: meetings, transcripts, notes, deals, clips, stories, collections, workspace admin (49 tools) | none |

Include a battery with a path relative to the root config:

```toml
include = ["../../batteries/claude-code/appa.toml"]
```

Each battery's `command` bindings run in the battery's own directory. A
complete deployment that includes both batteries and overrides parts of
them is in `examples/claude-code-battery/`. The Batteries page in the
website docs describes the format.
