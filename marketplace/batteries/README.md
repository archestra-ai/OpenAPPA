# Batteries

A battery is an OpenAPPA config for one tool set. It ships its tool rules,
Annotators, Sanitizers, and the scripts those run. A deployment adds a battery
with `include` in its root `appa.toml`. Root tool rules run before battery rules.
A root Annotator declaration replaces a battery Annotator with the same name,
so a deployment can customize its hint without editing the battery.

| Battery | Covers | Externals |
| --- | --- | --- |
| `claude-code/` | `host/claude-code/Bash` and `host/claude-code/Read` in a Claude Code session | The Claude Code model annotates Bash calls; static `Read` rules label the requester's secrets `self` |
| `slack/` | the claude.ai Slack connector, all 19 tools: read, search, send, canvases | the `slack` audience source: viewer, full members, user groups, one conversation's members |
| `github/` | the GitHub MCP server's default tool sets: profile, repositories, issues, pull requests, users (44 tools) | two annotators asking GitHub for a repository's visibility; the `github` audience source: viewer, org members, teams, a repository's collaborators |
| `google-workspace/` | no tools yet; audiences only | the `google-workspace` audience source: viewer, active Workspace users, a Workspace group with nested groups expanded |
| `linear/` | 65 Linear MCP tools; per-issue, per-team, per-project audiences and reviewed writes | the `linear` audience source: viewer, full members, team members and readers, an issue's, project's, or document's readers |
| `grain/` | the Grain MCP server: meetings, transcripts, notes, deals, clips, stories, collections, workspace admin (49 tools) | none |
| `sentry/` | the Sentry MCP server: 9 listed tools and the 55 catalog tools behind `execute_sentry_tool`; internal reads, reviewed writes | none |
| `notion/` | the hosted Notion MCP server, all 36 tools; internal reads (Notion exposes no page permissions), reviewed structural changes | none |
| `microsoft-learn/` | the Microsoft Learn MCP Server, all 3 read-only tools; suspicious public results, and every query must be sharable with `public` | none |
| `cloudflare-docs/` | Cloudflare's documentation MCP server, both tools; public results, and the search query must be sharable with `public` | none |
| `cloudflare-observability/` | Cloudflare's Workers Observability MCP server, all 8 tools; internal logs, telemetry and Worker reads, plus two public documentation reads, no writes | none |
| `launchdarkly/` | LaunchDarkly's official MCP server, all 20 tools: feature flags, environments, AI Configs, code references, audit log; internal reads, every write reviewed | none |
| `posthog/` | PostHog's MCP server, all 44 registry tools: analytics, insights, dashboards, error tracking, flags, experiments, surveys, docs; internal reads, public documentation input, reviewed writes | none |
| `pagerduty/` | the PagerDuty-hosted MCP server, all 18 tools: 11 `browse_*` reads of incidents, schedules, teams, status pages and activity, 7 `manage_*` writes; internal reads, every write reviewed | none |
| `databricks/` | Databricks' managed MCP servers Genie One (5 tools) and Databricks SQL (`execute_sql`), one namespace bound to both host servers; internal Genie reads, each SQL statement classified before it runs | the Claude Code model classifies each statement; the `databricks` audience source: viewer, active users, a group with nested groups expanded, a Genie space's readers |

Include a battery with a path relative to the root config:

```toml
include = ["../../batteries/claude-code/appa.toml"]
```

A battery names each tool by its canonical tool id (`mcp/<server>/<tool>`,
`host/claude-code/<name>`), never by the host's own spelling.

Each battery's `command` bindings run in the battery's own directory.
[`examples/README.md`](../../examples/README.md) explains how a root
config installs a battery and what it adds around it. The Batteries page
in the website docs describes the format.
