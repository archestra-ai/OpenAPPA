---
title: Claude Code
category: Works with
order: 6
description: Start with one protected Claude Code session, then carry the same policy boundary across your agents.
---

OpenAPPA is designed for multiple agent surfaces. **Claude Code is simply the first demo:** the integration makes the policy visible in a familiar terminal and gives you a fast way to try it.

## Install the Claude Code demo

You need Claude Code and `curl`.

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa plugin install claude-code
```

The installer downloads the release binary for your Linux or macOS machine,
verifies its checksum, and places `appa` in `~/.local/bin`. It prints a hint
when that directory is not on your `PATH`. Set `APPA_VERSION` to a release tag
to install that release instead of the latest one. On Windows, unpack the zip
from the [releases page](https://github.com/archestra-ai/OpenAPPA/releases)
and run `appa plugin install claude-code` from it. From a checkout, `cargo
install --path appa-runtime --force` builds the binary instead of downloading
one.

The install prints progress while it selects the version, updates Claude Code,
and starts the runtime. A first install at a terminal asks one question: whether
the agent may report its own blocked calls (`--agent-yell` or
`--no-agent-yell` answers it for a script). A release binary installs the
version published for its tag; a checkout build installs its own version,
exported from the commit it was built from. If an earlier APPA runtime is
already running at the endpoint, the installer stops it. If an unrelated process
occupies the port, the installer reports it and leaves it running.

Initialization installs `clappa` beside `appa` so the short command works below.

The installed binary is the only host-side code. The install deploys it under
APPA's data directory and registers it in your user-level Claude Code
settings: one hook entry per session event, each naming that binary by its
absolute path. It registers the runtime's `appa` MCP
server in Claude Code's user scope, writes the `appa-guide` skill to your
user skills directory, and installs `clappa`, a protected way to start Claude
Code that also shows APPA's status line: the session's current trust and
audience. Rerunning the install updates only OpenAPPA-managed files. Custom hooks,
MCP servers, and skills are preserved. If the installer detects unmanaged `appa`
entries, it halts to avoid overwriting your setup.
It preserves an existing policy and never edits your own status line. It does not replace
`claude` or change how ordinary sessions start.

The install keeps a copy of every battery of its version beside the
config, in `batteries/`, a directory each install replaces. A first
install includes the `claude-code` battery, which gates the session's own
tools, as one line of the config's include list,
`batteries/claude-code/appa.toml`. Every other battery is yours to add: the
install reads the MCP servers Claude Code has configured and prints the
`appa battery install` command for the batteries that cover them.
`appa battery list` shows what is included; `appa battery install
<name>...` and `appa battery remove <name>` add and remove lines.

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

The skill begins with `appa describe`, which reports the current config,
included batteries, policy tools, referenced groups, and membership wiring.
The command does not guess at session-only tools or connector accounts; the
skill merges those from the active Claude session and asks when an identity or
boundary is unavailable.

Before it writes anything, the skill shows the full proposal for approval. The result is deterministic policy config: exact tool contracts, audience rules, and any annotator definitions the setup needs. The model helps draft the file; the OpenAPPA runtime enforces the file.

### Tool names in the policy

The policy names a tool by its canonical tool id, not by Claude Code's own spelling. The Claude Code adapter maps every raw tool spelling onto one id:

| Claude Code spells it | The policy names it |
|---|---|
| `mcp__<server>__<tool>`, split at the first `__` after `mcp__` | `mcp/<server>/<tool>` — `mcp__github__create_issue` is `mcp/github/create_issue` |
| A built-in tool: `Bash`, `Read`, `Edit`, `Agent`, … | `host/claude-code/<name>` — `host/claude-code/Bash` |
| `mcp__appa__execute_remedy_plan`, the remedy tool of the runtime's own `appa` MCP server | `appa/execute_remedy_plan`, which no policy declares |

Argument selectors keep their shape: `host/claude-code/Read(file_path:*)`. `Agent` and `Task` start a child trajectory; the runtime derives that from the adapter, so the policy does not declare it. The [Policy reference](/contracts#tool-names) has the grammar.

### How a session reaches the runtime

The hook entries the install wrote run `appa hook` on every event. The command translates Claude Code's hook JSON into the hook protocol's wire envelope (`protocol: 1`), posts it to the runtime's `/hook` endpoint, and translates the decision back into the hook answer Claude Code reads. The envelope carries the raw tool spelling and the session's own ids; the runtime derives the canonical tool id and prefixes the trajectory id with `cc:`. A hook that gets no answer blocks the action.

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

Installing OpenAPPA does not force every Claude Code session through it. Use `clappa` when you want the policy boundary. Use `claude` when you do not.

The hook entries live in your user settings. A project whose settings set `disableAllHooks` turns every hook off for its sessions, so `clappa` cannot protect a session in such a project.

:::claude-session-choice:::

## Use Claude Code as an annotator

OpenAPPA can also call the installed Claude Code CLI as a model builtin: an authority or a sanitizer binds `builtin = "claude-code"` under `[externals]`, and an annotator names it on its own declaration; `builtin = "llm"` serves the same three kinds through an API-key profile in `[externals.llm]`. This example declares an annotator. A tool that names an annotator carries no static semantics: the annotator produces the call's complete contract — its `delta`, `requires`, and `emits` — fresh for every released call.

```toml
[[annotator]]
name    = "classify-customer"
builtin = "claude-code"
hint    = "Use suspicious for customer data from unvetted sources."

