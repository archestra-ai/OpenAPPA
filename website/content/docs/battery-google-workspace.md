---
title: Google Workspace battery
category: Batteries
order: 6.65
description: Rules for the claude.ai Google Drive connector's 11 tools, with audiences from your Google Workspace directory and groups.
sidebar: false
breadcrumb: Google Workspace
---

The Google Workspace battery covers all 11 tools of the claude.ai Google Drive connector and builds audiences from your Workspace directory.

Sharing a file needs `google-workspace-review`. The Claude Code plugin default permits every mark, so the person running the session reviews it; another root config must define an Authority permitting the mark.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/google-workspace).

## Tool behavior

- Reads, searches, metadata, and permission lookups return untrusted `self` data. The connector does not report who can see a file, and anyone can share a file with you or comment on one.
- Creating, copying, updating, and trashing files require trusted `internal` data and record `google-workspace.changed`.
- Sharing a file requires public input and review, and records `google-workspace.sensitive`.

## Audiences

The source builds:

- `self` from the current viewer;
- `internal` from active Workspace users; and
- named audiences from Google Workspace groups.

Members of nested groups are included. Suspended and archived users are excluded from `internal`.

The battery binds the source; map `self` and `internal` onto it in the root config and pass its token through `APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`. The token comes from an OAuth client that a Workspace admin creates once per organization in Google Cloud; the battery README lists the steps and scopes.

Its tests use saved Google API responses. They do not call Google.

## Limits

A file read through this battery is `self` data even when the whole organization can see it. The Gmail and Calendar connectors are not covered; their tools stay blocked until root rules name them.
