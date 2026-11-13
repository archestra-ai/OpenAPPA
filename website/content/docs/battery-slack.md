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

- Reading a channel, a thread, a channel's members, or a message's reactions returns data read by that conversation's members. The `slack.conversation-trust` annotator labels it `@slack:channel/$channel_id` from the call's channel id, and the battery's audience source answers who reads it: every full member for a public channel, since any of them may join it, and the members Slack reports for a private channel, group DM, or DM. A DM's history stays with its two ends. Slack shows a DM conversation (`D…`) only to those two ends, so a token of another account, a bot's included, cannot answer for it and the call is refused. A DM named by user id (`U…`) is read by that user and, when the token is a person's, the viewer; a bot token's account is no end of it.
- Trust follows who can write the text. The annotator asks Slack (`conversations.info`, or `users.info` for a DM named by user id) whether the conversation is shared with another organization through Slack Connect. A shared conversation, or a DM with a user from another organization, is untrusted. Any other conversation keeps the session's trust: members, guests, and installed integrations write as the workspace, because a member invited each guest and installed each integration. Without `APPA_PROVIDER_SLACK_TOKEN` the battery stays static and every conversation keeps the session's trust. With the token, a network error or rate limit is retried twice, and a failure left after that refuses the read. Each conversation read costs one extra Slack API call.
- Canvases, files, profiles, and public search return `internal` data, the built-in audience of the organization's members. A search across private conversations returns data only the viewer (`self`) is known to read.
- Adding a reaction or saving a personal draft requires trusted data and no approval.
- Sending or scheduling a message requires trusted data that every member of the target conversation may see (`audience = { contains = ["@slack:channel/$channel_id"] }`): a summary of one channel can go back to that channel and not to a wider one. Canvases and new channels require trusted data that reaches `internal`. Agents post autonomously while requester secrets (`self`) stay out.

To change how one tool or channel works, add a more specific rule to the root config. Root rules run before battery rules.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of the four conversation reads the `slack.conversation-trust` Annotator labels. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `slack.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. It applies where the Annotator finds a Slack Connect conversation. The audience does not change. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The model for each sanitizer is configured in the deployment, not in the battery. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `uses = "llm"` with an `[externals.llm]` profile, `uses = "claude-code"` where the `claude` CLI is installed, or a `url`. The default config of the Claude Code plugin binds every battery sanitizer to `claude-code`, so in `clappa` sessions it is active once the battery is installed. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.

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
