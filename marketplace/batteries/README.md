# Batteries

A battery is an OpenAPPA config for one tool set. It ships its tool rules,
Annotators, Sanitizers, and the scripts those run. A deployment adds a battery
with `include` in its root `appa.toml`. Root tool rules run before battery rules.
A root Annotator declaration replaces a battery Annotator with the same name,
so a deployment can customize its hint without editing the battery.

| Battery | Covers | Externals |
| --- | --- | --- |
| `coding/` | canonical coding-agent tools under `tool/coding/`: `shell`, `powershell`, `read_file`, `search_files`, `write_file`, `edit_file`, `fetch_url`, `web_search`; a client battery's `[aliases.tool.coding]` map binds them | The Claude Code model annotates shell and PowerShell calls and fetches of unlisted URLs; static rules label the requester's secrets `self`, the Databricks CLI's credential commands among them |
| `claude-code/` | Claude Code's built-in tools: `Read`, `Write` and `Edit` labeled per file by the file-tracker, `Bash` and SafeBash confined to the files and hosts they declare, `PowerShell`, `Monitor`, `Grep` and `NotebookEdit` blocked, `coding`'s web rules bound to `WebFetch` and `WebSearch`, and rules for the tools only Claude Code offers; include it with `coding` | The file-tracker annotates file calls; the SafeBash fallback needs `srt` |
| `opencode/` | selected OpenCode V2 file, web, shell, and foreground delegation tools; shell starts with `pwd` only | none |
| `slack/` | the claude.ai Slack connector, all 19 tools: read, search, send, canvases | an annotator asking Slack whether a conversation is shared through Slack Connect; the `slack` audience source: viewer, full members, user groups, one conversation's members |
| `github/` | the GitHub MCP server's default tool sets: profile, repositories, issues, pull requests, users (44 tools) | two annotators asking GitHub for a repository's visibility; the `github` audience source: viewer, org members, teams, a repository's collaborators |
| `google-workspace/` | the claude.ai Google Drive connector, all 11 tools; viewer-only untrusted reads, trusted writes, reviewed sharing | the `google-workspace` audience source: viewer, active Workspace users, a Workspace group with nested groups expanded |
| `linear/` | 65 Linear MCP tools; per-issue, per-team, per-project audiences and reviewed writes | the `linear` audience source: viewer, full members, team members and readers, an issue's, project's, or document's readers |
| `grain/` | the Grain MCP server: meetings, transcripts, notes, deals, clips, stories, collections, workspace admin (49 tools) | none |
| `sentry/` | the Sentry MCP server: 9 listed tools and the 55 catalog tools behind `execute_sentry_tool`; internal reads, reviewed writes | none |
| `monday/` | the hosted monday Platform MCP server, all 99 tools; internal reads, reviewed writes and sensitive changes, public documentation questions, viewer-only returned credentials | the `monday` audience source resolves a notification's user ID to its confirmed reader |
| `notion/` | the hosted Notion MCP server, all 36 tools; internal reads (Notion exposes no page permissions), reviewed structural changes | none |
| `microsoft-learn/` | the Microsoft Learn MCP Server, all 3 read-only tools; suspicious public results, and every query must be sharable with `public` | none |
| `cloudflare/` | Cloudflare's documentation (2 tools) and Workers Observability (8 tools) MCP servers, one namespace bound to both; internal logs, telemetry and Worker reads, public documentation reads whose query must be sharable with `public`, no writes | none |
| `launchdarkly/` | LaunchDarkly's official MCP server, all 20 tools: feature flags, environments, AI Configs, code references, audit log; internal reads, every write reviewed | none |
| `posthog/` | PostHog's MCP server, all 44 registry tools: analytics, insights, dashboards, error tracking, flags, experiments, surveys, docs; internal reads, public documentation input, reviewed writes | none |
| `pagerduty/` | the PagerDuty-hosted MCP server, all 18 tools: 11 `browse_*` reads of incidents, schedules, teams, status pages and activity, 7 `manage_*` writes; internal reads, every write reviewed | none |
| `huggingface/` | the hosted Hugging Face MCP server, all 11 built-in tools: account, search, repository reads and writes, Spaces, Jobs, sandbox; each repository's Hub visibility decides its readers, code run with the token reviewed | two annotators asking the Hub for each named repository's visibility and resource group; the `huggingface` audience source: viewer, one resource group's members |
| `databricks/` | Databricks' managed MCP servers Genie One (5 tools) and Databricks SQL (`execute_sql`), one namespace bound to both host servers; internal Genie reads, each SQL statement classified before it runs | the Claude Code model classifies each statement; the `databricks` audience source: viewer, active users, a group with nested groups expanded, a Genie space's readers |
| `archestra/` | Archestra's built-in sharing tools, the discovery tools `search_tools`, `search_files` and `load_skill`, `yell`, and the agent run and workspace tools (23 tools, 57 rules): projects, apps, knowledge bases and connectors, plugins, agents, MCP gateways, team membership, guardrails reports, agent runs and their workspaces | the `archestra` audience source: organization members, a team, one member |
| `xmemory/` | the hosted xmemory instance (15 tools) and admin (17 tools) MCP servers, one namespace bound to both; internal reads, writes from trusted input only, trusted admin and schema changes, reviewed schema migrations and instance deletion | none |
| `jev/` | no tools; one Annotator a deployment routes its own tools to | TypeSafe's Jev model annotates each routed call's audience and trust; the call's name, description, and arguments are sent to the TypeSafe API |

Every battery with a `suspicious` read except `claude-code` also ships a
tagged `tool_output` sanitizer, `<battery>.strip-instructions`: it removes
instructions from a confined result, which then keeps the session's trust.
`cloudflare`, `linear`, `pagerduty` and `sentry` add
`<battery>.redact-for-public`, which returns an `internal` record at `public`
once the model removes what outside readers may not see. `coding` and
`monday` add `<battery>.strip-secrets`, which returns a credential read at
`internal` instead of `self`. The model for a battery sanitizer is configured
in the deployment, not in the battery: the root binds the ones it runs under
`[externals.sanitizers]`, for example to `builtin = "llm"` or, where the
`claude` CLI is installed, to `builtin = "claude-code"`. A battery sanitizer
the root does not bind is left out, and the battery's `[policy.deployment] confined_results` entries that
only it covers are dropped with it, so the battery decides as it would
without sanitizers. The Claude Code plugin's default config binds the
`coding` sanitizers. `archestra` and
`xmemory` ship none: their reads keep the session's trust, and their records
have no public core to release. `claude-code` ships none either: Claude Code's
replacement of a result is not verified live for the `suspicious` tools it
names, so it cannot confine them.

Include a battery with a path relative to the including file. An install
replaces the deployment's battery store beside the root config, so the line it
writes reads:

```toml
include = ["batteries/coding/appa.toml", "batteries/claude-code/appa.toml"]
```

A battery names each tool by its canonical tool id (`mcp/<server>/<tool>`,
`host/claude-code/<name>`, `tool/<namespace>/<tool>`), never by the host's own spelling.

Each battery's `command` bindings run in the battery's own directory.
[`examples/README.md`](../../examples/README.md) explains how a root
config installs a battery and what it adds around it. The Batteries page
in the website docs describes the format.
