---
title: Claude Code
category: Works with
order: 6
description: Protect Claude Code sessions with deterministic information-flow control in your terminal.
---

OpenAPPA brings deterministic information-flow control directly to Claude Code. It runs alongside your terminal session, tracking data as Claude reads files and tools, and preventing exfiltration or unauthorized actions before any command executes.

## Install

You need Claude Code and `curl`.

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa plugin install claude-code
```

`appa plugin install claude-code` configures Claude Code's user environment:
1. Starts the APPA runtime and connects it to Claude Code as the `appa` MCP server.
2. Registers lifecycle hooks in your user-level Claude Code settings, which send each tool call to the APPA runtime.
3. Installs the `/appa-guide` skill, which [generates and updates your policy](#generate-and-update-your-policy-with-appa-guide).
4. Installs `clappa`, an alias for `claude` that you run when you want a protected session.

:::fig-claude-code-hooks:::

The hooks check every tool call before it runs: built-in tools like `Bash` and `Edit`, and every MCP tool. If the runtime doesn't answer, the call is blocked.

Existing custom hooks, MCP servers, and global status lines remain untouched.

## Generate and update your policy with `/appa-guide`

:::claude-policy-timing:::

Start a protected session that runs the skill:

```sh
clappa /appa-guide
```

The skill looks at the tools and MCP servers you have connected and works out, for each tool, whether it reads private data, returns untrusted content, or can send data out. It starts by telling you how much the current policy covers:

```text
> /appa-guide

● Current state: this session is protected with OpenAPPA, but only the
  Claude Code battery is included. The rest of your tools (about 178) are
  judged one call at a time, and any of those calls may stop to ask you.
```

The skill builds your policies on [batteries](/batteries): ready-made policies for popular servers such as Slack or Grain. A battery also brings [dynamic resolution](/contracts#configure-audience-membership): like a language server looking up a definition, it looks up who is in the Slack channel or group a call targets. The skill explains what it adds:

```text
● Batteries to include
  - Slack battery: Keeps each channel's content with that channel's
    members and never posts your private data into a channel.
  - Grain battery: Treats meeting content as outside text kept inside
    your organization. Sharing, invites and admin changes ask you first.

  Approve, or tell me what to change.
```

For tools without a battery, the skill writes the policies itself. When it can't work something out from the tools alone, it asks you. Here, two Slack servers were connected, and the Slack battery applies the same way to both only if they share a workspace:

```text
 ☐ slack server

Your `slack` MCP server: is its Slack the same workspace as your
claude.ai Slack connector?

❯ 1. Same workspace (Recommended)
  2. Different workspace
  3. Leave it per-call
```

You approve every change, and the skill reports what is now in effect:

```text
● This call adds the Slack, Grain and Sentry batteries to your policy and
  reloads the runtime. Please approve it on the card.

  ...

● Your policy is updated and live.
  - Kept inside your organization: Grain meetings, Sentry data, Slack
    channel content. Anything built from them can't be sent anywhere
    public.
  - Ask you first: Grain sharing and admin changes, Sentry changes.
  160 of your 211 session tools now have a fixed rule.

  Start a new clappa session to use the updated policy; this session
  keeps the policy it started with.
```

Restart `clappa` so OpenAPPA enforces the new policies on every call.

If a block looks wrong, the agent can report it with [`appa yell`](/yell):

```text
● appa - yell (MCP)(message: "Posting a summary back to the Slack channel
  it came from was blocked, although only that channel's members read
  it.", with_trajectory: true)
  ⎿  [appa] Reported. Receipt 3f2b9c1e-7a4d-4e1b-9c55-2d8e0f6a7b31.
```

Reports are [filtered](/yell#what-a-report-contains) and go to the OpenAPPA team by default, or to [your own receiver](/yell#choose-where-reports-go). With [observability](/observability#agent-reports) set up, each report also appears as an `appa.yell.report` event. See [Self-improving policies](/self-improving-policies) for turning reports into policy changes.

You can run `/appa-guide` again at any time: after you add an MCP server, when a call is blocked and you want to know why, or to make the defaults stricter or looser.

To edit the policies by hand, see [Policy configuration](/contracts).

## Example: data exfiltration to a public audience

Say `/appa-guide` added the Grain and GitHub batteries. Two of the policies they bring decide this example:

```toml
# Grain: meeting content was said by everyone in the meeting, including
# outside participants. It enters the session as untrusted, and anything
# built from it stays inside your organization.
[[policy.tool]]
name = "mcp/claude_ai_Grain/fetch_meeting_action_items"
delta = { trust = "suspicious", audience = ["internal"] }

# GitHub: a write needs trusted data that everyone who can read the
# repository may see. The battery looks up who that is for each call.
[[policy.tool]]
name = "mcp/github/issue_write"
annotator = "github.repository-readers"
```

Now start a protected session:

```sh
clappa
```

and ask Claude to turn a private meeting into a public issue:

```prompt
Create a public GitHub issue from the action items in my private meeting recording.
```

OpenAPPA stops Claude before it reads anything. Reading the meeting would make the session untrusted and limit its data to your organization, and after that nothing could go to a public repository. Claude gets the reason and the ways forward, and passes them on:

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](/images/claude-code-blocked-flow.png)

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
