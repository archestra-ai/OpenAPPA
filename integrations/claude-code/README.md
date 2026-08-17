# Claude Code integration

Everything needed to gate a Claude Code session through the
appa-runtime-v2 process lives in this directory: the plugin (hooks, the
`execute_remedy_plan` MCP server, the `appa-debug` skill), the
statusline script, example policies, and the install and uninstall
instructions below. The process itself is the `appa-runtime-v2` crate;
its [README](../../appa-runtime-v2/README.md) covers build,
configuration, and start.

How it works, in one paragraph: the plugin registers hooks on every
session event — prompt, tool call, tool result, subagent start and
finish. Each hook posts the event to the runtime process and blocks the
action unless the process answers yes. The hooks fail closed: while the
process is down, every action in a gated session is blocked — silence
never means yes. A subagent started with the `Agent` tool runs as a
child of the session: its own tool calls are gated, and its final
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

Release installers select the binary for the current operating system
and architecture. They verify its SHA-256 checksum before installation.
They also install the matching Claude Code plugin and configure the
runtime to start at login.

### Linux and macOS

Install the latest release with one command. The same command selects the
correct x86-64 or ARM64 archive:

```sh
curl -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh | sh
```

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by these release assets.

If you download `install.sh` first, run `sh install.sh`. HTTP downloads do
not preserve its executable bit, so `./install.sh` can report
`permission denied`.

Linux uses a systemd user service. macOS uses a LaunchAgent. If the
login service cannot run in the current environment, install without it
and start the printed command yourself:

```sh
curl -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh | APPA_SKIP_SERVICE=1 sh
```

### Windows

The PowerShell installer selects the x86-64 or ARM64 native Windows runtime
and creates a current-user Scheduled Task:

```powershell
irm https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.ps1 | iex
```

A streamed script does not go through the execution policy. To run a saved
copy, use `powershell -ExecutionPolicy Bypass -File .\install.ps1`.

The Windows installer replaces the POSIX hook commands with a native
PowerShell adapter. The adapter blocks prompt and tool authorization failures
and blocks a successful tool result when the runtime cannot admit it. It also
installs a PowerShell statusline.

### Installed files

| System | Runtime | Policy | Database and Claude plugin |
| --- | --- | --- | --- |
| Linux | `~/.local/bin/appa-runtime-v2` | `~/.config/appa/appa.toml` | `~/.local/share/appa/` |
| macOS | `~/.local/bin/appa-runtime-v2` | `~/Library/Application Support/appa/appa.toml` | `~/Library/Application Support/appa/` |
| Windows | `%LOCALAPPDATA%\appa\bin\appa-runtime-v2.exe` | `%APPDATA%\appa\appa.toml` | `%LOCALAPPDATA%\appa\` |

The runtime creates the starting policy only when the policy path does
not exist. Re-running an installer updates the runtime and plugin. It
does not replace the policy or database.

Set `APPA_VERSION` to install one release instead of the latest release.
Set `APPA_INSTALL_DIR`, `APPA_CONFIG_DIR`, or `APPA_DATA_DIR` before
running an installer to change these locations. Use the same overrides
when uninstalling.

## Gate a Claude Code session

The installer prints the installed plugin path. Keep normal `claude` sessions
ungated and define a separate `clappa` command. For Linux, add this line to
your shell configuration:

```sh
alias clappa='claude --settings "$HOME/.claude/appa-session-settings.json" --plugin-dir "$HOME/.local/share/appa/claude-code/plugin"'
```

For macOS:

```sh
alias clappa='claude --settings "$HOME/.claude/appa-session-settings.json" --plugin-dir "$HOME/Library/Application Support/appa/claude-code/plugin"'
```

For native Windows, add this function to your PowerShell profile. The
installer prints the same command with exact paths when overrides are used:

```powershell
function clappa { claude --settings "$HOME/.claude/appa-session-settings.json" --plugin-dir "$env:LOCALAPPDATA/appa/claude-code/plugin" @args }
```

Only sessions started with `clappa` are gated. Check the runtime first:

```sh
curl -sS -m 2 http://127.0.0.1:8787/health
```

The command must print `ok`. A gated session blocks every action while the
runtime is unavailable. Installers run the process in the background and
start it at each user login through systemd, launchd, or Windows Task
Scheduler.

The default policy names only Claude Code's built-in tools. APPA blocks every
installed MCP tool until the policy names it. Start `clappa` and run
`/appa-tool-sync`. The skill exists only in gated sessions. It inventories MCP
servers, proposes one policy entry per tool, and marks which tools read data
that must stay in the session or send data outward. It asks once about servers
it cannot judge. You review the complete proposal before it writes anything.

For development from a source checkout, use the same `--plugin-dir`
form with `integrations/claude-code/plugin`. Copy the default policy before
editing it, then keep the runtime running in the background:

```sh
cp integrations/claude-code/examples/claude-code.appa.toml appa.toml
nohup cargo run --bin appa-runtime-v2 -- --config appa.toml --db appa.db >appa-runtime.log 2>&1 &
```

If the process listens on a port other than 8787, set
`APPA_RUNTIME_URL=http://127.0.0.1:<port>` in the session's
environment; the hooks and the MCP server both follow it.

## Uninstall

Run the same installer with its uninstall option:

```sh
curl -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh | sh -s -- --uninstall
```

```powershell
& ([scriptblock]::Create((irm https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.ps1))) -Uninstall
```

Uninstall stops and removes login startup. It removes the runtime and
installed plugin files. It preserves the policy and database. Remove a
`clappa` alias or statusline overlay separately if you created one.

If a gated session is blocked because the runtime is not running, run
uninstall from a plain terminal or ungated Claude Code session.

## Statusline, manually

Claude Code reads `statusLine` only from your own settings — a plugin
cannot set it. The script shows the APPA pixel mascot plus the
session's current Trust and Audience, read from the process's
`GET /status`. Both platform scripts fail open: runtime down, session
not gated, or malformed input prints the mascot alone, never a blocked
action. The POSIX script also needs `jq` and `curl`.

To set it without delegating to Claude, write this to the overlay file
the alias loads (`~/.claude/appa-session-settings.json`). Point it at
the installed plugin or a source checkout:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/home/you/.local/share/appa/claude-code/plugin/statusline.sh"
  }
}
```

On native Windows, use the installed PowerShell script and forward slashes in
its absolute path. The installer prints the exact JSON for your installation:

```json
{
  "statusLine": {
    "type": "command",
    "command": "\"C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe\" -NoProfile -ExecutionPolicy Bypass -File \"C:/Users/you/AppData/Local/appa/claude-code/plugin/statusline.ps1\""
  }
}
```

Because the overlay loads only through the alias's `--settings` flag,
plain `claude` sessions keep whatever statusline your own settings
name. Merge the same block into `~/.claude/settings.json` instead if
you want the mascot in every session; it fails open, so ungated
sessions just show the mascot alone.

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
- **Stopping the process blocks gated sessions.** That is the design,
  not a fault. Start a plain `claude` if you want an ungated session.
- **The plugin adds roughly zero tokens to a session.** The gating is
  hooks and an MCP server, not prompt text.
