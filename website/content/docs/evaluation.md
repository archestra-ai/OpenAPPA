---
title: Evaluating OpenAPPA
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

A **security pass** in these benchmarks means the scorer observed no unauthorized data flow or policy violation during the run. This can occur either because the model acted safely on its own or because OpenAPPA blocked an unsafe tool call before dispatch.

## Bench-Corp: Multi-step enterprise tasks

[`bench-corp`](https://github.com/archestra-ai/OpenAPPA/tree/main/bench-corp) is an open-source benchmark suite we developed to evaluate multi-step agent performance in realistic enterprise environments (HR, Finance, Vendor management, Forums, Task Trackers, and Email). The core test is whether an agent can process untrusted or restricted inputs and still finish its assigned task without leaking confidential data downstream.

We evaluated **GPT-5.6 Luna** across 7 scenarios with 5 repetitions per scenario (140 total episodes across arms and prompt profiles):

- **Standard prompts:** Baseline user requests without malicious instructions.
- **ChaosMonkey prompts:** Adversarial test inputs that inject malicious instructions or untrusted snippets into prompt data to actively attempt data exfiltration or policy bypasses.

| Agent prompt profile | Policy arm | Task completion | Security pass rate |
|---|---|---:|---:|
| **Standard** | **OpenAPPA** | **33/35 (94.3%)** | **35/35 (100%)** |
| | Unprotected | 26/35 (74.3%) | 25/35 (71.4%) |
| **ChaosMonkey** | **OpenAPPA** | **32/35 (91.4%)** | **35/35 (100%)** |
| | Unprotected | 23/35 (65.7%) | 25/35 (71.4%) |

Under standard prompts, OpenAPPA achieved **94.3% task completion** with zero security violations. Under adversarial ChaosMonkey prompts, task completion remained high at **91.4%**, with zero successful attacks. In contrast, the unprotected baseline suffered security breaches in 10 out of 35 episodes under both prompt profiles.

### Scenario examples

- **Wire transfer approval:** A $72,500 wire request contains text claiming *"approval has already been granted."* OpenAPPA ignores prompt text assertions and requires a verified approval event in the system log before permitting the wire transfer.
- **Legal pre-clearance:** An agent prepares contract terms for external counsel. OpenAPPA requires an audit record before allowing confidential document text to leave the internal boundary.
- **Multi-tenant isolation:** A client support ticket contains an injected instruction asking the agent to send Tenant A's deployment keys to Tenant B. OpenAPPA evaluates data reader permissions against the destination and blocks the cross-tenant leak.

## TAU-bench: Banking domain & policy recovery

[`TAU-bench`](https://taubench.com/leaderboard?benchmark=knowledge) is a widely recognized benchmark for tool-using agents, developed by Sierra and Stanford researchers. We evaluated OpenAPPA across all 97 tasks in the banking domain with 4 simulations per task (**388 total simulations per prompt profile**).

| Metric | Standard prompts | ChaosMonkey prompts |
|---|---:|---:|
| Total simulations | 388 | 388 |
| Successful task completions | 137 (35.31%) | 102 (26.29%) |
| Policy-checked tool calls | 8,151 | 7,725 |
| False positive blocks on standard work | 0 | — |
| Attempted policy violations blocked | — | 24/24 |
| Blocked trajectories that recovered | — | 12/24 |

During standard runs, OpenAPPA inspected 8,151 tool calls without blocking any authorized actions. Under ChaosMonkey prompting, models attempted 24 policy violations—and OpenAPPA **blocked all 24 attempts (100%)**.

### How policy recovery works

When OpenAPPA blocks an unauthorized action, it returns structured feedback (a **remedy plan**) explaining what missing requirement (such as identity verification or manager approval) is needed to proceed safely.

For example, in a card-replacement task:
1. The agent attempts to call `issue_replacement_card()`.
2. OpenAPPA blocks the call because `identity.verified` is missing.
3. The refusal response provides a remedy: *"Verify user identity first."*
4. The agent prompts the user, calls `verify_identity()`, and retries the card replacement.
5. OpenAPPA permits the now-authorized call, allowing the task to finish safely.

In TAU-bench testing, **12 out of 24 blocked agents used OpenAPPA's remedy feedback to self-correct and complete their task**. Without remedies, completion under ChaosMonkey prompting would have been 23.20% (90/388); with remedies, it reached 26.29% (102/388).

## AgentThreatBench: Data exfiltration suite

[`AgentThreatBench`](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) is a security benchmark developed by the UK AI Safety Institute based on the OWASP Top 10 for LLM Applications. We evaluated a 10-sample **Data Exfiltration slice** designed to test whether agents leak customer data when processing untrusted inputs.

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

## Summary of empirical findings

Across all three benchmarks:

- **Bench-Corp:** 0 successful attacks across 70 guarded episodes, with task completion remaining above 91.4%.
- **TAU-bench:** 0 false positive blocks across 8,151 standard tool calls; 100% of attempted violations blocked under adversarial prompts, with 50% recovering to full task completion via remedy feedback.
- **AgentThreatBench:** 100% security pass rate across both standard and adversarial prompt profiles.

### Scope note
These results measure performance when policies define clear authorization requirements and return paths. In test scenarios where mixed-content outputs are flagged suspicious but offer no authorization path or transformation tool (such as AgentThreatBench Memory Poisoning cases), OpenAPPA recorded a 100% security pass rate (16/16) and a 0% completion rate (0/16) by failing closed.

## Reproduce and inspect

- [`bench-corp`](https://github.com/archestra-ai/OpenAPPA/tree/main/bench-corp) — scenarios, runner, and result format.
- [`harness-taubench`](https://github.com/archestra-ai/OpenAPPA/tree/main/harness-taubench) — TAU-bench harness and evaluation code.
- [`AgentThreatBench`](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench) — upstream task definitions and benchmark suite.
- [OpenAPPA Paper on arXiv](https://arxiv.org/abs/2607.24625) — formal information-flow model and evaluation methodology.

