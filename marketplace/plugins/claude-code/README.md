# Claude Code integration

Everything needed to protect a Claude Code session through the
appa-runtime process lives in this directory: the plugin (hooks, the
`execute_remedy_plan` MCP server, the `appa-guide` skill), the
statusline script, example policies, and the install and uninstall
instructions below. The process itself is the `appa-runtime` crate;
its [README](../../appa-runtime/README.md) covers build,
configuration, and start.

The plugin registers hooks for prompts, tool calls, tool results, and
subagent start and finish. Blocking hooks post their events to the runtime
and refuse the hooked action if the runtime is unavailable. This covers
actions at those hook boundaries, not every observation or emission inside
Claude Code. Root Stop events report turn completion; they do not gate
already-visible output. A subagent started with the `Agent` tool runs
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

## Security scope: plugin first, proxy next

The goal is to enforce the guarantees available through a Claude Code plugin
and its bundled APPA runtime. An inference proxy is a possible extension,
not a prerequisite for installing the plugin. Guarantees requiring an OS
sandbox, filesystem interception, or a modified Claude Code are non-goals.
Assume no process outside Claude Code edits workspace files. This does not
exclude subprocesses launched by Claude Code itself.

### Plugin and bundled runtime

- Check proposals that reach `PreToolUse` before releasing the hooked call.
  Admit reported observations and execute remedies against the host-bound
  trajectory. Coverage depends on Claude invoking the corresponding hooks.
- Keep policy evaluation, Label combination, and durable state in the shared
  runtime. The plugin translates harness events; it does not define a separate
  file-Label algebra or persistence format.
- Runtime-owned file tools can check before content-dependent validation and
  admit results before returning them over MCP. The draft implements this path;
  normal plugin installation does not establish exclusive use of these tools.
- Refuse unsupported calls where a blocking hook is available. Such refusal
  does not cover work performed before the hook. Report missing coverage rather
  than claiming complete provenance for a partially observed trajectory.

Native Edit is a known boundary gap: a Claude Code 2.1.268 probe returned a
content-dependent match error before `PreToolUse`, with no APPA proposal.
The plugin cannot prevent that observation by denying the later hook. Native
Read/Write prevalidation coverage is not established. The constrained
`appa claude-files` launcher is an experimental test path, not proof that a
plugin installation disables native tools or implicit reads.

The next plugin work is to integrate and test the runtime-owned file tools
through the installed bundle, verify failure and interruption handling, and
record which native paths remain unmediated. Tests must inspect actual file
versions and observed results, not just hook responses. Incomplete operations
currently retain a durable reservation and stop file calls; automatic recovery
is not implemented. Recovery for runtime-owned operations remains in scope.

### Capabilities deferred to an inference proxy

For requests actually routed through it, a proxy could:

- Check context against the configured provider's permitted audience before
  forwarding a request, including retries and helper requests.
- Bind requests to the same trajectory as plugin events and account for
  resumed context, attachments, and compaction without resetting their Labels.
  Unclassified context must be refused or conservatively classified; an HTTP
  payload alone does not establish its provenance.
- Restrict provider-run features and admit their observations.
- Gate provider-generated text before forwarding response bytes to Claude,
  using the intended recipient and trajectory Label. This includes streaming,
  not only completed responses.

These capabilities are not implemented. A proxy does not undo a local
pre-hook observation or gate locally generated tool output, diagnostics, or
arbitrary subprocess traffic. Proxy coverage requires requests to use it;
preventing all bypass connections would require enforcement beyond this scope.

### Non-goals for a plugin plus inference proxy

- Complete native filesystem mediation, including pre-hook validation,
  implicit harness reads, and local output that neither layer can intercept.
- Precise dependencies or confinement for arbitrary Bash, child processes,
  background jobs, and other tools' hidden filesystem or network effects.
  Supported contracts may conservatively bound flows; shell parsing or a
  before/after directory diff cannot prove which inputs a process read.
- Protection against a process that can disable the plugin, modify policy or
  ledger files, or bypass the proxy. Execution controls remain trusted host
  state; a private directory alone is not an OS access boundary.
- Metadata and timing-flow guarantees, or protection against outside writers.

Disabling or refusing a capability is a supported restriction, not evidence
that its internal flows are tracked. The plugin's guarantees must remain
explicit about the checked boundary and its unobserved inputs.

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

`appa plugin install claude-code` installs one bundle: the plugin belonging to
the running binary and that binary. It selects a version, verifies every
artifact against that version's descriptor before anything outside a temporary
file changes, retains them under the deployment's `.appa/` state so a later
install needs no network, and activates the plugin with that version's own
binary. The result does not depend on the working directory.

