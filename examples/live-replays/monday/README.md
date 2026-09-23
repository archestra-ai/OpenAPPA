# Test the monday battery

The offline trace checks ordinary read options, reviewed creation and comments
from internal content, a mixed tool's read action, reviewed GraphQL, and
refusal of public disclosure. The root uses a fictional
fixed audience. Its review authority cannot expand an audience, so successful
writes demonstrate that internal data does not need public declassification.

```sh
appa replay \
  --config examples/live-replays/monday/appa.toml \
  examples/live-replays/monday/monday-battery.appa
```

`appa replay` does not execute MCP tools; it supplies simulated approvals and
empty results. The audience command is only a fixture, not an ACL resolver.
Runtime tests separately cover trust selectors, recipient checks, mixed tool
actions, credential confinement, and external-submission audience approval.

Live clappa checks need an authenticated monday connection, disposable owned
fixtures, fresh sessions after policy reload, and independent provider-state
checks.
