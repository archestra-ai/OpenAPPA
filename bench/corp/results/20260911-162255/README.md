# Claude Sonnet 5 Corp comparison (`20260911-162255`)

This one-repetition run measured all 20 Corp scenarios with the `appa`,
`appa-open`, `auto`, and `auto-ifc` arms. Every arm used Claude Sonnet 5. The
APPA arms used OpenRouter; the Auto arms used Anthropic through Claude Code.

The run's embedded configuration records source commit
[`50971988`](https://github.com/archestra-ai/OpenAPPA/commit/509719884ceb37e7e2c63daf54cabfe3d9620380)
and a clean worktree. The archive index and GCS path incorrectly attribute it
to the later commit
[`c79958dc`](https://github.com/archestra-ai/OpenAPPA/commit/c79958dce63cf37ca4fc269c787eb54a8782e64b).
The [packaging script](https://ampcode.com/threads/T-01a08ff7-1130-7183-bddb-119bf3bb497f)
hard-coded that later commit into the index. The one-off upload workflow also
hard-coded it into the GCS path, as recorded in the
[successful upload](https://github.com/archestra-ai/OpenAPPA/actions/runs/34877464312).
No Corp executable code changed between these commits. The archive index is
preserved unchanged as a record of what was published.

[`summary.json`](summary.json) is copied from the published archive. The
[`archive index`](archive-index.json) identifies the complete evidence bundle
in the private benchmark bucket.
