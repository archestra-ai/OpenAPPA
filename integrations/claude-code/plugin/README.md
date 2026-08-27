# appa-runtime plugin

Protects a Claude Code session through the appa-runtime process:
the hooks send every event to it, and the `execute_remedy_plan` MCP
server lets the model pursue an offered remedy. Hooks fail closed —
while the process is down, every action in a protected session is
blocked.

The plugin protects only sessions launched with `APPA_GATE=1` (the
`clappa` command). The hooks read the variable from the Claude Code
process environment, fixed at launch, so a session cannot turn the
protection off. In every other session the plugin is inert: it checks
nothing, starts nothing, and prints nothing. The native `appa init
claude-code` command installs the runtime and registers this plugin.

The plugin ships the POSIX hook commands (`hooks/hooks.json`). Native
Windows swaps in `hooks/hooks.windows.json`, which drives the
`hooks/hook.ps1` adapter to block failed prompt, tool-call, and
successful tool-result admission; WSL runs the POSIX hooks as-is.
`statusline.sh` and `statusline.ps1` provide matching status displays
without changing Claude's settings automatically; unprotected sessions show
the mascot alone instead of runtime status.

Initialization and every protected session share one starter,
`hooks/ensure-runtime.sh` (on Windows, `hook.ps1 -EnsureRuntime`): it
launches the installed `appa-runtime` when nothing healthy answers
`/health` and returns only once one does. A running runtime answers
`stale <pid>` once initialization replaced its binary on disk; the starter
stops that process and starts the installed build in its place. The
last step of initialization runs it, so a protected session normally finds
the runtime already up and its SessionStart start is a single health
probe. The starter never installs software. A runtime that dies mid-session
still blocks the session until the next session start brings it back.

The `appa describe` command reports configuration and policy facts without
starting the runtime or creating files; session tool and account facts remain
the configuring session's responsibility.

Concurrent starts need no lock: the runtime binds the loopback port, so
the first process to bind serves and every later one exits at once. Each
hook declares a timeout longer than its own deadline, because Claude Code
kills a hook that outruns the timeout and then lets the action proceed —
an undeclared timeout is the one way these hooks fail open.

On session start the hooks also print `hooks/session-context.md` into
the model's context: short guidance on how to act in a protected session
(blocks are decisions, run a clear remedy plan without asking, explain
blocks to the user simply). Advice only; it enforces nothing.

Install and uninstall instructions live one level up
([README](../README.md)); build, configuration, and start of the
process itself in the crate's
[README](../../../appa-runtime/README.md). The short form:

```sh
# one session, no installation
APPA_GATE=1 claude --plugin-dir /path/to/OpenAPPA/integrations/claude-code/plugin

# installed from this checkout
cargo install --path appa-runtime --root ~/.local --force
~/.local/bin/appa init claude-code
~/.local/bin/clappa
```
