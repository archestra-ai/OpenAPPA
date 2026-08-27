# appa-runtime

One process that sits between Claude Code and its tools and checks
every step before it happens: the user's prompt, each tool call, each
tool result, and each child agent's start and finish. If the process
does not answer, the action is blocked — silence never means yes.

The process runs the real APPA decision engine: the `[policy]` table
in `appa.toml` compiles into the engine's registry at startup, and a
policy the deployment cannot honor refuses to start. Every decision is
persisted as engine facts in the SQLite log, and a reopened database
re-validates its persisted log before it is trusted.

## Install

The Claude Code adapter requires the `claude` command and `curl`.

From a source checkout, install `appa`, then initialize the harness adapter:

```sh
cargo install --path appa-runtime --force
appa init claude-code
```

From an extracted release archive on POSIX:

```sh
install -m 755 appa ~/.local/bin/
appa init claude-code --source ./claude-code
```

From a checkout, `init` installs that checkout's marketplace. From an extracted
release it uses the marketplace shipped beside the binaries. Otherwise it uses
`archestra-ai/OpenAPPA`. It replaces an existing APPA plugin instead of stacking
another copy, deploys the same `appa` build for its internal `runtime` command, creates `clappa`, installs the
statusline unless Claude already has a custom one, preserves an existing policy,
and starts the runtime. The [Claude Code integration guide](../integrations/claude-code/README.md)
covers the complete flow.

## Development quickstart

### 1. Build

```sh
cargo build -p appa
```

### 2. Prepare the configuration

`appa.toml` holds the policy — the dialect the policy-review guide
documents, nested under `[policy]` — and the settings for calls to
outside services. If the configured path does not exist at startup, the
process creates it from the complete Claude Code starting policy. It
never replaces an existing file.

You can instead write the file before startup. A minimal configuration
that releases one tool:

```toml
[policy]
version = 1

[[policy.tool]]
name = "Bash"

[externals]
timeout_ms = 5000
max_body_bytes = 65536
```

Implementation bindings live in `[externals]`: one
`[externals.<kind>.<name>]` entry per registered authority, sanitizer,
cast, or membership resolver, and per dynamic resolver that names no
`builtin` on its declaration — bound to a `url` or a `command`, or, for
authorities and sanitizers, a `builtin` (stock, a model transport, or a
module from `--modules-dir`); a cast's `builtin` is a model transport
only. A dynamic resolver names a model transport on its own
`[[dynamic_resolver]]` declaration with `builtin = "claude-code"` or
`builtin = "llm"`. An authority may stay unbound and then returns no
answer; every other registered name needs its entry, and an entry no
declaration registers refuses to start.

`integrations/claude-code/examples/claude-code.appa.toml` is a
complete starting point: it releases every built-in Claude Code tool
with the neutral annotation and marks the web tools' results
suspicious. Pass its path directly to `--config`.

### 3. Start the process

Before starting it, the installed `appa` CLI can describe the facts a
configuring human or agent may rely on:

```sh
appa describe --config appa.toml
```

`describe` is read-only. It works for a missing, malformed, or incomplete
config and never creates the default config or database. It reports config
state, includes and battery names, effective policy tools, referenced groups,
and membership wiring. Claude's session tool inventory and authenticated
connector identities are explicitly reported as unavailable because the
adapter does not expose them to a standalone process.
Without `--config`, the installed command reads the same platform config
directory used by the Claude Code starter (`APPA_CONFIG_DIR` can override it).

```sh
./target/debug/appa runtime --config appa.toml --db appa.db
```

`curl localhost:8787/health` prints `ok` when it is up. The listener
accepts loopback addresses only. Useful flags: `--listen
127.0.0.1:<port>` for another port, `-v` to log each hook and
decision, `-vv` for full detail. `--adapter` picks the harness codec
the process loads; `claude-code` is the default and the only one
today.

Start the process before the session. While it is down, every action
in a protected session is blocked, and that cannot be automated from
inside the session — the session's own commands are blocked too.

### 4. Protect a session

The Claude Code integration — the plugin, the statusline, the example
policies, and the install and uninstall instructions — lives in
[`integrations/claude-code/`](../integrations/claude-code/README.md).

### 5. See it work

Run a protected session and use it normally. With `-v` on the process you
see every hook arrive and every decision go out. The database shows
what was recorded:

```sh
# The whole log of one root, batch by batch. Everything the runtime knows —
# a branch's parent, whether it has ended, which dispatch it has open — is
# read back from these records; nothing is stored beside them.
sqlite3 appa.db "SELECT seq, facts FROM logs WHERE root = 'cc:<session-id>' ORDER BY seq;"
```

`GET /status?trajectory=cc:<session-id>` answers the same questions
over HTTP without SQL, and is the supported way to look — it is what the
statusline reads.

## Things to know

- **A changed policy is a new deployment.** Edit `[policy]` and new
  trajectories open under the edited one. Trajectories already open keep
  running under the policy they opened with — the runtime recompiles it
  from the copy stored in their log. The same `--db` path serves both.
- **Stopping the process blocks protected sessions.** That is the design,
  not a fault. Uninstall the plugin if you want unprotected sessions back.
- **This crate's `CLAUDE.md`** describes the layout: the process
  (`appa-runtime/`), the shared vocabulary (`appa-runtime-api/`), and
  the Claude Code adapter (`appa-adapter-claude-code/`).
