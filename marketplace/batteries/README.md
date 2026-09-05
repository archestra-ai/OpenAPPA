# Batteries

A battery is an OpenAPPA config for one tool set. It ships its tool rules,
Annotators, Sanitizers, and the scripts those run. A deployment adds a battery
with `include` in its root `appa.toml`. Root tool rules run before battery rules.
A root Annotator declaration replaces a battery Annotator with the same name,
so a deployment can customize its hint without editing the battery.

| Battery | Covers | Externals |
| --- | --- | --- |
| `claude-code/` | `host/claude-code/Bash` and `host/claude-code/Read` in a Claude Code session | The Claude Code model annotates Bash calls; static `Read` rules label the requester's secrets `self` |
| `slack/` | the claude.ai Slack connector, all 19 tools: read, search, send, canvases | none |
| `github/` | the GitHub MCP server's default tool sets: profile, repositories, issues, pull requests, users (44 tools) | none |
| `grain/` | the Grain MCP server: meetings, transcripts, notes, deals, clips, stories, collections, workspace admin (49 tools) | none |

Include a battery with a path relative to the root config:

```toml
include = ["../../batteries/claude-code/appa.toml"]
```

A battery names each tool by its canonical tool id (`mcp/<server>/<tool>`,
`host/claude-code/<name>`), never by the host's own spelling.

Each battery's `command` bindings run in the battery's own directory. A
complete deployment that includes both batteries and overrides parts of
them is in `examples/claude-code-battery/`. The Batteries page in the
website docs describes the format.
