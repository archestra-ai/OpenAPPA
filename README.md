# OpenAPPA

OpenAPPA checks whether a value derived from declared sources can flow into a
proposed sink before an agent acts. The [Claude Code integration](integrations/claude-code/README.md)
gates prompts, tool calls, tool results, and child-agent returns through the
OpenAPPA runtime.

## Install

Each command downloads the latest installer. The installer detects the
operating system and architecture, verifies the selected release archive, and
installs the runtime with its Claude Code plugin and statusline files.

### Linux and macOS

Supports x86-64 and ARM64. Linux requires glibc 2.34 or newer.

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.sh | sh
```

If you download `install.sh` first, run `sh install.sh`. HTTP downloads do not
preserve its executable bit, so `./install.sh` can report `permission denied`.

For an access-controlled repository, authenticate GitHub CLI and run:

```sh
gh release download --repo archestra-ai/OpenAPPA --pattern install.sh --output - | sh
```

### Windows

Supports x86-64 and ARM64. The installer selects native PowerShell hooks and a
PowerShell statusline for Windows Terminal Claude Code.

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; irm https://github.com/archestra-ai/OpenAPPA/releases/latest/download/install.ps1 | iex
```

Streaming avoids the execution-policy error that can reject an unsigned saved
script. To run a saved copy, use
`powershell -ExecutionPolicy Bypass -File .\install.ps1`.

For an access-controlled repository, authenticate GitHub CLI and run:

```powershell
gh release download --repo archestra-ai/OpenAPPA --pattern install.ps1 --output - | iex
```

Only Claude Code sessions started with the installed plugin are gated. A gated
session blocks while the runtime is down. See the
[integration guide](integrations/claude-code/README.md) for installed paths,
uninstall commands, and the `/appa-tool-sync` policy setup flow.

## Documentation

- [How OpenAPPA works](website/content/docs/how-it-works.md)
- [Policy contracts](website/content/docs/contracts.md)
- [Normative specification](docs/spec.md)
- [Runtime](appa-runtime-v2/README.md)
