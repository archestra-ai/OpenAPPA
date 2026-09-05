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
to require, through an authority, never the harness's default. The same eighteen cases run
over the A2A protocol alone in [../a2a/](../a2a/); [../README.md](../README.md)
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
`APPA_NAMESPACE` (default `kagent`), `APPA_CHILD` (default `log-analyst`) and `APPA_UNDECLARED` (default `release-manager`) set the release namespace and the two delegated agents. The two delegation cases ask for each agent by that name. They wait for the run to end before they read the page, because the child works out of sight for longer than the quiet period. The dashboard renders a call to an agent as a sub-agent card. Its header carries `<namespace>/<agent>`, the call id, and a status. The allowed delegation asserts the child's card with the status `Completed` and no confirmation card. With the card's output expanded, it asserts no denial, no injected instruction, none of kagent's own failure texts, and not the runtime's reason `the spawn did not take`. The dashboard shows `Completed` on those too, so the output is what pins that the child answered. A child's value is checked where the child stops, so the card's output carries what already crossed: the child's own words, or the derivation the runtime shaped them into. A withheld return fails the case whatever its reason, because it means the harness delivered the parent a message the child never returned, and nothing crossed. The case asserts the runtime's reason for the other withhold, `ended outside the return check`, is absent too. The runtime's `the spawn did not take` reason means the child's session opened under another parent's root, or under none, so this parent's prepared fork was never bound. The case delegates from two chat sessions in turn, each a fresh page, and asserts both cards: on the go cell one child session serves every parent, so a child opened per session instead of per (root, child) pair fails on the second. Each session delegates once, because a second delegation from one parent session sends a new fork at a child identity the family already opened, which the runtime refuses — one errand is one child trajectory. The undeclared delegation asserts that agent's card. In the expanded output, it asserts the runtime's denial, which quotes the wire name `<namespace>__NS__<agent>` with hyphens as underscores.
The change-board cases rule on the mock's side channel at
`APPA_MOCK_URL` (default `http://127.0.0.1:8081`). Assertions are on substance, never on the model's phrasing —
a failure means the gate, the remedy loop, or the data flow misbehaved,
not that the model chose different words.
