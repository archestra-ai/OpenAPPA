# The paid Tau pilot has four remaining gates

The contract/task inventory, terminal block behavior, direct audit correlation,
strict evaluator path, standard user model, stock control, and related-work
comparison are implemented and covered by preflight or a live task-102 smoke.
Do not launch the 388-simulation run until the remaining gates are cleared.

- [ ] **Run and scrutinize the matched ten-task pilot.** Run tasks 001, 004,
  010, 026, 032, 046, 050, 055, 072, and 102 once under guarded OpenAPPA,
  the scaffold-matched permissive contract, and stock Tau. Review all 30
  trajectories, evaluator calls, user reviews, APPA sidecars, retrieval calls,
  terminal states, and costs. Explain every reward difference rather than
  attributing any matched stochastic difference to policy by default.

- [ ] **Stabilize anything the pilot exposes.** Reject benchmark
  implementation defects, infrastructure errors, malformed evaluator evidence,
  uncorrelated audits, excessive or repeated retrieval loops, runaway context
  growth, and unreported hidden completions. Keep user-simulator deviations such
  as invented tools, extra mutations, role impersonation, premature actions, or
  treating tool errors as success in the scheduled result, but identify them
  separately from agent and policy failures.

- [ ] **Freeze and budget the final configuration.** Confirm the requested and
  provider-resolved agent, user, judge, and reviewer models; retrieval identity;
  all 388 task/trial seeds and limits; and every runtime override. Use the pilot
  to estimate participant, hidden replanning, embedding, evaluator, total
  dollar cost, model-call count, and duration for the full run.

- [ ] **Prove exact resume and submission validation.** Exercise an interrupted
  checkpoint and exact resume without duplicate scored simulations or missing
  audits. Verify four directly correlated trials per task and no infrastructure
  errors, then prepare the custom-scaffold disclosure. Describe Tau Knowledge
  as a utility evaluation; information-flow security claims require a separate
  attack set and security oracle.
