---
title: Batteries
category: Integration
order: 7
description: A proposal for combining OpenAPPA configs and running resolver scripts.
---

:::proposal
name: Batteries
date: 2026-08-21
author: Ildar Iskhakov

A battery is an OpenAPPA config for a set of tools, such as Claude Code or Slack. It may include small scripts that make decisions for each tool call.

```text
batteries/
|-- claude-code/
|   |-- appa.toml
|   |-- bash-review.py
|   `-- read-sensitivity.py
`-- slack/
    `-- appa.toml
```

A battery is ordinary OpenAPPA config. It needs no extra format or server.

## Add batteries

List batteries in the root `appa.toml`:

```toml
include = [
  "./batteries/claude-code/appa.toml",
  "./batteries/slack/appa.toml",
]

[policy]
version = 1
```

OpenAPPA combines these files into one config.

| Item | Rule |
|---|---|
| `include` | Only the root file can use it. |
| Paths | Paths are local and relative to the root file. |
| Version | Every file uses the same version. |
| Resolvers | Each resolver name must be unique. |
| Script path | A script path is relative to the config file that names it. |

OpenAPPA checks the combined config before using it. If a reload fails, the current config keeps running.

## Rule order

OpenAPPA checks root rules from top to bottom. It then checks each battery in `include` order, also from top to bottom.

:::battery-rule-order:::

The first match wins. The position of `include` in the root file does not change this order.

Trusted messages can go to engineering channels. Messages to other channels need fresh human approval.

```toml
[[policy.tool]]
name = "mcp__slack__slack_send_message(channel_id:C123*)"
requires = { trust = "trusted" }
effects = ["egress", "mutation"]
delta = {}

[[policy.tool]]
name = "mcp__slack__slack_send_message"
requires = { trust = "trusted", attention = ["hitl"] }
effects = ["egress", "mutation"]
delta = {}
```

Text inside parentheses matches one argument. `*` matches any text.

A bare tool name is the default. It matches when no earlier rule did.

Slack history uses the same first-match rule. Engineering history stays inside Slack. Other history is kept separate.

```toml
[[policy.tool]]
name = "mcp__slack__slack_get_channel_history(channel_id:C123*)"
delta = { trust = "suspicious", audience = ["slack-internal"] }

[[policy.tool]]
name = "mcp__slack__slack_get_channel_history"
delta = { trust = "suspicious", audience = ["slack-unclassified"] }
```

## Use resolvers in batteries

A battery can ship a resolver script beside its config. See [Dynamic resolvers](/contracts#dynamic-resolvers) for what resolvers receive and return.

The battery binds the resolver name to its script:

```toml
[[policy.dynamic_resolver]]
name = "claude-code.read-sensitivity"
returns = ["delta.audience"]

[[policy.tool]]
name = "Read"
uses = [{ resolver = "claude-code.read-sensitivity" }]
delta = { trust = "suspicious" }

[externals.dynamic."claude-code.read-sensitivity"]
command = ["python3", "./read-sensitivity.py"]
```

### Proposed local command runner

> **Proposal:** Running a resolver as a local command is part of this proposal. It is not implemented.

OpenAPPA starts the command only when the selected tool rule uses the resolver. It writes one JSON request, reads one JSON result, then waits for the script to exit.

Request:

```json
{"version":1,"resolver":"claude-code.read-sensitivity","args":{"name":"Read","arguments":{"file_path":".env"}}}
```

Result:

```json
{"version":1,"result":{"delta.audience":["claude-session"]}}
```

The resolver can be any program. Here is a Python example:

```python
import json
import sys

request = json.load(sys.stdin)
file_path = request["args"]["arguments"]["file_path"]
audience = ["claude-session"] if file_path.startswith(".") else "public"

json.dump(
    {"version": 1, "result": {"delta.audience": audience}},
    sys.stdout,
)
```

`args` is the complete call: `name`, `description` when the tool declares one, and `arguments`. The `Read` rule above declares none, so the request carries none. A battery resolver must check the version, resolver name, tool name, and argument types. It must exit with an error for bad input.

OpenAPPA runs the command without a shell. The script path is relative to the battery config. Its folder is the working folder.

Script changes apply on the next call. No restart is needed.

## Resolver precedence

Resolvers do not have their own order. The first matching tool rule decides which resolver, if any, runs.

This Claude Code battery handles `cargo test` directly. Other Bash commands go to the resolver:

```toml
[[policy.tool]]
name = "Bash(command:cargo test)"
requires = { trust = "trusted" }
delta = { trust = "suspicious", audience = ["claude-session"] }

[[policy.dynamic_resolver]]
name = "claude-code.bash-review"
returns = ["requires.attention"]

[[policy.tool]]
name = "Bash"
uses = [{ resolver = "claude-code.bash-review" }]
requires = { trust = "trusted" }
delta = { trust = "suspicious", audience = ["claude-session"] }

[externals.dynamic."claude-code.bash-review"]
command = ["python3", "./bash-review.py"]
```

`cargo test` matches the first rule, so `bash-review.py` does not run.

The resolver returns an empty list when no approval is needed. It returns `["hitl"]` for all other commands.

This only controls approval. It does not make Bash output Public. The output stays inside the Claude session.

Bash can read files and open network connections on its own. Use an operating-system sandbox to protect credentials and network access.

## Customise batteries for your own setup

Put your rules in the root config. They run before battery rules, even when they appear below `include`.

```toml
include = [
  "./batteries/claude-code/appa.toml",
  "./batteries/slack/appa.toml",
]

[policy]
version = 1

[[policy.tool]]
name = "Bash(command:cargo test)"
requires = { trust = "trusted", attention = ["hitl"] }
delta = { trust = "suspicious", audience = ["claude-session"] }

[[policy.dynamic_resolver]]
name = "local.read-sensitivity"
returns = ["delta.audience"]

[[policy.tool]]
name = "Read"
uses = [{ resolver = "local.read-sensitivity" }]
delta = { trust = "suspicious" }

[externals.dynamic."local.read-sensitivity"]
command = ["python3", "./local/read-sensitivity.py"]
```

The root asks for approval before `cargo test`. It also uses a local script for `Read`.

The battery files stay unchanged.

## Block when a script fails

OpenAPPA blocks the tool call when a resolver script is missing, crashes, times out, or returns bad data.

The error names the resolver and the problem. It does not show the data the resolver was checking.

Resolver scripts run as trusted local code. Review what they can read, run, and send.

A production file resolver must check full paths, hidden folders, symbolic links, files ignored by Git, and files outside the project.
:::
