# APPA AgentDojo evaluation

This package embeds the current `appa-sdk` call lifecycle through the
`appa_agent_python` PyO3 extension. Rust owns each check, exact dispatch, and
outcome report transaction; AgentDojo executes the function through a
single-threaded, capability-scoped server on `127.0.0.1`. APPA labels tools by
source type and never reads benchmark injection ground truth.

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

`appa-dojo-sidecar` and `SidecarClient` remain available as the JSON-lines
compatibility and parity path, but mediated benchmark pipelines use the native
extension and loopback bridge by default.

The Slack suite additionally exposes `appa-practical`, which leaves the
source-and-sink `get_webpage` request unguarded, and `appa-complete`, which
annotates that network release honestly and therefore blocks the indivisible
web-read call. APPA pipeline cache keys include the native binding, bridge
protocol, and policy digest; stock `none` cache names are unchanged.

The initial policy is deliberately trust-only. Legitimate flows that read
third-party content and then invoke a sink are indistinguishable from poisoned
flows in this label space, so their utility loss is part of the result.
