# appa-runtime plugin

Protects a Claude Code session through the appa-runtime-v2 process:
the hooks send every event to it, and the `execute_remedy_plan` MCP
server lets the model pursue an offered remedy. Hooks fail closed —
while the process is down, every action in a protected session is
blocked.

The plugin protects only sessions launched with `APPA_GATE=1` (the
`clappa` alias). The hooks read the variable from the Claude Code
process environment, fixed at launch, so a session cannot turn the
protection off. In every other session the plugin is inert: it checks
nothing, starts nothing, and only announces once, at session start,
that the beta is available and `clappa` starts a protected session
(`hooks/beta-announcement.md`) — or, when the runtime binary is not
installed, offers its installation as a prompted task
(`hooks/setup-appa.md`).

The plugin ships the POSIX hook commands (`hooks/hooks.json`). Native
Windows swaps in `hooks/hooks.windows.json`, which drives the
`hooks/hook.ps1` adapter to block failed prompt, tool-call, and
successful tool-result admission; WSL runs the POSIX hooks as-is.
`statusline.sh` and `statusline.ps1` provide matching status displays
without changing Claude's settings automatically; unprotected sessions show
the mascot with a `clappa` reminder instead of runtime status.

A protected session starts the runtime itself: at session start,
`hooks/ensure-runtime.sh` (on Windows, `hook.ps1`) launches the
installed `appa-runtime-v2` when nothing healthy answers `/health`, and
the session proceeds only once it does. When the binary is not
installed at all, an unprotected session offers the install as a
prompted task:
`hooks/setup-appa.md` tells the model how to download, verify, and
install the release binary on request, under the session's normal
command approval — so the plugin alone completes the install. A runtime
that dies mid-session still blocks the session until the next session
start brings it back.

On session start the hooks also print `hooks/session-context.md` into
the model's context: short guidance on how to act in a protected session
(blocks are decisions, run a clear remedy plan without asking, explain
blocks to the user simply). Advice only; it enforces nothing.

Install and uninstall instructions live one level up
([README](../README.md)); build, configuration, and start of the
process itself in the crate's
[README](../../../appa-runtime-v2/README.md). The short form:

```sh
# one session, no installation
APPA_GATE=1 claude --plugin-dir /path/to/OpenAPPA/integrations/claude-code/plugin

# installed from the repository marketplace
claude plugin marketplace add archestra-ai/OpenAPPA
claude plugin install appa-runtime@appa
alias clappa='APPA_GATE=1 claude'

# installed from a checkout (--scope local, --scope project, or no flag)
claude plugin marketplace add /path/to/OpenAPPA/integrations/claude-code
claude plugin install appa-runtime@appa
```
