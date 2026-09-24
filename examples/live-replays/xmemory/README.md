# Test the xmemory battery

The offline trace checks that text and structured writes run before and after
a memory read, that a read makes the trajectory untrusted and internal, that a
schema decision needs trusted data, that instance creation and metadata changes
always ask the authority, and that a schema migration and an instance deletion
are reviewed. The root uses a fictional fixed audience. Its review authority
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
