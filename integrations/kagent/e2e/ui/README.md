# The chat-UI matrix — real browser, real model, real decisions

Eighteen conversations through the kagent dashboard in headless
Chromium, covering the policy-feature matrix end to end: an allowed
read, the exfiltration ask that leaks nothing, the agent executing a
remedy on its own under its configured default (the sanitized result),
the chat steering it to accept the change instead, the chat steering it
to take no remedy, a forged offer id, the policy's human-review
authority answered both ways through kagent's Approve/Reject card, the
per-call annotator, the human-less release-window authority in and out
of window, the remote change board (an Authority backed by people
out of band) approving, denying, and staying silent, cross-pod
delegation, a delegation the policy never names (denied at the spawn),
both gated untrusted ingress sources, and a public-sink attempt after
audience narrowing.

The model, plugins, runtime decisions, and remedy executions are real.
The demo tool data and external policy answers are deterministic fixtures.
Only the two
human-review cases click a card — the person's answer is the `oncall`
authority's ruling. Nine of the other sixteen cases assert that no
card appears. Those are the exfiltration ask, the three steered
remedies, the forged offer, the in-window release change, both
delegations, and the board's approval. Human attention is the policy's
to require, through an authority, never the harness's default. The
[A2A suite](../a2a/) covers these policy paths plus repeated delegation,
GitHub battery flows, and a dedicated malformed-offer protocol probe.
[../README.md](../README.md)
is the matrix index across kagent versions, runtime plugins and drivers.

## Requirements

The full demo stack from [../../demo/README.md](../../demo/README.md):
the kind cluster with the gated images, the dedicated runtime chart on
the matrix policy (`chart/files/demo.appa.toml`), the fixture-only demo
chart with its separate mock Service, and the UI port-forwarded
(default `http://127.0.0.1:8901`, override with `APPA_UI_URL`).

## Run

```sh
cd integrations/kagent/e2e/ui
APPA_UI_E2E=1 uv run --with playwright --with "pytest>=8" --with pytest-rerunfailures pytest -v .
```

Run the guide row from `integrations/kagent/e2e` with
`./run-matrix.sh guide ui`. It verifies the fixture chart Agent uses the
shared runtime. It also creates an ungated migration fixture, verifies
init, diagnosis, rejected reload and battery actions, protects the
fixture, checks its resulting environment, and removes it.

Real model turns run tens of seconds each; the whole matrix takes
5–25 minutes. `APPA_UI_SHOTS` names the screenshot directory;
`APPA_UI_REPLY_TIMEOUT` (seconds) stretches the reply wait for slow
models. `APPA_AGENT` names the agent under test: `cluster-ops` (the
python runtime) by default, or `cluster-ops-go`, its twin on kagent's go
runtime — the same eighteen cases run against either cell.
Set `APPA_EXPECT_RUNTIME_DOWN=1` to run the separate fail-closed outage
case while the runtime Deployment is stopped.
`APPA_NAMESPACE` (default `kagent`), `APPA_CHILD` (default `log-analyst`), and
`APPA_UNDECLARED` (default `release-manager`) select the namespace and delegated
agents. Delegation checks wait for the parent run to end before reading the
child card. A completed card alone is insufficient: its output must contain
neither a runtime denial/withhold nor a transport failure. The undeclared child
must instead return the runtime's denial naming its canonical agent ID.

Both plugins use a fresh child context for each new delegation and retain the
paused context when resuming approval. The UI cases use separate conversations;
the A2A suite additionally exercises repeated calls in one conversation and turn.
Tool-result checks read only the card's Results, Error, or Output section, not
arguments or assistant prose. Restart, scale, and rollback assertions require
the corresponding structured result fields.
The change-board cases rule on the mock's side channel at
`APPA_MOCK_URL` (default `http://127.0.0.1:8081`). Assertions are on substance, never on the model's phrasing —
a failure means the gate, the remedy loop, or the data flow misbehaved,
not that the model chose different words.
