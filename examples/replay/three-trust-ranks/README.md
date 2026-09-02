# three-trust-ranks

A custom trust chain, lowest first: `trust_chain = ["untrusted", "vendor", "internal"]`.
A trajectory starts at the top rank. Content lowers it to the rank of its source. A
sink requires at least its declared rank.

| Tool | Contract |
|---|---|
| `ReadVendorDoc` | `delta = { trust = "vendor" }` |
| `ReadInboundEmail` | `delta = { trust = "untrusted" }` |
| `RunCommand` | `requires = { trust = "internal" }` |
| `PostToSlack` | `requires = { trust = "vendor" }` |

- `clean-trajectory.appa`: nothing lowered trust, and both sinks run.
- `vendor-doc.appa`: the document lowers trust to `vendor`. The command is denied. The
  Slack post is allowed, because `vendor` meets its requirement.
- `inbound-email.appa`: the email lowers trust to `untrusted`. Both sinks are denied.
