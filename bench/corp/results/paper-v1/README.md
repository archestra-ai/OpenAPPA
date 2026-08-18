# The runs behind the paper's Table 1

Each directory holds the `config.json` and `summary.json` of the run that produced one row
of the evaluation table in `sections/07-evaluation.tex` of the paper repo
([archestra-ai/openappa-paper](https://github.com/archestra-ai/openappa-paper), private). The per-episode directories
are not committed — they are several megabytes of transcripts and residual filesystem state
per run, and the summary carries every number the table quotes. Committing the pair fixes
what was previously unrecorded: which code state each published figure came from.

| paper row | directory | model id | run |
|---|---|---|---|
| Gemini 3.5 Flash-Lite | `gemini-3.5-flash-lite/` | `google/gemini-3.5-flash-lite` | `20260725-002623` |
| GPT-5.6 Luna | `gpt-5.6-luna/` | `openai/gpt-5.6-luna` | `20260725-002247` |
| GPT-4o | `gpt-4o/` | `openai/gpt-4o` | `20260725-003353` |
| Qwen 3.6 35B | `qwen3.6-35b/` | `qwen/qwen3.6-35b-a3b` | `20260725-084321` |

## Every run names the commit it ran against

`config.json` records `git_sha` and `git_dirty`, written by the runner at launch
(`src/bench_corp/cli.py`). All four runs report `c4d94cabec8d4ee1a74327561b3895d4887c9474`
with `git_dirty: true`, so the code state is that commit plus a working-tree diff that was
never recorded. Anyone reproducing these numbers should expect the commit to get close and
should not expect an exact match.

## The Qwen row came from the second run of that model

Two Qwen runs exist at the same commit. The earlier one, `20260725-002712`, reports 27/39
utility for the `appa` arm; the paper prints 28/39, which is the 08:43 run committed here.
The other three rows match their run exactly, arm for arm, including the remedy counts.

## Reproducing

A full sweep is 5 arms × 14 scenarios × 3 repetitions per model, run against a live model
through OpenRouter under provider-default sampling. Nothing is seeded, so repeated runs
differ; the paper reports that per-arm results varied by at most three episodes across its
three repetitions. See the root `README.md` of `bench-corp` for how to launch one.

## These runs predate the runtime-v1 retirement

Every run recorded here was scored under the earlier runtime. The mediation it
implemented differs from the current one in a way that reaches the numbers: an
authorized remedy used to dispatch the call it authorized, where the model now
proposes that call again (`RUL-5`), which costs one inference round and gives a
weaker model one more chance to do something else. Rerunning against the
current code is expected to move the figures; these files record what the
published rows actually came from, not a baseline the current code reproduces.
