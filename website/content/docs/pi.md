---
title: Pi
nav_title: Pi
category: Works with
order: 7.5
description: Protect Pi coding agent sessions with OpenAPPA through the community pi-openappa extension.
---

:::community-note:::

[Pi](https://pi.dev) is a terminal coding agent. [Andres Monge](https://github.com/aemonge) built the [pi-openappa](https://github.com/aemonge/pi-openappa) extension that connects it to OpenAPPA. The extension sends every tool call in a protected Pi session to the APPA runtime before the call runs. It also sends every tool result before the model sees it. The runtime owns every decision: the extension only translates Pi events into [hook events](/add-to-agent) and applies the answer.

If the runtime cannot answer, the extension blocks the call and returns the reason to the model.

## Install

You need the `appa` binary on `PATH` and a policy (`appa.toml`) that declares the tools your sessions use. See [Claude Code](/claude-code) for the `appa` install.

```sh
pi install npm:pi-openappa
```

## Protect sessions

Protection is opt-in. Pick one:

| Scope | How |
|---|---|
| One project | Create `<project>/.pi/openappa`. Its optional content names the project's policy file. |
| Every session | Run `/appa on` in Pi. `/appa off` disables it. |
| One launch | Start Pi with `APPA_GATE=1 pi`. |

```sh
cd your-project && mkdir -p .pi && echo "appa.toml" > .pi/openappa
```

A protected session starts the APPA runtime on its own. Run `/appa` to see protection state and runtime health.

See the [pi-openappa README](https://github.com/aemonge/pi-openappa) for configuration variables and the full event mapping.
