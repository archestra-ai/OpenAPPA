---
title: OpenAPPA vs Cedar
category: Comparison
order: 10.5
---

[Cedar](https://docs.cedarpolicy.com/) is a general-purpose authorization policy language and engine, while OpenAPPA is an agentic security framework. Cedar checks requests using the context you provide and returns an allow or deny decision. A policy can allow a refund only up to an employee's approval limit. With OpenAPPA, that business check lives in code outside the policy language.

OpenAPPA stores security history and carries data restrictions forward between actions. If an agent reads a private ticket, OpenAPPA can restrict a later attempt to create a public issue. Cedar can return a deny decision for that post too, but your application must track the private read, decide how restrictions combine, and supply that state to Cedar.

When an action is blocked, OpenAPPA builds remedy plans from the cleaning and approval options allowed by your policy. These can let the action proceed without removing restrictions from future actions. Cedar returns a decision; your application supplies any recovery workflow. See [How it works](/how-it-works).

Cedar checks the context you supply. OpenAPPA also carries data restrictions forward between actions.

| | Cedar | OpenAPPA |
|---|:---:|:---:|
| Policy language and evaluation engine | ✓ | ✓ |
| Express custom business rules directly in policy | ✓ | ✕ |
| Tracks action history to check later actions | ✕ | ✓ |
| Suggests ways to unblock an action | ✕ | ✓ |

## Deployment and integration

| | Cedar (open source) | Cedar through AWS services | OpenAPPA |
|---|---|---|---|
| Deployment | Embed the [Cedar library](https://github.com/cedar-policy/cedar) or run it behind your own service | Managed policy evaluation through [Verified Permissions](https://docs.aws.amazon.com/verifiedpermissions/latest/userguide/what-is-avp.html) or [AgentCore](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/policy.html) | Run alongside your agent process or deploy as a shared Kubernetes service |
| Agent connection | Your code supplies policies and context, then enforces decisions | Call Verified Permissions and enforce its decisions, or route tool calls through AgentCore for gateway enforcement | Use supplied [Claude Code](/claude-code) and [kagent](/kagent) integrations, or [connect your existing agent](/writing-an-integration) |

With the Cedar library or Verified Permissions, your application decides where to ask for permission and how to handle the answer. AgentCore puts that check in the tool-call path, so calls routed through its gateway can be blocked without adding a check inside each tool. Its history-based rules use Dogwood; see [OpenAPPA vs Dogwood](/openappa-vs-dogwood).

With OpenAPPA, the integration connects both tool calls and their results to the security checks. That lets information returned by one tool restrict a later call to another. When connecting an existing agent, the work is making sure those events reach OpenAPPA and denied calls cannot execute; you do not need to rebuild its rules for combining data restrictions.
