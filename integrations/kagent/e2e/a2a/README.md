# The A2A matrix — the chat-UI matrix without the chat UI

The mirror of [../ui/](../ui/): the same conversations against the
same live stack, driven over kagent's A2A endpoint alone — JSON-RPC
`message/send`, no browser. Eighteen cases, the UI matrix's twin,
including both answers to the policy's human-review remedy and the
remote change board approving, denying, and staying silent (the
matrix plays the board member on the mock's side channel,
`APPA_MOCK_URL`, default `http://127.0.0.1:8081`). A human-review
case suspends the task (`input-required`) with a confirmation request
on the wire; the client answers with the same `data` part the kagent UI
sends (`{"decision_type": "approve" | "reject"}`), and the runtime spends
that answer as the authority's ruling.

The matrix also checks both suspicious ingress sources and verifies that
an audience-narrowed session cannot use the public status sink.

## Requirements

The demo stack from [../../demo/README.md](../../demo/README.md),
with the `cluster-ops` agent reachable over A2A — for example:

```sh
kubectl port-forward -n kagent svc/cluster-ops 18089:8080
```

`APPA_A2A_URL` overrides the default `http://127.0.0.1:18089/`; point it
at `svc/cluster-ops-go` to run the same eighteen cases against the go
cell.
`APPA_NAMESPACE` (default `kagent`), `APPA_CHILD` (default `log-analyst`) and `APPA_UNDECLARED` (default `release-manager`) set the release namespace and the two delegated agents. The two delegation cases ask for each agent by that name. They read the parent's call to it off the task history under its wire name, `<namespace>__NS__<agent>` with hyphens as underscores. The name is matched as a prefix, because the go cell's names end in `_go`.

A child's value is checked where the child stops, and the parent's spawn result replays what crossed there. So a return the runtime shaped — a sanitizer's derivation, an attested body in canonical form — reaches the parent already substituted and reads exactly like one that crossed as spoken. The shapes below tell apart where the value was checked, not what it says.

The allowed delegation asserts that the call carries arguments, that its response carries the child's own answer, and that the response takes one of two shapes:

- kagent's own result with the child's `subagent_session_id` — what crossed at the child's stop, replayed;
- a `result` alone — the same crossing where kagent answered with a message instead of a task, so no child session id came back.

A denial fails the case. So does `{"appa": "withheld"}`: the parent's gate refused the message the harness delivered, because the child never returned it at a stop, and nothing crossed into the parent. A withhold whose text carries the runtime's reason `the spawn did not take` (the shape `spawn-not-taken`) fails it for a named cause. The runtime answers the parent's return with it when the child's session opened under another parent's root, or under none, so this parent's prepared fork was never bound. The case delegates from two fresh parent sessions in turn and asserts both returns: on the go cell one child session serves every parent, so a child opened per session instead of per (root, child) pair fails on the second parent. It delegates once per parent session, because a second delegation from one parent session sends a new fork at a child identity the family already opened, which the runtime refuses — one errand is one child trajectory. So does kagent's own failure text under `result`, which the python agent tool answers with when the child never answered (`Remote agent '…' request failed: …`, `… returned no result.`, `… failed.`), and so does the go agent tool's `{"error": …}`. The injected instruction in the logs never reaches the caller. The undeclared delegation asserts the opposite. Its response is the runtime's denial, `{"appa": "denied"}` with the reason `not declared by the policy`, and no `subagent_session_id` comes back.
`APPA_A2A_DECISION_SETTLE` (seconds, default 2) is the pause before a
decision is sent: kagent persists the confirmation-request event it
later matches the decision against concurrently with answering the
request, and a client that answers within milliseconds was observed
once in eight runs to miss the match. A person never answers that fast.

## Run

```sh
cd integrations/kagent/e2e/a2a
APPA_A2A_E2E=1 uv run --with "pytest>=8" --with pytest-rerunfailures pytest -v .
```
