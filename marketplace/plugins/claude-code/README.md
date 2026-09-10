# Claude Code integration

The Claude Code package: its manifest, the starting policies, the harness
conformance check, and the install and uninstall instructions below. The
host-side code — the hooks, the status line, the runtime start, the
`appa` MCP registration and the `appa-guide` skill — is the `appa` binary
itself, the `appa-runtime` crate; its
[README](../../appa-runtime/README.md) covers build, configuration, and
start.

How it works, in one paragraph: the install registers `appa hook` in the
user's Claude Code settings on every session event — prompt, tool call,
tool result, subagent start and finish. Each hook posts the event to the
runtime process and blocks the
action unless the process answers yes. The hooks fail closed: while the
process is down, every action in a protected session is blocked —
silence never means yes. A subagent started with the `Agent` tool runs
as a child of the session. The spawn is held until the session declares
what the subagent's final message may carry: as it is, floored at a
label, or through a sanitizer such as the schema attestation. The
subagent's own tool calls are checked the same way, and its final
message is checked when it stops. A stop whose message may not cross is
refused with the reason, or with the exact text to return when a
sanitizer rewrote it, and the subagent keeps running until it stops with
a message that crosses; the parent then receives that message unchanged.
A subagent definition that declares `maxTurns` blocks the session's
prompts: Claude Code ends such a subagent without the return check. The
project and user agent directories and the installed plugins are
scanned; agents passed on the command line are not.

## What is here

- `appa-package.toml` — the package manifest: the starting policy and the
  batteries a first install includes.
- `default.appa.toml` — a complete starting policy: every built-in Claude
  Code tool released with the neutral annotation, web tool results marked
  suspicious, and subagents run as children of the session.
- `hitl.appa.toml` — the same plus GitHub MCP tools, with issue writes
  requiring a human sign-off served over MCP elicitation.
- `live-gate-check.py` — the harness conformance check described below.

The `appa-guide` skill, which builds the initial tool policy and guides
later config changes, lives in `integrations/appa-guide/`; the install
writes it from the bytes compiled into the binary.

## Install

This flow needs the `claude` command, `curl`, and Cargo when building from a checkout.

`appa plugin install claude-code` installs one version: its packages and the
binary belonging to it. It selects the version, verifies every artifact
against that version's descriptor before anything outside a temporary file
changes, retains them under the deployment's `.appa/` state so a later
install needs no network, and activates Claude Code support with that
version's own binary. The result does not depend on the working directory.

