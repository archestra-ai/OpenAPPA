# Claude Code integration

Everything needed to protect a Claude Code session through the
appa-runtime process lives in this directory: the plugin (hooks, the
`execute_remedy_plan` MCP server, the `appa-guide` skill), the
statusline script, example policies, and the install and uninstall
instructions below. The process itself is the `appa-runtime` crate;
its [README](../../appa-runtime/README.md) covers build,
configuration, and start.

How it works, in one paragraph: the plugin registers hooks on every
session event — prompt, tool call, tool result, subagent start and
finish. Each hook posts the event to the runtime process and blocks the
action unless the process answers yes. The hooks fail closed: while the
process is down, every action in a protected session is blocked —
silence never means yes. A subagent started with the `Agent` tool runs
as a child of the session: its own tool calls are checked the same way,
and its final message is checked when the subagent stops. A stop whose
message may not cross is refused, and the subagent keeps running until
it returns an admissible message; the parent then receives that message
unchanged.

## What is here

- `plugin/` — the Claude Code plugin: `hooks/hooks.json`, the `appa`
  MCP server (`.mcp.json`), the `appa-guide` skill (builds the initial
  tool policy and guides later config changes), and
  `statusline.sh` plus `statusline.ps1`.
- `.claude-plugin/marketplace.json` — the marketplace manifest;
  `claude plugin marketplace add` points at this directory.
- `examples/claude-code.appa.toml` — a complete starting policy: every
  built-in Claude Code tool released with the neutral annotation, web
  tool results marked suspicious, and subagents run as children of the
  session.
- `examples/claude-code-hitl.appa.toml` — the same plus GitHub MCP
  tools, with issue writes requiring a human sign-off served over MCP
  elicitation.

## Install

This flow needs the `claude` command, `curl`, and Cargo when building from a checkout.

`appa init` installs one bundle: the plugin belonging to the running binary and
that binary. Release builds carry an immutable tag and artifact digest; clean
checkout builds carry an immutable commit and plugin-tree digest. The result
does not depend on the working directory.

From a release binary, that digest is baked in. The artifact is downloaded once,
verified against the digest before anything outside a temporary file changes,
and cached, so a later init needs no network:

```sh
appa init claude-code
```

A clean checkout build downloads the source archive for its exact commit,
stages the marketplace tree, and verifies the digest baked at compilation. A
build with local plugin changes uses that exact checkout and verifies that it
has not changed since compilation.

```sh
cargo install --path appa-runtime --force
appa init claude-code
```

Init reports each slow phase on stderr. If another installed APPA build owns
the runtime endpoint, init identifies its process and asks `Stop it and
continue? [Y/n]` before sending any signal. It never offers to stop an
unidentified listener or another user's process.

Init installs `clappa` beside `appa` so the short command works in later examples.

Init uninstalls an existing user-scoped APPA plugin and replaces its marketplace
before installing, so branch tests never stack two APPA hook sets.

Initialization deploys the `appa` binary to a private path under the data
directory and renders that exact path into the hooks, so a hook never resolves
`appa` through `PATH`. It creates the starting policy only when it is missing,
installs `clappa`, preserves a custom Claude statusline, registers the plugin,
and starts the runtime through the same starter used at SessionStart. A
successful command therefore proves that one runtime and one plugin from the
selected source are active.

Deployments are content-addressed and immutable: Claude is pointed at a
directory that cannot change under it, rather than at a checkout or a remote
marketplace. Re-running init repairs a deployment whose structure or rendered
paths are wrong and is otherwise a no-op.

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by the release assets.

The plugin ships POSIX and native Windows hook commands. `appa init` activates
the PowerShell adapter on native Windows; WSL uses the POSIX hooks.

### File locations

