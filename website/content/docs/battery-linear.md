---
title: Linear battery
category: Batteries
order: 6.65
description: Contracts for 65 Linear MCP tools with resource audiences and four policy profiles.
sidebar: false
breadcrumb: Linear
---

The Linear battery covers all 65 tools in its authenticated MCP snapshot, including the 36-tool read-only surface. It supplies a deterministic Python annotator, a membership source, and four policy profiles for the existing Claude Code and kagent integrations.

[Source and setup examples](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/linear).

## Behavior

Every tool needs an explicit resource-to-audience rule. Query filters, issue IDs, sharing recipients, related-content flags and other scope arguments must match the operator's configuration exactly. Missing or ambiguous mappings stop the call. Team membership alone is not an issue or project ACL.

Reads and mutation responses enter as suspicious and carry the configured resource audience. Writes require trusted data and emit a classified effect. The default profile additionally requires a fresh human review mark; production lockdown also requires explicit permission on each resource rule. Team-use omits the extra mark on routine edits, but mutations still need a trust authority because their responses contain outside content. The read-only profile registers only the captured read tools.

An approved Linear write does not make its result safe to publish to GitHub. The shipped GitHub battery accepts only trusted public data. Wider sharing requires a separate, explicitly bounded authority or sanitizer in the root policy.

## Setup

Install the battery with the host's server identifier, then configure resource audiences and bind the existing human review backend. Installing policy does not create the MCP connection or grant Linear access. Source examples cover each profile; the installer selects the approved-writes default.

The audience helper supports the authenticated viewer, workspace members and team members. It preserves `linear:<user-uuid>` readers; it never infers cross-provider identity from profile email. A deployment can explicitly route these readers through its existing identity mappings.

## Schema updates

The battery pins the full captured input schemas and validates them before annotation. Its capture utility compares both official endpoints without calling mutation tools. New tools, fields and schema vocabulary need review before regeneration. Run the drift check for the account and endpoint you intend to use.