A release binary installs the version published for its tag. The installer
verifies the checksum of the binary for Linux or macOS and places it in
`~/.local/bin` (Windows: unpack the zip from the releases page):

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa plugin install claude-code
```

A checkout build has no published version, so it installs itself: the version
is the commit it was built from, exported from that checkout without the
network, and its binary is the one running the command.

```sh
cargo install --path appa-runtime --force
appa plugin install claude-code
```

The install reports each slow phase on stderr and never prompts. If another
APPA deployment owns the runtime endpoint, it refuses and names the process to
stop; an unidentified listener or another user's process is never stopped.

The install puts `clappa` beside `appa` so the short command works in later
examples.

Activation deploys the `appa` binary to a private path under the data
directory and writes that exact path into every hook entry of the user's
Claude Code settings (`~/.claude/settings.json`), so a hook never resolves
`appa` through `PATH`. It registers the runtime's `appa` MCP server in Claude
Code's user scope, writes the `appa-guide` skill and its policy-review guide
under `~/.claude/skills/appa-guide/`, installs `clappa`, preserves a custom
Claude statusline, and starts the runtime through the deployed binary's own
start, the one every protected session performs at SessionStart. A first
install writes the starting policy; a later one keeps the file it finds. A
successful command therefore proves that one runtime from the installed
version is active.

An install owns only what names its deployed binary. Other hook entries, an
MCP server or a skill of your own are left alone; an `appa` MCP server or an
`appa-guide` skill that no install wrote stops the install before it writes
anything. Re-running the install rewrites what changed and is otherwise a
no-op, so two installs never stack two hook sets.

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by the release assets.

The hook entries run the binary directly on every platform; on native Windows
the status line runs it through PowerShell.

### File locations

| System | Runtime | Policy | Database |
| --- | --- | --- | --- |
| Linux | `~/.local/share/appa/bin/appa runtime` | `~/.config/appa/appa.toml` | `~/.local/share/appa/` |
| macOS | `~/Library/Application Support/appa/bin/appa runtime` | `~/Library/Application Support/appa/appa.toml` | `~/Library/Application Support/appa/` |
| Windows | `%LOCALAPPDATA%\appa\bin\appa.exe runtime` | `%APPDATA%\appa\appa.toml` | `%LOCALAPPDATA%\appa\` |

The harness binary is APPA's own, not something you put on `PATH`: the hook
entries and the status line name that absolute path. `clappa` stays where a
shell can find it.

The runtime creates the starting policy only when the policy path does
not exist. It never replaces the policy or database.

Set `APPA_INSTALL_DIR`, `APPA_CONFIG_DIR`, or `APPA_DATA_DIR` in the
environment to change these locations; the install and the hooks follow them.

## Protect a Claude Code session

The hook entries run in every session but are inert until a session
opts in with `APPA_GATE=1`. Keep normal `claude` sessions unprotected
and use a separate `clappa` command for protected ones. The install creates
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

Only sessions started with `APPA_GATE=1` are protected. The binary reads
the variable from the Claude Code process environment, fixed at launch,
so a session cannot turn the protection off mid-session. A plain
`claude` session stays unprotected, and the binary prints nothing
into it. The entries live in the user's settings: a project whose settings
set `disableAllHooks` turns them off for its sessions, and `clappa` cannot
protect a session there.

A protected session starts the installed runtime at SessionStart when
nothing healthy answers `/health` — normally a no-op, because the install
left it running — or replaces a runtime that answers `stale <pid>`,
which a running process does once an install replaced its binary on
disk. It then blocks every action while the runtime is unavailable. The starter
never installs software; rerun `appa plugin install claude-code` when the
binary is missing. There is no login service: a runtime
that dies mid-session blocks the session until the next session start
brings it back. Check the runtime with:

```sh
curl -sS -m 2 http://127.0.0.1:8787/health
```

The command must print `ok`. It prints `stale <pid>` when the binary
was installed again after this runtime started; the next protected
session start replaces the process.

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
with `APPA_RUNTIME_URL` — the installed hook entries, the MCP server, and
the statusline all follow it:

```sh
cp marketplace/plugins/claude-code/default.appa.toml appa.toml
nohup cargo run --bin appa -- runtime --config appa.toml --db appa.db --listen 127.0.0.1:8788 >appa-runtime.log 2>&1 &
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude
```

That runs the checkout's runtime behind the installed binary's hooks. To run
the checkout's hook client as well, install the build (`cargo install --path
appa-runtime --force && appa plugin install claude-code`), or run
`live-gate-check.py`, which launches the harness with `--settings` entries
naming the built binary.

The start leaves a runtime at a URL of your own alone, stale or not:
after a rebuild, restart it yourself. The last command is interactive and belongs to the user: a Claude
session performing this setup runs the first two and prints the third.

`APPA_RUNTIME_URL` is fixed at session launch, like `APPA_GATE`: a
running session cannot be pointed at a different runtime. To move
between the installed and the dev runtime, start a new session.

## Harness conformance check

`live-gate-check.py` runs two real headless `claude` sessions against a
runtime process it starts itself, under a policy that states one flow:
reading a file narrows its content to the session, and writing a file
releases content to the outside world. By default, Claude Code talks to a
deterministic local model fixture, so the check needs no Claude account and
consumes no model usage.

```sh
uv run marketplace/plugins/claude-code/live-gate-check.py
```

It launches the harness with `--settings` hook entries naming the appa
binary, the shape the install writes, and registers the runtime's MCP
server for the session. It judges the gate on what reached the disk and
the runtime's supported trajectory-status projection. One session writes
words of the model's own and the file lands. The other reads a private
file, narrows the trajectory to `session`, proposes the write, and that
line then appears in no file under any name. The allowed write is what
stops a runtime that is down from passing as a refusal: the hooks fail
closed, so a gate that is not answering blocks both sessions rather than
one.

The check does not depend on APPA's internal event-log encoding. It needs the
`claude` CLI on `PATH` and an appa binary: a local build, an installed one, or
`APPA_BIN`. Set `CLAUDE_BIN` to use a specific Claude Code binary.

Use the same scenarios as a small compatibility canary against the configured
Claude account:

```sh
uv run marketplace/plugins/claude-code/live-gate-check.py --model live
```

Only this explicit live mode consumes Claude usage.

## Upgrade

Rerun the installer (or `cargo install` from the new checkout), then rerun
`appa plugin install claude-code`. It replaces the deployed runtime and the
retained version together, and preserves policy and database files.

**Restart any running `clappa` session after an upgrade.** Claude loads a
session's hooks at session start, and the hook wire between the hooks and the
runtime carries no version, so a session running across an upgrade keeps
talking to the runtime it started with.

Init stops a runtime that is still executing the path a previous init deployed
to, and aborts before touching Claude if it cannot, rather than registering
new hooks against an old runtime. That covers the paths your current
environment resolves to. A runtime left by an init run under a different
`APPA_INSTALL_DIR` or `APPA_DATA_DIR` executes a path this init never computes:
it is reported so you can stop it yourself, never killed. An `appa` left at the
old install path is named in the receipt and never deleted; remove it when you
are ready.

## Uninstall

```sh
appa plugin remove claude-code
pkill -f 'appa runtime'
rm -rf ~/.local/share/appa/bin ~/.local/share/appa/cache
rm -f ~/.local/bin/appa
cargo uninstall appa   # checkout builds only
```

`appa plugin remove claude-code` takes back only what an install wrote: its
hook entries, its `statusLine`, the `appa` MCP server, the skill, and
`clappa`. A statusline, hook entry or skill of your own survives untouched.

The policy and database stay at the locations in the table above; delete
them only if you want the history gone. Remove a `clappa` shell alias
separately if you added one instead of the command.

## Statusline

The install adds `appa statusline` to your global settings unless you
already have a custom statusline. In a protected session it shows the APPA
pixel mascot plus the session's current Trust and Audience, read from the
process's `GET /status`. In an unprotected session it prints nothing and
never queries the runtime, so regular `claude` has no APPA statusline. It
fails open inside a protected session: runtime down, unknown trajectory, or
malformed input prints the mascot alone, never a blocked action.

To set it manually, merge this into `~/.claude/settings.json`, naming the
`appa` binary by its absolute path:

```json
{
  "statusLine": {
    "type": "command",
    "command": "'/path/to/appa' statusline --deployment-url 'http://127.0.0.1:8787'"
  }
}
```

On native Windows, run it through PowerShell, with forward slashes in the
absolute path:

```json
{
  "statusLine": {
    "type": "command",
    "command": "powershell.exe -NoProfile -Command \"& 'C:/path/to/appa.exe' statusline --deployment-url 'http://127.0.0.1:8787'\""
  }
}
```

The setting applies to every session, protected or not; the `APPA_GATE`
check keeps the two states distinguishable at a glance.

On POSIX systems, keep an existing statusline such as claude-powerline and add
the APPA rows beneath it by running both and teeing stdin. Pin the exact
version you vetted. `@latest` would fetch and run new third-party code on
every statusline refresh:

```json
{
  "statusLine": {
    "type": "command",
    "command": "input=$(cat); printf '%s' \"$input\" | npx -y @owloops/claude-powerline@1.4.0; printf '%s' \"$input\" | '/path/to/appa' statusline --deployment-url 'http://127.0.0.1:8787'"
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
- **The integration adds roughly zero tokens to a session.** The protection
  is hooks and an MCP server, not prompt text.
