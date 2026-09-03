---
title: Grain battery
category: Batteries
order: 6.64
description: Rules for 49 Grain meeting, transcript, deal, and administration tools.
sidebar: false
---

The Grain battery covers 49 tools for recordings, notes, transcripts, deals, clips, stories, collections, and workspace administration.

Your root config must define an Authority named `hitl` for actions that need a person's approval.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/grain).

## Tool behavior

- Meeting content, directories, and settings return private data.
- Resolving a URL has no extra restrictions.
- Writes within the workspace require trusted data.
- Public sharing, invitations, seat assignments, team changes, and workspace administration require approval every time.

To change how one tool works, add a more specific rule to the root config. Root rules run before battery rules.
