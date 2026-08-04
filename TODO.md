# The full Tau Knowledge run has seven blockers

The 2026-08-04 diagnostic showed that the current result mixes policy utility
cost, model errors, user-simulator errors, and evaluator errors. Do not buy the
388-simulation run until these blockers are cleared.

- [ ] **Resolve the contract/task mismatch.** Inventory the 97 tasks for
  customer-data reads followed by effects, then define and test the
  spec-grounded path for successful verification and each intentionally
  supported mutation, user-tool grant, or human transfer. The current contract
  makes Tau's normal read-then-`log_verification` sequence impossible.

- [ ] **Make terminal policy blocks terminal and auditable.** Stop hidden
  retries when no remedy can recover the required trust, reject stale remedy
  IDs clearly, and prevent success claims without a successful Tau result.
  Correlate every audit directly with Tau's simulation ID, task, trial, and
  seed.

- [ ] **Make Tau's score trustworthy.** Configure and preflight the
  NL-assertion judge, require one valid judgment per assertion, add the task 102
  regression, and fail preflight when any golden action cannot be replayed.
  Record the judge request, response, model, provider, and cost.

- [ ] **Stabilize the agent, retrieval, and user simulator.** Select exact
  agent and user models, validate that retrieval produces the required
  procedures without the diagnostic's search explosion, and reject material
  user deviations such as invented tools, extra mutations, role impersonation,
  or treating tool errors as success.

- [ ] **Run the paired pilot.** Run tasks 001, 004, and 102 for four matched
  trials under the current contract and a permissive OpenAPPA control contract.
  Review all 24 trajectories and proceed only if the pilot separates
  policy-caused failures from model/retrieval failures with acceptable
  variance.

- [ ] **Freeze and budget the final configuration.** Manifest the exact model
  and provider routes, user model, judge, Tau revision, policy, retrieval-index
  digest, task/trial seeds, limits, and every run-time override. Account for
  agent, hidden retry, user, embedding, judge, and review costs, then estimate
  the full run's spend and duration.

- [ ] **Prove the submission workflow before launch.** Pass preflight with the
  frozen configuration, exercise checkpoint interruption and exact resume,
  verify four directly correlated trials per task with no infrastructure
  errors, and prepare the custom-scaffold disclosure. Describe Tau Knowledge
  as a utility evaluation; security claims require a separate attack set.
