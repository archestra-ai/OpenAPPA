---
title: OpenAPPA vs OPA
category: Comparison
order: 10.7
---

[Open Policy Agent (OPA)](https://www.openpolicyagent.org/docs) is a general-purpose policy engine, while OpenAPPA is an agentic security framework. You write rules in OPA's language, Rego, to check application requests or infrastructure configurations. A policy can check whether a deployment uses an approved container registry and return a list of violations. OpenAPPA does not replace those configuration checks.

OpenAPPA stores security history and carries data restrictions forward between actions. If an agent reads private incident notes while preparing a deployment, OpenAPPA can restrict where it publishes the release summary. OPA can check that sharing rule too, but your application must track the private read, decide how restrictions combine, and make that state available to the policy.

When an action is blocked, OpenAPPA builds remedy plans from the cleaning and approval options allowed by your policy. See [How it works](/how-it-works). OPA policies can [return structured data](https://www.openpolicyagent.org/docs), not just allow or deny. You could use that output to request approval, but you write the rule and the code that obtains approval and retries the action.

OPA gives you one policy language across applications and infrastructure. OpenAPPA supplies the tracking and recovery behavior for agent workflows.

| | OPA | OpenAPPA |
|---|:---:|:---:|
| Policy language and evaluation engine | ✓ | ✓ |
| Express custom business rules directly in policy | ✓ | ✕ |
| Tracks action history to check later actions | ✕ | ✓ |
| Suggests ways to unblock an action | ✕ | ✓ |

## Deployment and integration

| | OPA | OpenAPPA |
|---|---|---|
| Deployment | [HTTP service, embedded Go library, or compiled WebAssembly policies](https://www.openpolicyagent.org/docs/integration) | Run alongside your agent process or deploy as a shared Kubernetes service |
| Agent connection | Your application or an integration requests and enforces decisions | Use supplied [Claude Code](/claude-code) and [kagent](/kagent) integrations, or [connect your existing agent](/writing-an-integration) |

For an agent, your integration supplies the facts each OPA policy needs and enforces the decision. OPA can [hold or retrieve policy data](https://www.openpolicyagent.org/docs/external-data), but it does not automatically track what the agent has read. Your application keeps that history up to date and makes it available when checking later actions.

OpenAPPA connects to the agent's workflow instead. It can carry restrictions from a tool result into a later call even when the two tools belong to different systems. For an agent that manages infrastructure, the two can serve different purposes: OPA checks whether the proposed configuration is allowed; OpenAPPA checks where the agent may send the information it has read.
