---
title: Claude Code
category: Works with
order: 6
description: Protect Claude Code sessions with deterministic information-flow control in your terminal.
---

OpenAPPA brings deterministic information-flow control directly to Claude Code. It runs alongside your terminal session and checks the tool calls and results that Claude Code reports through its lifecycle hooks. Calls that reach the blocking `PreToolUse` hook can be refused before Claude Code releases them.

## Install

You need Claude Code and `curl`.

```sh
curl -fsSL https://openappa.com/install.sh | sh &&
  ~/.local/bin/appa plugin install claude-code
```

The installer places `appa` in `~/.local/bin`.

`appa plugin install claude-code` configures Claude Code's user environment:
1. Deploys the runtime binary under APPA's data directory.
2. Registers lifecycle hooks in your user-level Claude Code settings.
3. Adds the runtime's `appa` MCP server to Claude Code.
4. Installs the `/appa-guide` onboarding skill.
5. Installs `clappa` (the protected session launcher) and includes the default `claude-code` battery for built-in tools.

Existing custom hooks, MCP servers, and global status lines remain untouched.

## 1. Teach OpenAPPA about your tools

:::claude-policy-timing:::

Start a protected session and run the policy setup skill:

```sh
clappa
```

```text
/appa-guide
```

The skill inspects the MCP servers and tools configured on your machine:
- **Discovers and connects batteries:** Identifies configured MCP servers and automatically includes matching batteries.
- **Maps data boundaries:** Determines which tools read private data and which tools can send data outside the session.
- **Resolves ambiguities:** Asks focused questions when an account identity, data sensitivity, or boundary needs clarification.
- **Fails closed during onboarding:** Unnamed tools route through a bounded fallback classifier until exact contracts or batteries cover them.

Once approved, `/appa-guide` writes deterministic policy configuration. To inspect or customize generated rules by hand, see [Policy configuration](/contracts).

## 2. Try a flow that should be blocked

Start a new protected Claude Code session with the updated policy:

```sh
clappa
```

Ask for an explicit transfer from a private source to a public destination:

```text
Create a public GitHub issue from the action items in my private meeting recording.
```

Claude can read the meeting, but that read narrows who may receive the resulting data. When the public GitHub write reaches `PreToolUse`, OpenAPPA checks the accumulated label against the destination boundary and blocks the tool call before execution.

The refusal names the policy conflict and provides available remedies (such as routing through a configured sanitizer or requesting authorized human review).

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](/images/claude-code-blocked-flow.png)

## How it works under the hood

:::fig-claude-code-hooks:::

OpenAPPA intercepts Claude Code events through native lifecycle hooks:

- **Lifecycle interception:** The integration hooks into Claude Code's native lifecycle events (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, and subagent events).
- **Hook-boundary tool coverage:** Checks built-in and external MCP tool calls when Claude Code reports them. Coverage depends on the corresponding hook firing.
- **Pre-execution evaluation:** `PreToolUse` passes a reported call to the local APPA runtime, which evaluates the flow against the session's accumulated label (`audience × trust`).
- **Fail-closed with remedies:** Allowed calls proceed. Disallowed calls that reach a blocking hook are refused before execution; OpenAPPA returns the policy conflict and available remedies. Unanswered blocking hooks fail closed.
- **Session isolation:** `clappa` launches Claude Code with APPA's policy enforcement and status line. Your regular `claude` command remains completely unchanged.

These hooks are an enforcement boundary, not complete mediation of everything Claude Code observes or emits. Native `Edit` can inspect a file and produce a content-dependent error before `PreToolUse`. OpenAPPA does not guarantee equivalent prevalidation coverage for native `Read` or `Write`. A root `Stop` event reports completion after the final response is already visible. The optional runtime-owned file tools provide stronger mediation for their own calls, but a normal plugin installation does not disable native file tools or force their exclusive use.

## Choose protection per session

Installing OpenAPPA does not force every Claude Code session through it. Use `clappa` when you want policy enforcement. Use `claude` when you do not.

:::claude-session-choice:::

Projects configured with `disableAllHooks: true` disable all hooks, preventing `clappa` from enforcing policy in that session.

## Uninstall

Remove OpenAPPA's hooks and MCP registration while keeping your local policy and database:

```sh
appa plugin remove claude-code
```

To remove the integration, stop the background runtime, and purge local policies, databases, and logs:

```sh
appa plugin remove claude-code --purge
rm -f ~/.local/bin/appa
```
