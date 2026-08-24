# Claude Code integration

Everything needed to protect a Claude Code session through the
appa-runtime process lives in this directory: the plugin (hooks, the
`execute_remedy_plan` MCP server, the `appa-debug` skill), the
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
and its final
message is checked — and rewritten or withheld — where the parent
receives it, in the `Agent` tool's result.

## What is here

- `plugin/` — the Claude Code plugin: `hooks/hooks.json`, the `appa`
  MCP server (`.mcp.json`), the `appa-tool-sync` skill (declares
  installed MCP tools in the running runtime's policy and marks which
  of them read private data or send data outward), and
  `statusline.sh` plus `statusline.ps1`.
- `.claude-plugin/marketplace.json` — the marketplace manifest;
  `claude plugin marketplace add` points at this directory.
- `examples/claude-code.appa.toml` — a complete starting policy: every
  built-in Claude Code tool released with the neutral annotation, web
  tool results marked suspicious, subagents run as children of the
  session and background subagents refused.
- `examples/claude-code-hitl.appa.toml` — the same plus GitHub MCP
  tools, with issue writes requiring a human sign-off served over MCP
  elicitation.

## Install

The plugin manager installs the protection; nothing else is downloaded ahead
of time:

```sh
claude plugin marketplace add archestra-ai/OpenAPPA
claude plugin install appa-runtime@appa
```

The runtime binary arrives as a prompted task: when it is missing, a
plain `claude` session offers to install it (`hooks/setup-appa.md`) —
download the release archive for the current system, verify its SHA-256
against the release `SHA256SUMS` and its version against `version.txt`,
and place the binary — each step under the session's normal command
approval. The last step starts the runtime through the plugin's own
starter and reports the answer `/health` gave, so the install ends with a
process that has actually run on this machine, the default policy written,
and no start left for the first protected session to pay for. While the repository is private, run
`gh auth login && gh auth setup-git` first.

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by the release assets.

The plugin ships the POSIX hook commands. On native Windows, use a
checkout marketplace and replace `plugin/hooks/hooks.json` with
`plugin/hooks/hooks.windows.json`, which drives the `hook.ps1` adapter;
WSL runs the POSIX hooks as-is.

### File locations

| System | Runtime | Policy | Database |
| --- | --- | --- | --- |
| Linux | `~/.local/bin/appa-runtime` | `~/.config/appa/appa.toml` | `~/.local/share/appa/` |
| macOS | `~/.local/bin/appa-runtime` | `~/Library/Application Support/appa/appa.toml` | `~/Library/Application Support/appa/` |
| Windows | `%LOCALAPPDATA%\appa\bin\appa-runtime.exe` | `%APPDATA%\appa\appa.toml` | `%LOCALAPPDATA%\appa\` |

The runtime creates the starting policy only when the policy path does
not exist. It never replaces the policy or database.

Set `APPA_INSTALL_DIR`, `APPA_CONFIG_DIR`, or `APPA_DATA_DIR` in the
environment to change these locations; the hooks and the setup
instructions follow them.

## Protect a Claude Code session

The plugin is present in every session but inert until a session
opts in with `APPA_GATE=1`. Keep normal `claude` sessions unprotected
and use a separate `clappa` command for protected ones. The setup task
creates
it as an executable beside the runtime binary — a PATH command works in
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
left it running — then blocks every action while the runtime is
unavailable. When the binary is not installed at all, an
unprotected session installs it as a prompted task: its session context
(`hooks/setup-appa.md`) has the model, only when asked, download the
release archive for the current system, verify its
checksum and version, and install the binary — each step under the
session's normal command approval. There is no login service: a runtime
that dies mid-session blocks the session until the next session start
brings it back. Check the runtime with:

```sh
curl -sS -m 2 http://127.0.0.1:8787/health
```

The command must print `ok`.

The default policy names only Claude Code's built-in tools. APPA blocks every
installed MCP tool until the policy names it. Start `clappa` and run
`/appa-tool-sync`. The skill exists only in protected sessions. It inventories MCP
servers, proposes one policy entry per tool, and marks which tools read data
that must stay in the session or send data outward. It asks once about servers
it cannot judge. You review the complete proposal before it writes anything.

For development from a source checkout, run the runtime on its own port
so an installed runtime on 8787 is untouched, and point a session at it
with `APPA_RUNTIME_URL` — the hooks, the MCP server, and the statusline
all follow it:

```sh
cp integrations/claude-code/examples/claude-code.appa.toml appa.toml
nohup cargo run --bin appa-runtime -- --config appa.toml --db appa.db --listen 127.0.0.1:8788 >appa-runtime.log 2>&1 &
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude --plugin-dir integrations/claude-code/plugin
```

The last command is interactive and belongs to the user: a Claude
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
`claude` CLI on PATH and logged in, and a runtime binary — a local
build, an installed one, or `APPA_RUNTIME_BIN`. It spends the machine's
Claude usage, so nothing runs it automatically.

## Upgrade

The plugin tracks the marketplace. To upgrade the runtime, stop the
running one, remove the binary from the location in the table above, and
ask a plain `claude` session to set up APPA again; it installs the latest
release and starts it. Stop it first: a runtime already answering on the
port keeps serving, and the new binary would never run.

## Uninstall

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
pkill -f appa-runtime
rm ~/.local/bin/appa-runtime ~/.local/bin/clappa ~/.local/bin/appa-statusline.sh

# drop the statusline entry the setup wrote, and keep one of your own:
jq 'if (.statusLine.command? // "") | test("appa-statusline") then del(.statusLine) else . end' \
  ~/.claude/settings.json > ~/.claude/settings.json.new &&
  mv ~/.claude/settings.json.new ~/.claude/settings.json
```

The setup writes the `statusLine` entry into your own settings, so
removing the script alone leaves Claude Code running a command that no
longer exists. The `jq` line removes that entry only while it runs
`appa-statusline.sh`, so a statusline of your own survives untouched.

The policy and database stay at the locations in the table above; delete
them only if you want the history gone. Remove a `clappa` shell alias
separately if you added one instead of the command.

## Statusline, manually

Claude Code reads `statusLine` only from your own settings — a plugin
cannot set it. In a protected session the script shows the APPA pixel
mascot plus the session's current Trust and Audience, read from the
process's `GET /status`. In an unprotected session it shows the mascot
alone and never queries the runtime. Both platform scripts fail open: runtime down, unknown
trajectory, or malformed input prints the mascot alone, never a blocked
action. The POSIX script also needs `jq` and `curl`.

To set it, merge this into `~/.claude/settings.json`, pointing at a
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
