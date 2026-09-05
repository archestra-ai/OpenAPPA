# The kagent end-to-end matrix

Three dimensions, every combination a row:

- **kagent version** — the version under test, with the ADK it locks.
- **runtime plugin** — `appa-kagent-adk` (python) or `appa-kagent-adk-go`
  (go), each built against that kagent version's ADK.
- **driver** — the kagent dashboard in headless Chromium ([ui/](ui/)), or
  the A2A protocol alone ([a2a/](a2a/)).

Every row runs the same eighteen conversations with a real model against
the shared `appa-runtime` on the demo policy: the allowed read, the
exfiltration ask, the agent's own remedies under configured and chat
steering, a forged offer, the human-review authority both ways, the
per-call annotator, the release window in and out of window, the remote
change board approving, denying and staying silent, cross-pod
delegation from two parent sessions in turn, a delegation the policy
never names, both untrusted ingress sources, and a public-sink attempt
after audience narrowing.

| kagent | Cell | Runtime plugin | Driver | Status |
|---|---|---|---|---|
| v0.9.12 | A-py | python (`cluster-ops`, google-adk 1.31.1) | dashboard | runs, 18/18 |
| v0.9.12 | A-py | python (`cluster-ops`, google-adk 1.31.1) | A2A | runs, 18/18 |
| v0.9.12 | A-go | go (`cluster-ops-go`, adk/v2 v2.1.0) | dashboard | runs, 18/18 |
| v0.9.12 | A-go | go (`cluster-ops-go`, adk/v2 v2.1.0) | A2A | runs, 18/18 |
| v0.10.0-rc4 | B1-py | python (google-adk 2.8.0) | dashboard, A2A | not run |
| v0.10.0-rc4 | B1-go | go (adk/v2 v2.1.0) | dashboard, A2A | not run |
| main `52cc4de2` | B2-py, B2-go | python (google-adk 2.8.0), go (adk/v2 v2.1.0 binary, kagent main locks v2.2.0) | dashboard, A2A | not run |

The cell ids are the plan's ([../IMPLEMENTATION.md](../IMPLEMENTATION.md)).
Only the kagent v0.9.12 rows run: the demo chart installs into a
v0.9.12 cluster and runs both runtime cells on it. The v0.10 rows name
the same eighteen cases on that release line's stack, and no run of
them exists. Local matrix runs assume a running stack. The scripts under
`e2e/ci` provision the A-py A2A row.

## Run

The stack from [../demo/README.md](../demo/README.md): the kind cluster
with the demo chart, the dashboard port-forwarded to `127.0.0.1:8901`,
the mocks' side channel to `127.0.0.1:8081`, and the agent under test
port-forwarded for the A2A driver.

```sh
cd integrations/kagent/e2e
./run-matrix.sh python ui      # APPA_AGENT=cluster-ops, the dashboard
./run-matrix.sh python a2a     # svc/cluster-ops on 127.0.0.1:18089
./run-matrix.sh go ui          # APPA_AGENT=cluster-ops-go
./run-matrix.sh go a2a         # svc/cluster-ops-go on 127.0.0.1:18090
./run-matrix.sh guide ui       # appa-guide lifecycle plus an Agent migration
./run-matrix.sh all            # every row that runs today, in sequence
```

Three cases depend on model steering: the configured default, accept,
and decline flows. They use `@pytest.mark.flaky(reruns=2)`: three fresh
conversations, and a third failure fails the row.

The model-free integration suite in [../tests/](../tests/) is a
fourth check of the gated path. It runs the same substance without a
cluster, a dashboard or a model, and is not a row of this matrix.
