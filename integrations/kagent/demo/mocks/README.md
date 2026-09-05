# Mock externals for the kagent demo

One stdlib-only HTTP service (`mock_externals.py`) answering
appa-runtime's consult wire for four registered components: an
Annotator that produces per-call contracts for `lookup_runbook`, a
human-less authority that rules on `scale_deployment` inside its
release window, a change board — an Authority backed by people
out of band — that parks each `rollback_deployment` consult until a
member rules on the side channel (`GET /pending`, `POST /decide`) or
the approval window closes (a clean no-answer), and a sanitizer that
answers one deterministic derivation. Deterministic, canned,
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

Request (the consult for `scale_deployment(catalog-cache, 2)`):

```json
{"version": 1, "kind": "authority", "name": "release-window",
 "declaration": {"hint": "Approve a change only for a deployment inside the release window.",
                 "permits": {"attention": ["release-window"], "effects_containing": []}},
 "artifact": {"tool": "scale_deployment",
              "arguments": {"name": "catalog-cache", "replicas": 2},
              "requirements": [{"kind": "attention", "mark": "release-window"}]}}
```

The ruling is `{"ruling": "approve"|"deny", "reason": "..."}` inside
the answer envelope: approve iff any top-level string argument equals
`catalog-cache`, deny otherwise. So `scale_deployment(catalog-cache, 2)`
is authorized machine-side while `scale_deployment(checkout-api, 5)`
stays denied.

### `POST /approve` — authority `change-board`

The change board takes the same envelope shape: `"name":
"change-board"`, the hint `Ask the change board through its approval
channel; it answers when a member rules.`, and an artifact for
`rollback_deployment` under the `change-approval` mark. The mock parks
the consult and answers it in one of two ways:

- A member rules on the side channel. `GET /pending` lists the parked
  consults (`id`, `tool`, `arguments`, `hint`, `age_s`). `POST /decide`
  with `{"id": "...", "ruling": "approve"|"deny", "reason": "..."}`
  answers the parked consult with `{"ruling": ..., "reason": ...}` in
  the answer envelope. The reason is optional.
- The approval window closes first (`--approval-window`, default 25 s):
  HTTP 504, a clean no-answer, and the offer stands.

The window must sit inside the policy's `externals.timeout_ms` (30 s in
the demo policy). Then an unanswered consult is a clean no-answer and
never a transport error.

### `POST /sanitize` — sanitizers `strip-secret-values`, `strip-instructions`

A sanitizer consult carries the value under `artifact.body`, and the
tool that produced it under `artifact.tool` where one did — a child
return names none. Request (verbatim from a live consult, the body
elided):

```json
{"version": 1, "kind": "sanitizer", "name": "strip-secret-values",
 "declaration": {"hint": "Describe what the data is for and which keys exist, ...",
                 "on": "tool_output",
                 "permits": {"audience": {"from": ["ops"], "to": "public"}}},
 "artifact": {"tool": "read_secret",
              "body": "{\"content\":[{\"type\":\"text\",\"text\":\"{\\n  \\\"PAYMENTS_API_KEY\\\": ...}"}}
```

The body is whatever the tool produced, as the harness delivered it —
here the MCP call result in full, secret values and all.

The answer is exactly `{"version": 1, "answer": {"body": "..."}}` —
`SanitizerAnswer` rejects an unknown key, so nothing else may ride
along. The mock ignores the hint and the name, and applies two
mechanical rules to the body:

| rule | effect |
|---|---|
| a value matching `pk_live_*` or `whsec_*` | replaced by `[redacted]` |
| a line carrying `ignore your previous instructions` or `SYSTEM:`, and the indented lines continuing it | dropped |

Both rules together cover the demo's hazards: the secret material
`read_secret` returns, and the instructions embedded in the crash logs
and the upstream status page. The demo's crash log wraps its injection
over two lines, which is why a continuation line goes with the line it
continues.

The demo chart and integration suite ([../../tests/](../../tests/)) bind
both sanitizers here. Sanitized-remedy cases therefore run without a
second model credential or a nondeterministic derivation.

## Policy wiring

What the demo policy carries for the annotator and the two authorities:

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
name = "scale_deployment"
delta = {}
[policy.tool.requires]
trust = "trusted"
attention = ["release-window"]

[[policy.authority]]
name = "release-window"
hint = "Approve a change only for a deployment inside the release window."
[policy.authority.permits]
attention = ["release-window"]

[[policy.tool]]
name = "rollback_deployment"
delta = {}
[policy.tool.requires]
trust = "trusted"
attention = ["change-approval"]

[[policy.authority]]
name = "change-board"
hint = "Ask the change board through its approval channel; it answers when a member rules."
[policy.authority.permits]
attention = ["change-approval"]

[externals]
timeout_ms = 30000

[externals.annotators.runbook-readers]
command = ["/usr/local/bin/python3", "-c", "<fixed HTTP forwarding adapter>",
           "http://appa-demo-mocks.kagent.svc.cluster.local:8081/annotate"]

[externals.authorities.release-window]
command = ["/usr/local/bin/python3", "-c", "<fixed HTTP forwarding adapter>",
           "http://appa-demo-mocks.kagent.svc.cluster.local:8081/authorize"]

[externals.authorities.change-board]
command = ["/usr/local/bin/python3", "-c", "<fixed HTTP forwarding adapter>",
           "http://appa-demo-mocks.kagent.svc.cluster.local:8081/approve"]
```

The sanitizers bind the same way where a deployment wants the
deterministic derivation instead of a model:

```toml
[externals.sanitizers.strip-secret-values]
command = ["/usr/local/bin/python3", "-c", "<fixed HTTP forwarding adapter>",
           "http://appa-demo-mocks.kagent.svc.cluster.local:8081/sanitize"]

[externals.sanitizers.strip-instructions]
command = ["/usr/local/bin/python3", "-c", "<fixed HTTP forwarding adapter>",
           "http://appa-demo-mocks.kagent.svc.cluster.local:8081/sanitize"]
```

The complete command is in
[`../chart/files/demo.appa.toml`](../chart/files/demo.appa.toml). It
forwards the consult envelope on stdin and writes the answer envelope to
stdout. No provider token or runtime environment enters the subprocess.

A direct `url` binding still accepts cleartext `http` only to a loopback
host (`appa-runtime/src/config.rs`, `validated_url`). The in-cluster mock
address would be refused as a URL binding:

```
appa runtime: the annotators endpoint "runbook-readers" uses cleartext http to a non-loopback host: ...
```

The fixture policy instead selects a local command implementation. That
command forwards only demo consult envelopes to the mock Service. A
transport failure or malformed answer remains a no-answer, so the gate
fails closed.

## Where it runs

The fixture chart ([../chart](../chart)) runs the mocks in their own
Deployment. `appa-demo-mocks` exposes consult endpoints to the fixed
command adapters and the change-board side channel (`/pending`,
`/decide`) to a member outside the pod. The image builds from
[Dockerfile](Dockerfile) and runs as uid 65532.

For a laptop run against a local `appa runtime`:

```sh
python3 integrations/kagent/demo/mocks/mock_externals.py --host 127.0.0.1 --port 8081
```

The integration suite starts the same file on a free loopback port with
`--approval-window 2`, so its change-board cases close in seconds.
