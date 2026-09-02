# OpenAPPA consults on ShellRisk-Bench

This harness evaluates `appa-runtime/src/consult.rs` against the pinned
[ShellRisk-Bench](https://huggingface.co/datasets/kontext-security/ShellRisk-Bench)
v0.1 test split. It measures one narrow capability: whether a model-backed
OpenAPPA consult classifies a proposed Bash command as risky. It does not
evaluate OpenAPPA's other flow decisions or execute any benchmark command.

The harness compares three paths through the same model:

| Arm | Question | OpenAPPA path |
|---|---|---|
| `annotator` | Can the native Annotator infer a `shell-risk-review` requirement? | Claude Code `/hook`, LLM Annotator, strict annotation schema, Engine decision |
| `authority` | Can a model authority apply the benchmark taxonomy? | `/hook` offer and vouch, MCP `execute_remedy_plan`, LLM authority, strict ruling schema |
| `bare` | How does the benchmark's one-word prompt perform? | Direct OpenAI-compatible chat completion; no OpenAPPA mediation |

The `annotator` arm deliberately gives the model only the self-describing mark.
It does not copy the benchmark taxonomy into untrusted Bash arguments or change
the native Annotator prompt. The `authority` and `bare` arms are controls that
receive the taxonomy explicitly.

## Setup and smoke test

```sh
cargo build --package appa
uv sync --project bench/shellrisk
export OPENROUTER_API_KEY=...
uv run --project bench/shellrisk appa-shellrisk preflight
uv run --project bench/shellrisk appa-shellrisk run \
  --arm annotator \
  --limit 2 \
  --jobs 1
```

Preflight makes no model request. It validates the pinned dataset and checks
that the selected runtime binary and credential variable exist.

Defaults use OpenRouter's OpenAI-compatible endpoint and
`openai/gpt-5.6-luna`. APPA arms support every provider implemented by the
runtime. The bare control currently requires an OpenAI-compatible profile.
Pass an empty `--url` to use a provider's default endpoint.

`smoke` runs six commands in every selected arm. Selection is deterministic,
approximately balanced by label, and interleaved by upstream source. The
authority arm is sequential because it uses one MCP session. Annotator and
bare requests honor `--jobs`; the runtime also enforces `--max-concurrent`.

## Complete evaluation

The complete test split contains 4,194 commands. A complete run is only
available through the explicit `--full` flag:

```sh
uv run --project bench/shellrisk appa-shellrisk run --full
```

Each arm makes one model request for each selected command. The command above
therefore makes 12,582 requests because it selects all three arms by default.

Do not treat a run as a general security score. ShellRisk's labels are derived
from command sources, so they can contain noise and source-specific artifacts.
Report aggregate and per-source results. Compare the two model-backed OpenAPPA
arms with the bare control under the same model profile.

Each ignored `runs/<timestamp>/` directory contains a manifest, incremental
per-command records, the generated APPA deployment, runtime logs, and summaries.
`predictions.jsonl` applies ShellRisk's published fallback: no answer counts as
`not_risky`. `predictions-fail-closed.jsonl` maps no answer to `risky`, matching
OpenAPPA's operational refusal. `summary.json` reports both projections,
precision, recall, F1, false-allow rate, false-alarm rate, and latency.
