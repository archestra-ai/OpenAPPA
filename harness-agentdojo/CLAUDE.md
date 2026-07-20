# Benchmarks (AgentDojo harness)

- In `harness-agentdojo`, contracts label tools by *source type* only
  (readers of third-party text are suspicious, pure-state readers trusted) —
  never by whether a given result actually carries an injection. That is the
  benchmark's ground truth and peeking is cheating.
- The trust-only limitation is the experiment's honest premise, not a bug: a
  benign send and a poisoned one are identical in label space, so a
  trusted-only sink policy pays a utility price. Audience is the dimension
  that would split them; that is data-plus-protocol work, not a new engine.
- A suite's contract table must cover every tool (the harness cross-checks
  and fails on drift); the three unknown-policies only separate under sparse
  annotation.
