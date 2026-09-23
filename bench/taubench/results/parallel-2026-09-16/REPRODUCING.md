# Reproduce the 2026-09-16 parallel TauBench arms

These instructions rerun the guarded and permissive arms reported in this directory. They reproduce the committed producer, experiment configuration, 97-task inventory, four trial seeds, and validation gates. Model trajectories are stochastic. A valid replication need not reproduce the published rewards, token totals, or costs.

| Execution profile | Requirement |
|---|---|
| Scope | Two arms, 388 scored simulations per arm |
| Original recorded cost | About $76.16 through OpenRouter, excluding unavailable embedding cost |
| Provider | An OpenRouter account with sufficient spend capacity |
| Memory | At least 15 GiB recommended for concurrency 20 |

The original guarded run exceeded 10 GiB of memory at peak. A smaller machine can lower `--max-concurrency`; concurrency is execution metadata and does not change the experiment identity.

## 1. Install the pinned environment

From the repository root:

```sh
cd bench/taubench

npm install -g @anthropic-ai/sandbox-runtime@0.0.23

# Ubuntu or Debian
sudo apt-get install ripgrep bubblewrap socat

# macOS instead needs only ripgrep from the platform packages.
# brew install ripgrep

./setup-taubench.sh
export OPENROUTER_API_KEY=...
uv run appa-taubench preflight
```

The setup script installs the locked Python environment. It checks out Tau 1.0.1 at `93ee97b8303ce0e89e0ad17e6207591a1846f84b`. Preflight makes no model call.

Verify that the checked-out producer is the one identified by the published configurations:

```sh
TAU2_DATA_DIR="$PWD/.tau2-bench/data" uv run python - <<'PY'
from appa_taubench.bench import implementation_digest
from appa_taubench.native import BINDING_IDENTITY

print(BINDING_IDENTITY)
print(implementation_digest())
PY
```

The final two output lines must be:

```text
appa-agent-python-v8
521484361e16585f611f89358622c1339a76794c040e451309e6d98cfca7bf5a
```

## 2. Validate both plans without spending API credit

The shell array below spells out every effective setting in the committed configurations. Do not set Inspect `max_connections` or `max_samples`. This harness intentionally leaves both unset.

```sh
common=(
  --retrieval-config alltools-qwen
  --model openrouter/openai/gpt-5.6-luna
  --reasoning-effort max
  --user-model openrouter/openai/gpt-5.2
  --judge-model openrouter/openai/gpt-4.1
  --review-model openrouter/openai/gpt-4.1
  --seed 300
  --num-trials 4
  --max-steps 200
  --max-concurrency 20
  --agent-prompt-profile standard
)

uv run appa-taubench run "${common[@]}" --dry-run \
  --policy-mode guarded \
  --run-name tau-knowledge-parallel-replication-guarded

uv run appa-taubench run "${common[@]}" --dry-run \
  --policy-mode permissive \
  --run-name tau-knowledge-parallel-replication-permissive
```

Each plan must report 97 tasks, four trials, and 388 simulations. The output directory must end in `019c238eb860` for guarded and `02ed8ab89cf6` for permissive. These suffixes are deterministic experiment digests, not random run IDs.

## 3. Run or exactly resume each arm

Run the arms separately unless the machine has enough memory and provider capacity for both:

```sh
uv run appa-taubench run "${common[@]}" \
  --policy-mode guarded \
  --run-name tau-knowledge-parallel-replication-guarded

uv run appa-taubench run "${common[@]}" \
  --policy-mode permissive \
  --run-name tau-knowledge-parallel-replication-permissive
```

If infrastructure or provider limits interrupt an arm, rerun the identical command. Tau checkpoints completed simulations. The harness resumes only when the experiment settings match. You can lower `--max-concurrency` when you resume. Do not change another option.

A successful command applies the strict integrity, Tau submission, evaluator-audit, and OpenAPPA-audit checks. It then writes a validated summary. Define the resulting directories:

```sh
guarded=runs/tau-knowledge-parallel-replication-guarded-019c238eb860
permissive=runs/tau-knowledge-parallel-replication-permissive-02ed8ab89cf6
```

The immutable `config` object in each generated `run-config.json` must match its committed counterpart. The `execution.max_concurrency_values` field can differ because it records each concurrency used across resumes.

```sh
diff -u \
  <(jq -S .config results/parallel-2026-09-16/guarded-config.json) \
  <(jq -S .config "$guarded/run-config.json")

diff -u \
  <(jq -S .config results/parallel-2026-09-16/permissive-config.json) \
  <(jq -S .config "$permissive/run-config.json")
```

Both diffs must be empty. Check the strict-validation outputs:

```sh
for run in "$guarded" "$permissive"; do
  jq -e '.status == "validated" and .simulation_count == 388' "$run/run-summary.json"
  jq -e '
    .simulations as $runs
    | ($runs | length) == 388
      and ([$runs[].id] | unique | length) == 388
      and all($runs[]; .reward_info != null and .user_only_review != null)
  ' "$run/results.json"
done
```

## 4. Generate the matched replication summary

```sh
TAU2_DATA_DIR="$PWD/.tau2-bench/data" \
  uv run python - "$guarded" "$permissive" runs/parallel-replication-matched-summary.json <<'PY'
import sys
from pathlib import Path

from appa_taubench.report import build_matched_summary

build_matched_summary(
    {"guarded": Path(sys.argv[1]), "permissive": Path(sys.argv[2])},
    Path(sys.argv[3]),
)
print(sys.argv[3])
PY
```

The generated summary requires identical task, trial, and seed identities across both arms. Compare its aggregate structure with `summary.json`. Treat reward, token, retrieval, duration, and cost differences as stochastic replication outcomes, not checksum failures.

## Evidence boundary

The committed summaries and configurations identify and rerun the experiment. The original complete evidence bundle contains trajectories, audits, retry logs, and snapshots. [`archive-index.json`](archive-index.json) identifies it, but the bundle remains local and unpublished. It is not required for replication. A new run produces its own `results.json`, `appa-audit/`, `evaluator-audit/`, `run-config.json`, and `run-summary.json`.
