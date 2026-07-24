# APPA AgentDojo evaluation

This package inserts the current `appa-sdk` call lifecycle immediately before
AgentDojo's in-process tool execution. APPA labels tools by source type and
never reads benchmark injection ground truth.

```sh
uv sync
export OPENROUTER_API_KEY=...

uv run appa-dojo bench \
  --defense appa \
  --user-tasks user_task_16 user_task_33 \
  --injection-tasks injection_task_0
```

The benchmark defaults to `openai/gpt-5.6-luna`; pass `--model` to override it.

Use `--defense none` for the stock AgentDojo baseline and `--defense
appa-open` to measure integration overhead with a no-op APPA policy. Runs are
cached under `--logdir` and can be sharded by passing disjoint user-task lists
to concurrent processes.

The Slack suite additionally exposes `appa-practical`, which leaves the
source-and-sink `get_webpage` request unguarded, and `appa-complete`, which
annotates that network release honestly and therefore blocks the indivisible
web-read call. APPA pipeline cache keys include the policy digest.

The initial policy is deliberately trust-only. Legitimate flows that read
third-party content and then invoke a sink are indistinguishable from poisoned
flows in this label space, so their utility loss is part of the result.
