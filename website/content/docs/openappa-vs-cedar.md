---
title: OpenAPPA vs Cedar
category: Comparison
order: 10.5
description: Cedar provides a policy language and engine. OpenAPPA adds persistent context tracking and remediation for AI agents.
---

[Cedar](https://docs.cedarpolicy.com/) is a language for writing authorization policies and an engine for evaluating them. It checks each request against your policies, using the context you provide, and returns an allow or deny decision. It supports application authorization broadly, including requests from AI agents.

OpenAPPA is a security framework built for AI agents. It includes persistent tracking of what an agent reads and does, rules for how that history affects later actions, and remedy plans for blocked actions.

| | Cedar | OpenAPPA |
|---|:---:|:---:|
| Policy language and evaluation engine | ✓ | ✓ |
| Express custom business rules directly in policy | ✓ | ✕ |
| Tracks the agent's action history | ✕ | ✓ |
| Uses earlier reads to limit later actions | ✕ | ✓ |
| Suggests ways to unblock an action | ✕ | ✓ |

Cedar gives you flexibility to define authorization rules. For example, a policy can allow a refund only up to the requesting employee's approval limit. Cedar can compare those values directly. OpenAPPA needs a separate component to evaluate that condition.

OpenAPPA gives you built-in security behavior for agents. Cedar can check whether an agent may send information to a destination, but your application must track what the agent has read, decide how restrictions combine, and supply that state to Cedar. OpenAPPA supplies that behavior out of the box.

OpenAPPA suits teams enforcing agent security at scale. The tracking, persistence, and recovery behavior are already part of the framework. See [How it works](/how-it-works) for the model and [Add to your agent](/writing-an-integration) for integration options.

## Example: a private ticket and a public issue

An agent has permission to read customer tickets and create public issues. It reads a private ticket, then tries to post an issue.

With Cedar, your application must record that the agent read private information and include that fact when asking Cedar whether publication is allowed. Cedar evaluates the policy and returns a deny decision. Your application then blocks the post. It must keep that context accurate throughout the agent's work.

With OpenAPPA, the ticket's tool rule marks the result as internal. Reading it restricts where the agent can send information. A later attempt to post publicly is blocked unless policy permits a remedy, such as cleaning the outgoing data or approving that specific post. The agent's restrictions remain in place for future actions.
