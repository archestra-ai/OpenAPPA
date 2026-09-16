# Standard three-arm Tau evaluation (2026-09-15)

On this Tau banking run with GPT-5.6 Luna, the APPA custom scaffold was associated with 25.53% fewer nominal agent tokens than stock, alongside 5.93 percentage points lower success. Permissive used 28.80% fewer agent tokens than stock. Guarded and permissive differed by one successful simulation; guarded used 4.60% more agent tokens.

This run covers all 97 `banking_knowledge` base-split tasks with four trials in each of three arms: 1,164 validated simulations. It started on September 15 and completed on September 16, 2026 (UTC). ChaosMonkey was not evaluated.

| Arm | Successful simulations | Mean reward | Mean agent tokens per simulation | Recorded cost |
|---|---:|---:|---:|---:|
| Guarded | 133/388 | 34.28% | 934,475 | $30.33 |
| Permissive | 134/388 | 34.54% | 893,405 | $30.50 |
| Stock | 156/388 | 40.21% | 1,254,821 | $35.62 |

Agent tokens include prompt and completion tokens, repeated prompts across calls, cached prompt tokens, and hidden completions within scored simulations. They exclude infrastructure-failed attempts. Matched tasks and seeds do not equate stochastic trajectories, so these differences do not establish causation or statistical equivalence.

The token gap reflects less retrieval and slower context growth, not policy blocking. It persists in the 108 matched simulations where both guarded and stock succeeded: guarded used 32.92% fewer agent tokens. Matching on success does not hold the work performed constant or isolate a policy effect.

## Configuration

The agent was `openrouter/openai/gpt-5.6-luna` at `reasoning_effort=max`, with `alltools-qwen` retrieval and binding `appa-agent-python-v7`. The user simulator was GPT-5.2/low; GPT-4.1 at temperature zero performed judgment and user review. The arms ran sequentially at concurrency 20, with matching task/trial/seed identities and identical common settings.

The measured source is [this revision](https://github.com/archestra-ai/OpenAPPA/commit/7f79ccc4d2273d235bdf871d86ed5e6c005ce36f). The [guarded](guarded-config.json), [permissive](permissive-config.json), and [stock](stock-config.json) manifests retain exact settings, implementation and policy hashes, retrieval identity, and the pinned Tau revision. Requested and resolved model names are retained, but OpenRouter aliases do not establish immutable provider snapshots.

## Refusal counters include scaffold validation

Guarded recorded zero blocks across 7,914 checks. Permissive recorded two `policy_blocks`, both from scaffold input validation rather than restrictions in the permissive contract:

- `task_098`, trial 0: the discoverable-tool name was not registered.
- `task_039`, trial 3: the discoverable-tool wrapper contained invalid JSON arguments.

Both simulations scored zero and remain in the results. Zero guarded blocks applies only to the evaluated calls, not every possible normal prompt.

## Resumes preserved scored outcomes

Every saved, previously scored identity and reward was preserved. Canonical JSON hashes also confirm that all 935 first-pass scored simulation records are unchanged. All three final arms contain 388 evaluated simulations, with zero execution failures and a user-review record for every simulation.

| Arm | Response-format failures retried | Credit-limit failures retried |
|---|---:|---:|
| Guarded | 20 | 0 |
| Permissive | 22 | 157 |
| Stock | 8 | 34 |

These are failed simulation attempts, not scored benchmark failures or counts of failed individual API calls. Only infrastructure failures were retried, with unchanged experiment settings and seeds. The complete arm directory was copied before each resume.

The backup key replaced the primary during permissive. Stock paused when the backup reached its limit and resumed its final 42 cases after the main key's daily reset. Neither key's limit was changed.

The reviewer flagged critical user-simulator errors in 158 guarded, 137 permissive, and 152 stock simulations. No review-flagged episodes were filtered out.

## Artifacts and cost coverage

[summary.json](summary.json) contains aggregate metrics, matched outcomes and simulation IDs, retry counts, and retention checks. The [archive index](archive-index.json) identifies the full trajectories, APPA and evaluator audits, provider responses, debug artifacts, pre-resume snapshots, and retry logs prepared for the private GCP bucket.

The archive contains 28,014 evidence files plus `archive-manifest.json`. Local filesystem paths in 183 files are replaced with `[LOCAL_PATH]`; no files or model calls are omitted. The manifest records original and archived hashes for every evidence file. Original local files remain unchanged. All three recorded implementation hashes were verified against the measured Git revision before packaging.

**GCP publication is pending repository configuration.** The [publishing workflow](https://github.com/archestra-ai/OpenAPPA/actions/runs/35089893136) downloaded the complete archive and verified its hash and index, then stopped before authentication because both `APPA_BENCH_GCS_BUCKET` and `APPA_BENCH_GCP_SERVICE_ACCOUNT` were unset. The private draft release `bench-relay-taubench-tau-knowledge-full-2026-09-15-00ab1e8555a5` retains the verified archive and index. The intended GCP destination is not yet populated by this workflow.

An administrator can configure the repository and retry without this orb:

```sh
gh variable set APPA_BENCH_GCS_BUCKET --repo archestra-ai/OpenAPPA \
  --body archestra-appa-bench-archive
gh secret set APPA_BENCH_GCP_SERVICE_ACCOUNT --repo archestra-ai/OpenAPPA \
  --body appa-bench-publish@friendly-path-465518-r6.iam.gserviceaccount.com
gh run rerun 35089893136 --repo archestra-ai/OpenAPPA
```

The workflow also requires the shared `APPA_GCP_WORKLOAD_IDENTITY_PROVIDER` secret. It deletes the draft relay only after reading the archive and index back from GCP and verifying both. After publication succeeds, download from this directory with project credentials:

```sh
prefix=gs://archestra-appa-bench-archive/bench/taubench/7f79ccc4d2273d235bdf871d86ed5e6c005ce36f/tau-knowledge-full-2026-09-15
archive=$(jq -r .archive archive-index.json)
gcloud storage cp "$prefix/$archive" "$archive"
jq -r '"\(.sha256)  \(.archive)"' archive-index.json | sha256sum --check -
tar --zstd -xf "$archive"
```

The recorded total is $96.45, not an exact provider bill: embedding usage and some failed-attempt usage are unavailable. The separate diagnostic request is not part of scored results.
