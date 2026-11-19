# amppa — OpenAPPA for Amp

amppa checks Amp tool calls and results against APPA information-flow policy.
It runs as an Amp TypeScript plugin, backed by the same Rust runtime as clappa.
It works in an orb or with a local Amp executor. Orb isolation and APPA flow
checks protect different boundaries; neither requires the other.

## Install from this checkout

V1 is source-distributed; `appa plugin install amp` is not supported. You need
Amp with `tool.call` and result-replacement hooks, plus Rust to build APPA.
SafeBash, the shell the example policy runs commands in, needs Anthropic's
sandbox runtime `srt`, which needs bubblewrap on Linux.

From the OpenAPPA checkout:

```sh
cargo build --locked -p appa
sudo apt-get install -y bubblewrap
npm install -g @anthropic-ai/sandbox-runtime
```

The policy runs `appa file-tracker`, so put the built `appa` on the runtime's
`PATH`, and `srt` on the plugin's. The plugin runs `appa` from its own `PATH`;
set `APPA_AMP_BINARY` to its absolute path to use another one.

Choose a persistent directory for this deployment's policy and database. For
example, copy the starting policy without replacing an existing configuration:

```sh
mkdir -p "$HOME/.config/appa/amp"
test -e "$HOME/.config/appa/amp/appa.toml" || \
  cp integrations/amp/appa.toml "$HOME/.config/appa/amp/appa.toml"
```

