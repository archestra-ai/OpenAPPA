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

## The more the agent knows, the better it works

Here is an agent doing a routine job: email the client an update on their project. Connect Salesforce, so it knows the account and the renewal at stake. Connect GitHub, so "almost done" means an approved PR sitting in final CI, not a guess. Connect Granola, so the update answers what the client actually raised on the last call.

:::fig-connected-agent:::

## Non-determinism is a data exfiltration risk

Nobody attacked anything in this next run. The task is the same, the connections are the same, the agent is the same. But the agent is a language model — helpful, eager, and non-deterministic. On some fraction of runs it decides a persuasive update needs *comparison context*, and nothing tells it that other clients' calls are not its material to use.

:::fig-exfiltration:::

## OpenAPPA is a deterministic guardrail

OpenAPPA wraps the agent in a boundary where the colors finally mean something. Every piece of data that crosses in carries a label — where it came from, who may read it. Everything the agent derives inherits the labels of what it read. And every flow out — a tool call, an email — is checked against a declared contract *before it happens*. The check is deterministic: same labels, same contract, same verdict, on run 1 and run 40. The agent stays free to be creative inside the boundary; the boundary decides what leaves it.

:::fig-guardrail:::

## OpenAPPA proactively communicates limitations instead of blocking

OpenAPPA's checks are legible: when a flow can't happen, the agent learns why *before* anything runs — and a reason is something a language model can act on.

:::fig-negotiation:::

## Where next

- [AgentDojo harness](/docs/agentdojo) — run the benchmark yourself.
