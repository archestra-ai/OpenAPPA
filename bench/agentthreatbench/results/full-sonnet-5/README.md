# Claude Sonnet 5 AgentThreatBench comparison

This one-repetition run measured all 24 AgentThreatBench tasks plus two Data
Exfiltration controls. It used the `stock`, `permissive`, `guarded`, `auto`,
and `auto-ifc` arms with Anthropic Claude Sonnet 5 at high reasoning effort.
The two controls are excluded from the 24-task headline rates.

The run records clean commit
`c79958dce63cf37ca4fc269c787eb54a8782e64b`, Inspect AI 0.3.252, and Inspect
Evals revision `0c737b01627b772db84aa223f68775c31199fdc9`.

[`summary.json`](summary.json) is copied from the published archive. The
[`archive index`](archive-index.json) identifies the complete evidence bundle
in the private benchmark bucket.
