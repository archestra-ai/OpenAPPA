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
   Attached to every running trajectory. A label tracks audience (which reader IDs are authorized to receive the trajectory's data) and trust rank (whether data comes from a vetted internal source or untrusted external data).

2. **Tool Contracts** (`delta` & `requires`)  
   Declarative rules configured per tool. Reading data restricts the trajectory's label (`delta`), while invoking an outbound tool verifies that the destination is permitted by the trajectory's current label (`requires`).

3. **Policy Remedies** (`remedy_plans`)  
   When a proposed tool dispatch exceeds the trajectory's current permissions, OpenAPPA returns a structured refusal containing actionable remedy plans:
   - Narrowing: accept restricted reach to continue internal tasks.
   - Sanitizers: clean data through a registered sanitizer to preserve reach.
   - Authorities: request targeted approval (e.g. human-in-the-loop) for an out-of-bounds call.
   - Child Branches: spin off a sub-execution to isolate sensitive reads from the main workflow.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, **data cannot be laundered** by passing it through intermediate steps or LLM prompts. Reading internal system records permanently marks the execution context as internal, and ingesting untrusted external data permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: an `authority` can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is computed directly from all values admitted so far—combining tool result restrictions and sanitized derivations—eliminating the need to re-evaluate full trajectory history:

```ts
label = admittedLabels.reduce(narrow, startingLabel)   // narrow only ever restricts
```

This monotonic structure provides a **formally provable non-interference guarantee**: because label transitions strictly narrow permitted reader sets over an execution trace, sensitive data is mathematically prevented from leaking to unauthorized destinations across arbitrary multi-step tool sequences.

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

The policy declares the security bounds. The deployment specifies who executes them in a separate `[externals]` table (e.g. binding `user` to a human approval prompt or `remove_pii` to an HTTP sanitizer endpoint):

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

Reading untrusted external files, third-party APIs, or confidential internal records normally restricts the entire agent session. Child trajectories isolate these label modifications within host-managed sub-executions.

A child process can read and reason over raw, untrusted data in its own sandboxed context without restricting the parent. When the child completes, it returns only a clean, bounded answer across the merge boundary. The main agent stays clean and retains its full reach to interact with public tools. Parent and child branches share a single append-only log so that all sends and approvals remain globally auditable.

## Engine refusals enumerate every valid remedy

Traditional guardrails act like a brick wall: they throw a generic exception that leaves the agent confused, trapped in retry loops, or crashed. OpenAPPA acts like a detour sign.

When an action cannot proceed as proposed, OpenAPPA returns a typed refusal listing the exact prerequisites needed to proceed safely: requesting authority approval, cleaning data with a sanitizer, running a prerequisite tool, or accepting a narrowing prompt. The agent takes the structured hint, executes the remedy, and completes its task.

:::fig-remedy-plan:::

```ts
{ outcome: "block",
  requirement_gaps: [...],  // unmet entries from `requires`
  narrowing: {...},         // present when the call's own delta narrows
  remedy_plans: [...] }     // valid remedy plans executable by id or tool call
```

A non-empty remedy list indicates that candidate paths exist, though external components may still decline a requested ruling. When an authority denies a request, that denial is appended to the log to prevent repeating the request for that specific call.

## Declarative contracts and annotators

Tool contracts are strictly declarative TOML. Instead of writing imperative access checks across code, developers declare tool requirements (`requires`), label restrictions (`delta`), and side effects (`effects`).

Every released tool call carries one complete annotation — its `delta`, its `requires`, and the effects it emits, with every dimension concrete. The policy produces that annotation in one of two ways:

- **Static declaration**: The `[[tool]]` entry writes the whole contract, and every call to the tool carries it.
- **Annotator**: Where the contract depends on the call parameters, the `[[tool]]` entry names a registered **annotator**. The annotator reads selected call data and returns the complete contract bounded by its **mandate**.

An Annotator declaration can include a trusted policy `hint`. The hint defines policy-specific values and the criteria for selecting them, but cannot expand the mandate. Every artifact identifies the proposed tool and includes its policy description (when present). An input mapping can restrict which argument values cross the consult boundary.

An annotation is pinned to the exact call it was produced for. A sanitizer rewrite that changes the arguments is annotated afresh, so no call ever runs under another call's annotation, and replay reconstructs every decision without consulting an annotator again.

An annotator that gives no valid answer — no route to it, a timeout, a malformed or out-of-mandate answer — stops the call before it runs. That refusal is operational, not a policy denial: the call was never judged, and nothing is appended to the log.

*(For complete syntax on ordered contracts, argument matching, and annotator declarations, see the [Policy reference](/contracts).)*

## A wildcard annotator covers the tools the policy never names

Real-world systems rarely annotate every tool up front. OpenAPPA supports partial coverage without a partial label: incompleteness lives at the policy boundary, never inside the algebra, so every admitted value carries one concrete label and every check has a two-valued answer.

A policy covers a proposed call in exactly one of these ways:

| Proposed call | What decides it |
|---|---|
| **Declared tool** | The first matching `[[tool]]` contract, written in full in the policy. |
| **Annotator-backed tool** | The annotator the matching `[[tool]]` entry names, answering the complete contract for this call. |
| **Any other tool, with a wildcard** | The wildcard entry `name = "*"` routes the call to its annotator, and the call is annotated like any other. |
| **Any other tool, without a wildcard** | The call is refused before it runs: the tool is not declared and no wildcard covers it. The refusal is typed and operational, not a policy denial. |

The wildcard entry carries no static contract and no metadata: it exists only to name the annotator that answers for the long tail. An exact declaration always wins over it. This lets teams annotate high-risk tools first and expand coverage incrementally — five declared tools and one wildcard annotator cover a deployment whose remaining tools the policy never names.

To inspect data before the LLM sees it, the deployment can list a tool in `confined_results`: the host then withholds the raw result, and a `tool_output` sanitizer's derivation can be admitted in its place. If the admitted label restricts the trajectory, OpenAPPA offers the agent the narrowing choice before delivery.

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
| Content scanners & argument-aware trust, audience, and review classifiers | Annotator (endpoint, command, or model builtin) inside its declared mandate |
| PII redactors & sanitizers | Sanitizer (`builtin = "redact-email"`, endpoint, command, or model builtin) |
| Directory / IAM group lookups | Membership Resolver |
| Imperative `if/else` access checks | Tool Contracts (`delta` & `requires`) |

Crucially, an authority or sanitizer can do only what its `permits` declares, and an annotator can answer only inside its declared mandate. Even if a third-party scanner or classifier makes a mistake, it cannot grant permissions beyond its pre-configured bounds.

## Next steps

- [Reading a policy](/contracts): Guide to reviewing and writing policy configuration.
- [Benchmarks](/evaluation): Empirical paper results on multi-step workflows and bench-corp.
- [OpenAPPA Paper](/paper): Formal information-flow model, theorems, and experimental methodology.
