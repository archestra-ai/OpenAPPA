# OpenAPPA Annotator on ShellRisk-Bench

This harness evaluates the native model-backed Annotator against the pinned
[ShellRisk-Bench](https://huggingface.co/datasets/kontext-security/ShellRisk-Bench)
v0.1 test split. It measures whether this Annotator configuration classifies a
proposed Bash command as risky, with a separately instructed bare LLM as a
reference. It does not
evaluate OpenAPPA's other flow decisions or execute any benchmark command.

The harness compares two configurations using the same model:

| Arm | Question | Path |
|---|---|---|
| `annotator` | Does the native Annotator require `shell-risk-review` for commands labelled risky? | Claude Code `/hook`, LLM Annotator, strict annotation schema, Engine decision |
| `bare` | How does the benchmark's explicit risky/safe prompt perform? | Direct OpenAI-compatible chat completion; no OpenAPPA mediation |

The Annotator receives its native security-annotation instructions and the
`shell-risk-review` mark, without the benchmark's risk taxonomy. The bare LLM
receives that taxonomy explicitly. Instructions, output schemas, and request
paths differ, so this is not a controlled measurement of APPA's effect on
classification accuracy. Results describe these configurations, not an inherent
advantage of an APPA role. The native Annotator has no trusted policy-guidance
field; placing classification instructions in tool arguments or the tool
description would put them in input that its system prompt treats as untrusted.

The Annotator policy declares an Authority only to make the review mark
available. The harness scores whether the mark is required; it never invokes
that Authority or executes a remedy plan.

## Setup and smoke test

```sh
cargo build --package appa
uv sync --project bench/shellrisk
export OPENROUTER_API_KEY=...
uv run --project bench/shellrisk appa-shellrisk preflight
uv run --project bench/shellrisk appa-shellrisk smoke
```

Preflight makes no model requests. It validates the pinned dataset and checks
that the selected runtime binary and credential variable exist.

Defaults use OpenRouter's OpenAI-compatible endpoint and
`openai/gpt-5.6-luna`. The Annotator supports every provider implemented by the
runtime. The bare reference currently requires an OpenAI-compatible profile.
Pass an empty `--url` to use a provider's default endpoint.

`smoke` runs six commands in both arms by default. Use `--arm annotator` or
`--arm bare` to select one. Selection is deterministic, approximately balanced
by label, and interleaved by upstream source. Both arms honor `--jobs`; the
runtime also enforces `--max-concurrent`.

## Complete evaluation

The complete test split contains 4,194 commands. A complete run requires the
explicit `--full` flag:

```sh
uv run --project bench/shellrisk appa-shellrisk run --full
```

Each arm makes one model request for each selected command. The command above
therefore makes 8,388 requests because it selects both arms by default.

Do not treat a run as a general security score. ShellRisk's labels are derived
from command sources, so they can contain noise and source-specific artifacts.
Report aggregate and per-source results, and identify each arm's instructions
alongside its scores.

Each git-ignored `runs/<timestamp>/` directory contains a manifest, incremental
per-command records, the generated APPA deployment, runtime logs, and summaries.
`predictions.jsonl` applies ShellRisk's published fallback: no answer counts as
`not_risky`. `predictions-fail-closed.jsonl` maps no answer to `risky`, matching
OpenAPPA's operational refusal. `summary.json` reports both projections,
precision, recall, F1, false-allow rate, false-alarm rate, and latency.
