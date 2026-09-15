---
title: Grain battery
category: Batteries
order: 6.64
description: Rules for 49 Grain meeting, transcript, deal, and administration tools.
sidebar: false
breadcrumb: Grain
---

The Grain battery covers 49 tools for recordings, notes, transcripts, deals, clips, stories, collections, and workspace administration.

Actions that need a person's approval carry the `hitl` mark. The Claude Code and kagent plugin defaults permit every mark, so the person running the session reviews them; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/grain).

## Tool behavior

- Meeting content, directories, and settings return `internal` data, the built-in audience of the organization's members.
- Resolving a URL has no extra restrictions.
- Writes within the workspace require trusted data.
- Public sharing, invitations, seat assignments, team changes, and workspace administration require approval every time.

To change how one tool works, add a more specific rule to the root config. Root rules run before battery rules.
