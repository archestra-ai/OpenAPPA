---
title: What is a battery
category: Batteries
order: 6.5
description: How batteries are structured, combined, and run.
---

A battery is an OpenAPPA config for a set of tools, such as Claude Code or Slack. It may include small scripts that make decisions for each tool call.

Most batteries are for MCP servers. A battery needs no extra format or server.

## Battery structure

Keep each battery and its annotator scripts together:

```text
batteries/
|-- claude-code/
|   `-- appa.toml
`-- slack/
    `-- appa.toml
```

List batteries in the root `appa.toml`:

```toml
include = [
  "./batteries/claude-code/appa.toml",
  "./batteries/slack/appa.toml",
]

[policy]
version = 2
```

OpenAPPA combines these files into one config.

| Item | Rule |
|---|---|
| `include` | Only the root file can use it. |
| Paths | Paths are local and relative to the root file. |
| Version | Every file uses the same version. |
| Annotators | Each annotator name must be unique. |
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

Slack history uses the same first-match rule. The battery labels all history `internal`, the built-in audience of the organization's members. A root rule can mark one channel as untrusted, for example a channel shared with people outside your company:

```toml
# root config: a shared channel may contain outsiders' words
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_read_channel(channel_id:C0SHARED*)"
delta = { trust = "suspicious", audience = ["internal"] }

# shipped battery: every other channel is internal and keeps its trust
[[policy.tool]]
name = "mcp__claude_ai_Slack__slack_read_channel"
delta = { audience = ["internal"] }
```

## Use annotators in batteries

A battery can ship an annotator script beside its config. See [Annotators](/contracts#annotators) for what annotators receive and return.

A tool rule that names an annotator carries no static `delta`, `requires`, or `effects`: the annotator produces the call's complete contract. The battery binds the annotator name to its script. An annotation names literal readers only, never `self` or `internal`, so a battery that labels organization or requester data writes static rules for it; an annotator decides trust and attention. This one asks a person before a hidden file is read:

```toml
[[policy.annotator]]
name = "local.read-attention"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "Read"
annotator = "local.read-attention"

[externals.annotators."local.read-attention"]
command = ["python3", "./read-attention.py"]
```

### Run a local annotator

On Unix systems, OpenAPPA starts the script when a tool call matches its rule. It sends one JSON consult, reads one JSON answer, and waits for the script to exit.

Other platforms reject script commands when the config loads.

Consult:

```json
{"version":1,"kind":"annotation","name":"local.read-attention","declaration":{"inputs":[],"trust_ranks":["suspicious","trusted"],"audiences":[],"attention_marks":["hitl"],"effects":[]},"artifact":{"args":{"name":"Read","arguments":{"file_path":".env"}}}}
```

Answer:

```json
{"version":1,"answer":{"delta":{},"requires":{"history":[],"attention":["hitl"]},"emits":[]}}
```

The annotator can be any program. Here is a Python example:

```python
import json
import sys
from pathlib import PurePath

request = json.load(sys.stdin)
file_path = request["artifact"]["args"]["arguments"]["file_path"]
attention = ["hitl"] if PurePath(file_path).name.startswith(".") else []

json.dump(
    {
        "version": 1,
        "answer": {
            "delta": {},
            "requires": {"history": [], "attention": attention},
            "emits": [],
        },
    },
    sys.stdout,
)
```

`artifact.args` contains the tool's `name`, `arguments`, and declared `description`. The `Read` rule above has no description, so the consult omits it.

When an annotator maps `inputs`, the consult includes one Value for each input. `declaration` carries the optional `hint`, input names, and mandate vocabulary.

A battery annotator must check the version, `kind`, `name`, tool name, and argument types. It must exit with an error when the input is invalid.

OpenAPPA runs the command without a shell. The script path is relative to the battery config. The battery folder is its working directory.

Script changes apply on the next call. No restart is needed.

## Annotator precedence

Annotators do not have their own order. The first matching tool rule decides which annotator, if any, runs.

A rule written above the rule that names an annotator wins without running it. Here `README.md` is read without a question; every other path asks the annotator:

```toml
[[policy.tool]]
name = "Read(file_path:README.md)"
delta = {}

[[policy.annotator]]
name = "local.read-attention"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "Read"
annotator = "local.read-attention"

[externals.annotators."local.read-attention"]
command = ["python3", "./read-attention.py"]
```

`README.md` matches the first rule, so `read-attention.py` does not run.

The Claude Code battery labels `Read` with static rules of the same shape:
hidden paths, credential and private-key names, and system secret locations
narrow the session to `self`, the requester; other paths keep its label.

The Claude Code battery also sends each Bash call to its built-in model
annotator. The annotator produces the command's complete contract: the
output's trust and the call's trust and attention requirements. Its mandate
names no reader, so the audience of a command's output is the session's.

This check is not an operating-system sandbox. Bash can read files
and open network connections. Use a sandbox to protect credentials and network
access.

## Customise a battery

Define tool rules and Annotator customizations in the root config. Root tool rules run before battery rules, even when they appear below `include`. A root Annotator declaration replaces the battery declaration with the same name.

```toml
include = [
  "./batteries/claude-code/appa.toml",
  "./batteries/slack/appa.toml",
]

[policy]
version = 2

[[policy.tool]]
name = "Bash(command:kubectl)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "Bash(command:kubectl *)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.annotator]]
name = "local.read-sensitivity"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "Read"
annotator = "local.read-sensitivity"

[externals.annotators."local.read-sensitivity"]
command = ["python3", "./local/read-sensitivity.py"]
```

No authority permits `blocked`, so both `kubectl` contracts have no remedy.
Every other Bash call reaches the battery's model annotator. The root also
replaces the battery's `Read` rules with a local script.

The battery files stay unchanged.

## Audience sources

OpenAPPA includes audience sources for Google Workspace, Slack, and GitHub. You can use them without including a battery config.

A policy uses these sources to build `self`, `internal`, and named group audiences. OpenAPPA contacts only the sources that the policy uses.

See [Audiences](/contracts#audiences) for the available selectors and the JSON that each source receives.

## Refuse when a script fails

OpenAPPA refuses the tool call when an annotator script is missing, crashes, times out, or returns bad data. This is an execution error, not a policy decision.

The error names the annotator and the problem. It does not show the data the annotator was checking.

Annotator scripts run as trusted local code. Review what they can read, run, and send.

A production file annotator must check full paths, hidden folders, symbolic links, files ignored by Git, and files outside the project.