| System | Runtime | Policy | Database |
| --- | --- | --- | --- |
| Linux | `~/.local/share/appa/bin/appa runtime` | `~/.config/appa/appa.toml` | `~/.local/share/appa/` |
| macOS | `~/Library/Application Support/appa/bin/appa runtime` | `~/Library/Application Support/appa/appa.toml` | `~/Library/Application Support/appa/` |
| Windows | `%LOCALAPPDATA%\appa\bin\appa.exe runtime` | `%APPDATA%\appa\appa.toml` | `%LOCALAPPDATA%\appa\` |

The harness binary is APPA's own, not something you put on `PATH`: hooks name
that absolute path. `clappa` and the statusline stay where a shell and Claude's
settings can find them.

The runtime creates the starting policy only when the policy path does
not exist. It never replaces the policy or database.

Set `APPA_INSTALL_DIR`, `APPA_CONFIG_DIR`, or `APPA_DATA_DIR` in the
environment to change these locations; `appa init` and the hooks follow them.

## Protect a Claude Code session

The plugin is present in every session but inert until a session
opts in with `APPA_GATE=1`. Keep normal `claude` sessions unprotected
and use a separate `clappa` command for protected ones. `appa init` creates
it as an executable beside the `appa` command — a PATH command works in
every open terminal with no shell reload, unlike an alias:

```sh
#!/bin/sh
exec env APPA_GATE=1 claude "$@"
```

When that directory is not on your `PATH`, use the alias form instead
and reload your shell: `alias clappa='APPA_GATE=1 claude'`. For native
Windows, add this function to your PowerShell profile:

```powershell
function clappa { $env:APPA_GATE = "1"; try { claude @args } finally { Remove-Item Env:APPA_GATE -ErrorAction SilentlyContinue } }
```

Only sessions started with `APPA_GATE=1` are protected. The hooks read
the variable from the Claude Code process environment, fixed at launch,
so a session cannot turn the protection off mid-session. A plain
`claude` session stays unprotected, and the plugin prints nothing
into it.

A protected session starts the installed runtime at SessionStart when
nothing healthy answers `/health` — normally a no-op, because the install
left it running — or replaces a runtime that answers `stale <pid>`,
which a running process does once an install replaced its binary on
disk. It then blocks every action while the runtime is unavailable. The starter
never installs software; rerun `appa init claude-code` when the binary or
plugin is missing. There is no login service: a runtime
that dies mid-session blocks the session until the next session start
brings it back. Check the runtime with:

```sh
curl -sS -m 2 http://127.0.0.1:8787/health
```

The command must print `ok`. It prints `stale <pid>` when the binary
was installed again after this runtime started; the next protected
session start replaces the process, and so does running the starter
by hand.

The default policy names Claude Code's built-in tools and sends every other
tool through a bounded, fail-closed Claude annotator. That compatibility net
keeps a newly installed MCP tool usable, but it is not a substitute for a
reviewed connector contract. Start `clappa` and run `/appa-guide init` from
that protected session. It inventories MCP servers, proposes exact policy
entries or maintained batteries, and marks which tools read data that must
stay in the session or send data outward. It asks once about servers it cannot
judge. You review the complete proposal before it writes anything.

For development from a source checkout, run the runtime on its own port
so an installed runtime on 8787 is untouched, and point a session at it
with `APPA_RUNTIME_URL` — the hooks, the MCP server, and the statusline
all follow it:

```sh
cp integrations/claude-code/examples/claude-code.appa.toml appa.toml
nohup cargo run --bin appa -- runtime --config appa.toml --db appa.db --listen 127.0.0.1:8788 >appa-runtime.log 2>&1 &
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude --plugin-dir integrations/claude-code/plugin
```

The starter leaves a runtime at a URL of your own alone, stale or not:
after a rebuild, restart it yourself. The last command is interactive and belongs to the user: a Claude
session performing this setup runs the first two and prints the third.

`APPA_RUNTIME_URL` is fixed at session launch, like `APPA_GATE`: a
running session cannot be pointed at a different runtime. To move
between the installed and the dev runtime, start a new session.

## Live check

`live-gate-check.py` runs two real headless `claude` sessions against a
runtime process it starts itself, under a policy that states one flow:
reading a file narrows its content to the session, and writing a file
releases content to the outside world.

```sh
uv run integrations/claude-code/live-gate-check.py
```

It judges the gate the way a user does, on what reached the disk. One
session writes words of the model's own and the file lands. The other
reads a private file, proposes the write, and that line then appears in no
file under any name. The allowed write is what stops a runtime that is
down from passing as a refusal: the hooks fail closed, so a gate that is
not answering blocks both sessions rather than one.

The check reads nothing of APPA's own log or database. It needs the
`claude` CLI on PATH and logged in, and an appa binary — a local build,
an installed one, or `APPA_BIN`. It spends the machine's
Claude usage, so nothing runs it automatically.

## Upgrade

Install the new `appa` package, then rerun `appa init claude-code`. Init replaces
the deployed runtime and the APPA marketplace together, always as one bundle,
and preserves policy and database files.

**Restart any running `clappa` session after an upgrade.** Claude loads a
session's hooks at session start, and the hook wire between plugin and runtime
carries no version, so a session running across an upgrade keeps talking to the
runtime it started with.

Init stops a runtime that is still executing the path a previous init deployed
to, and aborts before touching Claude if it cannot, rather than registering a
new plugin against an old runtime. That covers the paths your current
environment resolves to. A runtime left by an init run under a different
`APPA_INSTALL_DIR` or `APPA_DATA_DIR` executes a path this init never computes:
it is reported so you can stop it yourself, never killed. An `appa` left at the
old install path is named in the receipt and never deleted; remove it when you
are ready.

## Uninstall

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
pkill -f 'appa runtime'
rm -rf ~/.local/share/appa/bin ~/.local/share/appa/deployments ~/.local/share/appa/cache
rm -f ~/.cargo/bin/clappa ~/.local/bin/appa-statusline.sh
cargo uninstall appa

# drop the statusline entry appa init wrote, and keep one of your own:
jq 'if (.statusLine.command? // "") | test("appa-statusline") then del(.statusLine) else . end' \
  ~/.claude/settings.json > ~/.claude/settings.json.new &&
  mv ~/.claude/settings.json.new ~/.claude/settings.json
```

