---
title: Slack battery
category: Batteries
order: 6.61
description: Rules for all 19 claude.ai Slack connector tools, with audiences from Slack channels, users, and groups.
sidebar: false
breadcrumb: Slack
---

The Slack battery covers all 19 tools in the claude.ai Slack connector. A read of a conversation is labelled with that conversation's members; a message into a conversation requires trusted data its members may see.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/slack).

## Tool behavior

- Reading a channel, a thread, a channel's members, or a message's reactions returns data read by that conversation's members. The `slack.conversation-trust` annotator labels it `@slack:channel/$channel_id` from the call's channel id, and the battery's audience source answers who reads it: every full member for a public channel, since any of them may join it, and the members Slack reports for a private channel, group DM, or DM. A DM's history stays with its two ends.
- Trust follows who can write the text. The annotator asks Slack (`conversations.info`, or `users.info` for a DM named by user id) whether the conversation is shared with another organization through Slack Connect. A shared conversation, or a DM with a user from another organization, is untrusted. Any other conversation keeps the session's trust: members, guests, and installed integrations write as the workspace, because a member invited each guest and installed each integration. If Slack cannot answer (no token, an error, a rate limit), the conversation keeps the session's trust. Each conversation read costs one extra Slack API call.
- Canvases, files, profiles, and public search return `internal` data, the built-in audience of the organization's members. A search across private conversations returns data only the viewer (`self`) is known to read.
- Adding a reaction or saving a personal draft requires trusted data and no approval.
- Sending or scheduling a message requires trusted data that every member of the target conversation may see (`audience = { contains = ["@slack:channel/$channel_id"] }`): a summary of one channel can go back to that channel and not to a wider one. Canvases and new channels require trusted data that reaches `internal`. Agents post autonomously while requester secrets (`self`) stay out.

To change how one tool or channel works, add a more specific rule to the root config. Root rules run before battery rules.

## Files

```text
slack/
|-- README.md
|-- appa.toml
|-- audience-source.py
|-- conversation-trust.py
|-- slack_api.py
|-- test_audience_source.py
`-- test_conversation_trust.py
```

The audience source can build `self`, `internal`, Slack user-group, and per-conversation audiences. The battery binds it and the conversation annotator; map `self` and `internal` onto the source in the root config and pass the token through `APPA_PROVIDER_SLACK_TOKEN`. The annotator uses the same token and the scopes the per-conversation audience already needs, so it adds no setup. `slack_api.py` holds the Slack Web API client both scripts share.

Their tests use saved Slack API responses and check the id classification. They do not call Slack.

## Limit

Searches, canvases, and files name no conversation, so the battery does not check them per conversation. They keep the session's trust even when a result came from a Slack Connect conversation.
