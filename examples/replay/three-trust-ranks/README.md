# three-trust-ranks

This example has three trust levels: `untrusted`, `vendor`, and `internal`. New content
can lower the trust level. Some actions need a minimum level.

| Action | Result |
|---|---|
| `ReadVendorDoc` | Sets the trust level to `vendor`. |
| `ReadInboundEmail` | Sets the trust level to `untrusted`. |
| `RunCommand` | Needs `internal`. |
| `PostToSlack` | Needs `vendor`. |

- `three-trust-ranks.appa`: both actions are allowed at the start. Vendor content blocks a
  command but allows a Slack post. An inbound email then blocks both actions.
