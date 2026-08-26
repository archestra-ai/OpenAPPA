---
title: Batteries
category: Integration
order: 7
description: Combine OpenAPPA configs and run resolver scripts.
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

The Slack battery asks a person before any message is sent. A root rule placed above it lets one channel through:

```toml
# root config: trusted messages go to the engineering channel without a question
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_send_message(channel_id:C0ABC*)"
requires = { trust = "trusted" }
delta = {}

# shipped battery: everything else needs fresh human approval
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_send_message"
requires = { trust = "trusted", attention = ["hitl"] }
delta = {}
```

Text inside parentheses matches arguments. Write one or more `argument:pattern` clauses and separate them with commas; every clause must match. `*` matches any text.

A bare tool name is the default. It matches when no earlier rule did.

Slack history uses the same first-match rule. The battery keeps all history private. A root rule can mark one channel as untrusted, for example a channel shared with people outside your company:

```toml
# root config: a shared channel may contain outsiders' words
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_read_channel(channel_id:C0SHARED*)"
delta = { trust = "suspicious", audience = ["private"] }

# shipped battery: every other channel is private and keeps its trust
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_read_channel"
delta = { audience = ["private"] }
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

### Run a local resolver

On Unix systems, OpenAPPA starts the command only when the selected tool rule uses the resolver. It writes one JSON consult, reads one JSON answer, then waits for the script to exit. Other platforms reject the command binding when the configuration loads.

Consult:

```json
{"version":1,"kind":"dynamic","name":"claude-code.read-sensitivity","declaration":{"returns":["delta.audience"],"trust_ranks":["suspicious","trusted"],"attention_marks":[]},"artifact":{"args":{"name":"Read","arguments":{"file_path":".env"}}}}
```

Answer:

```json
{"version":1,"answer":{"delta.audience":["private"]}}
```

The resolver can be any program. Here is a Python example:

```python
import json
import sys
from pathlib import PurePath

request = json.load(sys.stdin)
file_path = request["artifact"]["args"]["arguments"]["file_path"]
audience = ["private"] if PurePath(file_path).name.startswith(".") else "public"

json.dump(
    {"version": 1, "answer": {"delta.audience": audience}},
    sys.stdout,
)
```

`artifact.args` is the complete call: `name`, `description` when the tool declares one, and `arguments`. The `Read` rule above declares none, so the consult carries none. `declaration` is the resolver's own registration — its `returns` and the policy vocabulary its answer must use. A battery resolver must check the version, `kind`, `name`, tool name, and argument types. It must exit with an error for bad input.

OpenAPPA runs the command without a shell. The script path is relative to the battery config. Its folder is the working folder.

Script changes apply on the next call. No restart is needed.

## Resolver precedence

Resolvers do not have their own order. The first matching tool rule decides which resolver, if any, runs.

A rule written above the rule that uses a resolver wins without running it. Here `README.md` is public by rule; every other path asks the resolver:

```toml
[[policy.tool]]
name = "Read(file_path:README.md)"
delta = {}

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

`README.md` matches the first rule, so `read-sensitivity.py` does not run.

The resolver returns `["private"]` for hidden paths, credential and private-key
names, system-secret locations, and sensitive symlink targets. It returns
`"public"` for other paths.

The Claude Code battery also sends each Bash call to its `claude-code` model
resolver. The resolver classifies the trust and audience requirements of the
command. Bash output remains suspicious and private.

This classification is not an operating-system sandbox. Bash can read files
and open network connections. Use a sandbox to protect credentials and network
access.

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
name = "Bash(command:kubectl)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["private"] }

[[policy.tool]]
name = "Bash(command:kubectl *)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["private"] }

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

No authority permits `blocked`, so both `kubectl` contracts have no remedy.
Every other Bash call reaches the battery's model resolver. The root also uses
a local script for `Read`.

The battery files stay unchanged.

## Block when a script fails

OpenAPPA blocks the tool call when a resolver script is missing, crashes, times out, or returns bad data.

The error names the resolver and the problem. It does not show the data the resolver was checking.

Resolver scripts run as trusted local code. Review what they can read, run, and send.

A production file resolver must check full paths, hidden folders, symbolic links, files ignored by Git, and files outside the project.
:::
