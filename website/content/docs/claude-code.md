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
  claude /appa-setup
```

The setup installs the local runtime and adds `clappa`, a protected way to start Claude Code. It does not replace `claude` or change how your ordinary sessions start.

Setup asks once whether it may count the install. If you agree, it sends one event with the version, operating system and architecture. It sends nothing that identifies you or your machine, and it stores nothing to recognise you later. If you decline, or say nothing, it sends nothing. `APPA_TELEMETRY=0` refuses it without being asked. The runtime never reports anything at any point.

## 1. Teach OpenAPPA about your tools

:::claude-policy-timing:::

Start a normal, unprotected session, then run the policy setup skill:

```sh
claude
```

```text
/appa-guide init
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

OpenAPPA can also call the installed Claude Code CLI as a model builtin: `builtin = "claude-code"` serves an authority, a sanitizer, a cast, or a tool-level dynamic resolver, and `builtin = "llm"` serves the same four kinds through an API-key profile in `[externals.llm]`. This example binds a dynamic resolver. When a tool attaches the resolver with `uses`, the resolver directly owns every `delta` and `requires` destination in its `returns` declaration. The tool does not reference those results in its fields.

```toml
[[dynamic_resolver]]
name    = "classify-customer"
builtin = "claude-code"
returns = ["delta.trust", "delta.audience", "requires.trust", "requires.audience", "requires.attention"]

[[tool]]
name        = "get_customer"
description = "Reads one customer record."
uses        = [{ resolver = "classify-customer" }]

[[authority]]
name = "operator"

[authority.permits]
trust_below = "trusted"
attention = ["privacy-review"]

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.claude_code]
command = "/usr/local/bin/claude"   # the executable; a service environment often strips PATH
model = "sonnet"                    # pin a model id here for stable classifications
timeout_ms = 60000                  # the consult's own budget — a model call is slower than an endpoint

[externals.authorities.operator]
builtin = "hitl"
```

The runtime uses the current user's Claude Code authentication. It starts one fresh safe-mode process per consult with no tools, hooks, project settings, or persisted session, in a temporary working directory, with every `APPA_*` environment variable removed. The system prompt carries the resolver's declaration — its `returns`, the policy trust chain, and the attention marks that authorities name under `permits.attention`; the only user turn is the artifact: what the tool's `uses` entry selected — the complete call (name, description when declared, arguments), or one value per declared input. Nothing about the trajectory is sent: no current label, no history. The classifier answers every result its resolver declares, so it may establish the output label and demand a call-time constraint in one consult. Requirements support a trust floor, an audience `contains` list and `within` list, and a fresh attention mark selected from that policy-provided list; history remains static. If no authority names any mark, the only valid dynamic attention answer is an empty list. At most four Claude consults run at once.

A model dynamic resolver is a trusted classifier rather than a sandboxed policy authority: there is no additional ceiling on its answer, and argument-level prompt-injection resistance is best-effort. Bound as an authority, sanitizer, or cast, the same model rules only within that component's `permits` or `may_cast`, like any other implementation. Process errors, timeouts, invalid fields, and trust or attention values outside policy produce no answer: the call is not checked, nothing is recorded, and the failure surfaces operationally — never as a policy denial.

To serve the same resolver from an API key instead of the subscription, bind `builtin = "llm"` and add one profile per deployment:

```toml
[externals.dynamic.classify-customer]
builtin = "llm"

[externals.llm]
provider       = "anthropic"        # anthropic | openai | gemini | ollama
model          = "claude-sonnet-4-5"
token_env      = "APPA_LLM_TOKEN"   # required, except for ollama
timeout_ms     = 30000
max_concurrent = 4
```

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
