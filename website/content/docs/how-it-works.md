---
title: How OpenAPPA works
category: Get started
order: 2
description: The whole model in one sitting — what OpenAPPA guarantees and what it costs.
---

## OpenAPPA enforces information-flow policy before tool dispatch

OpenAPPA sits between an agent and its tools to answer one question before every action: *is this data allowed to go to this destination?*

OpenAPPA keeps agents productive without making policy probabilistic. It catches restrictive reads before they consume the agent's reach and makes every refusal actionable: approve the exact call, sanitize the data, satisfy a prerequisite, accept a narrower Label, or learn that no modeled path can clear the policy.

| Runtime State | Scope | Engine Semantics |
|---|---|---|
| **Label** | Trajectory | Tracks audience (allowed reader set) and trust rank (`suspicious` vs `trusted`). Reading data intersects audiences and takes the lowest trust rank. |
| **Log** | Trajectory | Append-only record of execution history, recording tool dispatches, narrowing acceptances, authority approvals, and denials. |

Policy rules stay strictly declarative: contracts, authorities, sanitizers, and casts are data configurations rather than imperative code. Imperative judgment lives outside the engine in registered external components like regex filters, classification models, or human approval queues. Component registration bounds what an answer can grant: an authority mandate limits which policy gaps its rulings cover, while a sanitizer mandate bounds the single label transition its derivations can claim.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, data cannot be laundered by passing it through intermediate steps or LLM calls. Reading internal system records permanently marks the execution context as internal, and ingesting unvetted web content permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: an external component can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is calculated directly as a functional fold over all preceding deltas, eliminating the need to replay trajectory history:

```ts
label = deltasSoFar.reduce(narrow, startingLabel)   // narrow only ever restricts
```

## Reading data costs the agent reach

OpenAPPA stops an agent *before* a fetch that restricts its label, informing it of lost reach before data enters its context. Reading internal data does not leak information by itself, but it restricts all future steps in the trajectory to an internal context. Destinations requiring public reach become permanently unavailable, and previously unconstrained dispatches require explicit approval.

By evaluating the step before dispatch, OpenAPPA prevents scenarios where an agent ingests data only to discover three steps later that outbound dispatches are blocked. The engine presents this pre-fetch choice as a **narrowing** stop. If the agent accepts the narrowing, the acceptance is recorded in the log and the call proceeds. Subsequent steps that cause no additional restriction pass without stopping, so narrowing prompts occur once per level of increased restriction.

## A child's narrowing dies with it

Child trajectories isolate label modifications within host-managed sub-executions. A child starts with the parent trajectory's current label, but any data read within the child narrows the child's label exclusively. When the child completes, it returns a single value across its boundary that folds into the parent trajectory like any other tool read. If that raw return would narrow the parent, the merge stops at the boundary until accepted directly or cleaned through a registered sanitizer. Parent and child branches share a single append-only log so that all sends and approvals remain globally auditable.

## Worked example: preserve reach or approve the exact call

To illustrate policy enforcement, consider an agent configured with three tools: `get_ticket_from_crm`, `send_email`, and `file_github_issue`. The CRM tool contract declares a `delta` that restricts the trajectory to internal reach, `send_email` requires the recipient to match the trajectory audience, and `file_github_issue` requires public reach.

```toml
[[tool]]
name  = "get_ticket_from_crm"
delta = { audience = { exactly = ["internal"] } }   # "internal" is a single reader id

[[tool]]
name     = "send_email"                    # send_email(body, to: $recipient)
requires = { audience = { includes = ["$recipient"] } }
delta    = {}                              # annotated: the result carries nothing
effects  = ["egress"]

[[tool]]
name     = "file_github_issue"
requires = { audience = { includes = ["public"] } }
delta    = {}
effects  = ["egress", "mutation"]

[[sanitizer]]                              # the child route crosses this
name = "remove_pii"
on   = ["tool_output"]

[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[sanitizer.implementation]
resolver = { url = "https://pii.corp/redact", timeout_ms = 10000 }

[[authority]]                              # who can approve the auditor mail
name = "disclosure-officer"

[authority.mandate]
can_add_readers = { may_add = ["public"] } # may vouch any recipient

[authority.implementation]
resolver = { url = "https://approvals.corp/rule", timeout_ms = 30000 }
```

