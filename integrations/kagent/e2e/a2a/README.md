# The A2A matrix — the chat-UI matrix without the chat UI

The mirror of [../ui/](../ui/): the same conversations against the
same live stack, driven over kagent's A2A endpoint alone — JSON-RPC
`message/send`, no browser. The cases cover the UI matrix and repeated delegation,
including both answers to the policy's human-review remedy and the
remote change board approving, denying, and staying silent (the
matrix plays the board member on the mock's side channel,
`APPA_MOCK_URL`, default `http://127.0.0.1:8081`). A human-review
case suspends the task (`input-required`) with a confirmation request
on the wire; the client answers with the same `data` part the kagent UI
sends (`{"decision_type": "approve" | "reject"}`), and the runtime spends
that answer as the authority's ruling.

The matrix also checks both suspicious ingress sources, the GitHub battery's
operator-authored issue and repository-read remedy, and the denial of public
writes after audience or trust narrowing. GitHub calls use the demo's canned
repository and issue tools; these tests do not contact GitHub or create real issues.

## Requirements

The demo stack from [../../demo/README.md](../../demo/README.md),
with the `cluster-ops` agent reachable over A2A — for example:

```sh
kubectl port-forward -n kagent svc/cluster-ops 18089:8080
```

`APPA_A2A_URL` overrides the default `http://127.0.0.1:18089/`; point it
at `svc/cluster-ops-go` to run against the Go plugin, and set
`APPA_E2E_AGENT=cluster-ops-go` so the protocol-test clone uses the same plugin.
The forged-offer case requires `kubectl` access to this disposable demo cluster.
It creates a uniquely named Agent from `APPA_E2E_AGENT` (default `cluster-ops`),
preserves its runtime/model configuration, removes ordinary tools, and gives it
explicit negative-test instructions. It asserts the actual malformed call and
runtime denial, then deletes the clone. Refusal by the ordinary operations model
does not establish runtime rejection. The original agent is never changed.
`APPA_NAMESPACE` (default `kagent`), `APPA_CHILD` (default `log-analyst`) and `APPA_UNDECLARED` (default `release-manager`) set the release namespace and the two delegated agents. The delegation cases ask for each agent by that name. They read the parent's call from task history under its wire name, `<namespace>__NS__<agent>` with hyphens as underscores. Matching accepts that exact name or its `_go` variant, not arbitrary prefixes.

A child's value is checked where the child stops, and the parent's spawn result replays what crossed there. So a return the runtime shaped — a sanitizer's derivation, an attested body in canonical form — reaches the parent already substituted and reads exactly like one that crossed as spoken. The shapes below tell apart where the value was checked, not what it says.

The allowed delegation asserts that the call carries arguments, that its response carries the child's own answer, and that the response takes one of two shapes:

- kagent's own result with the child's `subagent_session_id` — what crossed at the child's stop, replayed;
- a `result` alone — the same crossing where kagent answered with a message instead of a task, so no child session id came back.

A denial or withheld return fails an allowed-delegation case. The reason
`the spawn did not take` identifies a child that was not bound to this parent's
prepared fork. Transport failures also fail the case. Both plugins allocate a
fresh child context for every new delegation; an approval resume retains the
paused child's context. Tests cover different parent conversations, repeated
messages in one conversation, and two delegations within one turn. They require
distinct child IDs and useful returned data, not merely a completed tool card.
The injected log instruction must not reach the caller.

The undeclared-delegation case requires the opposite result: the runtime's
`{"appa": "denied"}` response with `not declared by the policy`, and no child ID.
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