A release binary installs the version published for its tag. The installer
verifies the checksum of the binary for Linux or macOS and places it in
`~/.local/bin` (Windows: unpack the zip from the releases page):

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa plugin install claude-code
```

A checkout build has no published version, so it installs itself: the version
is the plugin tree of the commit it was built from, exported from that checkout
without the network, and its binary is the one running the command.

```sh
cargo install --path appa-runtime --force
appa plugin install claude-code
```

The install reports each slow phase on stderr and never prompts. If another
APPA deployment owns the runtime endpoint, it refuses and names the process to
stop; an unidentified listener or another user's process is never stopped.

The install puts `clappa` beside `appa` so the short command works in later
examples.

It uninstalls an existing user-scoped APPA plugin and replaces its marketplace
before installing, so branch tests never stack two APPA hook sets.

Activation deploys the `appa` binary to a private path under the data directory
and renders that exact path into the hooks, so a hook never resolves `appa`
through `PATH`. A first install writes the starting policy; a later one keeps
the file it finds. Activation installs `clappa`, preserves a custom Claude
statusline, registers the plugin, and starts the runtime through the same
starter used at SessionStart. A successful command therefore proves that one
runtime and one plugin from the installed version are active.

Deployments are content-addressed and immutable: Claude is pointed at a
directory that cannot change under it, rather than at a checkout or a remote
marketplace. Re-running the install repairs a deployment whose structure or
rendered paths are wrong and is otherwise a no-op.

Linux binaries require glibc 2.34 or newer. Alpine and other musl-only
systems are not supported by the release assets.

The plugin ships POSIX and native Windows hook commands. The install activates
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
environment to change these locations; the install and the hooks follow them.

## Protect a Claude Code session

The plugin is present in every session but inert until a session
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

Only sessions started with `APPA_GATE=1` are protected. The hooks read
the variable from the Claude Code process environment, fixed at launch,
so a session cannot turn the protection off mid-session. A plain
`claude` session stays unprotected, and the plugin prints nothing
into it.

A protected session starts the installed runtime at SessionStart when
nothing healthy answers `/health` — normally a no-op, because the install
left it running — or replaces a runtime that answers `stale <pid>`,
which a running process does once an install replaced its binary on
disk. Blocking hooks refuse their actions while the runtime is unavailable. The starter
never installs software; rerun `appa plugin install claude-code` when the
binary or plugin is missing. There is no login service: a runtime
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
bundle=$(mktemp -d)
rmdir "$bundle"
scripts/appa-stage-plugin-bundle.sh "$bundle"
cp marketplace/plugins/claude-code/default.appa.toml appa.toml
nohup cargo run --bin appa -- runtime --config appa.toml --db appa.db --listen 127.0.0.1:8788 >appa-runtime.log 2>&1 &
APPA_GATE=1 APPA_RUNTIME_URL=http://127.0.0.1:8788 claude --plugin-dir "$bundle/plugin"
```

Staging materializes the canonical `integrations/appa-guide` skill at
Claude's required `plugin/skills/appa-guide` path.

The starter leaves a runtime at a URL of your own alone, stale or not:
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

It judges the gate on what reached the disk and the runtime's supported
trajectory-status projection. One session writes words of the model's own and
the file lands. The other reads a private file, narrows the trajectory to
`session`, proposes the write, and that line then appears in no file under any
name. The allowed write is what stops a runtime that is down from passing as a
refusal: the hooks fail closed, so a gate that is not answering blocks both
sessions rather than one.

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
`appa plugin install claude-code`. It replaces the deployed runtime and the APPA
marketplace together, always as one bundle, and preserves policy and database
files.

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
rm -f ~/.local/bin/appa ~/.local/bin/clappa ~/.local/bin/appa-statusline.sh
rm -f ~/.cargo/bin/clappa && cargo uninstall appa   # checkout builds only

# drop the statusline entry the install wrote, and keep one of your own:
jq 'if (.statusLine.command? // "") | test("appa-statusline") then del(.statusLine) else . end' \
  ~/.claude/settings.json > ~/.claude/settings.json.new &&
  mv ~/.claude/settings.json.new ~/.claude/settings.json
```

The install writes the `statusLine` entry into your own settings, so
removing the script alone leaves Claude Code running a command that no
longer exists. The `jq` line removes that entry only while it runs
`appa-statusline.sh`, so a statusline of your own survives untouched.

The policy and database stay at the locations in the table above; delete
them only if you want the history gone. Remove a `clappa` shell alias
separately if you added one instead of the command.

## Statusline

Claude Code reads `statusLine` only from your own global settings — a plugin
cannot set it. The install adds the platform script there unless you already
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
    "command": "/path/to/OpenAPPA/marketplace/plugins/claude-code/plugin/statusline.sh"
  }
}
```

On native Windows, use the PowerShell script and forward slashes in its
absolute path:

```json
{
  "statusLine": {
    "type": "command",
    "command": "\"C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe\" -NoProfile -ExecutionPolicy Bypass -File \"C:/path/to/OpenAPPA/marketplace/plugins/claude-code/plugin/statusline.ps1\""
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
    "command": "input=$(cat); printf '%s' \"$input\" | npx -y @owloops/claude-powerline@1.4.0; printf '%s' \"$input\" | /path/to/OpenAPPA/marketplace/plugins/claude-code/plugin/statusline.sh"
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
