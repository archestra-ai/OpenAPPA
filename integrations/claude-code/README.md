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
  `statusline.sh`.
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

Download the installer from the latest release, inspect it, then run it:

```sh
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh
sh install.sh
```

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by these release assets.

For a private repository, use an authenticated GitHub CLI. The
installer also uses this authentication for its release downloads:

```sh
gh release download --repo archestra-ai/OpenAPPA --pattern install.sh --clobber
sh install.sh
```

Linux uses a systemd user service. macOS uses a LaunchAgent. If the
login service cannot run in the current environment, install without it
and start the printed command yourself:

```sh
APPA_SKIP_SERVICE=1 sh install.sh
```

### Windows

The PowerShell installer installs the native Windows runtime and
creates a current-user Scheduled Task:

```powershell
Invoke-WebRequest `
  https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.ps1 `
  -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

Use `gh release download --repo archestra-ai/OpenAPPA --pattern
install.ps1 --clobber` before the second command for a private
repository.

The released Claude Code hooks call POSIX tools such as `curl` and
`cat`. Native Windows runtime installation does not make those hooks
Windows-compatible. Install OpenAPPA inside WSL to gate Claude Code on
Windows.

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

The installer prints the installed plugin path. Start Claude Code with
that plugin directory. For Linux with default paths:

```sh
claude --plugin-dir "$HOME/.local/share/appa/claude-code/plugin"
```

For macOS with default paths:

```sh
claude --plugin-dir "$HOME/Library/Application Support/appa/claude-code/plugin"
```

Only sessions started with this flag are gated. A shell alias keeps the
choice explicit. For example, Linux users can add this line to their
shell configuration:

```sh
alias clappa='claude --plugin-dir "$HOME/.local/share/appa/claude-code/plugin"'
```

Check the runtime before starting a gated session:

```sh
curl -fsS http://127.0.0.1:8787/health
```

The command must print `ok`. A gated session blocks every action while
the runtime is unavailable. After startup, run `/appa-tool-sync` in the
gated session. The plugin skill inventories installed MCP tools and
proposes their policy entries for review.

The default policy names Claude Code's built-in tools. APPA blocks an
MCP tool until the policy names it.

For development from a source checkout, use the same `--plugin-dir`
form with `integrations/claude-code/plugin`. Start the runtime with:

```sh
cargo run --bin appa-runtime-v2 -- --config appa.toml --db appa.db
```

If the process listens on a port other than 8787, set
`APPA_RUNTIME_URL=http://127.0.0.1:<port>` in the session's
environment; the hooks and the MCP server both follow it.

## Uninstall

Run the same installer with its uninstall option:

```sh
sh install.sh --uninstall
```

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall
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
`GET /status`. It needs `jq` and `curl` and fails open: runtime down,
session not gated, or tools missing all print the mascot alone, never
a blocked action.

To set it without delegating to Claude, write this to the overlay file
the alias loads (`~/.claude/appa-session-settings.json`). Point it at
the installed plugin or a source checkout:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/OpenAPPA/integrations/claude-code/plugin/statusline.sh"
  }
}
```

Because the overlay loads only through the alias's `--settings` flag,
plain `claude` sessions keep whatever statusline your own settings
name. Merge the same block into `~/.claude/settings.json` instead if
you want the mascot in every session; it fails open, so ungated
sessions just show the mascot alone.

To keep an existing statusline (for example claude-powerline) and add
the APPA rows beneath it, run both and tee stdin. Pin the exact version
you vetted — `@latest` would fetch and run new third-party code on
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
