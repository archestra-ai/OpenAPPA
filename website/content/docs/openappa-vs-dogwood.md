---
title: OpenAPPA vs Dogwood
category: Comparison
order: 10.6
---

[Dogwood](https://dogwood-policy.github.io/dogwood/guide/00-introduction.html) is a policy language for AI agents. It builds on Cedar and adds rules about earlier actions, such as requiring approval within the last hour or limiting how often a tool runs.

OpenAPPA is a security framework for AI agents. It stores security history and carries data restrictions forward between actions. Mark a report as internal and configure where tools may send information; OpenAPPA keeps that restriction in place and combines it with restrictions from other reads. Dogwood can enforce a similar outcome, but you write the rules connecting the read to later actions.

OpenAPPA does not express time windows and event counts directly in its policy language; those checks need code outside it.

When an action is blocked, OpenAPPA builds remedy plans from the cleaning and approval options allowed by your policy. These can let the action proceed without removing restrictions from future actions. With Dogwood, your application supplies any recovery workflow. See [How it works](/how-it-works).

Both can use an agent's history to check its next action. The clearest practical difference is how you deploy them and connect them to your agents.

| | Dogwood | OpenAPPA |
|---|:---:|:---:|
| Policy language and evaluation engine | ✓ | ✓ |
| Express custom business rules directly in policy | ✓ | ✕ |
| Tracks action history to check later actions | ✓ | ✓ |
| Checks time windows and event counts directly in policy | ✓ | ✕ |
| Suggests ways to unblock an action | ✕ | ✓ |

## Deployment and integration

| | Dogwood (open source) | Dogwood through AgentCore | OpenAPPA |
|---|---|---|---|
| Deployment | [Rust library and CLI](https://github.com/dogwood-policy/dogwood) for exploring policies | [AWS-managed service](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/policy-temporal.html) | Run alongside your agent process or deploy as a shared Kubernetes service |
| Agent connection | Your code submits events and enforces decisions | Tool calls through AgentCore Gateway | Use supplied [Claude Code](/claude-code) and [kagent](/kagent) integrations, or [connect your existing agent](/writing-an-integration) |

With open-source Dogwood, you choose which events to submit and how to connect the decisions to your agent. With AgentCore, the gateway records the calls passing through it. Your application groups related calls into a [policy session](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/policy-session-based-temporal.html), so a rule can connect an earlier approval or read to a later action.

OpenAPPA's supplied integrations connect agent events to its checks and expose a tool for carrying out remedy plans. For another agent framework, you build that connection using the integration guide. The policy engine and recovery planning stay in OpenAPPA rather than in your adapter.
