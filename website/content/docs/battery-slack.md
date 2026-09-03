---
title: Slack battery
category: Batteries
order: 6.61
description: Rules for all 19 claude.ai Slack connector tools, with Slack-based audiences.
sidebar: false
breadcrumb: Slack
---

The Slack battery covers all 19 tools in the claude.ai Slack connector. Writes require trusted data bounded to `internal`.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/slack).

## Tool behavior

- Reads and searches return `internal` data, the built-in audience of the organization's members. OpenAPPA keeps their existing trust level.
- Adding a reaction or saving a personal draft requires trusted data and no approval.
- Messages, canvases, channels, and other visible changes require trusted data bounded to `internal` (`audience = { within = ["internal"] }`), enabling autonomous agent posting while keeping requester secrets (`self`) out.

To change how one tool or channel works, add a more specific rule to the root config. Root rules run before battery rules.

## Files

```text
slack/
|-- README.md
|-- appa.toml
|-- audience-source.py
`-- test_audience_source.py
```

The audience source can build `self`, `internal`, and Slack user-group audiences. Set it up in the root config and pass its token through `APPA_PROVIDER_SLACK_TOKEN`.

Its tests use saved Slack API responses. They do not call Slack.
