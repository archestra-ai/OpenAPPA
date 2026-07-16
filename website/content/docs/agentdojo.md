---
title: AgentDojo harness
category: Evaluation
order: 4
description: Benchmarking the engine against prompt injection.
---

The `harness-agentdojo` crate-adjacent Python package runs the core engine as a tool-call-veto defense inside [AgentDojo](https://github.com/ethz-spylab/agentdojo), the prompt-injection benchmark.

The key property: **the policy never reads the injected text**. It tracks which sources the conversation's context came from (a per-turn label fold) and blocks tool calls whose contract the folded context cannot satisfy.

## Layout

- `check` — stateless Rust policy check over the core engine: one JSON request (contracts + episode so far + proposed call) in on stdin, one decision out on stdout.
- `harness-agentdojo/src/baton_dojo/defense.py` — a drop-in replacement for AgentDojo's `ToolsExecutor` that consults the policy check before executing each tool call the LLM emits. Blocked calls come back on the normal tool-error channel and are never executed.
- `harness-agentdojo/contracts/workspace.toml` — the policy as data. Every suite tool is labeled by its *source type*, never by whether a given result actually carries an injection — that is the benchmark's ground truth, and peeking is cheating. Readers of third-party text are `suspicious`, pure-state readers `trusted`, sinks require a `trusted` context.

## Running the benchmark

```sh
uv sync

# Compare a defended and an undefended pipeline via OpenRouter:
uv run baton-dojo bench --model openai/gpt-4o-mini-2024-07-18 --defense baton
uv run baton-dojo bench --model openai/gpt-4o-mini-2024-07-18 --defense none
```

`bench` prints clean utility, utility under attack, attack success rate, and the number of policy-blocked calls. Per-episode JSON, including full message logs, lands under `--logdir`.

## Narrowing a run

```sh
uv run baton-dojo bench --model openai/gpt-4o-mini-2024-07-18 \
  --user-tasks user_task_0 user_task_13 --injection-tasks injection_task_0 \
  --attack important_instructions --unknown-policy allow_with_audit --logdir runs
```

## Sharding

AgentDojo runs one episode at a time, so a full suite is slow: the `workspace` suite is 40 clean + 40 × 14 attacked = 600 episodes per defense. Episodes are independent and cached, so you can split user tasks across several `bench` processes writing to the *same* `--logdir` — each fills different cells of the grid, cached cells are skipped, and a killed run resumes where it stopped.
