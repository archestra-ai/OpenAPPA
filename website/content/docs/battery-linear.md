---
title: Linear battery
category: Batteries
order: 6.65
description: Rules for 65 Linear MCP tools with per-issue, per-team, and per-project audiences, and reviewed writes.
sidebar: false
breadcrumb: Linear
---

The Linear battery covers 65 MCP tools and binds the `linear` audience source, which reads who may see each issue, team, project, and document from Linear itself.

Trust follows who can write the text. Workspace members write issues, comments, documents, and projects, and a member installed each integration that posts text, so reads and mutation responses keep the session's trust. Linear's public documentation (`search_documentation`) and images from arbitrary URLs (`extract_images`) enter suspicious. A tool that names a resource by id is labelled with that resource's readers: every full member for a public team's issue, the team's members for a private one, plus the people the issue is shared with. Writes into a resource require trusted data its readers may see and a review mark, and record their effects. Tools that name no resource, or name one by a free-text query, keep the `internal` audience.

Define `internal` as readers authorized for everything the connection can list; map `self` and `internal` onto the source in the root config and pass the token through `APPA_PROVIDER_LINEAR_TOKEN`. A resource named by a display name gets no answer, so the agent names it by id.

The [live replay](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/linear)
binds human review, maps the chain onto the source, and keeps one root override for a resource whose readers Linear does not model. The review authority cannot widen an audience, so it cannot authorize publishing private Linear content to a public GitHub repository. For read-only use, connect Linear's read-only MCP endpoint.

[Source and setup](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/linear).

## Sanitizers

The battery confines the results of `search_documentation`, `extract_images`, and every read labelled `internal`. A read that narrows the session then offers each sanitizer below beside the plain narrowing, and the agent chooses. A chosen sanitizer runs the Claude Code model (`builtin = "claude-code"`) on the confined result with the battery's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `linear.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. |
| `linear.redact-for-public` | `audience`: `internal` → `public` | Removes personal data of customers and colleagues, customer and partner names, deal details, internal URLs and hostnames, credentials and unreleased plans, and keeps the problem, reproduction steps, error text, status and decisions. The read does not narrow the session's audience, so it adds no audience gap to a later call bound for `public` readers. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts or settles with the other sanitizer. A sanitizer never clears a requirement, such as a review mark.

APPA does not verify what the model removed: including the battery registers these sanitizers as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. The model runs through `[externals.claude_code]`, so the host that runs OpenAPPA needs the `claude` command. Without it, a chosen sanitizer gets no answer and the result stays withheld.
