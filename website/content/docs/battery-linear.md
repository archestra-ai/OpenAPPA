---
title: Linear battery
category: Batteries
order: 6.65
description: TOML rules for 65 Linear MCP tools, internal audiences, and reviewed writes.
sidebar: false
breadcrumb: Linear
---

The Linear battery is a TOML policy and package manifest. It covers 65 MCP tools
using the existing runtime; no Python helper or provider credential is required.

Reads and mutation responses enter as suspicious and restricted to `internal`.
Writes require trusted internal input and a review mark, and record their effects.
Image extraction requires public input and review because it can fetch external
URLs. Upload preparation keeps signed URLs restricted to `self`.

Define `internal` as readers authorized for every resource the connection can
return. Membership alone does not establish resource access. Use a conservative
common audience and root TOML overrides for resources with different permissions.
Overrides supply the complete annotation; closed argument declarations prevent
unexpected scope fields from reusing a resource rule.

The [root example](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/linear-battery)
binds human review and demonstrates a resource audience. The review authority
cannot widen that audience, so it cannot authorize publishing private Linear
content to a public GitHub repository. For read-only use, connect Linear's
read-only MCP endpoint.

[Source and setup](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/linear).
