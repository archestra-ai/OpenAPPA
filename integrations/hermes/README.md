# OpenAPPA for Hermes Agent

This plugin checks [Hermes Agent](https://github.com/NousResearch/hermes-agent)
tool calls and results against APPA information-flow policy. It runs as a
Hermes plugin and speaks the shared hook wire to the same Rust runtime as the
Claude Code and Amp integrations.

## Install from this checkout

Build the runtime and choose a persistent directory for the policy and database:

```sh
cargo build --locked -p appa
mkdir -p "$HOME/.config/appa/hermes"
test -e "$HOME/.config/appa/hermes/appa.toml" || \
  cp integrations/hermes/appa.toml "$HOME/.config/appa/hermes/appa.toml"
```

Review that policy before use. It labels reads under `*/private/*` as
`internal`, refuses public web requests after those reads, refuses edits after
untrusted web results, and permits only `pwd` through `terminal`. **It is an
example policy, not a general-purpose one.** Declare your other tools, install
batteries for your MCP servers, or bind an Annotator. Undeclared tools are
refused.

Start the runtime and leave it running:

```sh
./target/debug/appa runtime --adapter hermes --listen 127.0.0.1:8789 \
  --config "$HOME/.config/appa/hermes/appa.toml" \
  --db "$HOME/.config/appa/hermes/appa.db"
```

Do not expose this port: `/hook` trusts its local client. Keep the database
across restarts; it holds the trajectory labels.

Install and enable the plugin, and connect the remedy tool as an MCP server
named exactly `appa`, in `~/.hermes/config.yaml`:

```sh
hermes plugins install archestra-ai/OpenAPPA/integrations/hermes/plugin --enable
```

```yaml
mcp_servers:
  appa:
    url: http://127.0.0.1:8789/mcp
```

The MCP connection needs Hermes' `mcp` extra (`hermes-agent[mcp]`). Without
it, Hermes connects no MCP server and logs only "Background MCP discovery
completed with zero connected servers"; `hermes mcp test appa` shows the cause.

The plugin reads `APPA_HERMES_RUNTIME_URL` (default `http://127.0.0.1:8789`).
Start a new session for protected work: installing the plugin does not label an
existing conversation.

## Tool identities

| Hermes tool name | Policy identity |
| --- | --- |
| `read_file`, `terminal`, `delegate_task`, plugin tools | `host/hermes/<exact-name>` |
| `mcp__github__create_issue` | `mcp/github/create_issue` |
| `mcp__appa__execute_remedy_plan` | `appa/execute_remedy_plan` |

Hermes builds MCP names from server and tool names with every character
outside `[A-Za-z0-9_]` replaced by `_`, so a server named `google-workspace`
appears as `mcp/google_workspace/...`. Pass `--server` to
`appa battery install` when the names differ.

With tool search on, Hermes reports a bridged call under the underlying tool's
name. `tool_search` and `tool_describe` themselves reach the policy; the
example policy declares them.

## Remedy plans

A read that narrows the trajectory, such as a file under `*/private/*`, is
first blocked with an offer. The model calls `mcp__appa__execute_remedy_plan`
with the offered `offer_id`, then retries the call. Without the `appa` MCP
server, the model sees the offer but cannot execute it.

A `tool_input` sanitizer authorizes a call with rewritten arguments. The model
must retry with exactly those arguments; the plugin does not substitute them,
because Hermes' `modify` directive merges arguments and cannot remove one.

## Coverage and limits

- The trajectory is the Hermes session, `hermes:<session-id>`. Labels survive
  turns, plugin reloads and runtime restarts. `/new`, `/branch` and compaction
  handoffs start a new Hermes session, and with it a new trajectory.
- A subagent started by `delegate_task` is checked on its parent's trajectory:
  it inherits the parent's label, and what it reads narrows the parent. There
  is no child trajectory and no checked return.
- `execute_code` runs sandboxed calls to a few built-in tools. Each fires the
  hook without a session; the plugin resolves it through its task. A call
  whose task it has not seen is blocked.
- Hermes lets a call run when a hook raises. The plugin catches every failure,
  including an unreachable runtime, and blocks the call. A result of a call the
  runtime did not release is replaced with a withholding message.
- The plugin does not gate inference requests or the assistant's final text.
- Hermes, its plugin host and other installed plugins are trusted. Disabling
  the plugin removes protection.

## Verify

```sh
cargo test --locked -p appa-adapter-hermes -p appa-runtime-api
cargo build --locked -p appa
uv run --project integrations/hermes pytest integrations/hermes
```

The suite drives the plugin's hooks against a real runtime with disposable
data: audience and trust denials, remedy plans through the runtime's MCP
tool, output sanitization, subagent and sandbox calls, restart persistence,
and an unreachable runtime. It does not start Hermes or call a model;
`examples/hermes-calendar-leak` does both.
