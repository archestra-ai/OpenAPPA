---
title: How OpenAPPA works
category: Get started
order: 2
description: The whole model in one sitting — what OpenAPPA guarantees and what it costs.
---

## OpenAPPA enforces information-flow policy before tool dispatch

OpenAPPA sits between an agent and its tools to answer one question before every action: *is this data allowed to go to this destination?*

APPA stands for **Agentic Permissions Policy Algebra**. It provides a formal system to track data sensitivity and trust as an agent executes. Instead of blindly blocking tasks when sensitive data is touched, APPA calculates exact label flow and presents the agent with valid, actionable remedies—such as isolating reads in child branches, invoking sanitizers, or requesting targeted approvals—so the agent stays productive without violating policy.

By evaluating checks *before* tool dispatch, OpenAPPA prevents agents from getting stranded mid-task. Every refusal presents clear options: approve the exact call, sanitize the data, satisfy a prerequisite, or accept a narrower label.

| Runtime State | Scope | Engine Semantics |
|---|---|---|
| **Label** | Trajectory | Tracks audience (allowed reader set) and trust rank (`suspicious` vs `trusted`). Reading data intersects audiences and takes the lowest trust rank. |
| **Log** | Trajectory | Append-only record of execution history, recording tool dispatches, narrowing acceptances, authority approvals, and denials. |

Policy definitions remain strictly declarative: contracts, authorities, sanitizers, and casts are simple data configurations. Developers do not write static allow or block rules for every tool interaction. Instead, they declare tool contracts—specifying what permissions a tool requires (`requires`), how its output restricts security labels (`delta`), and what side effects it causes (`effects`). From these contracts and the trajectory's current label, OpenAPPA automatically derives whether an action is permitted, blocked, or remediable across any multi-step workflow.

Imperative or model-based judgment—if necessary—lives outside the engine in registered external components such as regex filters, classification models, or human approval queues. These external components follow the exact same policy rules: an authority mandate caps which policy gaps a human or service can approve, while a sanitizer mandate bounds the exact label transition a scrubber can claim.

Either kind may also carry a `hint`: a sentence, in the operator's own words, on what the component is for. The hint travels with every remedy plan naming that component, so an agent choosing among plans reads stated purpose rather than a bare name, and a reviewer reads intent beside the mandate. A hint grants nothing—the mandate remains the only bound on power.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, data cannot be laundered by passing it through intermediate steps or LLM calls. Reading internal system records permanently marks the execution context as internal, and ingesting unvetted web content permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: a component named `authority` can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is calculated directly as a functional fold over all preceding deltas, eliminating the need to replay trajectory history:

```ts
label = deltasSoFar.reduce(narrow, startingLabel)   // narrow only ever restricts
```

## Reading data costs the agent reach

OpenAPPA stops an agent *before* a fetch that restricts its label, informing it of lost reach before data enters its context. Reading internal data does not leak information by itself, but it restricts all future steps in the trajectory to an internal context. Destinations requiring public reach become permanently unavailable, and previously unconstrained dispatches require explicit approval.

By evaluating the step before dispatch, OpenAPPA prevents scenarios where an agent ingests data only to discover three steps later that outbound dispatches are blocked. The engine presents this pre-fetch choice as a **narrowing** stop. If the agent accepts the narrowing, the acceptance is recorded in the log and the call proceeds. Subsequent steps that cause no additional restriction pass without stopping, so narrowing prompts occur once per level of increased restriction during run.

In deployments where the host can hold a raw tool result out of the model's context, a registered sanitizer offers a third path: the host runs the call, the sanitizer cleans the result, and only the cleaned derivation enters context — clearing the narrowing partially or entirely without a child branch.

## A child's narrowing dies with it

Child trajectories isolate label modifications within host-managed sub-executions. A child starts with the parent trajectory's current label, but any data read within the child narrows the child's label exclusively. When the child completes, it returns a single value across its boundary that folds into the parent trajectory like any other tool read. If that raw return would narrow the parent, the merge stops at the boundary until accepted directly or cleaned through a registered `sanitizer`. Parent and child branches share a single append-only log so that all sends and approvals remain globally auditable.

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

