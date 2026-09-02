# The chat-UI matrix — real browser, real model, real decisions

Seventeen conversations through the kagent dashboard in headless
Chromium, covering the policy-feature matrix end to end: an allowed
read, the exfiltration ask that leaks nothing, the agent executing a
remedy on its own under its configured default (the sanitized result),
the chat steering it to accept the change instead, the chat steering it
to take no remedy, a forged offer id, the policy's human-review
authority answered both ways through kagent's Approve/Reject card, the
per-call annotator, the human-less release-window authority in and out
of window, the remote change board (a URL authority backed by people
out of band) approving, denying, and staying silent, cross-pod
delegation, a delegation the policy never names (denied at the spawn),
and gated untrusted ingress.

Nothing here is mocked except the tool DATA: the model is a real LLM,
every gate decision is the live shared `appa-runtime`, and every remedy
the agent takes is a real `execute_remedy_plan` execution. Only the two
human-review cases click a card — the person's answer is the `oncall`
authority's ruling — and every other remedy test asserts that no card
appears: human attention is the policy's to require, through an
authority, never the harness's default. The same seventeen cases run
over the A2A protocol alone in [../a2a/](../a2a/); [../README.md](../README.md)
is the matrix index across kagent versions, runtime plugins and drivers.

## Requirements

The full demo stack from [../../demo/README.md](../../demo/README.md):
the kind cluster with the gated images, the shared runtime on the
matrix policy (`chart/files/demo.appa.toml`) with the model key set, the mock
externals on the runtime's loopback, and the UI port-forwarded
(default `http://127.0.0.1:8901`, override with `APPA_UI_URL`).

## Run

```sh
cd integrations/kagent/e2e/ui
APPA_UI_E2E=1 uv run --with playwright --with "pytest>=8" --with pytest-rerunfailures pytest -v .
```

Real model turns run tens of seconds each; the whole matrix takes
5–25 minutes. `APPA_UI_SHOTS` names the screenshot directory;
`APPA_UI_REPLY_TIMEOUT` (seconds) stretches the reply wait for slow
models. `APPA_AGENT` names the agent under test: `cluster-ops` (the
python runtime) by default, or `cluster-ops-go`, its twin on kagent's go
runtime — the same seventeen cases run against either cell. The change-board cases rule on the mock's side channel at
`APPA_MOCK_URL` (default `http://127.0.0.1:8081`). Assertions are on substance, never on the model's phrasing —
a failure means the gate, the remedy loop, or the data flow misbehaved,
not that the model chose different words.
