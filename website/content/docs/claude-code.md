---
title: Claude Code
category: Integration
order: 5
description: Start with one protected Claude Code session, then carry the same policy boundary across your agents.
---

OpenAPPA is designed for multiple agent surfaces. **Claude Code is simply the first demo:** the plugin makes the policy visible in a familiar terminal and gives you a fast way to try it.

## Install the Claude Code demo

```sh
claude plugin marketplace add archestra-ai/OpenAPPA &&
  claude plugin install appa-runtime@appa &&
  claude "set up APPA"
```

The setup installs the local runtime and adds `clappa`, a protected way to start Claude Code. It does not replace `claude` or change how your ordinary sessions start.

## 1. Teach OpenAPPA about your tools

:::claude-policy-timing:::

Start a normal, unprotected session, then run the policy setup skill:

```sh
claude
```

```text
/appa-tool-sync
```

The skill inspects the MCP servers and tools available to Claude Code. It uses their declared purpose to identify what they read and which actions can send data outside the session. When a data boundary is unclear, it asks you one focused question.

Before it writes anything, the skill shows the full proposal for approval. The result is deterministic policy config: exact tool contracts, audience rules, and any resolver definitions the setup needs. The model helps draft the file; the OpenAPPA runtime enforces the file.

## 2. Try a flow that should be blocked

Leave the setup session and start Claude Code with OpenAPPA:

```sh
clappa
```

Now ask for an explicit transfer from a private source to a public destination. For example:

```text
Create a public GitHub issue from the action items in my private meeting recording.
```

Claude can read the meeting, but that read narrows who may receive the resulting data. When it later proposes the public GitHub write, OpenAPPA checks the accumulated data against the destination and blocks the flow before the tool runs.

The refusal is not a generic warning. It names the policy conflict and can offer a valid path forward, such as using a permitted destination, applying a configured sanitizer, or asking an authorized reviewer.

![A protected Claude Code session refuses to post content from a private meeting recording to a public GitHub repo, and explains why](/images/claude-code-blocked-flow.png)

## Choose protection per session

Installing the plugin does not force every Claude Code session through OpenAPPA. Use `clappa` when you want the policy boundary. Use `claude` when you do not.

:::claude-session-choice:::

## Use Claude Code as a dynamic classifier

OpenAPPA can also call the installed Claude Code CLI as a built-in tool-level dynamic resolver:

```toml
[[dynamic_resolver]]
name = "classify-customer"
builtin = "claude-code"

[[tool]]
name = "get_customer"
resolvers = [
  { resolver = "classify-customer", returns = { delta = ["trust", "audience"], requires = ["trust", "audience", "attention"] } }
]

[[authority]]
name = "operator"

[authority.mandate]
can_cover_trust_to = "trusted"
attends = ["privacy-review"]

[externals]
timeout_ms = 60000
max_body_bytes = 65536

[externals.authorities.operator]
builtin = "hitl"
```

The runtime uses the current user's Claude Code authentication and fixed Sonnet configuration. It starts a fresh safe-mode process with no tools, hooks, project settings, or persisted session. The classifier sees the complete canonical arguments, current trust and audience, the policy trust chain, the attention marks named by authority mandates, and existing static attention requirements. It may return output `delta` and call-time `requires` together. Dynamic requirements support a trust floor, an audience `includes` floor and `cap` ceiling, and a fresh attention mark selected from that policy-provided list; history remains static. If no authority attends any mark, the only valid dynamic attention answer is an empty list.

This is a POC trusted classifier rather than a sandboxed policy authority: there is no additional ceiling on its answer, and argument-level prompt-injection resistance is best-effort. Process errors, timeouts, invalid fields, and trust or attention values outside policy fail closed. The older single-argument audience resolver syntax remains HTTP-only.

## Uninstall

To uninstall OpenAPPA from Claude Code, remove the plugin, stop the local runtime, and remove its binaries:

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
pkill -f appa-runtime
rm ~/.local/bin/appa-runtime ~/.local/bin/clappa ~/.local/bin/appa-statusline.sh

# drop the statusline entry the setup wrote, and keep one of your own:
jq 'if (.statusLine.command? // "") | test("appa-statusline") then del(.statusLine) else . end' \
  ~/.claude/settings.json > ~/.claude/settings.json.new &&
  mv ~/.claude/settings.json.new ~/.claude/settings.json

# optional — also remove the policy, database, and alias:
rm -rf ~/.config/appa ~/.local/share/appa      # Linux
rm -rf ~/Library/"Application Support/appa"    # macOS
sed -i.bak '/clappa/d' ~/.zshrc                # alias fallback only
```
