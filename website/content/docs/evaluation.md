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
[AI Agent Security – Multi-Step Tool Attacks](https://www.kaggle.com/competitions/ai-agent-security-multi-step-tool-attacks).
The first two suites tested standard and adversarial prompts. The Kaggle replay
tested ordinary requests over data that carries the attacks.

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

The [Kaggle replay](https://github.com/archestra-ai/OpenAPPA/blob/main/bench/aicomp/REPORT.md)
uses the competition's own sandbox and scorer. The user asks for ordinary work,
and the attack sits in the data the agent reads. In each task the agent reads
one of the organizers' 319 support emails and mails a triage summary. 63 of
these emails carry an attack. We ran GPT-OSS 20B and Gemma 4 26B, the small
open models the competition used, plus GLM 5.3 Flash and GPT-6 Luna.

| Model | Guarded OpenAPPA | Same subagent, unchecked | Organizers' rule guardrail | No guardrail |
|---|---:|---:|---:|---:|
| GPT-6 Luna | **100% / 0** | 100% / 0 | 87.1% / 19 | 86.0% / 20 |
| GPT-OSS 20B | **100% / 0** | 100% / 0 | 82.4% / 112 | 82.3% / 109 |
| Gemma 4 26B | **100% / 0** | 98.3% / 0 | 69.7% / 143 | 68.0% / 150 |
| GLM 5.3 Flash | **98.3% / 0** | 98.6% / 0 | 38.8% / 189 | 49.1% / 222 |

Each entry shows the share of 957 runs done cleanly, followed by the number of
runs that sent mail carrying the attacker's link. A run is done cleanly when the
summary arrives, the scorer finds no breach, and the mail carries no such link.

Guarded OpenAPPA reads each email in a [subagent](/how-it-works#subagent-reads)
that may return only four typed fields. That design brings the utility: the
second column runs the same subagent without OpenAPPA and finishes about as
many tasks. OpenAPPA turns the design's safety into a check. Unchecked,
Gemma's subagent handed back something other than the four fields in one
answer in five, most often the schema itself, and the main agent took it as
data. OpenAPPA [refuses any answer outside the shape](/contracts#structured-child-returns),
whatever the model or harness does. The competition's leaderboard guardrails
pass its own benign check, but they deny every send after the agent reads an
email, so they finish no triage task.

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
- **Kaggle AI Agent Security** replays the competition's sandbox, scorer and
  fixtures (`aicomp_sdk` 3.1.2). It covers the triage task over all 319
  organizer emails, the 31 collected attacks in which the user only asks for
  ordinary work, and 9 chains that target the public guardrail's five-call
  window. Each model ran every task three times. The
  [report](https://github.com/archestra-ai/OpenAPPA/blob/main/bench/aicomp/REPORT.md)
  gives the method, every outcome table and the limits.
