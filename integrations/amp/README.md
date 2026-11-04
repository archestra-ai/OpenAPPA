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

Review that policy before using it. It treats the orb's filesystem as part of
the trajectory and assumes the orb holds internal data under `*/private/*`:

- Reads of `*/private/*` and every shell result narrow the trajectory to
  internal. Public web requests and messages to other threads then stop.
- Each outbound connection through the [network proxy](#network-proxy-in-an-orb)
  forwards the orb's internal label, so no public host receives orb data.
- Untrusted web results and connections lower trust, which stops edits.

**This is an example policy, not a general-purpose coding policy.** Declare
your other tools, hosts trusted with internal data, or an Annotator through the
usual APPA configuration. An orb that holds only public data drops the internal
`delta` on shells and the `forwards` on outbound flows. Undeclared tools stop
with an operational refusal.

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

## Network proxy in an orb

An orb's filesystem is created for the thread and discarded with it, so amppa
treats it as part of the trajectory: a file write keeps its content inside the
trajectory, and no file tracking is needed. Data leaves the orb through Amp's
own tools, which the plugin checks, or over the network, which
`appa amp-proxy` checks.

Start the proxy beside the runtime, as a supervised service:

```sh
amp orb service start amppa-proxy --command "\"$(pwd)/target/debug/appa\" amp-proxy --listen 127.0.0.1:8789 --runtime http://127.0.0.1:8788 --thread $AMP_THREAD_ID"
```

Then route the agent's processes through it, for example in the orb's
environment variables:

```sh
HTTP_PROXY=http://127.0.0.1:8789
HTTPS_PROXY=http://127.0.0.1:8789
NO_PROXY=127.0.0.1,localhost
```

The proxy serves HTTP `CONNECT` tunnels, such as HTTPS, and plain `http://`
requests. Each connection is one call of `host/amp/appa_network` with the
string arguments `host` and `port`, in the thread's trajectory:

- `requires` is checked against the trajectory's label and the tool's
  `forwards` before the proxy opens the connection. A refused connection
  answers `403`, and its body carries the policy feedback and any remedy offer.
- The outcome is admitted after the connection opens and before the proxy
  relays a byte, so the host's `delta` narrows the trajectory before its answer
  reaches a file, a shell, or the model.
- Bytes reach only the checked host. The proxy resolves the name itself; a
  selector such as `host/amp/appa_network(host:*.example.com)` matches the
  lowercased name the client asked for.

HTTPS clients often hide a refused tunnel's body. `curl` reports only
`CONNECT tunnel failed, response 403`; retry with `curl -v` or read the proxy's
log to see the remedy offer.

Sending a message or a file to another thread with `send_thread_message` or
`create_thread` is an outbound flow, like an upload, and `requires` governs it.
The receiving thread starts without this thread's label, so the example policy
requires public data, and files attached through `media_paths` forward the orb's
label.

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
- Path selectors are not filesystem mediation. Aliases, symlinks and alternate
  read tools reach `*/private/*` without its selector; the example's shell
  `delta` covers shells only.
- The proxy checks the connections routed through it. Proxy variables are
  advisory: a program that ignores them, raw sockets, and DNS queries leave the
  orb directly. An egress firewall that admits only the proxy closes that path,
  but Amp's own executor shares the agent's user and process group in an orb and
  needs direct access to Amp, so such a firewall must exempt Amp's endpoints.
  An agent's shell can reach those endpoints too.
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
persistence across a runtime restart, files sent to another thread, and proxied
connections: refused before the host receives a request, and labelled before
the answer is relayed. Its MCP client follows remedy offers
directly; this does not test Amp's MCP discovery or human-approval UI. It makes
no inference calls. A successful `amp plugins exec` exit alone is not evidence
that the installed Amp version delivered the requested hook.
