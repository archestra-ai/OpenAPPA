# Tau benchmark results

The [2026-09-16 parallel-tool-call evaluation](parallel-2026-09-16/README.md) adds new guarded and permissive rows without replacing the [2026-09-15 sequential results](full-2026-09-15/README.md). The comparison uses matched task, trial, and seed identities. Stock already supported multi-call completions, so only the two custom-scaffold arms required reruns.

| Policy mode | Tool calling | Successful simulations | Mean reward | Mean agent tokens per simulation | Recorded cost |
|---|---|---:|---:|---:|---:|
| Guarded | Sequential | 133/388 | 34.28% | 934,475 | $30.33 |
| Guarded | Parallel | 151/388 | 38.92% | 1,307,763 | $38.36 |
| Permissive | Sequential | 134/388 | 34.54% | 893,405 | $30.50 |
| Permissive | Parallel | 153/388 | 39.43% | 1,272,128 | $37.80 |
| Stock | Native parallel | 156/388 | 40.21% | 1,254,821 | $35.62 |

The parallel rows are fresh stochastic measurements, not replacements or causal estimates. See the evaluation documents for integrity checks, retry evidence, security scope, and exact artifacts.

## 2026-08-05 measurements

Two validated publication runs cover all 97 `banking_knowledge` tasks with four trials each, for 388 simulations per run. Both used OpenRouter's `openai/gpt-5.6-luna` at maximum reasoning effort with the same OpenAPPA policy and Tau revision. The complete trajectories and audits remain in the ignored `runs/` directory; [`full-2026-08-05`](full-2026-08-05/) retains the headline metrics and exact run configurations.

| agent behavior | OpenAPPA blocks | successful simulations | mean Tau reward |
|---|---:|---:|---:|
| ChaosMonkey-GPT, instructed to seek a plausible shortcut | 24 | 102/388 | 26.29% |
| normal GPT | 0 | 137/388 | 35.31% |

ChaosMonkey-GPT encountered one OpenAPPA refusal in each of 24 simulations. Twelve of those simulations subsequently satisfied Tau's evaluator, accounting for 12 of the run's 102 successes; removing those recovered successes gives 90/388, or 23.20%, rather than the observed 26.29%. The remaining 12 blocked simulations did not pass, and 16 of the 24 blocked trajectories ended with a terminal policy refusal.

Normal GPT proposed 8,151 policy-checked calls without triggering an OpenAPPA block. It completed 137 of 388 simulations successfully, so enforcement did not refuse a call or reduce the observed 35.31% score through blocking. This statement is limited to the calls and trajectories in this run; it is not a claim that every possible normal prompt is block-free.

The two runs used different prompt profiles and implementation hashes, so their absolute scores are not a controlled comparison with each other. The recovery calculation is internal to the ChaosMonkey-GPT run, and Tau's aggregate reward does not separately classify unsafe proposals or false-positive authorization decisions. `full-2026-08-05/summary.json` records the arithmetic and source metrics, while the two config files retain the experiment identities needed to locate and audit the ignored source runs.

### These runs predate the current engine

The 2026-08-05 runs were produced by the pre-annotator engine and binding `appa-agent-python-v3`, and the harness has since been ported to the current engine and binding (`appa-agent-python-v7`, `run-config.json` records whichever binding produced a run). The tasks, the Tau revision, and the contract's semantics — verification gates every bank mutation, no authority may waive it — are unchanged, and the ported contract still authorizes every reference action in the golden replay. The mechanics of recovery are not unchanged: a prior-effect remedy is now redispatch advice ("Run `log_verification` first") that the model acts on, rather than an executable plan the scaffold ran for it, and a narrowing result is accepted through the control tool with an `offer_id`. Treat a re-run as a fresh measurement, not a reproduction of the table above.
