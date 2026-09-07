---
title: How it works
category: Deep Dive
order: 2
description: Deterministic security guarantees, flow tracking, and how agents self-correct.
---

## OpenAPPA enforces information-flow policy proactively

OpenAPPA sits between an agent and its tools to answer one question before every action: *is this data allowed to go to this destination?*

Powered by **APPA** (Agentic Permissions Policy Algebra), OpenAPPA tracks data sensitivity and trust across the tools an agent uses. The security engine stays outside the agent loop, so prompt injections cannot alter or bypass policy rules.

OpenAPPA also helps the agent remain useful under security restrictions. If an action is blocked by policy, OpenAPPA returns ways to continue: request human approval, clean sensitive fields, or isolate a read in a subagent.

## The Core Concepts

OpenAPPA operates with three concepts:

1. **Security Label**

   A trajectory is an agent's work on a conversation or task, including its tool calls. For each trajectory, OpenAPPA keeps a security label and the policy configuration needed to evaluate its next action. Each decision accounts for what the agent has already read and done.

   The security label can become more restrictive as the agent works, but it cannot become less restrictive. Once the agent reads restricted or untrusted data, those restrictions stay with the session.

   :::fig-label-fold:::

   Audience and Trust make up the security label. OpenAPPA also tracks Effects and checks Attention requirements:

   1. **Audience:** Who is authorized to access data in this agent session. Reading data for a smaller audience restricts where the agent can send data later. For example, after reading an internal customer record, the agent cannot send session data to a public destination.
   2. **Trust:** How much the data in the session can be trusted. Reading an untrusted web page can lower the session's trust, and tools that require trusted input will no longer be allowed to run.
   3. **Effect:** What the agent has already done, such as sending an email or changing a system. Effects accumulate in the session history. A policy can require an effect to have happened, or prevent an action after an effect has happened.
   4. **Attention:** Approval or review required for a specific action. Unlike effects, attention does not accumulate. An approval clears the attention requirement for that action only, and later calls must request attention again.

2. **Tool Contracts**

   Every tool has a contract in the OpenAPPA policy. The contract tells OpenAPPA how the tool interacts with the agent's security state:

   - **`delta`:** How data returned by the tool changes the security label.
   - **`requires`:** Which requirements the session must satisfy for OpenAPPA to allow the action.
   - **`effects`:** Which effects are recorded after a tool runs successfully.

   For example, a CRM tool can label its result as internal, while an email tool can require the recipient to be included in the session's audience.