The trajectory begins at `{public, trusted}` unless pre-existing context or user input introduces restricted labels. When the agent calls `get_ticket_from_crm()`, OpenAPPA intercepts the dispatch before execution. The engine offers two operational paths based on the deployment's branching capabilities:

| Execution Path | Trajectory Label Impact | Downstream Dispatch Impact |
|---|---|---|
| **Accept Narrowing** | Parent becomes `internal`. | `file_github_issue` is blocked; `send_email` requires authority approval. |
| **Child Branch + Sanitizer** | Parent remains `{public, trusted}`; child narrows to `internal`. | `remove_pii` sanitizes the return value; `file_github_issue` remains open on parent. |

:::fig-two-endings:::

If the goal is publishing the ticket on GitHub, the agent executes the child branch, passes the result through `remove_pii`, and files the public issue without modifying the parent label. If the goal is emailing raw CRM data to an external auditor, the agent accepts the narrowing in the parent trajectory. When `send_email(ticket, auditor@…)` subsequently runs, the engine checks whether `auditor@…` is in the `internal` audience. Because it is not, OpenAPPA blocks the call and generates an authority remedy plan for `disclosure-officer`.

The authority ruling receives a complete cryptographic manifest of the call, including argument digests, source labels, and message content. If `disclosure-officer` approves the request, the email dispatches and the egress event enters the log. The trajectory label remains `internal`, ensuring that subsequent emails to unapproved recipients require separate authority rulings.

## Engine refusals enumerate every sound remedy

When OpenAPPA refuses a flow, it doesn't leave the agent stranded. An empty remedy list proves that the call is unreachable within the planner's modeled transitions under the active policy configuration and log. When candidate paths exist, OpenAPPA returns a structured object enumerating every sound alternative available from the registered components and deployment capabilities: requesting a policy approval, cleaning data with a sanitizer, executing a prerequisite tool call, or accepting a narrowing prompt.

:::fig-remedy-plan:::

Because every remedy except narrowing acceptance derives from a registered component, the engine presents all valid options in a single refusal object:

```ts
{ outcome: "block",
  requirement_gaps: [...],  // unmet entries from `requires`
  narrowing: {...},         // present when the call's own delta narrows
  unestablished: [...],     // values whose labels could not be established
  remedy_plans: [...] }     // sound remedy plans executable by id or tool call
```

A non-empty remedy list indicates that candidate paths exist, though external components may still decline a requested ruling. When an authority denies a request, that denial is appended to the log. The refusal proof remains scoped to the current configuration, active component responses, and recorded denials for that specific call.

## Unknown labels propagate until a requirement checks them

Unannotated tools return data with an **Unknown** label state, representing unverified classification rather than a specific trust rank. Unknown labels propagate through trajectory operations, causing any trajectory that ingests an unknown value to become Unknown. Unregistered tool calls are refused directly rather than returning Unknown values.

| Execution Context | Impact of Unknown State |
|---|---|
| **Unannotated Tool Dispatch** | Succeeds and assigns **Unknown** label state to its output. |
| **Unregistered Tool Dispatch** | Refused directly by the engine before execution. |
| **Requirement Check (`requires`)** | Fails closed when consuming an **Unknown** label value. |
| **Child Merge Boundary** | Holds **Unknown** child returns until established by a registered cast. |

An Unknown label state does not halt execution until a tool contract's `requires` clause explicitly checks the value. To resolve an **Unknown** state, deployments register a **cast** component that assigns concrete labels based on static rules or external evaluation services. This design allows deployments to start with a few high-risk tool annotations and incrementally expand policy coverage over time.

## Host context control determines available remedies

OpenAPPA runs at an execution boundary where it can inspect trajectory context and withhold tool results prior to model ingestion. Basic MCP gateways observe tool calls without context, whereas host harnesses and inference proxies paired with pre-dispatch hooks provide necessary withholding capabilities. The host's withholding capability determines which remedy types the engine can offer to an agent.

