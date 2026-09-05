# appa-yell

The endpoint behind `appa yell`. It takes one gzipped `openappa.yell.v1`
document per request, writes it once, and answers with a receipt. It never
reads or lists what is stored, so a caller can add a report and learn nothing
else.

## What it refuses

In this order, so a caller learns nothing about a document it did not sign:

| Stage | Refusal | Why |
| --- | --- | --- |
| method | `405` | the request is not a POST |
| the body | `415` | it is not gzipped — refused before the body is read at all |
| | `413` | the compressed body, or the document it expands to, is over a cap |
| | `400` | it is not a gzip stream, is truncated, or carries a second gzip member behind the first |
| the salt | `401` | there is no matching `X-Appa-Signature` |
| the document | `400` | it is not JSON, or not an envelope this version knows |
| | `413` | the message or the trajectory's entry count is over a cap |
| storage | `503` | the write failed; the same report may be sent again |

The signature is a filter, not an identity. The salt in `salt.txt` ships in
this repository and is compiled into every build, so it turns away a scanner
that finds the URL and a process that posts by accident. It proves nothing
about which deployment sent a report, and every field of every document is
caller-controlled.

The envelope is strict and everything below it is opaque. What sits inside
`trajectory` is decided by the runtime's classification tables and gains
fields whenever the engine does; validating it here would refuse reports from
a newer runtime, which is the runtime most worth hearing from.

`MAX_PLAIN_BYTES` must stay equal to the client's own constant in
`appa-runtime/src/yell/report.rs`. A document a runtime is willing to send and
this function will not hold would fail every attempt, forever, and the
client's size loop cannot see this limit to build under it. A test asserts the
two agree.

## Storage

One object per report at `reports/<sha256 of the document>.json.gz`, holding
the gzip exactly as it arrived and written create-only. A retry of the same bytes is the same object and comes back as
`"duplicate": true`; two different reports can never collide. The name is a
digest rather than the document's `report_id` because that field is written by
whoever sent it, and naming objects by it would let one caller overwrite
another's report.

## Running the tests

```
uv run --project receiver/appa-yell pytest receiver/appa-yell
```

The suite needs no bucket: every case ends before storage is reached. The
accepted path — storage, idempotency, and the signature check as deployed — is
the smoke test in `smoke.py`, which the deploy workflow runs against the real
endpoint.

## How a binary finds this endpoint

`APPA_YELL_ENDPOINT` is compiled in. A release build requires it —
`appa-runtime/build.rs` refuses to produce one without it, because a binary
that cannot send refuses silently and the feature would ship inert. The value
is the `endpoint` output of the `appa-yell` Terraform root, set as a
repository variable of that name; the release workflow passes it through.

A development build has none, resolves an empty endpoint, and refuses to send.
`APPA_YELL_ENDPOINT` in the environment overrides the compiled value at run
time, which is how a test points at a local receiver.

## Who owns what

Terraform, in a root of its own in the infra repository, owns the bucket and
its retention, the runtime service account (`roles/storage.objectCreator` and
nothing else), the human readers, the deploy identity, and the function's
configuration: region, ingress, scaling, environment. `APPA_YELL_BUCKET` is
set there.

`.github/workflows/appa-yell.yml` owns the code, and only the code. It runs on
a manual dispatch from `main` and passes `gcloud functions deploy` no flag
that Terraform owns, so a deploy cannot roll configuration backward. For the
same reason in the other direction, the Terraform resource must ignore changes
to its `build_config` source — otherwise the next apply would roll the
deployed code back to whatever revision Terraform last recorded.
