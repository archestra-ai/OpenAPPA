# appa-guide walkthrough benchmark

This benchmark measures whether the `appa-guide` skill leaves a person able to
predict what APPA will do next. It also checks that the skill's chat stays
correct, asks before it writes, and fits the host's length limits.

An agent runs it, not a CLI. Models play the host agent, the newcomer who reads
the chat, and the judge. No real host session and no real person take part, so
treat a score as evidence about the skill text, not about users.

## One run chains three models

1. **Simulator.** A subagent plays the host agent. Its instructions are one
   version of the skill, printed by `build_skill.py`. A fixture gives every
   command result. The scenario scripts the user's turns.
   [`briefs/simulator.md`](briefs/simulator.md) is its brief.
2. **Reader.** ReadAs reads only the transcript as a developer who installed
   OpenAPPA ten minutes ago. It then answers a quiz, and it answers "don't know"
   when the chat gives no basis. [`briefs/reader.md`](briefs/reader.md) gives
   the exact call.
3. **Judge.** A subagent scores each answer against a key. It also lists claims
   that contradict the fixture and checks compliance. Run directories have
   random IDs, and the judge must not open the mapping from ID to version.
   [`briefs/judge.md`](briefs/judge.md) and
   [`briefs/judge-heldout.md`](briefs/judge-heldout.md) are its briefs.

| Scenario | User script | Fixture | Scored by |
|---|---|---|---|
| [S1](scenarios/s1-newcomer-setup.md) newcomer setup | `/appa-guide`; asks what `suspicious` does; approves; skips Slack; sets the Linear key | [claude-code](fixtures/claude-code.md) | [12-question quiz](keys/s1.md), [7 held-out questions](keys/heldout.md), audit, compliance |
| [S2](scenarios/s2-expert-setup.md) expert setup | "I know APPA well, skip the explanations" | claude-code | [Checklist](keys/s2.md) |
| [S3](scenarios/s3-explain.md) explain | "How does APPA work? What if Claude read my .env and pushed to GitHub?" | claude-code | [Checklist](keys/s3.md) |
| [K1](scenarios/k1-kagent-init.md) kagent init | `init` on the kagent demo cluster | [kagent](fixtures/kagent.md) | Proposal under kagent's 1,600-character limit, audit |

The quiz questions are inside the ReadAs persona texts in [`personas/`](personas/).

## Running it

You run the scripts. An Amp agent runs the model steps: it needs the Task tool
for subagents and `read_as`, the ReadAs tool from Amp's writing-review plugin.
Run directories go in `bench/appa-guide/runs/`, which Git ignores. One run
directory looks like this:

```text
runs/
  build/base.md, build/change.md   skill text for each version
  mapping.json                     {"r512": {"scenario": "s1", "version": "change", "model": "haiku-5.5"}, ...}
  r512/transcript.md               simulator output, every scenario
  r512/reader.md, r512/heldout.md  ReadAs answers, S1 only
  judge.md, judge-heldout.md       judge tables for all runs
```

1. Print each skill version you compare:

   ```sh
   mkdir -p bench/appa-guide/runs/build
   python3 bench/appa-guide/build_skill.py claude-code --ref origin/main > bench/appa-guide/runs/build/base.md
   python3 bench/appa-guide/build_skill.py claude-code > bench/appa-guide/runs/build/change.md
   ```

   For K1, pass `kagent` instead of `claude-code`.
2. Give every run a random ID and add it to `mapping.json`. Ask the agent to
   start one simulator subagent per run with [`briefs/simulator.md`](briefs/simulator.md),
   naming the built skill, the fixture, the scenario and the transcript path.
3. For each S1 run, ask the agent to call ReadAs twice, as
   [`briefs/reader.md`](briefs/reader.md) says. S2, S3 and K1 have no reader.
4. Ask the agent to start one judge subagent with [`briefs/judge.md`](briefs/judge.md)
   for the S1, S2 and S3 runs, and one with
   [`briefs/judge-heldout.md`](briefs/judge-heldout.md) for the S1 runs. Give
   them run IDs only, never the mapping. Check K1 by counting the characters of
   the final proposal against kagent's 1,600-character limit.
5. Count the words the user reads:

   ```sh
   python3 bench/appa-guide/word_count.py bench/appa-guide/runs/*/transcript.md
   ```

6. Join the judge tables with `mapping.json` to compare versions.

Use at least two simulator models, one of them small, such as Opus 5.5 and
Haiku 5.5. In the first run, Haiku and GPT gained most from a change that Opus
partly made up for on its own.

## Limits

- **Noise is about one point.** Two identical Opus runs scored 8.5 and 7.5 of
  12. Treat a difference of one point or less on a single run as noise.
- **The keys can be overfit.** A skill tuned until S1 scores 12 can still fail
  new questions. Before you trust a tuned version, write new held-out questions
  and score them on transcripts that already exist.
- **Fixtures go stale.** The Claude Code fixture copies `appa describe`, battery
  status and battery README facts from the default policy and batteries at the
  time it was written. Update it when those change, or the audit counts true
  claims as unsupported.
- **The reader is a model.** ReadAs answers more literally than a person, and
  one reader persona cannot stand for every user.
- **Some models refuse.** Sonnet 5.5 refused to keep simulating after two runs,
  so it gave no result for the final version.

## Examples

[`examples/`](examples/) holds verbatim runs from the test of
[PR #942](https://github.com/archestra-ai/OpenAPPA/pull/942). They show what a
transcript, a reader's answers and a score look like. They are not published
results: no run of this benchmark goes to the result archive.

| Example | Skill | Simulator | S1 quiz /12 | Held-out /7 |
|---|---|---|---|---|
| [s1-main-opus](examples/s1-main-opus/transcript.md) | main | Opus 5.5 | 8.5 | 4.5 |
| [s1-pr-opus](examples/s1-pr-opus/transcript.md) | #942 | Opus 5.5 | 12 | 6 |
| [s1-main-haiku](examples/s1-main-haiku/transcript.md) | main | Haiku 5.5 | 5 | 2 |
| [s1-pr-haiku](examples/s1-pr-haiku/transcript.md) | #942 draft | Haiku 5.5 | 11 | 6 |
| [s2-pr-opus](examples/s2-pr-opus/transcript.md) | #942 draft | Opus 5.5 | 4/4 checklist | |
| [s3-main-opus](examples/s3-main-opus/transcript.md) | main | Opus 5.5 | 3.5/5 checklist | |
| [s3-pr-opus](examples/s3-pr-opus/transcript.md) | #942 draft | Opus 5.5 | 4.5/5 checklist | |
| [k1-main-opus](examples/k1-main-opus/transcript.md) | main | Opus 5.5 | 1,420 characters | |
| [k1-pr-opus](examples/k1-pr-opus/transcript.md) | #942 | Opus 5.5 | 1,573 characters | |

"#942 draft" is the version tested just before the final edits. It differs
from the final skill by 15 words.