`appa init` writes the `statusLine` entry into your own settings, so
removing the script alone leaves Claude Code running a command that no
longer exists. The `jq` line removes that entry only while it runs
`appa-statusline.sh`, so a statusline of your own survives untouched.

The policy and database stay at the locations in the table above; delete
them only if you want the history gone. Remove a `clappa` shell alias
separately if you added one instead of the command.

## Statusline

Claude Code reads `statusLine` only from your own global settings — a plugin
cannot set it. `appa init` adds the platform script there unless you already
have a custom statusline. In a protected session the script shows the APPA pixel
mascot plus the session's current Trust and Audience, read from the
process's `GET /status`. In an unprotected session it prints nothing and never
queries the runtime, so regular `claude` has no APPA statusline. Both platform
scripts fail open inside a protected session: runtime down, unknown
trajectory, or malformed input prints the mascot alone, never a blocked
action. The POSIX script also needs `jq` and `curl`.

To set it manually, merge this into `~/.claude/settings.json`, pointing at a
checkout of this repository:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/OpenAPPA/integrations/claude-code/plugin/statusline.sh"
  }
}
```

On native Windows, use the PowerShell script and forward slashes in its
absolute path:

```json
{
  "statusLine": {
    "type": "command",
    "command": "\"C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe\" -NoProfile -ExecutionPolicy Bypass -File \"C:/path/to/OpenAPPA/integrations/claude-code/plugin/statusline.ps1\""
  }
}
```

The setting applies to every session, protected or not; the script's
`APPA_GATE` branch keeps the two states distinguishable at a glance.

On POSIX systems, keep an existing statusline such as claude-powerline and add
the APPA rows beneath it by running both and teeing stdin. Pin the exact
version you vetted. `@latest` would fetch and run new third-party code on
every statusline refresh:

```json
{
  "statusLine": {
    "type": "command",
    "command": "input=$(cat); printf '%s' \"$input\" | npx -y @owloops/claude-powerline@1.4.0; printf '%s' \"$input\" | /path/to/OpenAPPA/integrations/claude-code/plugin/statusline.sh"
  }
}
```

## Things to know

- **An edited policy installs without a restart.** `curl -X POST
  http://127.0.0.1:8787/reload` re-reads the `--config` file. The
  runtime validates before it installs, so a bad file answers 422 and
  changes nothing. Sessions started after the reload bind the new
  policy; sessions already running keep the file they opened with.
- **Stopping the process blocks protected sessions.** That is the
  design, not a fault. Start a plain `claude` if you want an
  unprotected session.
- **The plugin adds roughly zero tokens to a session.** The protection
  is hooks and an MCP server, not prompt text.