Review that policy before using it. It treats the orb's filesystem as part of
the trajectory, and labels what each call reads from it
([Files and the shell](#files-and-the-shell-in-an-orb)):

- A read of `.env*`, `~/.ssh/*` or another credential narrows the trajectory to
  `self`. Public web requests, hosts other than package registries, and
  messages to other threads then stop.
- Shell commands run in SafeBash, confined to the files and hosts each call
  declares. Amp's own `shell_command` is refused.
- Each connection from a process outside SafeBash crosses the
  [network proxy](#network-proxy-in-an-orb). It forwards the most restricted
  label the orb's files can carry, so only package registries receive one.
- Untrusted web results lower trust, which stops patches and writes to
  instructions, hooks and shell profiles.

**This is an example policy, not a general-purpose coding policy.** Declare
your other tools, hosts trusted with the orb's data, or more file-tracker path
rules through the usual APPA configuration. Undeclared tools stop with an
operational refusal.

Start the runtime in a separate terminal, leaving it running:

```sh
./target/debug/appa runtime --adapter amp --listen 127.0.0.1:8788 \
  --config "$HOME/.config/appa/amp/appa.toml" \
  --db "$HOME/.config/appa/amp/appa.db" \
  --batteries-dir "$(pwd)/marketplace/batteries"
```

The policy includes the `file-tracker` battery, which `--batteries-dir` finds in
the checkout.

In an **Amp orb**, use a supervised service instead. Run this from the checkout:

```sh
amp orb service start amppa-runtime --command "\"$(pwd)/target/debug/appa\" runtime --adapter amp --listen 127.0.0.1:8788 --config \"$HOME/.config/appa/amp/appa.toml\" --db \"$HOME/.config/appa/amp/appa.db\" --batteries-dir \"$(pwd)/marketplace/batteries\""
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

## Files and the shell in an orb

An orb's filesystem is created for the thread and discarded with it, so amppa
treats it as part of the trajectory: what the thread writes stays in its label,
and no file label needs to outlive the orb. What the orb starts with still needs
a label, and so does each command, so the example policy includes the
`file-tracker` battery, the one clappa uses. Its `[aliases.tool.file-tracker]`
map binds the battery's `read`, `write`, `edit`, `shell` and `unmediated` tools
to Amp's own, as the `claude-code` battery does for Claude Code's:

- `read` is `Read` and `view_media`, `write` is `create_file`, and `edit` is
  `edit_file`. A `view_media` of an `http(s)://` URL takes a root rule first:
  it requires public data and lowers trust.
- `Read`, `create_file`, `edit_file` and a local `view_media` carry the labels
  of the files they name. The file-tracker's built-in path rules make
  credentials `self`, require a trusted session to write instructions, hooks,
  shell profiles and `.agents/setup`, and refuse writes to `.amp/plugins/*` and
  `.amp/settings.json`. Add your own rules in `~/.config/appa/file-tracker.toml`.
- `shell` is `appa_safebash`, which amppa registers. It runs one command in `srt`. The call
  declares `input_files`, `output_files` and `network_hosts`, and the command
  reads, writes and reaches only those. A credential inside a declared
  directory stays hidden unless the call names it. The result carries the
  labels of what the call declared, so a command that read only `README.md`
  leaves the trajectory public.
- A call that names a host requires public data and lowers trust. A call whose
  hosts all belong to one package registry (npm, Yarn, the Go proxy,
  crates.io, PyPI) runs as a call without hosts does.
- `unmediated` is `shell_command`, `shell_command_status` and `Bash`. They are
  refused, because they
  read and send files out of the file-tracker's sight. Remove them from the
  agent's tools to save the model a call.

Without the include, the map binds nothing, and Amp's file tools and shells
have no rule and are refused. The root also widens the battery's
`file-tracker` Annotator to `marks = ["human-approval"]`, so a write to
`.amp/plugins/*` is a policy denial rather than an Annotator failure.

The file-tracker's recorder settles each write's reservation. Its store lives
in the orb, under `$XDG_STATE_HOME/appa`, and goes with it.

The agent pays for this in declarations. A command that under-declares fails,
and amppa adds a hint to its result that names what to declare, such as an
output's directory or a repository's `.git/`; the agent retries with it. The
hints are clappa's. A command cannot outlive its call, so servers that must keep running
need another way to start.

## Network proxy in an orb

Data leaves the orb through Amp's own tools, which the plugin checks, through
SafeBash, whose sandbox admits only the hosts each call declares, or through
some other process, such as a server Amp or an MCP server starts. That last
path is what `appa amp-proxy` checks.

Start the proxy beside the runtime, as a supervised service:

```sh
amp orb service start amppa-proxy --command "\"$(pwd)/target/debug/appa\" amp-proxy --listen 127.0.0.1:8789 --runtime http://127.0.0.1:8788 --thread $AMP_THREAD_ID"
```

Then route the orb's processes through it, for example in the orb's
environment variables. amppa leaves these out of SafeBash's environment: its
calls were checked with their hosts, and its sandbox runs its own proxy.

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

A process outside SafeBash can send any file in the orb, so the example
policy's connections forward `self`, the most restricted label the path rules
give. Only package registries, declared ahead of that rule, receive a
connection.

Sending a message or a file to another thread with `send_thread_message` or
`create_thread` is an outbound flow, like an upload, and `requires` governs it.
The receiving thread starts without this thread's label, so the example policy
requires public data, and files attached through `media_paths` forward the orb's
label.

## Contracts and remedy plans

| Amp tool spelling | Policy identity |
| --- | --- |
| `Read`, `shell_command`, `apply_patch`, etc. | `host/amp/<exact-name>` |
| Plugin tool's hook name, such as `appa_safebash` | `host/amp/<exact-name>` |
| A connection through `appa amp-proxy` | `host/amp/appa_network` |
| `mcp__github__create_issue` | `mcp/github/create_issue` |
| `mcp__appa__execute_remedy_plan` | `appa/execute_remedy_plan` |

Tool names and argument fields vary by Amp mode. Use the names the hooks report;
`amp plugins show-agent-options --json` lists the available builtin names.
Canonical identities in policy avoid dependence on tool discovery.

The first read of a `.env` file offers a plan to accept a narrower audience.
The agent calls `mcp__appa__execute_remedy_plan` with the offered `offer_id`,
then retries the read. Executing the plan does not perform the read itself.
Subsequent public web requests remain blocked because this trajectory now
carries `self` data. Without the MCP connection above, the
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
- A file the path rules do not name is public until a call reads labelled
  data. `apply_patch` names its files inside its text, so the file-tracker does
  not see them; the example requires a trusted session instead.
- A package's install script can send what its command reads to the registry
  the command declares.
- The proxy checks the connections routed through it. Proxy variables are
  advisory for processes outside SafeBash: a program that ignores them, raw
  sockets, and DNS queries leave the orb directly. An egress firewall that
  admits only the proxy closes that path, but Amp's own executor shares the
  agent's user in an orb and needs direct access to Amp, so such a firewall must
  exempt Amp's endpoints.
- An idle orb can be deleted. The next one restores the thread's git changes,
  but not the runtime's database or the file-tracker's store, so it starts the
  thread without its label. Keep the database outside the orb's disposable
  state, or start a new thread for protected work.
- Amp, its plugin host, the runtime, and other installed plugins are trusted.
  Disabling amppa removes protection. Plugin crash/unload behavior and hook
  ordering between plugins are host limitations, not guarantees supplied here.

To uninstall, remove only the copied `amppa.ts` and reload plugins. Stop the
runtime separately (`amp orb service stop amppa-runtime` in an orb). Keep the
policy and database if you intend to resume protected threads later.

## Verify

From the repository root, with Bun installed:

```sh
cargo test --locked -p appa-adapter-amp -p appa-runtime-api -p appa-file-tracker
cargo build --locked -p appa
bun test integrations/amp
```

The Bun suite drives the exported plugin against mock replies and a real
runtime with disposable data. It checks audience and trust denials, successful
reads, output sanitization, failures, cancellation, parallel result correlation,
persistence across a runtime restart, files sent to another thread, and proxied
connections: refused before the host receives a request, and labelled before
the answer is relayed. It checks that SafeBash calls carry the labels of the
files they declare, and that package registries stay reachable after a
credential read. Where `srt` is installed, it runs one SafeBash command and
checks that an undeclared credential stays hidden. Its MCP client follows remedy offers
directly; this does not test Amp's MCP discovery or human-approval UI. It makes
no inference calls. A successful `amp plugins exec` exit alone is not evidence
that the installed Amp version delivered the requested hook.
