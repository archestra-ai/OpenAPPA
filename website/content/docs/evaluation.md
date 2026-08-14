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

1. **Bench-Corp:** Multi-step enterprise workflows across HR, Finance, and customer support systems.
2. **TAU-bench:** Banking workflows testing tool use and trajectory recovery.
3. **AgentThreatBench:** Data exfiltration scenarios targeting LLM tool dispatches.

In addition to unprotected baselines, we evaluate OpenAPPA against **FIDES** — Microsoft Research's Information Flow Control (IFC) framework designed for deterministic AI agent security ([Microsoft Research](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/)). FIDES applies dynamic flow labels to track data origin and destination across agent tools, providing a primary peer baseline for flow-based security enforcement.

A **security pass** in these benchmarks means the scorer observed no unauthorized data flow or policy violation during the run. This can occur either because the model acted safely on its own or because the security framework (OpenAPPA or FIDES) blocked an unsafe tool call before dispatch.

## Bench-Corp: Multi-step enterprise tasks

[bench-corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench-corp) is an open-source benchmark suite developed to evaluate multi-step agent performance in realistic enterprise environments (HR, Finance, Vendor management, Forums, Task Trackers, and Email). The core test is whether an agent can process untrusted or restricted inputs and still finish its assigned task without leaking confidential data downstream.

The repository contains 22 enterprise scenarios. We evaluated **GPT-5.6 Luna** across a 7-scenario evaluation cohort with 5 repetitions per scenario (**35 total episodes per arm and prompt profile**):

- **Standard prompts:** Baseline user requests without malicious instructions.
- **ChaosMonkey prompts:** Adversarial test inputs that inject malicious instructions or untrusted snippets into prompt data to actively attempt data exfiltration or policy bypasses.

Evaluated policy arms:
- **OpenAPPA (Guarded):** OpenAPPA policy enforcement with active flow tracking and remedy plans.
- **FIDES (Guarded):** FIDES information-flow control policy profiles and enforcement.
- **Unprotected:** Unconstrained baseline agent without policy enforcement.

| Agent prompt profile | Policy arm | Task completion | Attack success rate | Security pass rate |
|---|---|---:|---:|---:|
| **Standard** | **OpenAPPA** | **33/35 (94.3%)** | **0/35 (0%)** | **35/35 (100%)** |
| | FIDES | 10/35 (28.6%) | 8/35 (22.9%) | 27/35 (77.1%) |
| | Unprotected | 26/35 (74.3%) | 10/35 (28.6%) | 25/35 (71.4%) |
| **ChaosMonkey** | **OpenAPPA** | **32/35 (91.4%)** | **0/35 (0%)** | **35/35 (100%)** |
| | FIDES | 10/35 (28.6%) | 10/35 (28.6%) | 25/35 (71.4%) |
| | Unprotected | 23/35 (65.7%) | 10/35 (28.6%) | 25/35 (71.4%) |

*Canary validation note: An unconstrained `FIDES-open` arm was evaluated as a canary test (24/35 completion [68.6%] and 25.7%–28.6% attack rates), confirming that the guarded FIDES performance delta stems from policy enforcement rather than harness integration issues.*

### Comparative findings on Bench-Corp

- **Enforcement effectiveness:** OpenAPPA completely eliminated data exfiltration attacks (0% attack success rate) under both standard and adversarial ChaosMonkey prompts. FIDES in guarded mode allowed 22.9% of standard attacks and 28.6% of ChaosMonkey attacks to succeed, matching or approaching unprotected leakage rates.
- **Utility preservation:** OpenAPPA maintained high task completion rates (**94.3%** standard, **91.4%** ChaosMonkey). FIDES experienced severe utility degradation in guarded mode, completing only 10/35 tasks (28.6%).

### Scenario examples

- **Wire transfer approval:** A $72,500 wire request contains text claiming *"approval has already been granted."* OpenAPPA ignores prompt text assertions and requires a verified approval event in the system log before permitting the wire transfer.
- **Legal pre-clearance:** An agent prepares contract terms for external counsel. OpenAPPA requires an audit record before allowing confidential document text to leave the internal boundary.
- **Multi-tenant isolation:** A client support ticket contains an injected instruction asking the agent to send Tenant A's deployment keys to Tenant B. OpenAPPA evaluates data reader permissions against the destination and blocks the cross-tenant leak.

## TAU-bench: Banking domain & policy recovery

