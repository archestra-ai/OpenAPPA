---
title: Grain battery
category: Batteries
order: 6.64
description: Rules for 49 Grain meeting, transcript, deal, and administration tools.
sidebar: false
breadcrumb: Grain
---

The Grain battery covers 49 tools for recordings, notes, transcripts, deals, clips, stories, collections, and workspace administration.

Actions that need a person's approval carry the `human-approval` mark. The Claude Code and kagent plugin defaults permit every mark, so the person running the session reviews them; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/grain).

## Tool behavior

- Meeting content, directories, and settings return `internal` data, the built-in audience of the organization's members.
- Meeting content (meetings, transcripts, notes, action items, coaching, clips, collections, stories, companies, people, dossiers, and deals) is untrusted: outside participants said part of it. Workspace users, teams, and settings keep the session's trust.
- Resolving a URL has no extra restrictions.
- Creating clips, stories, and collections, adding to them, and tagging meetings require only `internal` data, since Grain returns them as meeting content: a session that read a transcript can still cut a clip from it. Creating a smart topic requires trusted data.
- Public sharing, invitations, seat assignments, team changes, and workspace administration require approval every time.

To change how one tool works, add a more specific rule to the root config. Root rules run before battery rules.

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of the 21 reads of meeting content. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `grain.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The battery's sanitizers name no model. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.