3. **Remedy Plans**

   When an action does not meet its tool contract, OpenAPPA blocks it and returns the remedy plans allowed by the policy. A plan can involve cleaning data with a [sanitizer](#sanitizers), receiving approval from an [authority](#authorities), accepting a narrower audience, or isolating a sensitive read in a subagent.

   An offered plan can still be denied by an approval service or fail during data cleaning. If no permitted remedy succeeds, the action remains blocked.

   :::fig-remedy-plan:::

## Keeping Agents Useful Under Restrictions

If the security label becomes more restrictive as the agent works, how can the agent still do something useful?

OpenAPPA can ask for approval for a specific action or clean data before the agent sees it. It also uses information about each tool call and who may access the data to apply the appropriate restrictions. The policy configuration defines when these options are available.

### Authorities

An authority can approve a specific action that the session's restrictions would otherwise block. It can be a person, an approval service, or an LLM evaluator. Approval does not make the security label less restrictive; it only allows that one action.

For example, an agent has read a private customer report and wants to email it to an external auditor. An authority can approve that email without approving future emails or changing the session's restrictions.

```toml
[[authority]]
name = "reviewer"

[authority.permits]
# Allow the reviewer to approve sharing
# outside the session's audience.
audience_missing = ["public"]

[externals.authorities.reviewer]
# OpenAPPA's built-in human-in-the-loop authority.
builtin = "hitl"
```

See [Authorities in the policy reference](/contracts#authorities) for configuration and supported implementations.

### Sanitizers

A sanitizer cleans data before the agent receives it or sends it to a tool. Cleaning data before the agent sends it can allow an action that would otherwise be blocked.

For example, a sanitizer removes customer names and email addresses from a support ticket. The policy permits the agent to share that cleaned version in a public bug report.

```toml
[[sanitizer]]
name = "remove_customer_details"
on = ["tool_output"]

[sanitizer.permits]
# Allow the cleaned result to be shared publicly.
audience = { from = ["internal"], to = ["public"] }

[externals.sanitizers.remove_customer_details]
# Replace with your internal sanitization service URL.
url = "https://sanitizer.corp.example/sanitize"
```

See [Sanitizers in the policy reference](/contracts#sanitizers) for service configuration and where cleaning can happen.

### Annotators

An annotator applies policy dynamically when that is more convenient than writing a separate tool contract for every case. It describes what the tool call requires, the restrictions on its output, and its effects.

For example, when an agent reads a file, an annotator can check which directory it comes from. Files in a public documentation directory can have a public audience, while files in a customer records directory are restricted to internal users. The policy also defines how much data from each directory can be trusted.

```toml
[[annotator]]
name = "classify_file"
builtin = "llm"
hint = "Use the file's directory to determine its audience and trust."

[[tool]]
name = "read_file"
# Ask the annotator to classify the file being read.
annotator = "classify_file"
```

The built-in `llm` annotator runs in-process and calls the model configured in `[externals.llm]`. See [Model transports](/contracts#model-transports) for that configuration and [Annotators in the policy reference](/contracts#annotators) for limits on the rules it can return.

### Subagents Isolate Sensitive Reads

Reading confidential records or untrusted external data can restrict the main agent's whole session. A subagent can read and reason over that data in a separate context, keeping those restrictions local to its own session.

Before the subagent starts, the main agent declares what restrictions it will accept on the returned result and whether a sanitizer must clean it first. That declaration also limits what the subagent may read. A result that does not meet the declaration is blocked before it reaches the main agent.

For example, a subagent can examine a private customer ticket and return a summary through a sanitizer that removes customer identities. If the policy permits that cleaned result to be shared publicly, the main agent can use it in a public bug report without reading the private ticket itself. Both agents' actions and approvals remain in the same audit log.

The integration must support separate subagent contexts. See [Subagent Returns in the policy reference](/contracts#subagent-returns) for configuration and return rules.

### Audience Sources

An audience source tells OpenAPPA who belongs to an allowed group. Even when a session is restricted to a group, the agent can still share data with its members. OpenAPPA can check your company's existing directory to find out who those members are.

For example, before sending a Finance report, OpenAPPA checks whether the recipient belongs to the Finance group in Google Workspace.

```toml
[[audience.group]]
name = "finance"
from = ["google-workspace:group/finance@corp.com"]

[[tool]]
name = "get_finance_report"
# Restrict the report to members of the Finance group.
delta = { audience = ["@finance"] }
```

Connect the Google Workspace audience source to your directory service. See [Audience sources in the policy reference](/contracts#audience-sources) for supported providers and configuration.

## Example: sharing information from a private customer ticket

The example shows an agent reading a private customer ticket and then trying to share information from it.

The agent has three tools:

- **`get_ticket_from_crm`:** Read a customer support ticket.
- **`send_email`:** Send an email to a recipient.
- **`file_github_issue`:** Create a public GitHub issue.

The policy says:

- CRM tickets are internal.
- Emails may only go to people authorized to see the session's data.
- Public GitHub issues cannot contain internal data.

The policy also allows a sanitizer to remove customer details and a person to approve sharing outside the company.

See the [customer-ticket policy example](/contracts#example-customer-ticket-policy) for the tool rules and service configuration.

### What happens when the agent reads a ticket?

The agent has three options when reading the ticket:

1. **Read the original ticket itself.** Its session becomes internal. It can email colleagues, but posting publicly or emailing an external recipient requires approval.
2. **Have a sanitizer remove customer details first.** The agent sees only the cleaned ticket. This example's policy permits that cleaned version to be shared publicly.
3. **Have a subagent read the original ticket.** The subagent examines the private information and returns a sanitized result. The main agent never sees the private details and can use the cleaned result in public tools.

:::fig-two-endings:::

Suppose the agent takes the first option: it reads the original ticket and tries to email an external auditor at `auditor@external.com`. OpenAPPA blocks the email and offers human approval. If approved, that particular email is sent. Future external emails still need their own approval.

The agent can still finish useful work with private data, but sharing it outside the company requires either cleaning it or obtaining permission.

## Next steps

- [Policy Reference](/contracts): Guide to reviewing and writing policy configuration.
- [How to add it to your agent](/writing-an-integration): Integration guide, deployment models, and existing integrations.
- [Benchmarks](/evaluation): Empirical paper results on multi-step workflows and bench-corp.
- [OpenAPPA Paper](/paper): Formal information-flow model, theorems, and experimental methodology.
