---
title: Google Workspace battery
category: Batteries
order: 6.65
description: Builds audiences from your Google Workspace directory and groups.
sidebar: false
breadcrumb: Google Workspace
---

The Google Workspace battery reads your Workspace directory to build audiences. It does not contain tool rules yet.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/batteries/google-workspace).

## Audiences

The source builds:

- `self` from the current viewer;
- `internal` from active Workspace users; and
- named audiences from Google Workspace groups.

Members of nested groups are included. Suspended and archived users are excluded from `internal`.

Set up the source in the root config and pass its token through `APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN`.

Its tests use saved Google API responses. They do not call Google.
