---
title: OpenAPPA vs OPA
category: Comparison
order: 10.7
---

[Open Policy Agent (OPA)](https://www.openpolicyagent.org/docs) is a general-purpose policy engine used across applications and infrastructure. For AI agents, OPA can check [which tools an agent may call and which arguments are allowed](https://www.openpolicyagent.org/). Your application supplies the context and enforces the decision.

OpenAPPA is an agentic security framework. It supplies action-history tracking, persistent security state, and recovery plans. Reading internal data limits later sharing. Reading an untrusted source can prevent later use of tools that require trusted input. With OPA, your application must track those reads and define how they affect later actions.

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

OpenAPPA's integration connects tool calls and results to its security checks. When a call is blocked, the agent can use a remedy plan to clean the outgoing data or request approval, if your policy permits it. With OPA, your application defines those next steps and connects the services that carry them out.
