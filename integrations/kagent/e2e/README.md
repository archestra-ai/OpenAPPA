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
delegation, a delegation the policy never names, and gated untrusted
ingress.

| kagent | Runtime plugin | Driver | Status |
|---|---|---|---|
| v0.9.12 | python (`cluster-ops`) | dashboard | runs, 17/17 |
| v0.9.12 | python (`cluster-ops`) | A2A | runs, 17/17 |
| v0.9.12 | go (`cluster-ops-go`) | dashboard | runs, 17/17 |
| v0.9.12 | go (`cluster-ops-go`) | A2A | runs, 17/17 |
| v0.10 | python, go | dashboard, A2A | not run yet — the rows exist, the stack does not |

Only kagent v0.9.12 is covered today: the demo chart installs into a
v0.9.12 cluster and runs both runtime cells on it. The v0.10 rows are
the same cases on that release line's stack once it is installed.

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

The scripted-model scenarios in [test_scenarios.py](test_scenarios.py)
are a fourth, model-free check of the python plugin against the demo
tools and are not a row of this matrix.
