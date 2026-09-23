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
`trusted` only when the successful result is provider defined API metadata,
not workspace content, another user's text, a retrieved article, an agent
generated answer, or a returned credential. `audience` is separate: these
two schema tools still require and return `internal` under this battery.

The previous uniform `suspicious` label was a conservative label for every
provider response, including schema metadata. The code below gives a narrow
basis for two exceptions. The hosted build difference remains the limit of
this source based conclusion; reconfirm these output paths against the hosted
server before claiming live verification.

| Result | Classification | Basis |
| --- | --- | --- |
| `get_graphql_schema` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-graphql-schema-tool.ts#L33-L64) queries GraphQL introspection and returns field names, descriptions, and type names/kinds. It does not return board records. |
| `get_column_type_info` | `trusted/internal` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-column-type-info/get-column-type-info-tool.ts#L13-L85) accepts a fixed enum of standard column types and returns either code built guidelines or the provider's type schema. |

All remaining battery rules retain `suspicious` result trust, including:

| Result family | Why it remains suspicious |
| --- | --- |
| Board, item, update, document, meeting, user, automation, search, and mixed tool reads | They can return workspace authored or externally supplied text. `all_api_read` accepts arbitrary read queries, so its shape cannot establish a safe result. |
| `get_monday_knowledge` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/get-monday-knowledge/get-monday-knowledge.ts#L66-L115) returns a generated answer and retrieved snippets. A public audience for documentation does not make the answer trusted. |
| `all_widgets_schema` | [Implementation](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/core/tools/platform-api-tools/dashboard-tools/all-widgets-schema-tool.ts#L41-L84) passes widget schema descriptions through from API data. The inspected code does not prove every widget schema is authored by monday. |
| `get_type_details` | Its [query builder](https://github.com/mondaycom/mcp/blob/8fdc0b4af07a4b7e1059ca3f8320d64968737b8e/packages/agent-toolkit/src/monday-graphql/queries.graphql.ts#L323-L326) interpolates a free text `typeName` into GraphQL syntax. The public schema header alone does not prove a fixed, metadata only result path for every input. |
| Workflow, action, Vibe, display and other hosted only tools | `tools/list` describes their names and arguments, but the public commit does not establish their complete result construction or every input to it. |
| Writes and external submissions | Success payloads can include existing provider content, names, diagnostics, or echoed records. The separate `monday-review` attention requirement approves a call; it does not validate returned text. |
| `connect_external_agent` | Returns signing material and an API token; it remains `suspicious/self`. |

Coverage state: 99 discovered and classified; these two changed contracts
are offline tested by `monday_policy.rs` and composed by the marketplace test.
The changed trust classifications have not been live tested against the hosted MCP.
The policy has no new host connection or credential requirement.
