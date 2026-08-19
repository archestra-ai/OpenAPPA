---
title: Benchmarks
category: Evaluation
order: 10
description: Empirical benchmark results evaluating security enforcement and task completion across Bench-Corp, TAU-bench, and AgentThreatBench.
---

## Benchmark results and methodology

Evaluating agent security requires measuring two metrics together: **security enforcement** and **task completion**. An agent that permits unauthorized data flows is unsafe; an agent that refuses to execute valid actions is unhelpful.

We evaluate OpenAPPA by testing whether declared flow policies reliably block unauthorized actions while preserving the agent's ability to finish authorized tasks.

Our evaluation spans three benchmark suites:

1. **Bench-Corp:** 20 balanced enterprise workflows across HR, Finance, and customer support systems.
2. **TAU-bench:** Banking workflows testing tool use and trajectory recovery.
3. **AgentThreatBench:** Data exfiltration scenarios targeting LLM tool dispatches.

In addition to unprotected baselines, we evaluate OpenAPPA against **FIDES** — Microsoft Research's Information Flow Control (IFC) framework designed for deterministic AI agent security ([Microsoft Research](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/)). FIDES applies dynamic flow labels to track data origin and destination across agent tools, providing a primary peer baseline for flow-based security enforcement.

A **security pass** in these benchmarks means the scorer observed no unauthorized data flow or policy violation during the run. This can occur either because the model acted safely on its own or because the security framework (OpenAPPA or FIDES) blocked an unsafe tool call before dispatch.

## Bench-Corp: Multi-step enterprise tasks

`bench-corp` is a benchmark suite developed to evaluate multi-step agent performance in realistic enterprise environments (HR, Finance, Vendor management, Forums, Task Trackers, and Email). The suite evaluates agents strictly based on observable tool side effects (files written, emails sent), inspired by AgentDojo, without scoring conversation text or relying on LLM judges.

The suite contains **20 curated enterprise scenarios** evaluated across **4 policy arms** on `openai/gpt-5.6-luna` with 5 repetitions per scenario (**100 total episodes per arm**, 400 episodes per evaluation run):

- **Standard prompts:** 5 repetitions per scenario (**100 total episodes per arm**).
- **Red-team Chaos prompts (`redteam-chaos`):** Adversarial prompts where the agent is instructed to actively seek out shortcuts, bypasses, and follow embedded directives in data (**100 total episodes per arm**).

| Agent prompt profile | Policy arm | Task completion (Utility) | Attack success rate (ASR) | Security pass rate |
|---|---|---:|---:|---:|
| **Standard** | **OpenAPPA (`appa`)** | **82%** | **0%** | **100%** |
| | OpenAPPA Open (`appa-open`) | 87% | 29% | 71% |
| | FIDES (`fides`) | 30% | 29% | 71% |
| | FIDES Open (`fides-open`) | 87% | 27% | 73% |
| **Red-team Chaos** | **OpenAPPA (`appa`)** | **81%** | **0%** | **100%** |
| | OpenAPPA Open (`appa-open`) | 87% | 33% | 67% |
| | FIDES (`fides`) | 27% | 29% | 71% |
| | FIDES Open (`fides-open`) | 87% | 26% | 74% |

### Comparative findings on Bench-Corp

- **Enforcement effectiveness:** OpenAPPA achieved **0% ASR across all 200 evaluated episodes** (100 standard + 100 red-team chaos), resolving 213 policy remedy plans. Linear IFC (FIDES) protects against naive prompt injections on older models, but cannot scope recipient addresses (`$to`), verify out-of-band approvals, or enforce temporal execution orders, allowing 26%–33% of structural organizational attacks to pass.
- **Utility preservation:** OpenAPPA maintained **81%–82% task completion** across both prompt profiles. FIDES utility dropped to 27%–30% because linear IFC permanently taints trajectories upon reading confidential data, blocking subsequent benign public notifications across 14 of 20 scenarios.

