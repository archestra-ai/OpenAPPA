# Test the monday battery

The `monday-battery.appa` trace checks the bounded name-only write, structural
duplication refusal, `create_update` refusal because its response reads the
existing item name, and arbitrary GraphQL refusal. `appa replay` evaluates
policy only; it never executes Monday or an MCP call. The focused runtime test
covers internal audience-source requirements, restricted read-to-write flow,
and future unknown-tool refusal.

Run the policy-only replay from the repository root:

```sh
appa replay \
  --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```

The separate clappa smoke uses the authenticated Streamable HTTP endpoint
`https://mcp.monday.com/mcp`, a disposable fixture, and a unique marker. Keep
tokens, fixture IDs, and customer data in the private runtime environment.
The smoke must independently verify provider state before and after each
positive or negative mutation; this replay does not do that.