[[tool]]
name        = "mcp/crm/get_customer"
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
# the executable; a service environment often strips PATH
command = "/usr/local/bin/claude"
# pin a model id here for stable classifications
model = "sonnet"
# the consult's own budget — a model call is slower than an endpoint
timeout_ms = 60000
# how many consults this deployment runs at once
max_concurrent = 4

[externals.authorities.operator]
builtin = "hitl"
```

The runtime uses the current user's Claude Code authentication. It starts one fresh safe-mode process per consult with no tools, hooks, project settings, or persisted session, in a temporary working directory, with every `APPA_*` environment variable removed. The system prompt carries OpenAPPA's label guide — the rule and the criteria for each trust and audience leaf, and worked examples spelled in the mandate's names — then the annotator's trusted `hint`, which overrides the guide, and its mandate vocabulary: the trust ranks, audiences, attention marks, and effect kinds an answer may use. The only user turn is the artifact selected by the annotator's `inputs` mapping: the complete call (`name`, declared `description`, and `arguments`) when it maps no inputs, or one value per mapped input. Nothing about the trajectory is sent: no current label, no history. The annotator answers one complete annotation, so it establishes the output label, the call's requirements, and its emitted effects in one consult. At most `max_concurrent` Claude consults, 4 by default, run at once across the runtime, all sessions included; an accepted reload applies a changed value to later consults of every session.

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
token_env      = "APPA_LLM_TOKEN"   # needed to open, except for ollama
timeout_ms     = 30000
max_concurrent = 4
```

## Uninstall

`appa plugin remove claude-code` takes OpenAPPA's entries out of your Claude Code profile and leaves the runtime, policy, and database in place. `--purge` also stops the runtime and deletes the policy, database, logs, and retained versions, so the next install starts from nothing:

```sh
appa plugin remove claude-code           # the Claude Code profile only
appa plugin remove claude-code --purge   # also stop the runtime and delete the deployment
rm -f ~/.local/bin/appa
cargo uninstall appa   # checkout builds only
sed -i.bak '/clappa/d' ~/.zshrc                # alias fallback only
```

`appa runtime stop` stops the runtime on its own. Both stop only a runtime that is your own `appa` process; a runtime at `APPA_RUNTIME_URL` is yours and is left alone.
