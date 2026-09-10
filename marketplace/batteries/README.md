# Batteries

A battery is a security contract for a specific tool interface. Keep it as lean
as possible, but not leaner: use static TOML or argument matching when sufficient,
and add provider machinery when a useful decision needs current facts or
interpretation. The default package is `appa.toml`, `appa-package.toml`, and a
README. Optional helpers must solve a concrete integration problem.

The deployment supplies server bindings, credentials, audience and identity
configuration, and review authorities. Root tool rules take precedence and replace
the complete annotation. A root annotator can replace a packaged declaration of
the same name. Provider membership alone does not establish resource permissions.

See the [authoring guide](../../website/content/docs/write-a-battery.md) and the
[existing battery assessment](ASSESSMENT.md).

| Battery | Covers | Externals |
| --- | --- | --- |
| `claude-code/` | `host/claude-code/Bash` and `host/claude-code/Read` in a Claude Code session | The Claude Code model annotates Bash calls; static `Read` rules label the requester's secrets `self` |
| `slack/` | the claude.ai Slack connector, 19 tools | Optional Slack audience source |
| `github/` | the GitHub MCP server's default tool sets (44 tools), with public-repository defaults | Optional GitHub audience source |
| `grain/` | the Grain MCP server: meetings, transcripts, notes, deals, clips, stories, collections, workspace admin (49 tools) | none |
| `google-workspace/` | Directory audiences; no tool contracts yet | Google Workspace audience source |

Include a battery with a path relative to the root config:

```toml
include = ["marketplace/batteries/claude-code/appa.toml"]
```

A battery names each tool by its canonical tool id (`mcp/<server>/<tool>`,
`host/claude-code/<name>`), never by the host's own spelling.

Each battery's `command` bindings run in the battery's own directory. A
complete deployment that includes both batteries and overrides parts of
them is in `examples/claude-code-battery/`. The Batteries page in the
website docs describes the format.
