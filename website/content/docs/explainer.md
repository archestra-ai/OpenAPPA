---
title: Explainer
category: Get started
order: 3
description: The problem and the boundary, one figure at a time.
---

## The more the agent knows, the better it works

Here is an agent doing a routine job: email the client an update on their project. Connect Salesforce, so it knows the account and the renewal at stake. Connect GitHub, so "almost done" means an approved PR sitting in final CI, not a guess. Connect Granola, so the update answers what the client actually raised on the last call.

:::fig-connected-agent:::

## Non-determinism is a data exfiltration risk

Nobody attacked anything in this next run. The task is the same, the connections are the same, the agent is the same. But the agent is a language model — helpful, eager, and non-deterministic. On some fraction of runs it decides a persuasive update needs *comparison context*, and nothing tells it that other clients' calls are not its material to use.

:::fig-exfiltration:::

## OpenAPPA is a deterministic policy boundary

OpenAPPA wraps the agent in a boundary where data origins and access rights carry clear, mathematical weight. Every piece of data that crosses in carries a label—where it came from and who may read it. Everything the agent derives inherits the labels of what it read. And every outbound action—a tool call, an email—is checked against a declared contract *before it happens*. The check is deterministic: same labels, same contract, same verdict, on run 1 and run 40. The agent stays free to be creative inside the boundary; the policy decides what leaves it.

:::fig-guardrail:::

## OpenAPPA keeps agents productive instead of just blocking

OpenAPPA's checks are legible and actionable: when a flow cannot proceed directly, the agent learns why *before* execution and receives exact, sound remedy plans. Instead of getting stuck or silently failing, the language model can request policy approvals, sanitize sensitive fields, or accept necessary reach restrictions to safely complete its task.

:::fig-negotiation:::
