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

The Claude Code adapter requires the `claude` command, `curl`, and Cargo when building from a checkout.

Every `appa` build knows the version it belongs to and installs no other. On
Linux and macOS the installer fetches
the release archive, verifies its checksum, and places `appa` in
`~/.local/bin`; the install then fetches the version published for that
release, verifies it, and retains it:

```sh
curl -fsSL https://openappa.com/install.sh | sh &&
  ~/.local/bin/appa plugin install claude-code
```

A build from a checkout carries its exact Git commit, so the same command
installs that build's own version, without the network:

```sh
cargo install --locked --path appa-runtime --force
appa plugin install claude-code
```

The result does not depend on the working directory. It deploys the same `appa` build for its internal
`runtime` command and registers it in the user's Claude Code settings as every session hook, rewriting the
entries an earlier install wrote instead of stacking another set; it creates `clappa`, which loads
APPA's statusline for the sessions it starts, preserves an existing policy,
and starts the runtime. The [Claude Code integration guide](../marketplace/plugins/claude-code/README.md)
covers the complete flow.

## Local dashboard and battery setup

Run `appa ui` to open the local dashboard. Overview shows which MCP servers
have rules and where each rule comes from (the root configuration or a
battery), and opens each server into its contracts. Its server list joins the
MCP servers Claude Code configures with the namespaces the policy names; it
excludes APPA's internal namespace. Claude Code reports no tool inventory, so
coverage is per server, not per tool. Batteries shows readiness and inline
configuration.
The UI checks battery readiness on opening and uses the battery result as its
status; absence of a token alone is not a failure when CLI login is supported.
`appa describe` runs the same readiness checks for configured batteries.
A battery is ready when nothing it needs is missing: its declared executables,
the variables its policy binds as `token_env`, and the result of its provider
check when it declares one. A battery that declares nothing is ready. Without
a provider check, a set token is ready but untested.
The runtime serves the UI on the same port as its APIs. Missing credentials or an
invalid policy leave enforcement unavailable while the UI stays accessible.
`appa ui` opens the running server; it never starts a separate UI process.

Configure several proposed batteries on one screen:

```sh
appa ui --config /path/to/appa.toml --setup --battery github,slack
appa battery status --config /path/to/appa.toml --battery github,slack --json --check
```

Use `--no-open` to print the browser address. `--runtime-url` selects the running
loopback runtime; `--config` verifies that it serves the expected configuration.
Open the runtime URL directly, or use `appa ui`; no login, session token, or
expiring link is required. The UI and credential API are restricted to loopback,
with Host and browser-origin checks. The web server has the runtime's lifetime.

**Save and check** saves all submitted credentials, runs bounded read-only checks,
and attempts to apply the configuration. A failed reload preserves an already active
policy; saved credential changes remain saved and are used by subsequent helper
launches. Provider credentials retained by HTTP/model bindings refresh on a
successful reload. Before the first valid deployment, enforcement endpoints
return HTTP 503. `/health` reports process liveness; `/ready` returns HTTP 200 only
when enforcement is active, and HTTP 503 otherwise.

Credentials live in `credentials.db` beside the canonical configuration file,
scoped by that configuration's path. This is separate from the trajectory database
and its diagnostic exports. Values are plaintext at rest; Unix database permissions
are `0600`. Do not include this file in source control or deployment bundles.

The runtime's environment takes precedence over saved values, including an explicitly
empty environment value. Otherwise APPA supplies the saved value under the same
`APPA_PROVIDER_*` variable the helper already reads. An absent value leaves existing
battery CLI fallbacks available. Each helper receives only its declared APPA
credential. Embedding hosts, including Archestra, retain environment-only behavior.

Browser prerequisite inspection and connection checks always run in the runtime's
environment. The CLI status command can also inspect local setup without a server.
The UI configures battery helpers; it does not authenticate MCP connectors.

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
version = 2

[[policy.tool]]
name = "Bash"

[externals]
timeout_ms = 5000
max_body_bytes = 65536
```

Implementation bindings live in `[externals]`: one
`[externals.<kind>.<name>]` entry per registered authority, sanitizer,
or membership resolver, and per annotator that names no `builtin` on
its declaration — bound to a `url` or a `command`, or, for authorities
and sanitizers, a `builtin` (stock, a model transport, or a module from
`--modules-dir`). An annotator names a model transport on its own
`[[policy.annotator]]` declaration with `builtin = "claude-code"` or
`builtin = "llm"`; a bound entry for a builtin annotator refuses to
start. An authority may stay unbound and then returns no answer; every
other registered name needs its entry, and an entry no declaration
registers refuses to start.

`marketplace/plugins/claude-code/default.appa.toml` is a
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

The Claude Code integration — the hook entries, the statusline, the example
policies, and the install and uninstall instructions — lives in
[`marketplace/plugins/claude-code/`](../marketplace/plugins/claude-code/README.md).

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

## File mediation (draft)

A second, opt-in mode makes the runtime own the file tools themselves. Add `[file_tracking]`
with `initial_trust` and `initial_audience` to the APPA configuration. The table's presence
enables the mode. Each root session binds its first file call to that session's working
directory, so one runtime can serve different workspaces.
The runtime serves `appa_read_file`,
`appa_write_file`, `appa_edit_file`, `appa_copy_file`, `appa_move_file` and, with
`--file-process-backend`, `appa_process_files`. The runtime pins each file version, checks
the call with the pinned Label, performs the operation and records the version it published,
so file content is checked before it is read and Labels survive operations that never show
bytes to the model. The mode is experimental, it assumes the runtime owns the workspace, and
it refuses every call that is not one of those tools.

[`FILE-MEDIATION.md`](FILE-MEDIATION.md) is the architecture note: component map, call
sequence, the session-local ledger and its reservation lifecycle, the Label algebra, the isolated Process
contract, and the list of what is and is not covered.

## Things to know

- **A changed policy is a new deployment.** Edit `[policy]` and new
  trajectories open under the edited one. Trajectories already open keep
  running under the policy they opened with — the runtime recompiles it
  from the copy stored in their log. The same `--db` path serves both.
- **Stopping the process blocks protected sessions.** That is the design,
  not a fault. Start a plain `claude` session if you want an unprotected one.
- **This crate's `CLAUDE.md`** describes the layout: the process
  (`appa-runtime/`), the shared vocabulary (`appa-runtime-api/`), and
  the Claude Code adapter (`appa-adapter-claude-code/`).