[[sanitizer]]                              # both sanitized routes cross this
name = "remove_pii"
on   = ["tool_output"]
hint = "Removes customer identities from a CRM record."   # advisory; grants nothing

[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[sanitizer.implementation]
resolver = { url = "https://pii.corp/redact", timeout_ms = 10000 }

[[authority]]                              # who can approve the auditor mail
name    = "user"
builtin = "hitl"

[authority.mandate]
can_cover_readers = { may_add = ["public"] } # may cover any recipient
```

The trajectory begins at `{public, trusted}` unless pre-existing context or user input introduces restricted labels. When the agent calls `get_ticket_from_crm()`, OpenAPPA intercepts the dispatch before execution. The engine offers three operational paths, and the block names all of them:

| Execution Path | Trajectory Label Impact | Downstream Dispatch Impact |
|---|---|---|
| **Accept Narrowing** | Parent becomes `internal`. | `file_github_issue` is blocked; `send_email` requires authority approval. |
| **Sanitize the Result** | Parent remains `{public, trusted}`. | The raw ticket is withheld from the model; `remove_pii`'s derivation is admitted in its place. |
| **Child Branch + Sanitizer** | Parent remains `{public, trusted}`; child narrows to `internal`. | The child reads the raw ticket and returns the sanitized derivation across the merge. |

:::fig-two-endings:::

The second and third paths differ in what the model gets to read. Sanitizing the result never shows anyone the raw ticket — the derivation is all that exists downstream. The child branch lets the child read the raw ticket and reason over it, and sanitizes only what crosses back. Choose the branch when the work itself needs the restricted content; choose the result sanitizer when the derivation is what you wanted anyway. Each sanitizer's `hint` states what its derivation drops, so that choice is informed.

If the goal is emailing raw CRM data to an external auditor, neither sanitized route applies and the agent accepts the narrowing in the parent trajectory. When `send_email(ticket, auditor@…)` subsequently runs, the engine checks whether `auditor@…` is in the `internal` audience. Because it is not, OpenAPPA blocks the call and generates an authority remedy plan for `user`. Once `user` approves the request, the email dispatches and the egress event enters the log. The trajectory label remains `internal`, ensuring that subsequent emails to unapproved recipients require separate authority rulings.

## Engine refusals enumerate every valid remedy

When OpenAPPA refuses a flow, it doesn't leave the agent stranded. An empty remedy list proves that the call is unreachable within the planner's modeled transitions under the active policy configuration and log. When candidate paths exist, OpenAPPA returns a structured object enumerating every valid alternative available from the registered components and deployment capabilities: requesting a policy approval, cleaning data with a sanitizer, executing a prerequisite tool call, or accepting a narrowing prompt.

:::fig-remedy-plan:::

Because every remedy except narrowing acceptance derives from a registered component, the engine presents all valid options in a single refusal object:

```ts
{ outcome: "block",
  requirement_gaps: [...],  // unmet entries from `requires`
  narrowing: {...},         // present when the call's own delta narrows
  unestablished: [...],     // values whose labels could not be established
  remedy_plans: [...] }     // valid remedy plans executable by id or tool call
```

A non-empty remedy list indicates that candidate paths exist, though external components may still decline a requested ruling. When an authority denies a request, that denial is appended to the log. The refusal proof remains scoped to the current configuration, active component responses, and recorded denials for that specific call.

## Unknown labels propagate until a requirement checks them

Real-world deployments can be difficult to annotate in a single pass. Similar to gradual typing in Python or TypeScript codebases, OpenAPPA supports partial annotation—delivering immediate value from day one. 

Unannotated tools return data with an **Unknown** label state, representing unverified classification rather than a specific trust rank. Unknown labels propagate through trajectory operations, causing any trajectory that ingests an unknown value to become Unknown. Unregistered tool calls are refused directly rather than returning Unknown values.

| Execution Context | Impact of Unknown State |
|---|---|
| **Unannotated Tool Dispatch** | Succeeds and assigns **Unknown** label state to its output. |
| **Unregistered Tool Dispatch** | Refused directly by the engine before execution. |
| **Requirement Check (`requires`)** | Fails closed when consuming an **Unknown** label value. |
| **Child Merge Boundary** | Unknown child returns merge and absorb like any read; registered casts may resolve them at the boundary. |

An Unknown label state does not halt execution until a tool contract's `requires` clause explicitly checks the value. To resolve an **Unknown** state, deployments register a **cast** component that assigns concrete labels based on static rules or external evaluation services. This design allows deployments to start with a few high-risk tool annotations and incrementally expand policy coverage over time.

## Model guarantees depend on four explicit assumptions

OpenAPPA guarantees hold strictly within defined system boundaries. The engine assumes a benign but confusable agent, untampered logs, valid component definitions, and that untrusted input arrives via ingested data. These four explicit assumptions bound the scope of automated policy enforcement:

| Assumption | Scope Boundary |
|---|---|
| **Benign but confusable agent** | Covert channels and intentional secret smuggling by the model are out of scope. |
| **Attacks arrive via ingested data** | Pre-trusted sources are trusted by definition; compromised trusted data is not caught. |
| **Registered components are correct** | Misconfigured authorities or permissive casts void their respective guarantees. |
| **Log is durable and strictly ordered** | Trajectory verification depends on total ordering and persistence of log entries. |

In short: declarative tool contracts set automatic bounds, while external components handle dynamic cases like human approvals or content scanners. As long as the execution log is persisted, OpenAPPA ensures every decision remains provable and auditable under your team's security mandates.

## Existing checks map onto registered engine components

Deployments migrate existing security controls into OpenAPPA by registering them as policy components with explicit mandates or ceilings:

| Existing Security Control | OpenAPPA Component |
|---|---|
| Human review / HITL prompts | `builtin = "hitl"` Authority |
| Custom approval webhooks / LLM evaluators | Authority Resolver |
| Content scanners & trust classifiers | Cast Resolver |
| Regex / ML PII scrubbers & redactors | Sanitizer |
| Directory / IAM group lookups | Membership Resolver |
| Imperative `if/else` access checks | Tool Contracts |

Registering security controls as OpenAPPA components prevents prompt injections from bypassing policy. Because the engine evaluates structured tool dispatches at the boundary rather than model output text, a compromised model cannot talk its way past policy rules.

Crucially, external components are capped by registered mandates: even if an ML classifier or third-party scanner makes a mistake, it cannot grant permissions beyond its pre-configured ceiling. A membership resolver carries no mandate — its answers are trusted directory input, and the model refuses only the reserved `public` reader in an answer. Furthermore, declaring tool bounds in contracts removes scattered guardrail scripts and imperative `if` checks from your application code.

## Operational impact: How OpenAPPA simplifies security

Adopting OpenAPPA shifts your security model from manual code checks to formal algebraic guarantees:

| Dimension | Traditional Approach (Before) | OpenAPPA Model (After) |
|---|---|---|
| **Policy Verification** | Unverifiable `if/else` checks: impossible to prove whether manual rules cover all multi-step tool sequences. | **Mathematical provability**: deterministic label algebra guarantees information-flow safety across any chain of tool calls. |
| **Workflow Scalability** | **Combinatorial explosion**: writing explicit rules for every tool and data combination. | **Declarative contracts**: define bounds per tool once (`delta`, `requires`), and the engine derives allowed flows dynamically. |
| **Human Review** | Approving every sensitive tool call manually, leading to severe reviewer fatigue. | **Targeted approvals**: agents accept narrowings autonomously for internal work, consulting humans *only* when dispatches exceed bounds. |
| **Adoption Pace** | All-or-nothing requirement: every endpoint must be audited before deployment. | **Incremental rollout**: annotate high-risk tools on day one; `Unknown` labels handle unannotated tools safely. |

## Next steps

- [Reading a policy](/docs/contracts): Guide to reviewing and writing policy configuration.
- [Evaluating OpenAPPA](/docs/evaluation): Empirical paper results on multi-step workflows and bench-corp.
- `docs/spec.md`: Normative specification containing all formal rule identifiers.
- `docs/rationale.md`: Design rationales behind algebraic invariants and label dimensions.
