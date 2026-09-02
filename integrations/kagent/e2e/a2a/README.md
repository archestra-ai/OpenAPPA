# The A2A matrix — the chat-UI matrix without the chat UI

The mirror of [../ui/](../ui/): the same conversations against the
same live stack, driven over kagent's A2A endpoint alone — JSON-RPC
`message/send`, no browser. Seventeen cases, the UI matrix's twin,
including both answers to the policy's human-review remedy and the
remote change board approving, denying, and staying silent (the
matrix plays the board member on the mock's side channel,
`APPA_MOCK_URL`, default `http://127.0.0.1:8081`). A human-review
case suspends the task (`input-required`) with a confirmation request
on the wire; the client answers with the same `data` part the kagent UI
sends (`{"decision_type": "approve" | "reject"}`), and the runtime spends
that answer as the authority's ruling.

## Requirements

The demo stack from [../../demo/README.md](../../demo/README.md),
with the `cluster-ops` agent reachable over A2A — for example:

```sh
kubectl port-forward -n kagent svc/cluster-ops 18089:8080
```

`APPA_A2A_URL` overrides the default `http://127.0.0.1:18089/`; point it
at `svc/cluster-ops-go` to run the same seventeen cases against the go
cell.
`APPA_A2A_DECISION_SETTLE` (seconds, default 2) is the pause before a
decision is sent: kagent persists the confirmation-request event it
later matches the decision against concurrently with answering the
request, and a client that answers within milliseconds was observed
once in eight runs to miss the match. A person never answers that fast.

## Run

```sh
cd integrations/kagent/e2e/a2a
APPA_A2A_E2E=1 uv run --with "pytest>=8" pytest -v .
```
