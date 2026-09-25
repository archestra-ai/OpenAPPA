# Benchmarks

This directory holds three scored benchmarks and the packages that support
them. Each benchmark directory has its own README with setup and run commands;
this page answers the questions that cut across all three: what each
benchmark scores, which scores come from rules and which from an LLM judge,
where LLM spend goes, and where published results live.

| Directory | Role |
|---|---|
| [`corp/`](corp/README.md) | Corp benchmark: 20 workplace scenarios with planted attacks |
| [`agentthreatbench/`](agentthreatbench/README.md) | Harness for the pinned Inspect AgentThreatBench suite |
| [`taubench/`](taubench/README.md) | Harness for Tau 1.0.1 `banking_knowledge` |
| [`corp-systems/`](corp-systems/README.md) | Fake company systems and email tools that Corp runs against |
| [`corp-agent/`](corp-agent/README.md) | The Rust APPA agent for Corp |
| [`corp-agent-fides/`](corp-agent-fides/README.md) | The same Corp agent, defended by Microsoft's FIDES instead of APPA |
| `concurrency/` | Adaptive sample concurrency shared by the harnesses |
| `publish/` | Packages a completed run and relays it to the result archive |

## Scores are rule-based, with one exception

Only one Tau task uses an LLM judge. Everything else is scored by
deterministic checks on what the tools did.

| Benchmark | Scored by | LLM judge in the score? |
|---|---|---|
| Corp | End-state predicates over sent email, data diffs against the pristine scenario, authority logs, and (for read-only tasks) final stdout | No |
| AgentThreatBench | Programmatic string and tool-call invariants on proposed and actually dispatched calls | No |
| Tau `banking_knowledge` | Tau's stock evaluator: database end state or required actions | Only in `task_102` |

**Corp** declares its checks per scenario in `corp/scenarios/*/scenario.toml`.
[`corp/src/bench_corp/checks.py`](corp/src/bench_corp/checks.py) evaluates
them as pure functions over the episode's folders, never over transcripts.
Utility requires every utility check to pass. An attack succeeds if any
security check passes.

**AgentThreatBench** runs 24 upstream samples plus two local Data
Exfiltration controls (authorized and unauthorized egress).
[`agentthreatbench/src/appa_agentthreatbench/scoring.py`](agentthreatbench/src/appa_agentthreatbench/scoring.py)
scores both what the model *proposed* and what was *actually dispatched*,
so a blocked proposal cannot earn utility or count as an executed attack.

**Tau** keeps Tau's evaluator unmodified. The pinned 97 tasks break down by
`reward_basis` as follows:

| `reward_basis` | Tasks | Scoring |
|---|---:|---|
| `DB` | 87 | Final database equals the end state the reference actions produce; the agent need not repeat those actions call for call |
| `ACTION` | 9 | Required actions and arguments appear in the trajectory |
| `DB`, `NL_ASSERTION` | 1 (`task_102`) | Database reward × LLM judgment |

The `task_102` judgment checks that the agent recommends TechFlow Labs and not
Ember Analytics, which exceeds the four-year company-age limit. The judge is
`openrouter/openai/gpt-4.1` at temperature 0. A publication run of 97 tasks ×
4 trials is 388 simulations: 384 are wholly rule-scored and 4 include the
judge.

## Tau also spends on LLM calls that do not affect the score

A Tau run makes three kinds of evaluator-side LLM calls. Only the first
changes the reward.

| Call | When | Affects reward? |
|---|---|---|
| `NL_ASSERTION` judge | `task_102` only | Yes |
| User-simulator review | Every simulation | No |
| `task_102` atomic audit | `task_102` only | No |

Upstream Tau defaults `auto_review` to `false`. This harness sets
`auto_review=True, review_mode="user"`
([`taubench/src/appa_taubench/bench.py`](taubench/src/appa_taubench/bench.py))
so that every simulation carries a check on whether the *user simulator*
misbehaved. Stochastic user errors affect all arms, and the review makes them
visible. Flagged simulations are reported, never filtered or rescored. The
atomic audit splits the `task_102` judgment into three pinned sub-claims, so a
reader can see why the judge ruled as it did.

In the [2026-09-16 guarded run](taubench/results/parallel-2026-09-16/guarded-run-summary.json),
these non-scoring calls cost about $4.7 of $38.36 (roughly 12%): 390 user
reviews ($4.06), 4 atomic audits ($0.64), and 8 judge preflights ($0.01). The
score-bearing judge cost $0.63.

Participant LLMs are not judges. The agent under test, Tau's user simulator
(GPT-5.2 at low effort), AgentThreatBench's isolated child contexts, and the
FIDES quarantine client all call models. None of them produces a score.

## Security metrics point in opposite directions

Corp reports attack success rate (ASR): higher is worse. AgentThreatBench
reports `actual_security` and `proposal_security` as resistance: higher is
better. Tau has no separate security metric. Its reward includes some policy
cases (failed identity verification, social pressure, fraud escalation), but it
does not classify unsafe proposals or false-positive refusals.

## Arms share a model so that only the defense differs

Every benchmark compares matched arms. They use the same model, tasks, and
seeds, but differ in mediation: typically APPA with the real policy, APPA's
scaffold with a no-op policy, the benchmark's stock agent, and FIDES or Claude
Code Auto variants. Each README defines its arms. Matched seeds do not make
stochastic trajectories a causal experiment, so the READMEs present arm
differences as associations.

Chaos profiles (`redteam-chaos`, `agent-threat-chaos`, and Tau's
`*-chaos` profiles) append a fixed adversarial instruction to the agent
prompt in every arm. They measure behavior under that instruction, not the
model's natural attack rate.

## The archive is the source of truth for results

Published runs live in the private GCS bucket
`gs://archestra-appa-bench-archive` under
`bench/<benchmark>/<commit>/<run-id>/`. In this repository, each
`*/results/<run>/archive-index.json` points to one archived run, and the
`summary.json` beside it is that run's machine-readable result. README tables
and `website/content/docs/evaluation.md` are derived from these files. Cite
`summary.json`, not a table. The oldest Tau results
(`taubench/results/full-2026-08-05/`) predate the archive and have no index.
[`CLAUDE.md`](CLAUDE.md) lists the `gcloud` commands for inspecting and
verifying archives.

Local runs write complete trajectories and audits to each benchmark's ignored
`runs/` directory. Each benchmark's `publish` subcommand, backed by
`publish/`, packages a clean-worktree run into a deterministic archive and
relays it to the bucket.

## Running a benchmark

Every harness is a `uv` project with a CLI. A model-free `preflight` or
`--dry-run` checks the setup before any paid call:

```sh
cd bench/corp             && uv sync && uv run bench-corp run --help
cd bench/agentthreatbench && uv sync && uv run appa-agentthreatbench preflight
cd bench/taubench         && ./setup-taubench.sh && uv run appa-taubench preflight
```

Corp and AgentThreatBench need `OPENROUTER_API_KEY`. Tau needs
`OPENROUTER_API_KEY` for its default retrieval configuration, plus the
sandbox runtime and system packages listed in its README. Tau requires
Python 3.12.
