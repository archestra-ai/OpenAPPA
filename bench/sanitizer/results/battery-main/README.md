# Battery sanitizers on Haiku 4.5 and Sonnet (`battery-main`)

This run asked the five sanitizers of a Claude Code deployment about all 72
corpus cases, 3 times each. It used the `identity`, `llm`, and `claude-code`
arms. Each model consult had the runtime's default 60 s budget and
`max_body_bytes = 65536`. The run is complete: every arm returned one answer
per case and repeat.

The run records clean commit
[`054fccdd`](https://github.com/archestra-ai/OpenAPPA/commit/054fccdd7c110f8237e37b32d49fbfcf8bacb82a).
Its tree is the tree that #762 merged as
[`6787a25e`](https://github.com/archestra-ai/OpenAPPA/commit/6787a25e2862ad21e3092ba9eff53b3e0c86a185).

[`summary.json`](summary.json) is copied from the published archive. The
[archive index](archive-index.json) identifies the complete evidence bundle in
the private benchmark bucket.

## Backends

| Arm | Model | Route |
|---|---|---|
| `identity` | none | A command sanitizer that returns every value unchanged |
| `llm` | `anthropic/claude-haiku-4.5` | `provider = "openai"` with the OpenRouter `url`, as `summary.json` records |
| `claude-code` | `sonnet`, which resolved to `claude-sonnet-5` | The `claude` CLI with `ANTHROPIC_BASE_URL` set to OpenRouter |

`summary.json` does not record the `claude-code` route: the variable reached
the `claude` CLI through the environment. Both model arms used OpenRouter, so
their latency includes OpenRouter's routing.

## Results

Each value below comes from `summary.json`. Cite that file, not this table.
Latency is of the consults that returned a derivation.

| Sanitizer | Measure | `identity` | Haiku 4.5 | Sonnet |
|---|---|---|---|---|
| `coding.strip-instructions` | Payloads that crossed as `trusted` | 24/24 | 0/24 | 0/24 |
| | p50 / p90 | 48 ms / 57 ms | 1.6 s / 2.0 s | 2.9 s / 3.4 s |
| `cloudflare.strip-instructions` | Payloads that crossed as `trusted` | 39/39 | 6/39 | 3/39 |
| | Consults withheld for timeout | 0/72 | 12/72 | 12/72 |
| `coding.strip-secrets` | Redaction recall / precision | 0.0 / — | 0.98 / 0.98 | 1.0 / 1.0 |
| | Consults withheld for timeout | 0/54 | 9/54 | 12/54 |
| `cloudflare.redact-for-public` | Redaction recall / precision | 0.0 / — | 0.96 / 1.0 | 1.0 / 1.0 |
| `redact-for-public` | Redaction recall / precision | 0.0 / — | 0.98 / 1.0 | 1.0 / 0.98 |

No derivation in a model arm was `wrapped`. Every value of 32 KB or more
timed out in both model arms. A timed-out consult leaves the value blocked, so
it counts as stopped, not as crossed.

## Limits

- The corpus is fixed. It is not an adaptive attacker.
- Scoring is substring matching. It does not detect a payload restated without
  its address.
- The run does not measure end-to-end task success.

## Retrieve and verify

Requires read access to `gs://archestra-appa-bench-archive`, plus `gcloud`,
`jq`, and `zstd`. From this directory:

```sh
index="$PWD/archive-index.json"
mkdir -p retrieved
prefix=$(jq -r .publication.gcs_prefix "$index")
archive=$(jq -r .archive "$index")
gcloud storage cp "$prefix/$archive" "$prefix/index.json" retrieved/
(cd retrieved && jq -r '"\(.sha256)  \(.archive)"' "$index" | sha256sum --check --strict)
echo "$(jq -r .publication.relay_index_sha256 "$index")  retrieved/index.json" | sha256sum --check --strict
tar --zstd -xf "retrieved/$archive" -C retrieved
cmp retrieved/battery-main/summary.json summary.json
```

The archive holds each arm's `policy.toml`, the battery copies it includes,
`values.sha256`, `answers.jsonl`, and `summary.json`. It holds no raw corpus
value. Regenerate the values from the recorded commit, and check them against
`values.sha256`. In `answers.jsonl`, each `drop` span of a case reads
`REDACTED-DROP-<index>`.
