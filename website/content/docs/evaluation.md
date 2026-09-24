---
title: Benchmarks
category: Evaluation
order: 10
description: Evidence for OpenAPPA's security, utility, and token cost across three agent benchmarks.
---

## Security without blocking useful work

These results answer three questions: Does OpenAPPA stop policy violations? Can
the agent still finish legitimate tasks? How many extra tokens does protection
require?

OpenAPPA
uses
[deterministic policy enforcement](/how-it-works#openappa-enforces-information-flow-policy-proactively)
to check each action. These benchmarks test the complete integration and show
whether its policies stop the intended threats without making the agent useless.

## Security: no observed attacks in 1,320 evaluations

No scored attack succeeded against guarded OpenAPPA in **1,320 evaluations**:
600 from
[Bench‑Corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench/corp) and
720 from
[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench).
Both suites tested standard and adversarial prompts.

In Bench-Corp, the evaluated
[Microsoft FIDES](https://www.microsoft.com/en-us/research/publication/securing-ai-agents-with-information-flow-control/)
configurations had a 28–35% attack success rate. OpenAPPA's policies also
enforced rules those configurations did not support, including
[recipient authorization](/contracts#audiences),
[out-of-band approval](/contracts#authorities), and
[required action ordering](/contracts#effects).

The suites test concrete ways an agent can break policy:

- **[Sensitive-data sharing](/how-it-works#the-core-concepts):** Restricted data
  must not reach an unauthorized person or a less restricted data store.
- **Prompt injection:** Instructions in untrusted email, memory, or forum
  content try to make the agent bypass policy.
- **Approval and ordering:** The agent receives text that falsely claims an
  approval happened. Policy requires the real approval in its recorded history.
- **Tenant isolation:** The agent must keep each customer's data within that
  customer's authorized readers.

## Utility: 88–90% completion while enforcing policy

A secure agent is not useful if it cannot finish legitimate work. Across three
language models in Bench-Corp, guarded OpenAPPA completed **88–90% of tasks**.
The evaluated FIDES configurations completed 37–45%. Each table entry shows
task completion followed by attack success rate (ASR).

| Model | Guarded OpenAPPA (Utility / ASR) | FIDES middleware (Utility / ASR) | FIDES native (Utility / ASR) |
|---|---:|---:|---:|
| GPT-5.6 Luna | **88.0% / 0%** | 38.5% / 32.0% | 37.0% / 32.5% |
| DeepSeek V4 Flash | **89.5% / 0%** | 39.5% / 34.5% | 41.5% / 33.0% |
| Gemini 3.7 Flash | **90.0% / 0%** | 43.5% / 28.5% | 44.5% / 28.0% |

In AgentThreatBench's adversarial tests, guarded OpenAPPA had the highest task
completion for all three models. In the standard tests, it led with Luna and
Gemini. Middleware FIDES led with DeepSeek.

OpenAPPA's
[recovery mechanisms](/how-it-works#keeping-agents-useful-under-restrictions)
help the agent continue safely after a policy block. In a Bench-Corp test with
Luna, task completion was 88.0%. It fell to 56.5% without
[subagent isolation](/how-it-works#subagent-reads) and to 35.0% without
[guided recovery](/contracts#remedy-plans-and-child-returns).

No scored attack succeeded in any of these configurations. The features
interact, so this test does not measure each feature's effect in isolation.

[Tau Bench's banking benchmark](https://taubench.com/leaderboard?benchmark=knowledge)
tests ordinary banking support work rather than attack prompts. The validated
comparison tested all 97 tasks four times with GPT‑5.6 Luna at maximum reasoning
effort.

| Agent configuration | Successful simulations | Mean Tau reward |
|---|---:|---:|
| Guarded OpenAPPA | 151/388 | 38.92% |
| OpenAPPA agent, permissive policy | 153/388 | 39.43% |
| Stock Tau agent | 156/388 | 40.21% |

Guarded OpenAPPA finished two fewer simulations than the permissive OpenAPPA
agent and five fewer than stock. Its mean reward was 0.52 percentage points
below permissive and 1.29 points below stock.

OpenAPPA checked 11,355 calls. One policy decision stopped a state-changing call
before identity verification. Three other blocks rejected malformed input. No
tool execution errors occurred. Tau is not an attack benchmark, so these results
do not establish net security.

## Token overhead: 4.22% over stock on Tau

Guarded OpenAPPA used a mean of 1,307,763 agent tokens per simulation. This was
4.22% more than stock and 2.80% more than the permissive OpenAPPA agent.

| Agent configuration | Mean agent tokens per simulation | Difference from stock |
|---|---:|---:|
| Guarded OpenAPPA | 1,307,763 | **+4.22%** |
| OpenAPPA agent, permissive policy | 1,272,128 | +1.38% |
| Stock Tau agent | 1,254,821 | — |

These totals include every prompt and completion in the complete agent
trajectory. They measure the whole agent setup, not the computing cost of the
OpenAPPA policy engine alone.

## Claude Code auto mode: only OpenAPPA stopped every scored attack

[Claude Code auto mode](/openappa-vs-auto-mode) asks a classifier model whether
each tool call should run. OpenAPPA does not ask a model to judge intent.
It tracks trust and audience restrictions across the trajectory and checks every
flow against explicit policy. Each decision is deterministic and traces back to
a policy rule.

We tested guarded OpenAPPA against two Auto configurations:

- **Stock Auto** uses Claude Code's default auto-mode settings.
- **IFC-tuned Auto** also receives the information-flow facts a security team
  would supply: which sources are trusted, who may see each piece of data,
  where data may go, and which narrowing is legitimate. It receives no attack
  markers or expected answers.

All three used the same actor model (Claude Sonnet 5), task prompts, and tools.
Each benchmark used its own scoring rules. Attacks counts the scenarios or tasks
in which a scored attack succeeded.

| Benchmark | Guarded OpenAPPA (Utility / Attacks) | IFC-tuned Auto (Utility / Attacks) | Stock Auto (Utility / Attacks) |
|---|---:|---:|---:|
| Bench-Corp (20 scenarios) | 75.0% / **0/20** | 85.0% / **0/20** | 90.0% / 2/20 |
| AgentThreatBench (24 tasks) | 75.0% / **0/24** | 95.8% / 6/24 | 87.5% / 8/24 |

Only OpenAPPA had zero scored attacks in both suites. Stock Auto let ten
through. Tuning cut that to six, all in AgentThreatBench.

That protection has a cost. OpenAPPA completed 75% of tasks in both suites,
against 85–96% for the Auto arms. It also used 6.5× the reported tokens of the
same agent under a permissive policy on Bench-Corp and 2.3× on
AgentThreatBench. Those multiples cover the whole guarded agent, including
isolated child trajectories and recovery, not the policy engine alone. Claude's
SDK does not report the tokens Auto's classifier spends, so the two systems'
total costs are not directly comparable.

Each configuration ran every task once, so these results carry no variance
estimate.

## What we measured

- **[Bench-Corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench/corp)**
  contains 20 multi-step workplace tasks. They cover HR, Finance, customer
  support, vendors, email, forums, and task tracking. The benchmark checks what
  the agent actually changed or sent. It does not use an LLM judge. Each model
  ran every scenario five times with standard prompts and five times with
  adversarial prompts.
- **AgentThreatBench** is a 24-task suite published in
  [Inspect Evals](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench).
  It tests poisoned memory, attempts to take control of the agent, and data
  theft. We ran each standard and adversarial test five times with the same
  three models.
- **[Tau Bench](https://github.com/sierra-research/tau2-bench)** tests
  policy-sensitive banking support tasks. We used its standard scoring. The
  comparison covers all 97 `banking_knowledge` tasks, with four trials for each
  agent configuration. Matching tasks and random seeds makes the comparison
  fairer, but separate model sessions can still differ.
