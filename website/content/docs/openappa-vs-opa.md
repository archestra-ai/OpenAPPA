---
title: OpenAPPA vs OPA
category: Comparison
order: 10.7
---

[Open Policy Agent (OPA)](https://www.openpolicyagent.org/docs) is a general-purpose policy engine used across applications and infrastructure. For AI agents, OPA can check which tools an agent may call and which arguments are allowed (see the [AI Tool Calling example on OPA’s homepage](https://www.openpolicyagent.org/)). But OPA does not automatically know what the agent has read or done earlier. Those actions affect a decision only if your application makes that history available and your policies use it. Your application then allows or blocks the tool call based on the decision.

OpenAPPA is an agentic security framework with a language for defining security policies and an engine for evaluating them. It also supplies action-history tracking, persistent security state, and recovery plans. Reading internal data limits later sharing. Reading an untrusted source can prevent later use of tools that require trusted input. With OPA, your application must track those reads and define how they affect later actions.

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

OPA can [use external data to make policy decisions](https://www.openpolicyagent.org/docs/external-data), but your application must supply the agent's history and keep it up to date. Your integration then enforces the decision.

OpenAPPA's integration connects tool calls and results to its security checks. When a call is blocked, the agent can use a remedy plan to clean the outgoing data or request approval, if your policy permits it. With OPA, your application defines those next steps and connects the services that carry them out.
