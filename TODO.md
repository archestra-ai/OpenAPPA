# The full Tau Knowledge run is gated on these fixes

The 2026-08-04 diagnostic ran one trial on ten purposefully selected tasks with
`openrouter/z-ai/glm-5`, the GPT-4.1-mini user simulator, `alltools-qwen`, and
the committed OpenAPPA contract. One task passed, all simulations terminated
normally, and the audits recorded 55 policy blocks across 241 agent
completions. The result exposed policy, harness, evaluator, model, and user
simulation problems that must be separated before buying the 388-simulation
run.

## The contract must support each intended authenticated flow

- [ ] Inventory all 97 tasks as ordered logical flows and identify every
  customer/account read followed by verification, mutation, user-tool grant,
  or transfer. Classify each effect as intentionally refused or supported by
  the contract before changing policy to improve benchmark reward.
- [ ] Resolve the verification contradiction: customer-record readers lower
  the trajectory's trust to `suspicious`, while the required
  `log_verification` mutation requires `internal`. Implement a spec-grounded
  Authority, Transformer, or remedy plan that records successful verification
  without permitting arbitrary effects derived from suspicious content.
- [ ] Decide and encode the authorized path, if any, for granting a
  discoverable user tool after customer data has been admitted. Cover the cash
  back dispute and check-deposit flows exercised by tasks 026 and 055.
- [ ] Decide and encode the authorized path, if any, for internal human
  transfer after customer data has been admitted. Cover both ordinary transfer
  and Tau's task-specific speedbump transfer tools.
- [ ] Decide and encode which customer-service mutations may follow admitted
  customer data, including disputes, credits, account opening and closing,
  card replacement, credit-limit changes, and transfers between accounts.
  Keep unsupported flows explicitly refused rather than weakening the trust
  requirement globally.
- [ ] Add deterministic policy tests for every supported and intentionally
  refused read-to-effect class. Include the exact logical calls from tasks 004,
  010, 026, 046, 050, 055, 072, and 102.
- [ ] Document the expected utility consequence of every intentional refusal
  in the custom benchmark methodology before interpreting aggregate reward.

## The harness must stop wasting calls and preserve an auditable trajectory

- [ ] Thread Tau's simulation ID, trial, and seed into `EpisodeAudit`; an audit
  currently has an unrelated UUID and can only be joined by task ID, which is
  ambiguous with four trials.
- [ ] Make audit and summary counters use one unit. `sequential_blocks` counts
  refused calls while one `sequential_block` event represents a refused model
  completion.
- [ ] Stop automatic hidden retries when a block has no remedy and the
  trajectory cannot recover the required trust. Do not let the model vary
  arguments or wrappers for the same permanently refused effect.
- [ ] Reject stale remedy IDs with model-visible feedback that identifies the
  current pending call, then test the behavior observed in tasks 026 and 050.
- [ ] Require every success claim to follow a successful Tau tool result. Add
  regression coverage for task 055, where the model claimed that blocked
  account openings had succeeded.
- [ ] Persist the NL-assertion judge model, actual model provider routes,
  retrieval-index digest, and every run-time override in `run-config.json` so
  resume and review use the same effective configuration.
- [ ] Account separately for agent calls, hidden policy retries, user calls,
  embedding construction and queries, NL judging, and review calls. The
  diagnostic's reported $0.4956 covered only recorded agent and user
  conversation inference.
- [ ] Estimate the full run's token cost, wall-clock duration, rate-limit
  exposure, audit size, and retry overhead from a completed pilot before
  starting 388 simulations.

## Tau's scored result must be valid and reproducible

- [ ] Configure the NL-assertion judge through the harness and preflight its
  credential. The pinned evaluator defaults to a direct OpenAI model, while
  the current `alltools-qwen` preflight neither requires that key nor records a
  judge override.
- [ ] Fix the NL-assertion evaluator to require exactly one result for every
  requested assertion, match results to the original assertions, and fail on
  missing, duplicate, malformed, or contradictory judgments. `all([])` must
  not award a passing score.
