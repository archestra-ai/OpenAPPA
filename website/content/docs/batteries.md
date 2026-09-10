---
title: What is a battery
category: Batteries
order: 6.5
description: How batteries are structured, combined, and run.
---

A battery is a reusable OpenAPPA policy configuration for a set of tools, such as Slack MCP or built-in Claude Code tools. It can also include [annotators](/contracts#annotators), [sanitizers](/contracts#sanitizers), and [authorities](/contracts#authorities), with implementations that can be any executable program.

Most batteries are for MCP servers.

## Battery structure

Keep each battery and its annotator scripts together:

```text
batteries/
|-- claude-code/
|   `-- appa.toml
`-- slack/
    |-- appa.toml
    `-- audience-source.py
```

To include batteries in your config, list them in `appa.toml`:

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
| Audience sources | A battery binds the membership service it ships and declares its selector templates. A root entry for the same provider may carry only `lookup`. |

OpenAPPA checks the combined config before using it. If a reload fails, the current config keeps running.

## Policy order when batteries are used

OpenAPPA checks root rules from top to bottom. It then checks each battery in `include` order, also from top to bottom. The first match wins.

:::battery-rule-order:::

In this example, the Slack battery requires human approval for every message. The root config overrides this rule for one channel, so messages to that channel do not need approval:

```toml
# root config: trusted messages go to the engineering channel without a question
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_send_message(channel_id:C0ABC*)"
requires = { trust = "trusted" }
delta = {}

# included battery: everything else needs fresh human approval
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_send_message"
requires = { trust = "trusted", attention = ["hitl"] }
delta = {}
```

In another example, the battery labels all Slack history `internal`. A root rule can mark one channel as untrusted, such as a channel shared with people outside your company:

```toml
# root config: a shared channel may contain outsiders' words
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_read_channel(channel_id:C0SHARED*)"
delta = { trust = "suspicious", audience = ["internal"] }

# included battery: every other channel is internal and keeps its trust
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_read_channel"
delta = { audience = ["internal"] }
```

## Use annotators in batteries

A battery can ship an annotator script beside its config. An annotator sets a tool's contract for each call. A root config can use an annotator supplied by an included battery. See [Annotators](/contracts#annotators) for what annotators receive and return.

This example uses `approve-hidden-file-read`. It calls a local Python script that checks whether the file name starts with a dot. If it does, the script returns an attention requirement before the file is read:

```toml
[[policy.annotator]]
name = "approve-hidden-file-read"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "host/claude-code/Read"
annotator = "approve-hidden-file-read"

[externals.annotators.approve-hidden-file-read]
command = ["python3", "./approve-hidden-file-read.py"]
```

## Use sanitizers in batteries

A battery can ship a sanitizer script beside its config. A sanitizer rewrites data before a tool receives it or before a result returns to the agent. A root config can use a sanitizer supplied by an included battery. See [Sanitizers](/contracts#sanitizers) for what sanitizers receive and return.

This example uses `remove-email-addresses`. It calls a local Python script that removes email addresses before a message is sent:

```toml
[[policy.tool]]
name = "mcp/messaging/SendMessage"
tags = ["messages"]
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.sanitizer]]
name = "remove-email-addresses"
on = ["tool_input"]
tags = ["messages"]

[policy.sanitizer.permits]
audience = { from = ["private"], to = ["public"] }

[externals.sanitizers.remove-email-addresses]
command = ["python3", "./remove-email-addresses.py"]
```

## Use authorities in batteries

A battery can ship an authority script beside its config. An authority approves or denies one blocked tool call within declared limits. A root config can use an authority supplied by an included battery. See [Authorities](/contracts#authorities) for what authorities receive and return.

This example uses `approve-small-payment`. It calls a local Python script that approves payments of USD 100 or less. Larger payments remain blocked:

```toml
[[policy.authority]]
name = "approve-small-payment"

[policy.authority.permits]
attention = ["payment-approval"]

[[policy.tool]]
name = "mcp/payments/SendPayment"
requires = { attention = ["payment-approval"] }
delta = {}

[externals.authorities.approve-small-payment]
command = ["python3", "./approve-small-payment.py"]
```

## Audience sources

A battery that covers a provider with its own directory ships a membership service beside its config and binds it. The binding declares the selector templates the service understands under `selectors`; the battery's own contracts may then name those collections, including per-resource ones such as one channel's members:

```toml
# batteries/slack/appa.toml
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_read_channel"
parameters = { type = "object", properties = { channel_id = { type = "string" } }, required = ["channel_id"] }
delta = { audience = ["@slack:channel/$channel_id"] }

[externals.audience.slack]
command = ["python3", "audience-source.py"]
token_env = "APPA_PROVIDER_SLACK_TOKEN"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "full-members", feeds = "internal" },
  { template = "user-group/<handle>" },
  { template = "channel/<id>" },
]
```

The root config decides what the built-in audiences mean and supplies the credential. It maps `self` and `internal` under `[policy.audience]`, which only the root can carry, and exports the token variable the battery names. To route the provider's member lookups elsewhere, the root adds an entry for the same provider whose only key is `lookup`; any other key beside the battery's binding is a duplicate and fails to load.

```toml
# root config
include = ["./batteries/slack/appa.toml"]

[policy]
version = 2

[policy.audience]
self = ["slack:viewer"]
internal = ["slack:full-members"]
```

Every consult OpenAPPA sends the service carries the declared templates as `declaration.templates`. The shipped services compare that list with the templates they serve and refuse a mismatch before reading their token, and the start-up probe of `viewer` and `full-members` triggers that check before any agent call. See [Configure audience membership](/contracts#configure-audience-membership) for the declaration and the request protocol.

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
name = "host/claude-code/Bash(command:kubectl)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "host/claude-code/Bash(command:kubectl *)"
requires = { attention = ["blocked"] }
delta = { trust = "suspicious", audience = ["internal"] }

# This declaration replaces the annotator with the same name in the battery.
[[policy.annotator]]
name = "local.read-sensitivity"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "host/claude-code/Read"
annotator = "local.read-sensitivity"

[externals.annotators."local.read-sensitivity"]
command = ["python3", "./local/read-sensitivity.py"]
```

No authority permits `blocked`, so both `kubectl` contracts have no remedy.
Every other Bash call reaches the battery's model annotator. The root also
replaces the battery's `host/claude-code/Read` rules with a local script.

The battery files stay unchanged.
