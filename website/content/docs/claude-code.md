---
title: Claude Code
category: Integration
order: 5
description: Putting OpenAPPA between Claude Code and its tools.
---

This page describes how to run Claude Code with OpenAPPA. OpenAPPA integrates with Claude Code as a plugin; while it is in beta, the plugin ships a `clappa` command that runs Claude Code protected by OpenAPPA.

## Install

```sh
claude plugin marketplace add archestra-ai/OpenAPPA &&
  claude plugin install appa-runtime@appa &&
  claude "set up APPA"
```

This installs the OpenAPPA plugin into your Claude Code. While OpenAPPA is in beta, the plugin ships a `clappa` command that runs Claude Code protected by OpenAPPA. If you want to just keep using Claude Code, start it as usual — plain `claude` sessions stay untouched.

![A Claude Code session protected by OpenAPPA; the statusline shows the session's current trust and audience](/images/claude-code-protected-session.png)

`clappa` makes sure the [OpenAPPA runtime](#technical-details) is live before the session starts.

The plugin ships the `/appa-tool-sync` skill to guide you through the policy configuration: it finds your MCP servers and proposes a policy config for them.

## Uninstall

To uninstall OpenAPPA from Claude Code, ask Claude to remove the plugin and the [runtime](#technical-details) binaries:

```sh
claude "uninstall APPA: uninstall the appa-runtime plugin, remove the appa
plugin marketplace, delete appa-runtime-v2, clappa, and appa-statusline.sh
from ~/.local/bin, and in ~/.claude/settings.json delete the statusLine
entry only if it runs appa-statusline.sh — if it chains other commands,
remove just the appa part"
```

Optionally, also remove the policy file, the decision database, and the shell alias:

```sh
claude "finish removing APPA: delete the appa config and data directories,
and remove the clappa alias from my shell rc if present"
```

## Technical details

OpenAPPA runs on your machine as a single binary with a built-in web server. That server is what receives the Claude Code hooks.

The plugin registers Claude Code hooks. Each hook sends its event to the OpenAPPA runtime, and the OpenAPPA runtime answers before the action runs.

:::fig-claude-code-hooks:::
