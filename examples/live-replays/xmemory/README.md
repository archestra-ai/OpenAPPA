# Test the xmemory battery

The offline trace checks that a memory read makes the trajectory internal and
keeps its trust, that text and structured writes, schema decisions, and
instance creation run from trusted data before and after a read, that once a
web fetch lowers the trust a write and a metadata change ask the authority, and
that a schema migration and an instance deletion are reviewed. The root uses a fictional fixed audience. Its review authority
cannot expand an audience.

```sh
appa replay \
  --config examples/live-replays/xmemory/appa.toml \
  examples/live-replays/xmemory/xmemory-battery.appa
```

`appa replay` does not execute MCP tools; it supplies simulated approvals and
empty results. The audience command is only a fixture, not an xmemory ACL
resolver. `appa-runtime/tests/xmemory_policy.rs` separately checks the
effects each write records.
