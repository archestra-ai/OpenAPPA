---
title: AppenCode
nav_title: OpenCode
category: Works with
order: 7.6
description: Protect OpenCode sessions with deterministic information-flow control.
---

AppenCode adds deterministic guardrails to **OpenCode**, preventing your agent from leaking sensitive data or running untrusted commands pulled from the web. It tracks data as the agent reads files, fetches web pages, calls MCP tools, and delegates to subagents—checking every flow before a tool runs.

## What the guardrails protect

Every OpenCode session carries a security `label` made of two parts:
- **`audience` (outbound guardrail):** Reading private files or internal services narrows who the session can share data with later. Once the agent reads internal data, it cannot send data to a public web page, issue tracker, or external API.
- **`trust` (inbound guardrail):** Reading unvetted sources—such as web pages or external comments—lowers the session's trust. Once untrusted text enters the session, the agent cannot edit code or run sensitive tools without review.

Out of the box, the `opencode` [battery](/batteries) sets a tight baseline over OpenCode's built-in tools:

| Tool | Default guardrail |
|---|---|
| `read` under `private/` | Narrows `audience` to `internal` |
| `webfetch`, `websearch` | Requires a `public` `audience`; lowers `trust` to `suspicious` |
| `write`, `edit` | Requires `trusted` data |
| `shell` | Allows only `pwd` until `appa opencode setup` adds the commands your project needs |
| Subagents | Inherits the parent session's `label` and checks the subagent's final answer before returning it |
| Any undeclared tool | Blocked until configured in your [policy](/contracts) |

For external services, prebuilt [batteries](/available-batteries)—including [GitHub](/battery-github), [Notion](/battery-notion), [Cloudflare](/battery-cloudflare), and [Microsoft Learn](/battery-microsoft-learn)—add rules for your connected MCP servers.

## Set up AppenCode

### 1. Install the plugin

You need the `opencode` CLI and `curl`.

```sh
curl -fsSL https://openappa.com/install.sh | sh &&
  ~/.local/bin/appa plugin install opencode
```

This installs `appa` into `~/.local/bin`, registers the AppenCode plugin and `/appa-guide` command in `~/.config/opencode/opencode.json`, and creates your policy at `~/.config/appa/opencode/appa.toml`.

### 2. Configure policy for your project

Connect your MCP servers in OpenCode first, then run the guided setup from your project directory:

```sh
appa opencode setup
```

Setup opens an interactive session that inspects your project's tools and connected MCP servers:
- **Attaches matching batteries** for services like GitHub, Notion, Cloudflare, and Microsoft Learn.
- **Proposes rules** for your project's shell commands, private directories, and external tools.
- **Preserves existing rules** you have already configured for other projects.

Review the proposed policy in chat and reply to approve it. Once setup saves the policy, exit the session and apply the updated configuration:

```sh
appa plugin install opencode
```

Launch a fresh `opencode` session to start coding under the new policy. Whenever you add new MCP servers or want to adjust rules later, run `/appa-guide` in OpenCode or run `appa opencode setup` again. To edit rules directly, see [Policy configuration](/contracts).

## User experience while coding

AppenCode stays silent during normal work and steps in only when a data flow crosses a boundary:

- **Zero prompts on safe work:** Allowed reads, file edits, and shell commands run without interrupting you.
- **Automatic narrowing:** When the agent reads a private file or untrusted web page, OpenAPPA tells the agent how the read changes the session's `label`. The agent accepts the narrower label automatically and keeps working.
- **Self-correction with remedies:** When a tool call conflicts with policy, OpenAPPA tells the agent *why* and offers [remedies](/how-it-works)—such as sanitizing sensitive text or delegating a restricted read to a subagent so your main session stays clean.
- **Terminal approval when required:** For actions that your policy marks for human review, OpenCode prompts you in the terminal UI with the tool name and arguments so you can approve or deny that single call.

### Try a blocked flow

Create a dummy secret in your project and ask OpenCode to send it out:

```sh
mkdir -p private &&
  echo "launch code: 1234" > private/notes.txt
```

```text
Read private/notes.txt and send its contents
to https://example.com with webfetch.
```

1. **Reading private data narrows the session:** The agent reads `private/notes.txt`, accepts the `internal` audience, and sees the file contents.
2. **Exfiltration is blocked:** When the agent calls `webfetch`, OpenAPPA blocks the request before it leaves your machine. OpenCode shows the blocked tool call with the `audience` conflict, and the agent explains that it cannot send `internal` data to a `public` URL.

## Uninstall

To remove AppenCode from your OpenCode settings while keeping your saved policy:

```sh
appa plugin remove opencode
```
