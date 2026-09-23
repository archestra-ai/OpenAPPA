# monday MCP result trust review

Reviewed 2026-09-23 against OpenAPPA revision `392f361f`. Target: the hosted
Platform MCP at `https://mcp.monday.com/mcp`, advertised as version 1.0.0.
The battery covers the 99 tools discovered through authenticated `tools/list`
on 2026-09-22. The public implementation inspected here is mondaycom/mcp
commit `8fdc0b4af07a4b7e1059ca3f8320d64968737b8e`; its identity as the
hosted build has **not** been verified. This review uses the public source and
the earlier hosted declaration capture, without a fresh live tool call.

## Decision rule

OpenAPPA's [trust specification](../../website/content/docs/contracts.md#trust)
orders the default ranks `suspicious < trusted`. A lower ranked result lowers
the whole trajectory; a later trusted result cannot repair it. We use
`trusted` when an admitted result has a bounded provider controlled path,
such as API metadata, aggregate counts, or a fixed confirmation with provider
issued identifiers. Workspace content, another user's text, retrieved articles
and generated answers stay suspicious. `audience` is separate: all trusted
variants here still require and return `internal` under this battery.

The original uniform `suspicious` label was a conservative label for every
provider response, including bounded metadata and confirmations. The code below
gives a narrow basis for six exceptions. The hosted build difference remains
the limit of this source based conclusion; reconfirm these output paths against
the hosted server before claiming live verification.

| Result | Classification | Basis |
| --- | --- | --- |
| `get_graphql_schema` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-graphql-schema-tool.ts#L33-L64) queries GraphQL introspection and returns field names, descriptions, and type names/kinds. It does not return board records. |
| `get_column_type_info` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-column-type-info/get-column-type-info-tool.ts#L13-L85) accepts a fixed enum of standard column types and returns either code built guidelines or the provider's type schema. |
| `get_automation_statistics(breakdown:totals)` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/automations-tools/get-automation-statistics/get-automation-statistics-tool.ts#L87-L109) returns counts and an ID from a fixed aggregate query. The `by_entity` branch passes through untyped statistics and keeps the suspicious fallback. |
| `get_asset_upload_url` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-asset-upload-url-tool/get-asset-upload-url-tool.ts#L63-L89) asks the provider to create an upload and returns its issued ID, presigned URL and expiry. The URL is a capability and still needs the host's upload and egress checks. |
| `create_form` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/workforms-tools/create-form-tool/index.ts#L29-L50) returns a fixed confirmation, new board ID and provider issued form token, without existing form or board text. The token remains audience restricted. |
| `move_object(objectType:Folder)` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/move-object-tool/move-object-tool.ts#L60-L93) returns a fixed confirmation and folder ID. Board and overview branches can return a provider message on failure and retain the suspicious fallback. |

All remaining tool or action variants retain `suspicious` result trust, including:

| Result family | Why it remains suspicious |
| --- | --- |
| Board, item, update, document, meeting, user, automation, search, and mixed tool reads | They can return workspace authored or externally supplied text. `all_api_read` accepts arbitrary read queries, so its shape cannot establish a safe result. |
| `get_monday_knowledge` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-monday-knowledge/get-monday-knowledge.ts#L66-L115) returns a generated answer and retrieved snippets. A public audience for documentation does not make the answer trusted. |
| `all_widgets_schema` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/all-widgets-schema-tool.ts#L41-L84) passes widget schema descriptions through from API data. The inspected code does not prove every widget schema is authored by monday. |
| `get_automation_statistics(breakdown:by_entity)` | Passes API statistics through as untyped objects; their full field and text provenance is not established by the inspected client. |
| `get_type_details` | Its [query builder](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/monday-graphql/queries.graphql.ts#L323-L326) interpolates a free text `typeName` into GraphQL syntax. The public schema header alone does not prove a fixed, metadata only result path for every input. |
| Workflow, action, Vibe, display and other hosted only tools | `tools/list` describes their names and arguments, but the public commit does not establish their complete result construction or every input to it. |
| Other writes and external submissions | Success payloads can include existing provider content, names, diagnostics, or echoed records. The separate `monday-review` attention requirement approves a call; it does not validate returned text. |
| `connect_external_agent` | Returns signing material and an API token; it remains `suspicious/self`. |

Coverage state: 99 discovered and classified; these six changed contracts
are offline tested by `monday_policy.rs` and composed by the marketplace test.
The changed trust classifications have not been live tested against the hosted MCP.
The policy has no new host connection or credential requirement.
