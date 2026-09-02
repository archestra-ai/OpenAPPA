# Mock externals for the kagent demo

One stdlib-only HTTP service (`mock_externals.py`) answering
appa-runtime's consult wire for three registered components: an
Annotator that produces per-call contracts for `lookup_runbook`, a
human-less authority that rules on `scale_deployment` inside its
release window, and a change board — a URL authority backed by people
out of band — that parks each `rollback_deployment` consult until a
member rules on the side channel (`GET /pending`, `POST /decide`) or
the approval window closes (a clean no-answer). Deterministic, canned,
and logged — the decisions the runtime makes with these answers are
real.

## The wire

The runtime POSTs one envelope per consult and reads back
`{"version": 1, "answer": <object>}` — exactly those two keys
(`appa-runtime/src/external.rs`). Any non-2xx status is a clean
no-answer, never a denial; the runtime never parses an error body.
Answers are read strictly (`appa-runtime/src/consult.rs`): an unknown
key, a missing key, or a value outside the declared mandate is no
answer.

### `POST /annotate` — Annotator `runbook-readers`

Request (verbatim from a live consult):

```json
{"version": 1, "kind": "annotation", "name": "runbook-readers",
 "declaration": {"inputs": [], "trust_ranks": [], "audiences": ["ops"],
                 "attention_marks": [], "effects": []},
 "artifact": {"args": {"name": "lookup_runbook",
                       "arguments": {"runbook": "ops-database-failover"}}}}
```

The runbook id decides the produced contract:

| runbook id | answer |
|---|---|
| `public-*` | `{"version":1,"answer":{"delta":{},"requires":{"history":[],"attention":[]},"emits":[]}}` |
| `ops-*` | `{"version":1,"answer":{"delta":{"audience":["ops"]},"requires":{"history":[],"attention":[]},"emits":[]}}` |
| anything else | HTTP 404 — no answer; the runtime refuses the call operationally |

`requires` always carries its `history` and `attention` arrays — the
decoder treats their absence as no answer. The `ops` audience is
answered only when the consult's declaration lists it; a mandate that
does not admit it gets no answer instead of a malformed one.

### `POST /authorize` — authority `release-window`

Request (verbatim from a live remedy):

```json
{"version": 1, "kind": "authority", "name": "release-window",
 "declaration": {"hint": "Approve a restart only for a deployment inside the release window.",
                 "permits": {"attention": ["release-window"], "effects_containing": []}},
 "artifact": {"tool": "restart_deployment",
              "arguments": {"name": "catalog-cache"},
              "requirements": [{"kind": "attention", "mark": "release-window"}]}}
```

The ruling is `{"ruling": "approve"|"deny", "reason": "..."}` inside
the answer envelope: approve iff any top-level string argument equals
`catalog-cache`, deny otherwise. So `restart_deployment(catalog-cache)`
is authorized machine-side while `checkout-api` stays denied.

## Policy wiring

What the demo policy carries for these two components:

```toml
[[policy.annotator]]
name = "runbook-readers"
ranks = []            # the produced contract writes no trust rank
audiences = ["ops"]   # ... and no reader outside this mandate
marks = []
effects = []

[[policy.tool]]
name = "lookup_runbook"
annotator = "runbook-readers"

[[policy.tool]]
name = "restart_deployment"
delta = {}
[policy.tool.requires]
trust = "trusted"
attention = ["release-window"]

[[policy.authority]]
name = "release-window"
hint = "Approve a restart only for a deployment inside the release window."
[policy.authority.permits]
attention = ["release-window"]

[externals.annotators.runbook-readers]
url = "http://127.0.0.1:8081/annotate"

[externals.authorities.release-window]
url = "http://127.0.0.1:8081/authorize"
```

**The URLs must be loopback.** A `url` binding accepts cleartext
`http` only to a loopback host — a 127.0.0.0/8 address, `[::1]`, or
the literal `localhost` (`appa-runtime/src/config.rs`,
`validated_url`). `http://appa-demo-mocks.kagent.svc.cluster.local`
is refused at startup:

```
appa runtime: the annotators endpoint "runbook-readers" uses cleartext http to a non-loopback host: ...
```

`https` reaches anywhere, but the runtime offers no CA override, so a
self-signed in-cluster certificate fails verification and every
consult becomes a no-answer — the gate fails closed. The wiring that
works in the demo: run the mock inside the gated agent's pod, beside
`appa-runtime` (the quickstart already runs the runtime on
`127.0.0.1:8787`), and bind `http://127.0.0.1:8081/...`.

## Where it runs

In the demo chart ([../chart](../chart)) the mocks are a sidecar of the
`appa-runtime` pod: the runtime consults them over the pod's loopback
(`http://127.0.0.1:8081/...` in the policy's `[externals]` bindings —
a `url` binding takes cleartext http to loopback only), and the
`appa-demo-mocks` Service exposes the change board's side channel
(`/pending`, `/decide`) to a member outside the pod. The image builds
from [Dockerfile](Dockerfile).

For a laptop run against a local `appa runtime`:

```sh
python3 integrations/kagent/demo/mocks/mock_externals.py --host 127.0.0.1 --port 8081
```
