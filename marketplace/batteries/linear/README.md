# Linear battery

A handwritten TOML policy for 65 Linear MCP tools. The manifest registers it with
the marketplace. There are no helper processes, provider credentials, generated
policies, or bundled schemas.

Reads and mutation responses enter as suspicious, restricted to `internal`.
Queries also require input that may be shared with `internal`. Every mutation
requires trusted input and the `linear-review` mark; routine edits emit
`linear.changed`, and administrative, sharing and destructive operations emit
`linear.sensitive`. Upload preparation keeps its signed URL restricted to `self`.
Image extraction can fetch external URLs, so its input must be public and reviewed.

## Configure the deployment

Install with `appa battery install linear --server <host-server-name>`, or include
`appa.toml` from the root policy. The package must be in your installed release;
use the source example while developing from a checkout. Installation adds policy,
not an MCP connection or Linear permissions.

Configure `internal` through the deployment's existing organization audience
sources. Without those sources, unoverridden calls cannot proceed. Its readers
must be authorized for **every resource
this connection can return**. Workspace or team membership alone does not prove
that access. For mixed resource permissions, use a conservative common audience
and override individual tools/resources with verified audiences. Broader audience
labels permit broader downstream sharing, so do not label private records with
all workspace members by default.

Root rules take precedence over battery rules. They must state the complete
annotation, including write requirements and effects. Use `parameters` with
`additionalProperties = false` when an override assumes a fixed argument shape:
related-content flags or reassignment fields must not silently reuse that scope.
The battery itself does not mirror Linear's API schema or infer resource ACLs.

[The root example](../../../examples/linear-battery/appa.toml) shows a fictional
ENG-1 resource mapped to Alice, a closed argument set, and the existing human
review authority. That authority can satisfy trust and review requirements; it
cannot widen the audience. Reading Linear content therefore does not authorize
publishing it to a public GitHub repository.

For read-only deployments, connect Linear's `/mcp/readonly` endpoint. More
permissive or restrictive write policies belong in deployment-owned root rules;
this battery has one default policy rather than generated profiles.

## Maintenance

Edit the TOML rules directly when tool behavior changes. Unknown tools receive no
permission from this battery. Review each tool's destination, returned content,
side effects and review requirements; tool descriptions and MCP annotations do
not grant permission. Provider schema capture or hashing can be done externally;
neither is needed to execute this policy.

```sh
cargo test --locked -p appa --test linear_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
