---
title: Comparison
category: Get started
order: 1.5
description: How OpenAPPA compares with Cedar for securing AI agents.
---

## OpenAPPA vs Cedar

[Cedar](https://docs.cedarpolicy.com/) is a policy language and engine for application authorization. Your application supplies facts about a proposed action, asks Cedar whether it is allowed, and enforces the answer. You can embed the engine or call a service that runs it.

OpenAPPA is a security framework built for AI agents. It tracks what an agent reads and carries the resulting restrictions forward as the agent uses tools and shares information.

The main difference is how much of the agent's security behavior you need to build yourself. Cedar checks the context you supply. Your application must track what the agent has read, decide how restrictions combine, and supply that state to Cedar. OpenAPPA includes that behavior out of the box.

:::fig-comparison-cedar:::

For example, an agent may be allowed to read private tickets and create public issues. Once it reads a private ticket, OpenAPPA blocks public posting unless the policy permits a way to share that information. Cedar can check the same restriction, but your application must track the private read and tell Cedar about it.

OpenAPPA suits teams enforcing agent security at scale. It includes restriction tracking, tool policy checks, and remedy plans for blocked actions. You connect the agent, declare how its tools handle data, and configure any services needed for cleaning or approval.
