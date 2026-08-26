---
title: How it works
category: Deep Dive
order: 2
description: Deterministic security guarantees, flow tracking, and how agents self-correct.
---

## OpenAPPA enforces information-flow policy proactively

OpenAPPA sits between an agent and its tools to answer one question before every action: *is this data allowed to go to this destination?*

Powered by **APPA** (Agentic Permissions Policy Algebra), it provides a formal system to track data sensitivity and trust deterministically across heterogeneous tools. Security context, policy labels, and audit trails flow entirely out-of-band—outside the agent's prompt and token stream. Because enforcement lives at the runtime boundary rather than inside the model's context, prompt injections cannot alter or bypass policy rules.

When an action cannot proceed as proposed, OpenAPPA does not simply throw a dead-end error. It returns a structured **remedy plan**—such as requesting human approval, scrubbing sensitive fields, or isolating reads in a sub-execution—giving the agent the exact playbook to self-correct and finish its task safely.

Because policy checks happen prospectively before tools run, sensitive data is never exposed to unauthorized tools, and the agent is never left stranded mid-workflow.

### The core mental model

OpenAPPA operates on three runtime concepts:

1. **Security Labels** (`label`)  
   Attached to every running trajectory. A label tracks audience (which reader IDs are authorized to receive the trajectory's data) and trust rank (whether data comes from a vetted internal source or unvetted web content).

2. **Tool Contracts** (`delta` & `requires`)  
   Declarative rules configured per tool. Reading data restricts the trajectory's label (`delta`), while invoking an outbound tool verifies that the destination is permitted by the trajectory's current label (`requires`).

3. **Policy Remedies** (`remedy_plans`)  
   When a proposed tool dispatch exceeds the trajectory's current permissions, OpenAPPA returns a structured refusal containing actionable remedy plans:
   - Narrowing: accept restricted reach to continue internal tasks.
   - Sanitizers: run a redactor or scrubber to clean data and preserve reach.
   - Authorities: request targeted approval (e.g. human-in-the-loop) for an out-of-bounds call.
   - Child Branches: spin off a sub-execution to isolate sensitive reads from the main workflow.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, **data cannot be laundered** by passing it through intermediate steps or LLM prompts. Reading internal system records permanently marks the execution context as internal, and ingesting unvetted web content permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: an `authority` can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is computed directly from all values admitted so far—combining tool result restrictions and sanitized derivations—eliminating the need to re-evaluate full trajectory history:

```ts
label = admittedLabels.reduce(narrow, startingLabel)   // narrow only ever restricts
```

## Worked example: preserve reach or approve the exact call

To see how this works in practice, consider an agent configured with three tools: `get_ticket_from_crm`, `send_email`, and `file_github_issue`:

```toml
[[tool]]
name  = "get_ticket_from_crm"
delta = { audience = ["internal"] }   # reading CRM data restricts the trajectory to "internal"

[[tool]]
name       = "send_email"
parameters = { type = "object", properties = { recipient = { type = "string" }, body = { type = "string" } }, required = ["recipient", "body"] }
requires   = { audience = { contains = ["$recipient"] } }   # recipient must be in current audience
delta      = {}
effects    = ["egress"]

[[tool]]
name     = "file_github_issue"
requires = { audience = { contains = ["public"] } }         # requires public reach
delta    = {}
effects  = ["egress", "mutation"]

[[sanitizer]]
name = "remove_pii"
on   = ["tool_output"]
hint = "Removes customer identities from a CRM record."
[sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }         # declassifies internal to public

[[authority]]
name = "user"
[authority.permits]
audience_missing = ["public"]                                # user can approve public egress
```

<<<<<<< HEAD
The policy names the components. The deployment says who performs them, in a
separate `[externals]` table, one `[externals.<kind>.<name>]` entry per
registered name — so swapping a redactor or moving approval to a person
changes no policy:
||||||| parent of 408e63a (docs: restructure how-it-works for high digestibility with early worked examples)
The policy names the components. The deployment says who performs them, in a
separate `[externals]` table — so swapping a redactor or moving approval to a
person changes no policy:
=======
The policy declares the security bounds. The deployment specifies who executes them in a separate `[externals]` table (e.g. binding `user` to a human approval prompt or `remove_pii` to an HTTP scrubber):
>>>>>>> 408e63a (docs: restructure how-it-works for high digestibility with early worked examples)

```toml
[externals.sanitizers.remove_pii]
url       = "https://pii.corp/redact"
token_env = "APPA_PII_TOKEN"               # sent as a bearer token

[externals.authorities.user]
builtin = "hitl"                           # ask a person
```

### What happens when the agent reads a ticket?

When the agent calls `get_ticket_from_crm()`, OpenAPPA intercepts the dispatch before execution and presents three clear paths:

| Execution Path | Trajectory Label Impact | Downstream Dispatch Impact |
|---|---|---|
| **Accept Narrowing** | Trajectory becomes `internal`. | `file_github_issue` is blocked; `send_email` requires authority approval for external recipients. |
| **Sanitize the Result** | Trajectory stays `{public, trusted}`. | Raw ticket is withheld from the model; `remove_pii`'s sanitized derivation is admitted in its place. |
| **Child Branch + Sanitizer** | Parent stays `{public, trusted}`; child narrows to `internal`. | Child reads raw ticket, reasons over it, and returns the sanitized derivation across the merge boundary. |

:::fig-two-endings:::

If the agent accepts narrowing to `internal` and later attempts `send_email(body, "auditor@external.com")`, OpenAPPA detects that `auditor@external.com` is outside the `internal` audience, halts dispatch, and returns a remedy plan pointing to the `user` authority. Once the human approves, the email dispatches and the event is permanently logged.

## Sub-agents isolate sensitive reads

Reading an untrusted web page, a poisoned forum post, or a confidential HR record normally restricts the entire agent session. Child trajectories isolate these label modifications within host-managed sub-executions.

A child process can read and reason over raw, untrusted data in its own sandboxed context without restricting the parent. When the child completes, it returns only a clean, bounded answer across the merge boundary. The main agent stays clean and retains its full reach to interact with public tools. Parent and child branches share a single append-only log so that all sends and approvals remain globally auditable.

## Engine refusals enumerate every valid remedy

Traditional guardrails act like a brick wall: they throw a generic exception that leaves the agent confused, trapped in retry loops, or crashed. OpenAPPA acts like a detour sign.

When an action cannot proceed as proposed, OpenAPPA returns a typed refusal listing the exact prerequisites needed to proceed safely: requesting authority approval, scrubbing data with a sanitizer, running a prerequisite tool, or accepting a narrowing prompt. The agent takes the structured hint, executes the remedy, and completes its task.

:::fig-remedy-plan:::

```ts
{ outcome: "block",
  requirement_gaps: [...],  // unmet entries from `requires`
  narrowing: {...},         // present when the call's own delta narrows
  unestablished: [...],     // sources whose unresolved dimensions no registered cast reaches
  remedy_plans: [...] }     // valid remedy plans executable by id or tool call
```

A non-empty remedy list indicates that candidate paths exist, though external components may still decline a requested ruling. When an authority denies a request, that denial is appended to the log to prevent repeating the request for that specific call.

## Declarative contracts and dynamic resolvers

Tool contracts are strictly declarative TOML. Instead of writing imperative access checks across code, developers declare tool requirements (`requires`), label restrictions (`delta`), and side effects (`effects`).

Where policy needs dynamic runtime context (such as evaluating document ACLs, recipient memberships, or tool-argument classifications), tools attach **dynamic resolvers**:

- **Input mapping**: A resolver receives declared arguments from the proposed call (e.g. `subject = "document.pdf"`).
- **Contract fields**: The resolver supplies specific fields of the tool contract (`delta.audience`, `requires.trust`, etc.).
- **Pinned verification**: A resolver's validated answer is permanently pinned to the exact inputs it evaluated, guaranteeing deterministic execution logs and immutable audit replay.

*(For complete syntax on ordered contracts, argument matching, and resolver schemas, see the [Policy reference](/contracts).)*

## Unknown labels propagate until a consumer checks them

Real-world systems rarely annotate every tool up front. Similar to gradual typing in TypeScript or Python, OpenAPPA supports partial annotation.

Unannotated tools return data with an **Unknown** label state, representing unverified classification rather than a specific trust rank. A tool is unannotated when its contract has no `delta` key and no resolver establishes an output dimension; `delta = {}` is the opposite, declaring that the result carries no restriction. Unknown labels propagate through operations per dimension: the trajectory keeps every known restriction, and each dimension the value left unresolved stays Unknown for that source until a cast establishes it. Unregistered tool calls are refused directly before execution.

| Execution Context | Impact of Unknown State |
|---|---|
| **Unannotated Tool Dispatch** | Succeeds and assigns **Unknown** label state to its output. |
| **Unregistered Tool Dispatch** | Refused directly by the engine before execution. |
| **Requirement Check (`requires`)** | Drives the casts registered for the value, then checks the label the first admitted answer establishes. When no registered cast reaches the value, the call is blocked and the block names the source under `unestablished`. When a registered cast gives no answer, nothing is decided or recorded, and the call can be tried again. |
| **Child Merge Boundary** | Unknown child returns merge like any read: unresolved identities cross while every known restriction holds. Registered casts resolve them where the return policy consumes the dimension. |

An Unknown label state does not halt execution on its own: an unannotated result is admitted, and the trajectory keeps working, until a consumer of that dimension reads it — a tool contract's `requires` clause, a sanitizer's applicability or `permits` check, or a pending-cast admission. Each of those fails closed. When a `requires` clause checks the value, OpenAPPA triggers the registered **cast** for that value—resolving the label using a fixed rule or an external classifier—and validates the result against the cast's declared ceiling. If no cast reaches the value, it remains Unknown: the dependent call is blocked, and the block names the source under `unestablished`. If a registered cast gives no answer, nothing is decided or recorded, and the call can be tried again. This allows teams to annotate high-risk tools first and expand policy coverage incrementally.

To inspect data before the LLM sees it, a tool contract can declare a pending dimension with `delta = { trust = "unknown" }`. When configured in `confined_results`, the runtime withholds the raw result while the cast evaluates the value. If the cast resolves to a non-restricting label, the data is delivered directly; if it restricts the label, OpenAPPA offers the agent a narrowing choice before delivery.

## Deployment: Where OpenAPPA fits in your stack

You can drop OpenAPPA into your architecture at three levels:

| Deployment Option | How it works | Best for |
|---|---|---|
| **LLM Gateway** | Point your agent's `BASE_URL` to OpenAPPA. It intercepts tool calls directly in the inference stream. | Zero-code integration across existing agent stacks. |
| **Agent Middleware / Hooks** | Add pre-tool hooks inside your agent loop (e.g. Claude Code, LangChain, PydanticAI). | Local CLI tools and custom Python/TypeScript agents. |
| **Tool Proxy** | Run OpenAPPA in front of your remote APIs or MCP servers. | Shared enterprise tool infrastructure and microservices. |

## Threat model: What OpenAPPA protects

OpenAPPA is designed for real-world enterprise agent workflows:

- **What it protects against:** Prompt injections, poisoned external data, confused agent actions, and accidental data leaks across multi-step workflows.
- **How it stops attacks:** At the deterministic runtime boundary. Even if the LLM is completely tricked by an attacker, unauthorized tool calls physically cannot dispatch.
- **System boundaries:** Pre-vetted internal data is trusted by configuration. Custom authorities (like human review queues) are trusted within their declared permissions.
- **Auditability:** Every check, dispatch, and remedy decision is recorded in an append-only, tamper-evident log for post-hoc audit and deterministic replay.

## Migrating existing controls to OpenAPPA

You don't need to throw away existing security controls. OpenAPPA unifies them as declarative policy components:

| Existing Security Control | OpenAPPA Component |
|---|---|
| Human review / HITL prompts | `builtin = "hitl"` Authority |
| Custom approval webhooks / LLM evaluators | Authority (`url`, `command`, or model builtin) |
| Content scanners & trust classifiers | Cast (`url`, `command`, or model builtin) under a `may_cast` ceiling |
| Argument-aware trust, audience, and review classification | Dynamic Resolver (endpoint, command, or model builtin) |
| PII redactors & sanitizers | Sanitizer (`builtin = "redact-email"` or custom resolver) |
| Directory / IAM group lookups | Membership Resolver |
| Imperative `if/else` access checks | Tool Contracts (`delta` & `requires`) |

Crucially, an authority or sanitizer can do only what its `permits` declares, and a cast only what its `may_cast` ceiling allows. Even if a third-party scanner or classifier makes a mistake, it cannot grant permissions beyond its pre-configured ceiling.

## Operational impact: How OpenAPPA simplifies security

Adopting OpenAPPA shifts your security model from manual code checks to formal algebraic guarantees:

| Dimension | Traditional Guardrails (Before) | OpenAPPA Model (After) |
|---|---|---|
| **Policy Verification** | Unverifiable `if/else` checks: impossible to prove whether manual rules cover multi-step tool sequences. | **Mathematical provability**: deterministic label algebra guarantees information-flow safety across any tool sequence. |
| **Agent Reliability** | **Brick wall failures**: generic `403` exceptions crash agents and drop task completion to 41%. | **Guided recovery**: structured remedy plans guide agents around blocks, maintaining 89% task completion. |
| **Taint Containment** | **Coarse session locking**: reading one sensitive file permanently blocks all future public actions. | **Branch-isolated reach**: child sub-agents isolate risky reads without restricting parent capabilities. |
| **Adoption Pace** | All-or-nothing requirement: every endpoint must be audited before launch. | **Incremental rollout**: annotate high-risk tools on day one; `Unknown` labels handle unannotated tools safely. |

## Next steps

- [Reading a policy](/contracts): Guide to reviewing and writing policy configuration.
- [Benchmarks](/evaluation): Empirical paper results on multi-step workflows and bench-corp.
- [OpenAPPA Paper](/paper): Formal information-flow model, theorems, and experimental methodology.
