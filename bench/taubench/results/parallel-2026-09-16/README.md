# Parallel tool-call Tau evaluation (2026-09-16–17)

Parallel tool calling was associated with higher utility and fewer model rounds than the prior sequential custom-scaffold runs. It did not reduce tokens. Wider retrieval coincided with about 40% more agent tokens despite about 22% fewer model calls.

The evaluation covers all 97 `banking_knowledge` base tasks with four trials per arm. Each arm has 388 strictly validated simulations. Both use GPT-5.6 Luna at maximum reasoning effort, `alltools-qwen`, seed 300, and binding `appa-agent-python-v8`.

[`REPRODUCING.md`](REPRODUCING.md) gives the complete setup, identity check, dry-run, execution, resume, validation, and matched-summary procedure for both arms.

| Policy mode | Tool calling | Successful simulations | Mean reward | Mean agent tokens per simulation | Visible agent model calls | Recorded cost |
|---|---|---:|---:|---:|---:|---:|
| Guarded | Sequential | 133/388 | 34.28% | 934,475 | 9,854 | $30.33 |
| Guarded | Parallel | 151/388 | 38.92% | 1,307,763 | 7,679 | $38.36 |
| Permissive | Sequential | 134/388 | 34.54% | 893,405 | 9,692 | $30.50 |
| Permissive | Parallel | 153/388 | 39.43% | 1,272,128 | 7,483 | $37.80 |
| Stock | Native parallel | 156/388 | 40.21% | 1,254,821 | — | $35.62 |

The sequential and stock rows come from the [2026-09-15 evaluation](../full-2026-09-15/README.md). Stock is Tau's unmediated scaffold, which already supported multi-call completions and did not need a rerun. The sequential and parallel custom-scaffold runs use matched task, trial, and seed identities. Their trajectories are stochastic and ran on different dates, so differences are associations rather than causal estimates.

Recorded costs include retained and discarded agent attempts when usage was available. Tau does not expose embedding usage or cost.

## Utility improved, but individual outcomes moved both ways

Guarded gained 18 passes, or 4.64 percentage points. Of the 388 matched identities, 45 changed from fail to pass and 27 changed from pass to fail. Permissive gained 19 passes, or 4.90 percentage points, with 48 fail-to-pass and 29 pass-to-fail changes.

The two parallel arms agree on 306 outcomes: 111 both pass and 195 both fail. Guarded alone passes 40 identities, while permissive alone passes 42. The two-pass aggregate gap does not imply policy equivalence.

## Fewer model rounds produced more retrieval and more tokens

Parallel guarded used 22.07% fewer visible agent model calls than sequential guarded. Parallel permissive used 22.79% fewer. However, retrieval calls increased by 82.50% and 84.71%, respectively. Dense retrieval rose from 122 to 1,699 calls in guarded and from 99 to 1,722 in permissive.

The extra documents grew later prompts. Mean guarded agent tokens increased from 934,475 to 1,307,763 per simulation. Mean permissive agent tokens increased from 893,405 to 1,272,128. Redundant document hits also increased by 128.14% and 124.33%.

Observed summed simulation duration fell by 18.69% in guarded and 19.01% in permissive. These totals exclude failed infrastructure attempts and do not isolate execution concurrency from changed trajectories.

## Security observations are narrow

Tau is a utility and cost benchmark. It does not establish that parallel execution improves or weakens net security.

Of the guarded arm's 11,355 checks, four calls were blocked. One contract decision prevented a state-changing call before identity verification. The harness abandoned its already-allowed sibling, and the trajectory replanned. The other three blocks rejected malformed nested discoverable-tool JSON. Permissive performed 11,167 checks and rejected four malformed nested calls. These were scaffold validation failures, not policy restrictions.

Blocked batches abandoned six released siblings in guarded and 29 in permissive before tool execution. The 388 scored guarded audits contain 2,624 multi-call completions with 8,211 calls, up to 16 in one completion. The permissive audits contain 2,588 multi-call completions with 8,105 calls, up to 15. Every policy event and result uses a unique call ID, and each allowed non-abandoned call has one result. Neither arm recorded an assistant or user tool execution error.

This validates call correlation and blocked-sibling cancellation in these trajectories. It does not make external tools transactional or rule out races in other workloads.

## Concurrency and retries

Tau requested concurrency 20 and repeatedly reported 20 active samples in both large Amp orbs. Inspect `max_connections` and `max_samples` remained unset. No provider concurrency throttling was observed.

Concurrency 20 exceeded the first 4 GiB orb's memory as checkpoints grew. Exact resume on 15 GiB orbs sustained 20. Guarded peaked at 10.224 GiB of service memory, and permissive peaked at 8.72 GiB of process memory. These measurements distinguish Tau sample concurrency from provider connection limits.

OpenRouter spending limits interrupted both runs. Exact resume retained valid scored identities and retried only infrastructure failures. Both final `results.json` files contain 388 unique, rewarded, reviewed simulations with zero execution failures. Both `run-summary.json` files have status `validated`.

## Artifacts

[summary.json](summary.json) contains matched outcomes, sequential-to-parallel transitions, aggregate metrics, and parallel-call diagnostics. The [guarded](guarded-run-summary.json) and [permissive](permissive-run-summary.json) run summaries retain the authoritative per-arm figures. The [guarded](guarded-config.json) and [permissive](permissive-config.json) configurations retain the exact experiment identities.

The [archive index](archive-index.json) specifies the complete evidence bundle. It contains final runs, pre-resume snapshots, retry logs, audits, and these committed result artifacts. Both component archives and the combined bundle were rebuilt twice with identical bytes.

- **Size:** 504,615,088 bytes
- **SHA-256:** `5d141d1b4789bb2d4a60eaea1a1d22d2a720493f858bfc91fbba92926910c1d1`
- **Verification:** [GitHub Actions run 36029310446](https://github.com/archestra-ai/OpenAPPA/actions/runs/36029310446) downloaded the GCP object, verified its SHA-256, compared the uploaded index byte-for-byte, and then deleted the draft relay release.
- **Private storage:**

```text
gs://archestra-appa-bench-archive/bench/taubench/e6ed6ba5a66cff0c6ebc9883425df06f98ee6ce2/tau-knowledge-parallel-2026-09-16/
```

Authenticated project contributors can retrieve and verify the bundle:

```sh
prefix=gs://archestra-appa-bench-archive/bench/taubench/e6ed6ba5a66cff0c6ebc9883425df06f98ee6ce2/tau-knowledge-parallel-2026-09-16
archive=tau-knowledge-parallel-2026-09-16-5d141d1b4789bb2d4a60eaea1a1d22d2a720493f858bfc91fbba92926910c1d1.tar.zst

gcloud storage cp "$prefix/$archive" .
echo "5d141d1b4789bb2d4a60eaea1a1d22d2a720493f858bfc91fbba92926910c1d1  $archive" \
  | sha256sum --check --strict
```
