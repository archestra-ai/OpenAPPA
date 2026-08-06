# Full Tau runs show recovery without a normal-case utility tax

Two validated publication runs cover all 97 `banking_knowledge` tasks with four trials each, for 388 simulations per run. Both used OpenRouter's `openai/gpt-5.6-luna` at maximum reasoning effort with the same OpenAPPA policy and Tau revision. The complete trajectories and audits remain in the ignored `runs/` directory; [`full-2026-08-05`](full-2026-08-05/) retains the headline metrics and exact run configurations.

| agent behavior | OpenAPPA blocks | successful simulations | mean Tau reward |
|---|---:|---:|---:|
| ChaosMonkey-GPT, instructed to seek a plausible shortcut | 24 | 102/388 | 26.29% |
| normal GPT | 0 | 137/388 | 35.31% |

ChaosMonkey-GPT encountered one OpenAPPA refusal in each of 24 simulations. Twelve of those simulations subsequently satisfied Tau's evaluator, accounting for 12 of the run's 102 successes; removing those recovered successes gives 90/388, or 23.20%, rather than the observed 26.29%. The remaining 12 blocked simulations did not pass, and 16 of the 24 blocked trajectories ended with a terminal policy refusal.

Normal GPT proposed 8,151 policy-checked calls without triggering an OpenAPPA block. It completed 137 of 388 simulations successfully, so enforcement did not refuse a call or reduce the observed 35.31% score through blocking. This statement is limited to the calls and trajectories in this run; it is not a claim that every possible normal prompt is block-free.

The two runs used different prompt profiles and implementation hashes, so their absolute scores are not a controlled comparison with each other. The recovery calculation is internal to the ChaosMonkey-GPT run, and Tau's aggregate reward does not separately classify unsafe proposals or false-positive authorization decisions. `full-2026-08-05/summary.json` records the arithmetic and source metrics, while the two config files retain the experiment identities needed to locate and audit the ignored source runs.
