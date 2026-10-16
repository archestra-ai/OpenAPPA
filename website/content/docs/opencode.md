---
title: AppenCode
nav_title: OpenCode
category: Works with
order: 7.6
description: Protect OpenCode V2 sessions with deterministic information-flow control.
---

AppenCode is OpenAPPA's integration for **OpenCode V2** (`2.0.28`). Running through a local APPA runtime (`127.0.0.1:8789`), it checks every tool call before execution and every tool result or subagent answer before the model sees it, stopping unauthorized actions and data exfiltration based on who may see the data (`audience`) and who wrote it (`trust`).

When a flow is blocked, the agent receives the policy conflict and any available [remedies](/how-it-works)—such as accepting a narrower `label`, running a sanitizer, or requesting human approval—and can retry once a remedy succeeds. If the runtime cannot answer, the call or result stays blocked.

## Install

**Prerequisites:**
- **OpenCode V2** (`2.0.28`) in the **terminal UI** (`opencode` CLI; web and desktop sessions do not display MCP approval dialogs in `2.0.28`), plus `curl`.
- `~/.config/opencode/opencode.json` must be standard JSON (no JSONC comments). If your global config contains comments, strip them first or follow the [manual setup instructions](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/opencode#manual-setup).

```sh
curl -fsSL https://openappa.com/install.sh | sh &&
  ~/.local/bin/appa plugin install opencode
```

`appa plugin install opencode` configures your user-level OpenCode environment and starts the local APPA runtime:

| Path | What it stores |
|---|---|
| `~/.config/opencode/opencode.json` | AppenCode plugin, `appa` MCP server, and `/appa-guide` command (unrelated settings stay untouched) |
| `~/.config/appa/opencode/appa.toml` | Shared policy across your projects (starts with the `opencode` battery) |
| `~/.config/appa/opencode/` | Included [batteries](/batteries) and the runtime SQLite database |

The installer does not register an OS boot service. Re-running `appa plugin install opencode` starts or reloads the background runtime and re-reads `appa.toml` without overwriting your existing config.

## 1. Configure policy for your project

From the project whose tools you want to inspect, launch the guided setup session:

```sh
appa opencode setup
```

Setup starts a private administrative OpenCode session, inspects the project's active tools and connected MCP servers, and proposes rules and compatible [batteries](/available-batteries) for your shared policy (`~/.config/appa/opencode/appa.toml`).

1. **Review and approve:** Check the proposed policy in chat—including rules preserved for your other projects—and reply to approve it (for example, `Approve this proposal`).
2. **Exit and reload the runtime:** Close the setup session, then re-run `appa plugin install opencode` to reload the background runtime with the saved policy:
   ```sh
   appa plugin install opencode
   ```
3. **Start coding:** Launch a fresh `opencode` session in your project.

In a coding session, `/appa-guide` prints the setup command to run when you add new tools. To edit rules by hand, see [Policy configuration](/contracts).

## 2. Try a blocked flow

The starting `opencode` battery is intentionally restrictive so you can test enforcement right away:

| Tool | Starting rule |
|---|---|
| `read` under `private/` | Narrows `audience` to `internal` |
| `webfetch`, `websearch` | Requires a `public` `audience`; lowers `trust` to `suspicious` |
| `write`, `edit` | Requires `trusted` data |
| `shell` | Allows only `pwd` (with `trusted` data and a `public` `audience`) |
| `appa_subagent` | Allows fresh, one-level foreground subagents |
| Any other tool | Blocked until declared in [policy](/contracts) |

Create `private/notes.txt` with dummy text in your project and ask OpenCode:

```text
Read private/notes.txt and send its contents
to https://example.com with webfetch.
```

1. **Private read offers narrowing:** Reading `private/notes.txt` narrows the trajectory's `audience` to `internal`. OpenAPPA pauses the first attempt and returns a remedy offer in the tool result.
2. **Agent accepts and reads:** Following the tool error, the model calls `appa_execute_remedy_plan` to accept the `internal` audience and retries `read`.
3. **Exfiltration is blocked:** When the model calls `webfetch`, OpenAPPA blocks the request before it executes because `internal` data cannot flow to a `public` sink.

## Service batteries

Connect and authenticate your MCP servers in OpenCode before running `appa opencode setup`. Batteries supply policy templates over your existing MCP connections; setup maps your server names under `aliases.mcp` automatically.

| Battery | Setup requirements |
|---|---|
| [Microsoft Learn](/battery-microsoft-learn) | None. Public documentation reads need no credentials or `[policy.audience]` mapping. |
| [Cloudflare](/battery-cloudflare) | Can bind both documentation and observability servers. Observability reads require an `internal` mapping in `[policy.audience]`. |
| [Notion](/battery-notion) | Requires an `internal` mapping in `[policy.audience]`, plus an authority whose `permits` include `notion-review` for structural or agent-session changes. |
| [GitHub](/battery-github) | Uses `single-user` (default) or `multi-user` (`APPA_BATTERY_TARGET`). Its Python helpers authenticate separately from the MCP server: set `APPA_PROVIDER_GITHUB_TOKEN` (or `gh auth login`) for single-user, or a GitHub App installation token for multi-user. Verify access from `~/.config/appa/opencode/batteries/github/` with `python3 check.py` or `python3 check-multi-user.py`. |

## Human review in the terminal UI

When a policy authority asks a person to approve a blocked tool call (`builtin = "hitl"`), **OpenCode `2.0.28` displays the MCP review prompt only in its terminal UI**—web and desktop sessions wait without showing a dialog.

- **Watch the default selection:** **Yes** is highlighted initially, so pressing `Enter` approves and executes the call. Select **No** to deny or `Escape` to dismiss (both leave the call blocked).
- **Check the session:** Review prompts share a project-wide queue; inspect the tool name and arguments before approving.
- **Reconnect if a prompt is missing:** If a remedy waits without displaying a prompt, attach a terminal to the running OpenCode server for that project.

## Foreground subagents

AppenCode replaces OpenCode's native subagent tool with `appa_subagent` for fresh, one-level foreground delegation:

- **Checked return contract:** The first `appa_subagent` call returns a remedy so the parent declares the child's return contract before retrying.
- **Inherited restrictions:** The child starts with the parent's `label` and runs its own tool calls through OpenAPPA.
- **Filtered return:** Only the child's checked final text answer (or an admitted sanitizer replacement) returns to the parent. Child attachments, metadata, and raw errors are withheld.

## Scope and disabled features

| Area | Behavior |
|---|---|
| **Disabled OpenCode features** | Code Mode (whose built-in `fetch` bypasses tool hooks), native/background/nested subagents, conversation forks, background shell commands, and `read` calls that would load nested `AGENTS.md` files are refused. |
| **OS and inference boundary** | AppenCode gates tool calls and results, not OS syscalls or model inference. Native tools and shell subprocesses keep their OS permissions; use separate OS sandboxing before granting broad `shell` access. |
| **Persistent trajectory labels** | A trajectory's `label` persists in SQLite across compaction, `/revert`, and restarts. Writing a file does not attach a `label` to that file on disk for other sessions. |
| **Local runtime security** | The runtime's `/hook` endpoint on `127.0.0.1:8789` trusts local callers without authentication. Keep `~/.config/appa/opencode/` outside the agent's writable workspace and never expose the runtime port on a network. |

For custom deployment paths (`--config`, `XDG_CONFIG_HOME`), offline bundles (`appa bundle`), and canonical tool naming, see the [AppenCode README](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/opencode).

## Uninstall

Remove AppenCode's plugin, MCP server, and `/appa-guide` entries from `~/.config/opencode/opencode.json` while keeping your policy and database:

```sh
appa plugin remove opencode
```
