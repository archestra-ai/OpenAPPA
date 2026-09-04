# The kagent end-to-end matrix

Three dimensions, every combination a row:

- **kagent version** — the version under test, with the ADK it locks.
- **runtime plugin** — `appa-kagent-adk` (python) or `appa-kagent-adk-go`
  (go), each built against that kagent version's ADK.
- **driver** — the kagent dashboard in headless Chromium ([ui/](ui/)), or
  the A2A protocol alone ([a2a/](a2a/)).

Every row runs the same seventeen conversations with a real model against
the shared `appa-runtime` on the demo policy: the allowed read, the
exfiltration ask, the agent's own remedies under configured and chat
steering, a forged offer, the human-review authority both ways, the
per-call annotator, the release window in and out of window, the remote
change board approving, denying and staying silent, cross-pod
delegation from two parent sessions in turn, a delegation the policy
never names, and gated untrusted ingress.

| kagent | Cell | Runtime plugin | Driver | Status |
|---|---|---|---|---|
| v0.9.12 | A-py | python (`cluster-ops`, google-adk 1.31.1) | dashboard | runs, 17/17 |
| v0.9.12 | A-py | python (`cluster-ops`, google-adk 1.31.1) | A2A | runs, 17/17 |
| v0.9.12 | A-go | go (`cluster-ops-go`, adk/v2 v2.1.0) | dashboard | runs, 17/17 before the per-pair child opening of the go plugin, not re-run after it |
| v0.9.12 | A-go | go (`cluster-ops-go`, adk/v2 v2.1.0) | A2A | runs, 17/17 before the per-pair child opening of the go plugin, not re-run after it |
| v0.10.0-rc4 | B1-py | python (google-adk 2.8.0) | dashboard, A2A | not run |
| v0.10.0-rc4 | B1-go | go (adk/v2 v2.1.0) | dashboard, A2A | not run |
| main `52cc4de2` | B2-py, B2-go | python (google-adk 2.8.0), go (adk/v2 v2.1.0 binary, kagent main locks v2.2.0) | dashboard, A2A | not run |

The cell ids are the plan's ([../IMPLEMENTATION.md](../IMPLEMENTATION.md)).
Only the kagent v0.9.12 rows run: the demo chart installs into a
v0.9.12 cluster and runs both runtime cells on it. The v0.10 rows name
the same seventeen cases on that release line's stack, and no run of
them exists. Nothing in the tree provisions a cluster: every row
assumes a running stack.

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
./run-matrix.sh all            # every row that runs today, in sequence
```

Three cases in each driver depend on the model honoring a steer — the configured default, the chat steering it to accept, and the chat steering it to decline. The gate's substance holds either way (the secret never leaks; what flowed is read off the tool results), but a model that picks another remedy fails the assertion, so those three carry `@pytest.mark.flaky(reruns=1)`: one fresh conversation, and a second failure fails the row.

The model-free integration suite in [../tests/](../tests/) is a
fourth check of the gated path. It runs the same substance without a
cluster, a dashboard or a model, and is not a row of this matrix.
