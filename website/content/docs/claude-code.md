---
title: Claude Code
category: Integration
order: 5
description: Putting OpenAPPA between Claude Code and its tools.
---

This page describes how to run Claude Code with OpenAPPA. OpenAPPA integrates with Claude Code as a plugin; while it is in beta, the plugin ships a `clappa` command that runs Claude Code protected by OpenAPPA.

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](/images/claude-code-blocked-flow.png)

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

To uninstall OpenAPPA from Claude Code, remove the plugin, stop the [runtime](#technical-details), and remove its binaries:

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
pkill -f appa-runtime-v2
rm ~/.local/bin/appa-runtime-v2 ~/.local/bin/clappa ~/.local/bin/appa-statusline.sh

# optional — also remove the policy, database, and alias:
rm -rf ~/.config/appa ~/.local/share/appa      # Linux
rm -rf ~/Library/"Application Support/appa"    # macOS
sed -i.bak '/clappa/d' ~/.zshrc                # alias fallback only
```

## Technical details

OpenAPPA runs on your machine as a single binary with a built-in web server. That server is what receives the Claude Code hooks.

The plugin registers Claude Code hooks. Each hook sends its event to the OpenAPPA runtime, and the OpenAPPA runtime answers before the action runs.

:::fig-claude-code-hooks:::
