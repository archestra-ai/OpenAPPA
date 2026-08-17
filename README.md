# OpenAPPA

OpenAPPA checks whether a value derived from declared sources can flow into a
proposed sink before an agent acts. The [Claude Code integration](integrations/claude-code/README.md)
gates prompts, tool calls, tool results, and child-agent returns through the
OpenAPPA runtime.

## Install

Linux and macOS:

```sh
curl -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.ps1 | iex
```

Each installer selects the x86-64 or ARM64 build for the current system,
verifies its checksum, and installs the runtime with its Claude Code plugin and
statusline. Linux requires glibc 2.34 or newer. To read a script before you run
it, replace `| sh` with `| less`, or `| iex` with `| more`.

Only Claude Code sessions started with the installed plugin are gated. A gated
session blocks while the runtime is down. See the
[integration guide](integrations/claude-code/README.md) for installed paths,
version pinning, uninstall commands, and the `/appa-tool-sync` policy setup
flow.

## Documentation

- [How OpenAPPA works](website/content/docs/how-it-works.md)
- [Policy contracts](website/content/docs/contracts.md)
- [Normative specification](docs/spec.md)
- [Runtime](appa-runtime-v2/README.md)
