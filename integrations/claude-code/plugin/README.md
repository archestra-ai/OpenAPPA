# appa-runtime plugin

Gates a Claude Code session through the appa-runtime-v2 process: the
hooks send every event to it, and the `execute_remedy_plan` MCP server
lets the model pursue an offered remedy. Hooks fail closed — while the
process is down, every action in a gated session is blocked.

Release installers select platform hooks. Linux and macOS use the bundled
POSIX commands. Native Windows uses `hooks/hook.ps1` to block failed prompt,
tool-call, and successful tool-result admission. `statusline.sh` and
`statusline.ps1` provide matching status displays without changing Claude's
settings automatically.

On session start the hooks also print `hooks/session-context.md` into
the model's context: short guidance on how to act in a gated session
(blocks are decisions, run a clear remedy plan without asking, explain
blocks to the user simply). Advice only; it enforces nothing.

Install and uninstall instructions live one level up
([README](../README.md)); build, configuration, and start of the
process itself in the crate's
[README](../../../appa-runtime-v2/README.md). The short form:

```sh
# one session, no installation
claude --plugin-dir /path/to/OpenAPPA/integrations/claude-code/plugin

# installed (--scope local, --scope project, or no flag for all projects)
claude plugin marketplace add /path/to/OpenAPPA/integrations/claude-code
claude plugin install appa-runtime@appa
```

Start the process first; it cannot be started from inside a gated
session, because the session's own commands are blocked until it
answers.