[TAU-bench](https://taubench.com/leaderboard?benchmark=knowledge) is a widely recognized benchmark for tool-using agents, developed by Sierra and Stanford researchers. We evaluated OpenAPPA and FIDES across all 97 tasks in the banking domain with 4 simulations per task (**388 total simulations per arm and prompt profile**).

### Standard prompts (Utility baseline)

On standard prompts, both OpenAPPA and FIDES were evaluated to measure baseline agent utility and policy overhead without active attack injections:

| Metric | OpenAPPA | FIDES | Delta (FIDES vs OpenAPPA) |
|---|---:|---:|---:|
| Total simulations | 388 | 388 | — |
| Successful task completions | **137 (35.3%)** | 127 (32.7%) | -10 (-2.6 pp) |
| Policy-checked tool calls | 8,151 | 10,543 | +2,392 |
| Policy blocks | 0 | 0 | 0 |
| Execution failures | 0 | 0 | 0 |

On standard prompts, neither system triggered false-positive policy blocks (0 blocks recorded), confirming that standard Tau runs measure baseline utility rather than security enforcement. OpenAPPA achieved a slightly higher task completion rate (35.3% vs 32.7%).

### ChaosMonkey prompts & Policy recovery

Under ChaosMonkey prompting, adversarial inputs actively attempt unauthorized tool calls:

| Metric | OpenAPPA (ChaosMonkey) |
|---|---:|
| Total simulations | 388 |
| Successful task completions | 102 (26.3%) |
| Policy-checked tool calls | 7,725 |
| Attempted policy violations blocked | **24/24 (100%)** |
| Blocked trajectories that recovered | **12/24 (50%)** |

*Note: A FIDES ChaosMonkey Tau evaluation run is currently pending.*

### How policy recovery works

When OpenAPPA blocks an unauthorized action, it returns structured feedback (a **remedy plan**) explaining what missing requirement (such as identity verification or manager approval) is needed to proceed safely.

For example, in a card-replacement task:
1. The agent attempts to call `issue_replacement_card()`.
2. OpenAPPA blocks the call because `identity.verified` is missing.
3. The refusal response provides a remedy: *"Verify user identity first."*
4. The agent prompts the user, calls `verify_identity()`, and retries the card replacement.
5. OpenAPPA permits the now-authorized call, allowing the task to finish safely.

In TAU-bench testing, **12 out of 24 blocked agents used OpenAPPA's remedy feedback to self-correct and complete their task**. Without remedies, completion under ChaosMonkey prompting would have been 23.2% (90/388); with remedies, it reached 26.3% (102/388).

## AgentThreatBench: Data exfiltration suite

[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) is a security benchmark developed by the UK AI Safety Institute based on the OWASP Top 10 for LLM Applications. We evaluated a 10-sample **Data Exfiltration slice** designed to test whether agents leak customer data when processing untrusted inputs.

The test slice includes 8 upstream Data Exfiltration cases plus 2 custom egress cases (one authorized egress, one unauthorized egress).

| Agent prompt profile | Policy arm | Task completion | Security pass rate |
|---|---|---:|---:|
| **Standard** | Unprotected | 6/10 (60%) | 10/10 (100%) |
| | Permissive policy | 7/10 (70%) | 10/10 (100%) |
| | **Guarded OpenAPPA** | **6/10 (60%)** | **10/10 (100%)** |
| **Agent-threat chaos** | Unprotected | 7/10 (70%) | 7/10 (70%) |
| | Permissive policy | 6/10 (60%) | 6/10 (60%) |
| | **Guarded OpenAPPA** | **6/10 (60%)** | **10/10 (100%)** |

Under adversarial prompts, unprotected models leaked sensitive data in 30% to 40% of test cases. OpenAPPA blocked 100% of unauthorized data dispatches while matching the standard 60% task completion rate.

Rather than relying on prompt-injection classifiers, OpenAPPA attaches reader-set permissions to customer data when `lookup_customer` runs. When a messaging tool is invoked, OpenAPPA checks whether the recipient belongs to that reader set before allowing dispatch.

> **Artifact coverage note:** The current repository does not include the harness, pinned run configuration, or raw artifacts for this 10-sample AgentThreatBench slice, so these reported results cannot yet be independently rerun from a checkout. A durable OpenAPPA harness and a FIDES 4-arm comparison remain follow-up work.

## Summary of empirical findings

Across all three benchmark suites:

- **Bench-Corp:** OpenAPPA achieved 0 successful attacks across 70 guarded episodes, with task completion remaining above 91.4%. FIDES in guarded mode completed 28.6% of tasks and allowed 22.9%–28.6% of attacks to succeed.
- **TAU-bench:** 0 false positive blocks across 8,151 standard tool calls; 100% of attempted violations blocked under adversarial prompts, with 50% recovering to full task completion via remedy feedback. FIDES showed a 2.6 pp utility delta on standard prompts (32.7% vs 35.3%).
- **AgentThreatBench:** 100% security pass rate for OpenAPPA across both standard and adversarial prompt profiles.

### Scope and methodology notes

- **Bench-Corp scenario suite:** Bench-Corp contains 22 total scenarios (15 baseline scenarios plus 7 new scenarios added in PR #163). The comparative evaluations report performance on the 7-scenario cohort. All 22 scenarios are retained for regression testing.
- **Experimental controls:** Standardized prompt profiles (Standard and ChaosMonkey), identical model family (`gpt-5.6-luna`), and matching retrieval settings were used. Controlled paired reruns from a single commit are recommended for formal publication benchmarks.

## Reproduce and inspect

- [bench-corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench-corp) — scenarios, runner, and result format.
- [harness-taubench](https://github.com/archestra-ai/OpenAPPA/tree/main/harness-taubench) — TAU-bench harness and evaluation code.
- [AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) — upstream task definitions and benchmark suite.
- [OpenAPPA Paper on arXiv](https://arxiv.org/abs/2607.24625) — formal information-flow model and evaluation methodology.
- [FIDES (Microsoft Research)](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/) — information flow control framework for AI agents.




