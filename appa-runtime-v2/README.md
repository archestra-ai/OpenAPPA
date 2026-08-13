# appa-runtime-v2

One process that sits between Claude Code and its tools and checks
every step before it happens: the user's prompt, each tool call, each
tool result, and each child agent's start and finish. If the process
does not answer, the action is blocked — silence never means yes.

The process runs the real APPA decision engine: the `[policy]` table
in `appa.toml` compiles into the engine's registry at startup, and a
policy the deployment cannot honor refuses to start. Every decision is
persisted as engine facts in the SQLite log, and a reopened database
re-validates its persisted log before it is trusted.

## Quickstart

### 1. Build

```sh
cargo build -p appa-runtime-v2
```

### 2. Write the configuration

`appa.toml` holds the policy — the dialect the policy-review guide
documents, nested under `[policy]` — and the settings for calls to
outside services. A minimal file that releases one tool:

```toml
[policy]
version = 1

[[policy.tool]]
name = "Bash"

[externals]
timeout_ms = 5000
max_body_bytes = 65536
```

Implementation bindings (authority and sanitizer endpoints) live in
`[externals]`, never inline in the policy.

`integrations/claude-code/examples/claude-code.appa.toml` is a
complete starting point: it releases every built-in Claude Code tool
with the neutral annotation and marks the web tools' results
suspicious. Pass its path directly to `--config`.

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

Start the process before the session. While it is down, every action
in a gated session is blocked, and that cannot be automated from
inside the session — the session's own commands are blocked too.

### 4. Gate a session

The Claude Code integration — the plugin, the statusline, the example
policies, and the install and uninstall instructions — lives in
[`integrations/claude-code/`](../integrations/claude-code/README.md).

### 5. See it work

Run a gated session and use it normally. With `-v` on the process you
see every hook arrive and every decision go out. The database shows
what was recorded:

```sh
sqlite3 appa.db 'SELECT id, parent, ended FROM trajectories;'
sqlite3 appa.db 'SELECT trajectory, tool, state FROM dispatches;'
```

## Things to know

- **A changed policy is a new deployment.** Edit `[policy]` and the
  old database refuses to open; use a fresh `--db` path.
- **Stopping the process blocks gated sessions.** That is the design,
  not a fault. Uninstall the plugin if you want ungated sessions back.
- **`docs/runtime.md`** is the contract the crates in this directory
  implement — the process (`runtime/`), the shared vocabulary
  (`api/`), and the Claude Code adapter (`adapters/claude-code/`) —
  and the directory's `CLAUDE.md` describes the layout and the
  engine-boundary status.
