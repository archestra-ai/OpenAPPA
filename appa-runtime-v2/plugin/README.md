# appa-runtime plugin

Gates a Claude Code session through the appa-runtime-v2 process: the
hooks send every event to it, and the `execute_remedy_plan` MCP server
lets the model pursue an offered remedy. Hooks fail closed — while the
process is down, every action in a gated session is blocked.

Build, configuration, start, and install instructions live in the
crate's [README](../README.md). The short form:

```sh
# one session, no installation
claude --plugin-dir /path/to/OpenAPPA/appa-runtime-v2/plugin

# permanent
claude plugin marketplace add /path/to/OpenAPPA/appa-runtime-v2
claude plugin install appa-runtime@appa
```

Start the process first; it cannot be started from inside a gated
session, because the session's own commands are blocked until it
answers.