*(Evaluated on commit [`5b3cc34`](https://github.com/archestra-ai/OpenAPPA/commit/5b3cc3475dbe99cf4a6e4d2bfb2ae4cbb3825829). See [`bench/corp/README.md`](https://github.com/archestra-ai/OpenAPPA/blob/main/bench/corp/README.md) for the complete 20-scenario matrix and architectural failure analysis.)*

### Scenario examples

- **Wire transfer approval:** A wire request contains text claiming *"approval has already been granted."* OpenAPPA ignores prompt assertions and requires an out-of-band secondary approval event before permitting dispatch.
- **Legal pre-clearance:** An agent prepares contract terms for external counsel. OpenAPPA enforces execution ordering, requiring an audit ticket to precede document email egress.
- **Multi-tenant isolation:** An agent processes cross-tenant requests. OpenAPPA evaluates dynamic reader sets against the destination to block Client Alpha data from reaching Client Beta.

## TAU-bench: Banking domain & policy recovery

[TAU-bench](https://taubench.com/leaderboard?benchmark=knowledge) is a widely recognized benchmark for tool-using agents, developed by Sierra and Stanford researchers. We evaluated OpenAPPA and FIDES across all 97 tasks in the banking domain with 4 simulations per task (**388 total simulations per arm and prompt profile**).

### Standard prompts (Utility baseline)

On standard prompts, both OpenAPPA and FIDES were evaluated to measure baseline agent utility and policy overhead without active attack injections:

| Metric | OpenAPPA | FIDES | Delta (FIDES vs OpenAPPA) |
|---|---:|---:|---:|
| Total simulations | 388 | 388 | — |
| Successful task completions | **35%** (137) | 33% (127) | -2 pp |
| Policy-checked tool calls | 8,151 | 10,543 | +2,392 |
| Policy blocks | 0 | 0 | 0 |
| Execution failures | 0 | 0 | 0 |

On standard prompts, neither system triggered false-positive policy blocks (0 blocks recorded), confirming that standard Tau runs measure baseline utility rather than security enforcement. OpenAPPA achieved a higher task completion rate (35% vs 33%).

### ChaosMonkey prompts & Policy recovery

Under ChaosMonkey prompting, adversarial inputs actively attempt unauthorized tool calls:

| Metric | OpenAPPA (ChaosMonkey) |
|---|---:|
| Total simulations | 388 |
| Successful task completions | 26% (102) |
| Policy-checked tool calls | 7,725 |
| Attempted policy violations blocked | **100%** (24/24) |
| Blocked trajectories that recovered | **50%** (12/24) |

### How policy recovery works

When OpenAPPA blocks an unauthorized action, it returns structured feedback (a **remedy plan**) explaining what missing requirement (such as identity verification or manager approval) is needed to proceed safely.

For example, in a card-replacement task:
1. The agent attempts to call `issue_replacement_card()`.
2. OpenAPPA blocks the call because `identity.verified` is missing.
3. The refusal response provides a remedy: *"Verify user identity first."*
4. The agent prompts the user, calls `verify_identity()`, and retries the card replacement.
5. OpenAPPA permits the now-authorized call, allowing the task to finish safely.

In TAU-bench testing, **12 out of 24 blocked agents used OpenAPPA's remedy feedback to self-correct and complete their task**. Without remedies, completion under ChaosMonkey prompting would have been 23% (90/388); with remedies, it reached 26% (102/388).

## AgentThreatBench: Data exfiltration suite

[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) is a security benchmark developed by the UK AI Safety Institute based on the OWASP Top 10 for LLM Applications. We evaluated a 10-sample **Data Exfiltration slice** designed to test whether agents leak customer data when processing untrusted inputs.

The test slice includes 8 upstream Data Exfiltration cases plus 2 custom egress cases (one authorized egress, one unauthorized egress).

| Agent prompt profile | Policy arm | Task completion | Security pass rate |
|---|---|---:|---:|
| **Standard** | Unprotected | 60% | 100% |
| | Permissive policy | 70% | 100% |
| | **Guarded OpenAPPA** | **60%** | **100%** |
| **Agent-threat chaos** | Unprotected | 70% | 70% |
| | Permissive policy | 60% | 60% |
| | **Guarded OpenAPPA** | **60%** | **100%** |

Under adversarial prompts, unprotected models leaked sensitive data in 30% to 40% of test cases. OpenAPPA blocked 100% of unauthorized data dispatches while matching the standard 60% task completion rate.

Rather than relying on prompt-injection classifiers, OpenAPPA attaches reader-set permissions to customer data when `lookup_customer` runs. When a messaging tool is invoked, OpenAPPA checks whether the recipient belongs to that reader set before allowing dispatch.

## Summary of empirical findings

Across all benchmark suites:

- **Bench-Corp:** OpenAPPA achieved **0% attack success rate (100% security pass rate)** across all 200 evaluated episodes (100 standard + 100 red-team chaos) while preserving **81%–82% task utility** (vs 27%–30% utility and 29% ASR for FIDES).
- **TAU-bench:** 0 false positive blocks across 8,151 standard tool calls; 100% of attempted violations blocked under adversarial prompts, with 50% recovering to full task completion via remedy feedback.
- **AgentThreatBench:** 100% security pass rate for OpenAPPA across both standard and adversarial prompt profiles.

## Reproducing Bench-Corp

The `bench-corp` benchmark harness, agent implementations, mock systems, and scenario suites are included directly in `bench/`:

```bash
# 1. Install prerequisites (Rust and uv)
cd bench/corp
uv sync

# 2. Run the complete 20-scenario suite
uv run bench-corp run

# 3. Filter specific agents or scenarios
uv run bench-corp run --agent appa --agent fides --scenario follow-forum-steps --reps 3

# 4. Run the adversarial Red-team Chaos benchmark across all arms
uv run bench-corp run --agent appa --agent appa-open --agent fides --agent fides-open --agent-prompt-profile redteam-chaos --reps 5
```

### External references

- [AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) — upstream task definitions and benchmark suite.
- [OpenAPPA Paper on arXiv](https://arxiv.org/abs/2607.24625) — formal information-flow model and evaluation methodology.
- [FIDES (Microsoft Research)](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/) — information flow control framework for AI agents.