- [ ] Add a regression case for task 102. Its judge said the agent did not
  establish Ember's disqualifying age and nevertheless returned
  `metExpectation = true`.
- [ ] Retain the judge request, structured response, provider, token usage,
  cost, and justification beside each NL assertion so a disputed grade can be
  audited.
- [ ] Make preflight replay every golden action for every base task and fail on
  any exception. The pinned environment evaluator currently logs a failed
  golden action and continues scoring against the resulting prefix state.
- [ ] Label action checks as unscored reference-trajectory diagnostics when
  `ACTION` is absent from `reward_basis`; do not present their match ratio as
  partial DB correctness.
- [ ] Decide whether evaluator fixes can land upstream without changing the
  accepted Tau revision. If the pin changes, update code, data, manifest
  identity, tests, and submission disclosure together.

## The agent and user simulator must produce trustworthy trials

- [ ] Choose the final agent model and exact provider configuration after the
  policy ablation. GLM-5 is a flagship model rather than a lightweight GLM;
  use GLM-4.7 Flash if the goal is specifically to stress a weaker actor.
- [ ] Improve or constrain Knowledge retrieval before the full run. The
  diagnostic repeatedly selected business documents for personal products,
  issued redundant BM25 searches, and consumed 6.88 million agent prompt
  tokens across ten tasks despite having dense search and shell available.
- [ ] Verify that the agent follows retrieved procedures rather than merely
  finding them. Add focused live checks for transfer speedbumps, dispute
  prerequisites, net fee credits, referral eligibility, and account-opening
  order.
- [ ] Make permanent policy feedback actionable: follow an offered remedy once,
  avoid stale remedies, and explain a terminal refusal without calling the
  same effect repeatedly.
- [ ] Use an explicit, sufficiently capable user-simulator model for the full
  run and validate its tool behavior. GPT-4.1-mini submitted extra referrals,
  impersonated the agent, invented tools, and treated tool errors as success in
  tasks 010 and 055.
- [ ] Add text-mode user fidelity review or another deterministic admission
  check. The current hallucination retry path only protects full-duplex runs.
- [ ] Define how trials with material user-simulator deviations are reported
  without silently discarding or relabeling them as model failures.

## A paired pilot must separate model weakness from policy utility cost

- [ ] Run tasks 001, 004, and 102 with four matched trials under the current
  contract and a permissive OpenAPPA control contract, using identical task
  seeds, warmed retrieval cache, agent model, user model, and limits.
- [ ] Manually review the 24 pilot trajectories and their audits. Treat task
  001 as the allowed-path control, task 004 as the clean policy-sensitive flow,
  and task 102 as the model/retrieval-sensitive flow.
- [ ] Add a stock Tau arm only after the two OpenAPPA arms are stable; otherwise
  scaffold and policy differences remain confounded.
- [ ] Pin or record the actual OpenRouter provider deployment for every trial.
  Temperature zero and a run-level seed do not make a routed hosted model
  deterministic.
- [ ] Set quantitative go/no-go thresholds for infrastructure errors, user
  deviations, unexplained evaluator disagreements, retry amplification, cost,
  and within-task variance.

## The submission run must start only after every gate passes

- [ ] Run the complete no-cost preflight with the exact final models,
  retrieval configuration, evaluator configuration, policy, Tau revision, and
  platform executables.
- [ ] Exercise checkpoint interruption and exact-config resume on the final
  configuration without mixing audits or trials.
- [ ] Confirm that every task has four valid trials, no infrastructure-error
  simulation, directly correlated Tau and OpenAPPA artifacts, and a complete
  cost record.
- [ ] Prepare the custom-scaffold disclosure before launch, including policy
  refusals, hidden retries, prompt changes, model/provider routing, evaluator
  changes, and the limits of Tau Knowledge as a utility benchmark.
- [ ] Keep security claims out of the Tau result: the Knowledge split has no
  indirect prompt-injection attack set, so security evidence requires a
  separate attack evaluation.
