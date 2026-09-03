---
title: Batteries
category: Deep Dive
order: 4
description: Combine OpenAPPA configs and run annotator scripts.
---

A battery is an OpenAPPA config for a set of tools, such as Claude Code or Slack. It may include small scripts that make decisions for each tool call.

```text
batteries/
|-- claude-code/
|   `-- appa.toml
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

On Unix systems, OpenAPPA starts the command only when the selected tool rule names the annotator. It writes one JSON consult, reads one JSON answer, then waits for the script to exit. Other platforms reject the command binding when the configuration loads.

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

`artifact.args` is the complete call: `name`, `description` when the tool declares one, and `arguments`. The `Read` rule above declares none, so the consult carries none. When the annotator maps `inputs`, the consult carries one value per mapped input instead. `declaration` is the annotator's own registration — the mandate vocabulary its answer must use. A battery annotator must check the version, `kind`, `name`, tool name, and argument types. It must exit with an error for bad input.

OpenAPPA runs the command without a shell. The script path is relative to the battery config. Its folder is the working folder.

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

The Claude Code battery also sends each Bash call to its stock model
annotator. The annotator produces the command's complete contract: the
output's trust and the call's trust and attention requirements. Its mandate
names no reader, so the audience of a command's output is the session's.

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

This build ships one audience source per provider — `google-workspace`, `slack`, and `github` — each with a fixed set of selector templates, available with no include. A policy maps their collections into the built-in `self` and `internal` audiences and into `[[audience.group]]` entries; only providers the policy references enter the policy identity. The [Policy reference](/contracts#audiences) has the catalog, the level rules, and the consult each source answers.

## Refuse when a script fails

OpenAPPA refuses the tool call when an annotator script is missing, crashes, times out, or returns bad data. The call is not judged: the refusal is operational, not a policy decision.

The error names the annotator and the problem. It does not show the data the annotator was checking.

Annotator scripts run as trusted local code. Review what they can read, run, and send.

A production file annotator must check full paths, hidden folders, symbolic links, files ignored by Git, and files outside the project.
