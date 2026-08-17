# OpenAPPA

## Install

```sh
gh auth login && gh auth setup-git   # while the repo is private
claude plugin marketplace add archestra-ai/OpenAPPA
claude plugin install appa-runtime@appa
claude "set up APPA"
```

`clappa` starts a gated session; plain `claude` stays ungated.

## Setup

The default policy covers Claude Code's built-in tools only. Start
`clappa` and run `/appa-tool-sync` to add your MCP tools to the policy.

## Upgrade

The plugin tracks the marketplace. To upgrade the runtime, remove
`~/.local/bin/appa-runtime-v2` and ask a plain `claude` session to set up
APPA again; it installs the latest release.

## Uninstall

```sh
claude plugin uninstall appa-runtime
claude plugin marketplace remove appa
rm ~/.local/bin/appa-runtime-v2

# optional — also remove the policy, database, and alias:
rm -rf ~/.config/appa ~/.local/share/appa      # Linux
rm -rf ~/Library/"Application Support/appa"    # macOS
sed -i.bak '/clappa/d' ~/.zshrc                # or ~/.bashrc
```

## Development

```sh
claude "set up APPA from this repo for local development"
```

Claude starts the dev runtime from source on its own port and prints
the command that starts a gated session against it. The steps live in
the [integration guide](integrations/claude-code/README.md).

## Documentation

- [Integration guide](integrations/claude-code/README.md) — Windows, file locations
- [How OpenAPPA works](website/content/docs/how-it-works.md)
- [Policy contracts](website/content/docs/contracts.md)
- [Normative specification](docs/spec.md)
- [Runtime](appa-runtime-v2/README.md)