| Host Capability Level | Spec Classification | Enabled Remediation Features |
|---|---|---|
| Inspect dispatches, withhold nothing | Standard Deployment | All policy checks and refusals; no result-cleaning remedies. |
| Bound child context, capture single return | Context Control | Child trajectory isolation, raw result containment, and sanitizer merges. |
| Withhold result bytes from all contexts | Confining Deployment | Pending-cast holds and quarantined extraction children. |

A deployment lacking result-withholding capabilities still enforces policy checks and blocks unauthorized flows. However, it cannot offer sanitization remedies that hold back raw data, requiring policies to account for immutable context updates. As a result, agents in non-withholding deployments reach more direct refusal boundaries when attempting restricted operations.

## Model guarantees depend on four explicit assumptions

OpenAPPA guarantees hold strictly within defined system boundaries. The engine assumes a benign but confusable agent, untampered logs, valid component definitions, and that untrusted input arrives via ingested data. These four explicit assumptions bound the scope of automated policy enforcement:

| Assumption | Scope Boundary |
|---|---|
| **Benign but confusable agent** | Covert channels and intentional secret smuggling by the model are out of scope. |
| **Attacks arrive via ingested data** | Pre-trusted sources are trusted by definition; compromised trusted data is not caught. |
| **Registered components are correct** | Misconfigured authorities or permissive casts void their respective guarantees in the log. |
| **Log is durable and strictly ordered** | Trajectory verification depends on total ordering and persistence of log entries. |

Tool contracts specify behavioral bounds, while registered authorities handle cases beyond static algebra. Log persistence requires an append-only store with serialized appends across concurrent child branches. System administrators configure component mandates to reflect organization-specific security policies.

## Existing checks map onto registered engine components

Deployments migrate existing security logic into OpenAPPA by registering external endpoints as policy components with explicit mandates:

| Existing Security Logic | OpenAPPA Component | Enforcement Mandate / Ceiling |
|---|---|---|
| Permission prompts or auto-approvers | `builtin = "hitl"` Authority | Bounded by mandate gap coverage (`can_add_readers`, trust ranks). |
| Action evaluation models | Authority Resolver | Bounded by mandate gap coverage. |
| Content trustworthiness classifiers | Cast Resolver | Bounded by declared `may_cast` target states. |
| PII redact or injection filters | Sanitizer | Bounded by declared label transition (`from` $\rightarrow$ `to`). |
| Structural flow-control `if` statements | Tool Contracts | Handled directly by label algebra; imperative code deleted. |

Converting existing logic to registered components prevents prompt manipulation from bypassing checks, as queries evaluate structured tool calls rather than model-generated text. Component authority remains capped by registered mandates, ensuring that compromised classifiers cannot grant arbitrary permissions.

The last row highlights how OpenAPPA simplifies codebases. Policy rules governing system interactions—such as restricting CRM data from reaching external channels—are declared natively in tool contracts. This eliminates bespoke guardrail logic and conditional checks from application code.

## Operational cost is driven by approval volume

Adopting OpenAPPA involves three main operational tasks, with human review volume representing the primary operational constraint:

1. **Contract Definition:** Initial contracts are derived from existing tool schemas and ACLs. Reviewing a contract requires verifying a few declarative lines against tool behavior.
2. **Approval Volume Management:** Routing every restricted dispatch to human approvers causes approval fatigue. OpenAPPA mitigates volume by allowing agents to accept narrowings autonomously, consulting authorities only when dispatches exceed trajectory bounds.
3. **Incremental Coverage:** Policy enforcement scales incrementally by annotating high-risk tools first and defining casts as additional controls are required.

## Next steps

- [Reading a policy](/docs/contracts): Guide to reviewing and writing policy configuration.
- [AgentDojo harness](/docs/agentdojo): Running OpenAPPA against prompt-injection benchmarks.
- `docs/spec.md`: Normative specification containing all formal rule identifiers.
- `docs/rationale.md`: Design rationales behind algebraic invariants and label dimensions.
