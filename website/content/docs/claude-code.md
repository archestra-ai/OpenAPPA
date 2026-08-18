---
title: Claude Code
category: Integration
order: 5
description: Putting OpenAPPA between Claude Code and its tools.
---

This page describes how to run Claude Code with OpenAPPA. OpenAPPA integrates with Claude Code as a plugin; while it is in beta, the plugin ships a `clappa` command that runs Claude Code protected by APPA.

## Install

```sh
claude plugin marketplace add archestra-ai/OpenAPPA
claude plugin install appa-runtime@appa
```

This installs the OpenAPPA plugin into your Claude Code. While OpenAPPA is in beta, the plugin ships a `clappa` command that runs Claude Code protected by OpenAPPA. If you want to just keep using Claude Code, start it as usual — plain `claude` sessions stay untouched.

![A Claude Code session protected by OpenAPPA; the statusline shows the session's current trust and audience](/images/claude-code-protected-session.png)

`clappa` makes sure the OpenAPPA runtime is live before the session starts. The runtime is what the Claude Code hooks call on every action.

The plugin ships the `/appa-tool-sync` skill to guide you through the policy configuration: it finds your MCP servers and proposes a policy config for them.

## Uninstall

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
rm ~/.local/bin/appa-runtime-v2
```

The policy file and the decision history stay on disk; delete them only if you want them gone.

## Technical details

OpenAPPA runs on your machine as a single binary with a built-in web server. That server is what receives the Claude Code hooks.

The plugin registers Claude Code hooks. Each hook sends its event to the APPA runtime, and the APPA runtime answers before the action runs.

:::fig-claude-code-hooks:::
