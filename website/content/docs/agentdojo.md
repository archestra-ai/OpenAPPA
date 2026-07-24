---
title: AgentDojo harness
category: Evaluation
order: 4
description: Benchmarking the engine against prompt injection.
---

The `harness-agentdojo` Python package runs OpenAPPA as a tool-call defense inside [AgentDojo](https://github.com/ethz-spylab/agentdojo), the prompt-injection benchmark.

The key property: **the policy never reads the injected text**. It tracks which sources the conversation's context came from (a per-turn label fold) and blocks tool calls whose contract the folded context cannot satisfy.

## Layout

- `appa-agent-python` — the PyO3 adapter that keeps each dispatch handle in Rust and owns the complete check, execution, and outcome-admission transaction.
- `harness-agentdojo/src/appa_dojo/defense.py` — a drop-in replacement for AgentDojo's `ToolsExecutor`. Allowed calls execute through a capability-scoped loopback bridge; blocked calls return on the normal tool-error channel and never execute.
- `harness-agentdojo/src/appa_dojo/contracts/workspace.toml` — the packaged policy data. Every suite tool is labeled by its *source type*, never by whether a given result actually carries an injection — that is the benchmark's ground truth, and peeking is cheating. Readers of third-party text are `suspicious`, pure-state readers `trusted`, sinks require a `trusted` context.
- `appa-dojo-sidecar` — the retained JSON-lines compatibility path; benchmark pipelines use PyO3 by default.

## Running the benchmark

```sh
uv sync

# Compare a defended and an undefended pipeline via OpenRouter:
uv run appa-dojo bench --model openai/gpt-4o-mini-2024-07-18 --defense appa
uv run appa-dojo bench --model openai/gpt-4o-mini-2024-07-18 --defense none
```

`bench` prints clean utility, utility under attack, attack success rate, and the number of policy-blocked calls. Per-episode JSON, including full message logs, lands under `--logdir`.

## Narrowing a run

```sh
uv run appa-dojo bench --model openai/gpt-4o-mini-2024-07-18 \
  --user-tasks user_task_0 user_task_13 --injection-tasks injection_task_0 \
  --attack important_instructions --logdir runs
```

## Sharding

AgentDojo runs one episode at a time, so a full suite is slow: the `workspace` suite is 40 clean + 40 × 14 attacked = 600 episodes per defense. Episodes are independent and cached, so you can split user tasks across several `bench` processes writing to the *same* `--logdir` — each fills different cells of the grid, cached cells are skipped, and a killed run resumes where it stopped.
