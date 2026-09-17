---
title: Linear battery
category: Batteries
order: 6.65
description: Rules for 65 Linear MCP tools with per-issue, per-team, and per-project audiences, and reviewed writes.
sidebar: false
breadcrumb: Linear
---

The Linear battery covers 65 MCP tools and binds the `linear` audience source, which reads who may see each issue, team, project, and document from Linear itself.

Reads and mutation responses enter as suspicious. A tool that names a resource by id is labelled with that resource's readers: every full member for a public team's issue, the team's members for a private one, plus the people the issue is shared with. Writes into a resource require trusted data its readers may see and a review mark, and record their effects. Tools that name no resource, or name one by a free-text query, keep the `internal` audience.

Define `internal` as readers authorized for everything the connection can list; map `self` and `internal` onto the source in the root config and pass the token through `APPA_PROVIDER_LINEAR_TOKEN`. A resource named by a display name gets no answer, so the agent names it by id.

The [live replay](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/linear)
binds human review, maps the chain onto the source, and keeps one root override for a resource whose readers Linear does not model. The review authority cannot widen an audience, so it cannot authorize publishing private Linear content to a public GitHub repository. For read-only use, connect Linear's read-only MCP endpoint.

[Source and setup](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/linear).
