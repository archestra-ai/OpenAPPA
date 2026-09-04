---
title: Claude Code
category: Integrations
order: 5
description: Start with one protected Claude Code session, then carry the same policy boundary across your agents.
---

OpenAPPA is designed for multiple agent surfaces. **Claude Code is simply the first demo:** the plugin makes the policy visible in a familiar terminal and gives you a fast way to try it.

## Install the Claude Code demo

You need Claude Code and `curl`.

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa init claude-code
```

The installer downloads the release binary for your Linux or macOS machine,
verifies its checksum, and places `appa` in `~/.local/bin`. It prints a hint
when that directory is not on your `PATH`. Set `APPA_VERSION` to a release tag
to install that release instead of the latest one. On Windows, unpack the zip
from the [releases page](https://github.com/archestra-ai/OpenAPPA/releases)
and run `appa init claude-code` from it. From a checkout, `cargo install --path
appa-runtime --force` builds the binary instead of downloading one.

Initialization prints progress while it resolves the matching plugin, updates
Claude Code, and starts the runtime. If a different APPA build already owns the
runtime endpoint, it asks before stopping that process; an unidentified
listener is never stopped automatically.

Initialization installs `clappa` beside `appa` so the short command works below.

The native `appa` command installs the runtime, the matching Claude Code
plugin, the statusline, and `clappa`, a protected way to start Claude Code. A
release binary resolves its plugin from its baked tag and artifact digest; a
checkout build resolves it from its baked commit and plugin-tree digest. Init
replaces an existing APPA installation instead of stacking another hook set. It
preserves an existing policy and custom statusline. It does not replace `claude`
or change how ordinary sessions start.

## 1. Teach OpenAPPA about your tools

:::claude-policy-timing:::

Start a protected session, then run the policy setup skill:

```sh
clappa
```

```text
/appa-guide init
```

The skill inspects the MCP servers and tools available to Claude Code. It uses their declared purpose to identify what they read and which actions can send data outside the session. When a data boundary is unclear, it asks you one focused question.

Before this sync, a fresh installation routes unnamed tools through a bounded Claude annotator. The fallback fails closed and keeps newly installed tools from becoming an immediate configuration outage; exact contracts and maintained batteries produced by the skill take precedence over it.

It begins with `appa describe`, which reports the current config,
included batteries, policy tools, referenced groups, and membership wiring.
The command does not guess at session-only tools or connector accounts; the
skill merges those from the active Claude session and asks when an identity or
boundary is unavailable.

Before it writes anything, the skill shows the full proposal for approval. The result is deterministic policy config: exact tool contracts, audience rules, and any annotator definitions the setup needs. The model helps draft the file; the OpenAPPA runtime enforces the file.

## 2. Try a flow that should be blocked

Start a new protected Claude Code session with the updated policy:

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

## The working directory of a call

Claude Code reports `cwd` on every tool-call hook: the directory a Bash command starts in. A `cd` that persists in one Bash call moves it. A `cd` to a directory outside the project does not persist: Claude Code resets the shell to the project root for the next call. A failed call leaves it unchanged. One command can still change directory before it acts, so an annotator that decides by directory also reads the command text for `cd`, `pushd`, and `-C`. Every annotation consult carries the reported directory as `artifact.cwd`, as written, or `null` when none was reported. See [Annotators](/contracts#annotators).

## Use Claude Code as an annotator

OpenAPPA can also call the installed Claude Code CLI as a model builtin: an authority or a sanitizer binds `builtin = "claude-code"` under `[externals]`, and an annotator names it on its own declaration; `builtin = "llm"` serves the same three kinds through an API-key profile in `[externals.llm]`. This example declares an annotator. A tool that names an annotator carries no static semantics: the annotator produces the call's complete contract — its `delta`, `requires`, and `emits` — fresh for every released call.

```toml
[[annotator]]
name    = "classify-customer"
builtin = "claude-code"
hint    = "Use suspicious for customer data from unvetted sources."

[[tool]]
name        = "get_customer"
description = "Reads one customer record."
annotator   = "classify-customer"

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

The runtime uses the current user's Claude Code authentication. It starts one fresh safe-mode process per consult with no tools, hooks, project settings, or persisted session, in a temporary working directory, with every `APPA_*` environment variable removed. The system prompt carries the annotator's trusted `hint` and mandate vocabulary: the trust ranks, literal readers, attention marks, and effect kinds an answer may use. The only user turn is the artifact selected by the annotator's `inputs` mapping: the complete call (`name`, declared `description`, and `arguments`) when it maps no inputs, or one value per mapped input, beside `cwd`, the working directory Claude Code reported for the call. Nothing about the trajectory is sent: no current label, no history. The annotator answers one complete annotation, so it establishes the output label, the call's requirements, and its emitted effects in one consult. At most four Claude consults run at once.

A model annotator is a trusted classifier rather than a sandboxed policy authority: it rules the whole contract of every call it covers, bounded only by its declared mandate, and argument-level prompt-injection resistance is best-effort. Bound as an authority or sanitizer, the same model rules only within that component's `permits`, like any other implementation. Process errors, timeouts, invalid fields, and values outside the mandate produce no answer: the call is not judged, nothing is recorded, and the failure surfaces operationally — never as a policy denial.

To serve the same annotator from an API key instead of the subscription, declare `builtin = "llm"` and add one profile per deployment:

```toml
[[annotator]]
name    = "classify-customer"
builtin = "llm"
hint    = "Use suspicious for customer data from unvetted sources."

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
pkill -f 'appa runtime'
rm -rf ~/.local/share/appa/bin ~/.local/share/appa/deployments ~/.local/share/appa/cache
rm -f ~/.local/bin/appa ~/.local/bin/clappa ~/.local/bin/appa-statusline.sh
rm -f ~/.cargo/bin/clappa && cargo uninstall appa   # checkout builds only

# drop the statusline entry appa init wrote, and keep one of your own:
jq 'if (.statusLine.command? // "") | test("appa-statusline") then del(.statusLine) else . end' \
  ~/.claude/settings.json > ~/.claude/settings.json.new &&
  mv ~/.claude/settings.json.new ~/.claude/settings.json

# optional — also remove the policy, database, and alias:
rm -rf ~/.config/appa ~/.local/share/appa      # Linux
rm -rf ~/Library/"Application Support/appa"    # macOS
sed -i.bak '/clappa/d' ~/.zshrc                # alias fallback only
```
