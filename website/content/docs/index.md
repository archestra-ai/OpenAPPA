---
title: What is OpenAPPA
category: Get started
order: 1
description: An information-flow policy engine for LLM agents — running code first, specification second.
---

OpenAPPA is deterministic security for LLM agents. Instead of filtering
prompts and outputs — asking a classifier "does this text look malicious?" —
it asks a question that has a provable answer: **can this value, derived from
these sources, legally flow into this sink?**

Every turn of an agent's trajectory carries a label: who may read it
(audience), how much its origin is trusted (trust), what side effects
producing it implied (effects). Labels fold as the conversation grows; before
any tool call, the folded label is checked against that tool's declared
contract. The check has three outcomes — holds, provably fails, or
unprovable — and a failing or unprovable flow is blocked before the call is
ever executed, or routed to an authority (a human, a judge model, a webhook)
whose approval is applied fail-closed and recorded as an audited
declassification.

The property that makes this different from guardrails: **the policy never
reads the attacker's text**. A prompt injection in a pod log or an email
doesn't need to be recognized — it needs to be *unable to reach a trusted
sink*, because everything derived from a suspicious source stays suspicious
no matter how persuasive it sounds. The walkthrough below builds the idea
one step at a time.

## The more the agent knows, the better it works

Here is an agent doing a routine job: email the client an update on their project. Connect Salesforce, so it knows the account and the renewal at stake. Connect GitHub, so "almost done" means an approved PR sitting in final CI, not a guess. Connect Granola, so the update answers what the client actually raised on the last call.

:::fig-connected-agent:::

## Non-determinism is a data exfiltration risk

Nobody attacked anything in this next run. The task is the same, the connections are the same, the agent is the same. But the agent is a language model — helpful, eager, and non-deterministic. On some fraction of runs it decides a persuasive update needs *comparison context*, and nothing tells it that other clients' calls are not its material to use.

:::fig-exfiltration:::

## OpenAPPA is a deterministic guardrail

OpenAPPA wraps the agent in a boundary where the colors finally mean something. Every piece of data that crosses in carries a label — where it came from, who may read it. Everything the agent derives inherits the labels of what it read. And every flow out — a tool call, an email — is checked against a declared contract *before it happens*. The check is deterministic: same labels, same contract, same verdict, on run 1 and run 40. The agent stays free to be creative inside the boundary; the boundary decides what leaves it.

:::fig-guardrail:::

## It's about making agents eventually work

OpenAPPA's checks are legible: when a flow can't happen, the agent learns why *before* anything runs — and a reason is something a language model can act on.

:::fig-negotiation:::

## Where next

- [AgentDojo harness](/docs/agentdojo) — run the benchmark yourself.
