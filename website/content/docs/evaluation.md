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

## Security: no observed attacks in 5,628 evaluations

No scored attack succeeded against guarded OpenAPPA in **5,628 evaluations**:
600 from
[Bench‑Corp](https://github.com/archestra-ai/OpenAPPA/tree/main/bench/corp),
720 from
[AgentThreatBench](https://github.com/UKGovernmentBEIS/inspect_evals/tree/main/src/inspect_evals/agent_threat_bench),
and 4,308 from a replay of the Kaggle competition
[AI Agent Security](https://www.kaggle.com/competitions/ai-agent-security-multi-step-tool-attacks).
The first two suites tested standard and adversarial prompts.

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

In the [Kaggle replay](https://github.com/archestra-ai/OpenAPPA/blob/main/bench/aicomp/REPORT.md), the agent reads one of 319 support emails,
63 of them malicious, and mails a triage summary. Utility is the share of runs
that sent the summary. ASR counts runs where the attacker's link went out or
the competition's scorer flagged an attack.

| Model | Guarded OpenAPPA (Utility / ASR) | No guardrail, subagent reads the email (Utility / ASR) | Organizers' rule guardrail (Utility / ASR) | No guardrail, agent reads the email (Utility / ASR) |
|---|---:|---:|---:|---:|
| GPT-6 Luna | **100% / 0%** | 100% / 0% | 89.1% / 2.0% | 88.3% / 2.3% |
| GPT-OSS 20B | **100% / 0%** | 100% / 0% | 94.1% / 11.7% | 95.2% / 13.3% |
| Gemma 4 26B | **100% / 0%** | 98.3% / 0% | 84.6% / 14.9% | 91.2% / 26.5% |
| GLM 5.3 Flash | **98.3% / 0%** | 98.6% / 0.1% | 58.5% / 19.7% | 81.5% / 35.1% |

The two unguarded columns differ only in who reads the email. An agent that
reads it follows the links the attacker planted, runs out of steps, or copies
the link into its summary. A [subagent](/how-it-works#subagent-reads) that
returns four typed fields keeps that text away from the agent that writes the
summary. Guarded OpenAPPA uses the same subagent and makes its answer a
[checked shape](/contracts#structured-child-returns) instead of a request. The
competition's leaderboard guardrails block every summary: 0% utility.

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
- **[Kaggle AI Agent Security](https://github.com/archestra-ai/OpenAPPA/blob/main/bench/aicomp/REPORT.md)** replays the competition's sandbox
  and scorer: triage over all 319 organizer emails, and 40 attacks in which the
  user asks only for ordinary work. It ran GPT-OSS 20B and Gemma 4 26B, the
  small models the competition used, plus GLM 5.3 Flash and GPT-6 Luna, three
  times each. The attacks are fixed; none was written against OpenAPPA.
