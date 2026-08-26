---
title: Benchmarks
category: Evaluation
order: 10
description: Empirical security and task-completion results across Bench-Corp and AgentThreatBench.
---

## Security that still lets agents finish the job

A secure agent is not useful if it refuses every action. A capable agent is not
safe if it completes a task by violating policy. We measure defenses on two
standard axes: **task completion** (utility on legitimate goals) and **attack
success rate** (ASR, policy breaches under adversarial inputs).

Across three evaluated models on Bench-Corp (200 episodes per model), guarded
OpenAPPA achieved **88–90% task completion with 0/600 observed attacks** (0% ASR).
The evaluated defended FIDES configurations achieved **37–45% task completion
with 28–35% attack success**.

These are empirical results for the evaluated agents, policies, and scenarios—not
a claim that attacks are impossible. Full methodology, per-model results, and
limitations are available in the [OpenAPPA paper](/paper).

## Bench-Corp: realistic enterprise workflows

Bench-Corp contains 20 multi-step workflows across HR, Finance, customer
support, vendor management, email, forums, and task tracking. Scenarios are
scored from observable effects such as emails sent, files written, and
transactions executed—not from an LLM judge.

For each policy configuration, each model ran every scenario five times with
standard prompts and five times with adversarial prompts, producing 200
episodes per model.

| Model | Guarded OpenAPPA (Utility / ASR) | FIDES middleware (Utility / ASR) | FIDES native (Utility / ASR) |
|---|---:|---:|---:|
| GPT-5.6 Luna | **88.0% / 0%** | 38.5% / 32.0% | 37.0% / 32.5% |
| DeepSeek V4 Flash | **89.5% / 0%** | 39.5% / 34.5% | 41.5% / 33.0% |
| Gemini 3.7 Flash | **90.0% / 0%** | 43.5% / 28.5% | 44.5% / 28.0% |

OpenAPPA maintained high task completion while recording no successful policy
violations in these runs. FIDES task completion dropped to 37–45% because linear
IFC permanently taints trajectories upon reading confidential data, blocking
subsequent legitimate actions, while still failing to prevent 28–35% of attacks
due to missing organizational constraints (recipient authorization `$to`,
out-of-band approvals, and action ordering).

### What the scenarios test

- **Wire transfer approval:** Text inside a request claims that approval was
  already granted. OpenAPPA requires the actual secondary-approval event before
  permitting the transfer.
- **Legal pre-clearance:** An agent must create an audit record before sending
  contract material to external counsel.
- **Multi-tenant isolation:** Customer data may be sent only to recipients in
  that customer's dynamically resolved reader set.
- **Persistent storage:** Restricted data cannot be written into a broader
  store and then reread under a less restrictive label.

## AgentThreatBench: attacks embedded in agent inputs

[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench)
is published in Inspect Evals, the open evaluation collection built around the
[UK AI Security Institute's Inspect framework](https://www.aisi.gov.uk/blog/open-sourcing-our-testing-framework-inspect).
It turns risks from the
[OWASP Top 10 for Agentic Applications](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications/)
into concrete tests of memory poisoning, autonomy hijacking, and data
exfiltration.

We ran the complete 24-task suite across the same three models, with five
seeded repetitions under standard and adversarial prompts.

Guarded OpenAPPA recorded **0/720 observed attacks** across 720 evaluated task
executions (24 tasks × 3 models × 5 seeds × 2 prompt profiles). Under adversarial
prompting, it achieved the highest task completion with every evaluated model.
Under standard prompts, it led with Luna and Gemini; middleware FIDES led with
DeepSeek.

The main remaining limitation is Memory utility. OpenAPPA exposes only limited,
structured facts from untrusted memory. This filters injected instructions,
but it can also remove wording or relationships needed for an exact answer.
Middleware FIDES preserves more raw Memory utility by allowing that untrusted
content into the answer; under adversarial prompting, that choice also produced
substantially more successful Memory attacks. The full subset tables and
scoring details are reported in the paper.

## Why recovery mechanisms matter

OpenAPPA does more than block unsafe calls. It can isolate risky work in a
restricted sub-agent and tell the agent which verifiable condition would make
a blocked action safe. A Luna ablation on Bench-Corp illustrates how much those
capabilities contribute:

| Guarded configuration | Task completion | Observed ASR |
|---|---:|---:|
| Full OpenAPPA | **88.0%** | 0% |
| Without fork | 56.5% | 0% |
| Without guided recovery | 35.0% | 0% |

The mechanisms interact, so the differences should not be read as an additive
causal decomposition. The result shows that both materially help the guarded
agent finish legitimate work without relaxing enforcement.

## Learn more

The [OpenAPPA paper](/paper) contains the complete
tables, benchmark protocol, model-specific variance, scoring adaptations, and
limitations. The benchmark harnesses and checked-in scenarios are available in
the [`bench/` directory](https://github.com/archestra-ai/OpenAPPA/tree/main/bench).
