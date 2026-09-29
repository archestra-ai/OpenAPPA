# amppa — OpenAPPA for Amp

amppa checks Amp tool calls and results against APPA information-flow policy.
It runs as an Amp TypeScript plugin, backed by the same Rust runtime as clappa.
It works in an orb or with a local Amp executor. Orb isolation and APPA flow
checks protect different boundaries; neither requires the other.

## Install from this checkout

V1 is source-distributed; `appa plugin install amp` is not supported. You need
Amp with `tool.call` and result-replacement hooks, plus Rust to build APPA.

From the OpenAPPA checkout:

```sh
cargo build --locked -p appa
```

Choose a persistent directory for this deployment's policy and database. For
example, copy the starting policy without replacing an existing configuration:

```sh
mkdir -p "$HOME/.config/appa/amp"
test -e "$HOME/.config/appa/amp/appa.toml" || \
  cp integrations/amp/appa.toml "$HOME/.config/appa/amp/appa.toml"
```

Review that policy before using it. It labels reads whose `path` matches
`*/private/*` as internal, refuses public web requests after those reads, and
refuses trusted-only edits after untrusted web results. It permits only `pwd`
through shell tools. **This is an example policy, not a general-purpose coding
policy.** Declare your other tools or bind an Annotator through the usual APPA
configuration. Undeclared tools stop with an operational refusal.

Start the runtime in a separate terminal, leaving it running:

```sh
./target/debug/appa runtime --adapter amp --listen 127.0.0.1:8788 \
  --config "$HOME/.config/appa/amp/appa.toml" \
  --db "$HOME/.config/appa/amp/appa.db"
```

In an **Amp orb**, use a supervised service instead. Run this from the checkout:

```sh
amp orb service start amppa-runtime --command "\"$(pwd)/target/debug/appa\" runtime --adapter amp --listen 127.0.0.1:8788 --config \"$HOME/.config/appa/amp/appa.toml\" --db \"$HOME/.config/appa/amp/appa.db\""
```

Do not expose this runtime through a portal: `/hook` trusts its local client
and does not authenticate callers. Keep the database across restarts; it holds
the trajectory labels. Each fresh orb needs the runtime and plugin installed.

**Connect the remedy tool before starting protected work.** Merge this into the
target project's `.amp/settings.json`, preserving its other settings:

```json
{
  "amp.mcpServers": {
    "appa": { "url": "http://127.0.0.1:8788/mcp" }
  }
}
```

Approve the workspace MCP server in Amp, or run `amp mcp approve appa` from that
project. Reload MCP if the session is already open. This is an executor-local
connection, including in orbs; do not register this loopback address as a remote
MCP definition on ampcode.com. Keep the exact server name `appa`.

Copy `amppa.ts` into the **target project's** `.amp/plugins/` directory:

```sh
mkdir -p /path/to/your-project/.amp/plugins
cp -i integrations/amp/amppa.ts /path/to/your-project/.amp/plugins/amppa.ts
```

Reload plugins in that project's Amp session, or start a new session. Protection
applies whenever this plugin is loaded. Start a **new thread** for protected
work: installation does not retroactively label an existing conversation.
The default endpoint is `http://127.0.0.1:8788`; set `APPA_AMP_RUNTIME_URL` in the
plugin process's environment before loading it to use another address.

For local CLI execute mode, use `amp --plugin-ready-timeout 10 -x 'your task'`
so Amp waits for plugin initialization. The plugin cancels a new turn if its
runtime cannot be reached. Calls fail closed on connection errors, timeouts,
invalid replies, or runtime refusals; unchecked results are replaced with a
withholding message.

## Contracts and remedy plans

| Amp tool spelling | Policy identity |
| --- | --- |
| `Read`, `shell_command`, `apply_patch`, etc. | `host/amp/<exact-name>` |
| Plugin tool's hook name | `host/amp/<exact-name>` |
| `mcp__github__create_issue` | `mcp/github/create_issue` |
| `mcp__appa__execute_remedy_plan` | `appa/execute_remedy_plan` |

Tool names and argument fields vary by Amp mode. Use the names the hooks report;
`amp plugins show-agent-options --json` lists the available builtin names.
Canonical identities in policy avoid dependence on tool discovery.

The first read of `/workspace/private/customer.txt` offers a plan to accept a
narrower audience. The agent calls `mcp__appa__execute_remedy_plan` with the
offered `offer_id`, then retries the read. Executing the plan does not perform
the read itself. Subsequent public web requests remain blocked because this
trajectory now carries internal data. Without the MCP connection above, the
agent can see the offer but cannot execute it.

Confined results can also be sanitized before delivery when the policy declares
a sanitizer. The included example declares none; the integration test adds its
own email-redaction policy to verify that path.

The corresponding direct MCP call passes through the control hook. Other
servers' lookalike tools are checked normally. Automatic MCP configuration and
human-approval UI integration are not part of v1. In particular, nested MCP
calls inside `code_exec` must not be assumed to produce individual hooks; the
example policy leaves `code_exec` undeclared.

## Coverage and limits

- Calls carry Amp's opaque `toolUseID`, so parallel calls and out-of-order
  results remain correlated. Labels live under `amp:<thread-id>` in APPA's
  database and survive turns, compaction, plugin reload, and runtime restart.
- Failed tools report both error text and partial output for checking.
  Cancelled tools report an indeterminate outcome and withhold partial output.
- V1 covers the tool events Amp delivers to this plugin. It does not gate
  inference requests, assistant prose, transcript sharing, or uploads outside
  those events. `agent.end` settles calls; it is not an assistant-response gate.
- Delegation is an ordinary tool call in v1. Child threads do not inherit APPA
  labels, and child returns and transferred files have no cross-thread
  provenance guarantee. Add delegation contracts only with that limitation
  understood; the example policy declares none.
- Path selectors are not filesystem mediation. Aliases, symlinks, alternate
  read tools, arbitrary shell programs, and nested calls need their own policy
  coverage. The plugin is not OS-level taint tracking or network confinement.
- Amp, its plugin host, the runtime, and other installed plugins are trusted.
  Disabling amppa removes protection. Plugin crash/unload behavior and hook
  ordering between plugins are host limitations, not guarantees supplied here.

To uninstall, remove only the copied `amppa.ts` and reload plugins. Stop the
runtime separately (`amp orb service stop amppa-runtime` in an orb). Keep the
policy and database if you intend to resume protected threads later.

## Verify

From the repository root, with Bun installed:

```sh
cargo test --locked -p appa-adapter-amp -p appa-runtime-api
cargo build --locked -p appa
bun test integrations/amp
```

The Bun suite drives the exported plugin against mock replies and a real
runtime with disposable data. It checks audience and trust denials, successful
reads, output sanitization, failures, cancellation, parallel result correlation,
and persistence across a runtime restart. Its MCP client follows remedy offers
directly; this does not test Amp's MCP discovery or human-approval UI. It makes
no inference calls. A successful `amp plugins exec` exit alone is not evidence
that the installed Amp version delivered the requested hook.
