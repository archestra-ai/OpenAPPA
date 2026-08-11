# appa-runtime-v2

One process that sits between Claude Code and its tools and checks
every step before it happens: the user's prompt, each tool call, each
tool result, and each child agent's start and finish. If the process
does not answer, the action is blocked — silence never means yes.

Until the real decision engine is integrated, the process runs a mock
engine that permits everything and logs a warning saying so. The
wiring is real; the policy is not enforced yet.

## Quickstart

### 1. Build

```sh
cargo build -p appa-runtime-v2
```

### 2. Write the configuration

`appa.toml` holds the policy (empty until the real engine arrives) and
the settings for calls to outside services:

```toml
[policy]

[externals]
timeout_ms = 5000
max_body_bytes = 65536
```

### 3. Start the process

```sh
./target/debug/appa-runtime-v2 --config appa.toml --db appa.db
```

`curl localhost:8787/health` prints `ok` when it is up. The listener
accepts loopback addresses only. Useful flags: `--listen
127.0.0.1:<port>` for another port, `-v` to log each hook and
decision, `-vv` for full detail. `--adapter` picks the harness codec
the process loads; `claude-code` is the default and the only one
today.

`--mock offer` swaps the permissive mock for one that first blocks
every call with a narrowing offer; the session then accepts each call
through `execute_remedy_plan` before it runs. Slow and chatty, but it
shows the whole remedy loop working. Use a fresh `--db` path when
switching modes.

Start the process before the session. While it is down, every action
in a gated session is blocked, and that cannot be automated from
inside the session — the session's own commands are blocked too.

### 4. Install the plugin

For one session, no installation:

```sh
claude --plugin-dir /path/to/OpenAPPA/appa-runtime-v2/plugin
```

Permanently, through the marketplace manifest in this directory:

```sh
claude plugin marketplace add /path/to/OpenAPPA/appa-runtime-v2
claude plugin install appa-runtime@appa
```

The plugin brings all the hooks and the `execute_remedy_plan` MCP
server; it adds roughly zero tokens to a session. Note that a
permanent install gates **every** Claude Code session on the machine —
each one then needs the process running. For trying things out, prefer
`--plugin-dir`.

If the process listens on a port other than 8787, set
`APPA_RUNTIME_URL=http://127.0.0.1:<port>` in the session's
environment; the hooks and the MCP server both follow it.

### 5. See it work

Run `claude` and use it normally. With `-v` on the process you see
every hook arrive and every decision go out. The database shows what
was recorded:

```sh
sqlite3 appa.db 'SELECT id, parent, ended FROM trajectories;'
sqlite3 appa.db 'SELECT trajectory, tool, state FROM dispatches;'
```

## Things to know

- **A changed policy is a new deployment.** Edit `[policy]` and the
  old database refuses to open; use a fresh `--db` path.
- **Stopping the process blocks gated sessions.** That is the design,
  not a fault. Uninstall the plugin (`claude plugin uninstall
  appa-runtime@appa`) if you want ungated sessions back.
- **`docs/runtime.md`** is the contract the crates in this directory
  implement — the process (`runtime/`), the shared vocabulary
  (`api/`), and the Claude Code adapter (`adapters/claude-code/`) —
  and the directory's `CLAUDE.md` describes the layout, the
  mock-engine status, and how the real engine plugs in.
