# three-trust-ranks

This example has three trust levels: `untrusted`, `vendor`, and `internal`. New content
can lower the trust level. Some actions need a minimum level.

| Action | Result |
|---|---|
| `mcp/docs/read_vendor_doc` | Sets the trust level to `vendor`. |
| `mcp/mail/read_inbound` | Sets the trust level to `untrusted`. |
| `mcp/shell/run_command` | Needs `internal`. |
| `mcp/slack/post_message` | Needs `vendor`. |

- `three-trust-ranks.appa`: both actions are allowed at the start. Vendor content blocks a
  command but allows a Slack post. An inbound email then blocks both actions.
