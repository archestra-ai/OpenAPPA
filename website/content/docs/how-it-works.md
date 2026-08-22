---
title: How it works
category: Get started
order: 2
description: Deterministic security guarantees, flow tracking, and how agents self-correct.
---

## OpenAPPA enforces information-flow policy proactively

OpenAPPA sits between an agent and its tools to answer one question before every action: *is this data allowed to go to this destination?*

Powered by **APPA** (Agentic Permissions Policy Algebra), it provides a formal system to track data sensitivity and trust as an agent executes. When an action would violate policy, OpenAPPA does not simply throw a generic error. Instead, it calculates exact label flow and presents the agent with valid, actionable remedy plans—such as requesting human approval, scrubbing sensitive fields, or isolating reads in a sub-execution—so the agent can self-correct and finish its task safely.

Because policy checks happen prospectively (before actions execute), sensitive data is never exposed to unauthorized tools, and the agent is never left stranded mid-workflow.

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

### Declarative policy rules

Policy definitions remain strictly declarative TOML configurations. Instead of writing static allow or block rules for every tool interaction, developers declare tool contracts—specifying what permissions a tool requires (`requires`), how its output restricts security labels (`delta`), and what side effects it causes (`effects`). From these contracts, OpenAPPA automatically derives whether an action is permitted, blocked, or remediable across multi-step agent workflows.

Dynamic judgment—such as regex filters, ML classifiers, or human approval queues—lives in registered components. Authorities and sanitizers run as HTTP endpoints (`resolver`) or in-process modules (`builtin`). Their `mandate` bounds their power. Casts declare a fixed label or use a resolver under a `may_cast` ceiling.

A tool can also use dynamic resolvers that classify each proposed call before dispatch. A resolver declares the inputs it reads and the results it returns, each result named for the one contract field it establishes: `delta.trust` and `delta.audience` for the output label, and `requires.trust`, `requires.audience`, and `requires.attention` for call-time constraints. A tool maps every declared input from the proposed call — its complete self, its name, its description, its arguments, or one argument — and points its own fields at the results it wants. Each field holds one value, written or resolved, so ownership is exclusive and requirements do not combine across resolvers. The validated answer is pinned to the exact value its resolver received, travels with the call into the record, and is revalidated — never re-asked — on replay.

A resolver is implemented either by an HTTP endpoint or by an in-process builtin — the same choice authorities and sanitizers offer. Whichever it is, every request carries the policy's trust chain and the attention marks named by authority mandates: trust answers must select a rank from that chain, and attention answers must select literal marks from that attended set, preserving per-mark authority routing. A resolver failure or an out-of-policy value produces no evidence and stops the check operationally — never a policy denial. A resolver has no mandate or ceiling of its own, so its returned evidence is part of the trusted deployment boundary.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, data cannot be laundered by passing it through intermediate steps or LLM calls. Reading internal system records permanently marks the execution context as internal, and ingesting unvetted web content permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: a component named `authority` can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is calculated directly as a functional fold over the labels of all previously admitted values — a tool result's declared restriction, or a sanitizer derivation's cleaner label when one was admitted in its place — eliminating the need to replay trajectory history:

```ts
label = admittedLabels.reduce(narrow, startingLabel)   // narrow only ever restricts
```

## Reading data limits future actions

OpenAPPA evaluates tools *proactively before dispatch*, informing the agent of lost reach before data enters its context. Reading internal data restricts future steps to internal context, making public destinations unavailable unless explicitly approved or sanitized.

This pre-fetch choice is presented as a **narrowing** stop. If the agent accepts the narrowing, the choice is logged and the call proceeds. Subsequent steps at the same restriction level proceed without repeating prompts. Alternatively, a registered **sanitizer** (such as a PII scrubber) can derive a clean output to preserve public reach.

## Sub-agents isolate sensitive reads

Child trajectories isolate label modifications within host-managed sub-executions. A child starts with the parent trajectory's current label, but any data read within the child narrows the child's label exclusively. When the child completes, it returns a single value across its boundary that folds into the parent trajectory like any other tool read. If that raw return would narrow the parent, the merge stops at the boundary until accepted directly or cleaned through a registered `sanitizer`. Parent and child branches share a single append-only log so that all sends and approvals remain globally auditable.

## Worked example: preserve reach or approve the exact call

To illustrate policy enforcement, consider an agent configured with three tools: `get_ticket_from_crm`, `send_email`, and `file_github_issue`. The CRM tool contract declares a `delta` that restricts the trajectory to internal reach, `send_email` requires the recipient to match the trajectory audience, and `file_github_issue` requires public reach.

```toml
[[tool]]
name  = "get_ticket_from_crm"
delta = { audience = { exactly = ["internal"] } }   # "internal" is a single reader id

[[tool]]
name       = "send_email"                  # send_email(body, recipient)
parameters = { type = "object", properties = { recipient = { type = "string" }, body = { type = "string" } }, required = ["recipient", "body"] }
requires   = { audience = { includes = ["$recipient"] } }   # $recipient reads the required string argument
delta      = {}                            # annotated: the result carries nothing
effects    = ["egress"]

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

[[authority]]                              # who can approve the auditor mail
name    = "user"

[authority.mandate]
can_cover_readers = { may_add = ["public"] } # may cover any recipient
```

The policy names the components. The deployment says who performs them, in a
separate `[externals]` table — so swapping a redactor or moving approval to a
person changes no policy:

```toml
[externals.sanitizers.remove_pii]
url = "https://pii.corp/redact"

[externals.authorities.user]
builtin = "hitl"                           # ask a person
```

The trajectory begins at the deployment's starting label — `{public, trusted}` unless the `[deployment]` table declares otherwise — recorded once on the opening record; user input and other principal context admit nothing to the fold. When the agent calls `get_ticket_from_crm()`, OpenAPPA intercepts the dispatch before execution. The engine offers three operational paths, and the block names all of them:

| Execution Path | Trajectory Label Impact | Downstream Dispatch Impact |
|---|---|---|
| **Accept Narrowing** | Parent becomes `internal`. | `file_github_issue` is blocked; `send_email` requires authority approval. |
| **Sanitize the Result** | Parent remains `{public, trusted}`. | The raw ticket is withheld from the model; `remove_pii`'s derivation is admitted in its place. |
| **Child Branch + Sanitizer** | Parent remains `{public, trusted}`; child narrows to `internal`. | The child reads the raw ticket and returns the sanitized derivation across the merge. |

:::fig-two-endings:::

**Result Sanitization** keeps raw data out of model context by deriving a clean output before ingestion. **Child Branching** lets a sub-execution read and reason over raw content, sanitizing only what crosses back into the parent trajectory.

If emailing raw CRM data to an external auditor is required, the agent accepts the narrowing to `internal`. When `send_email(ticket, auditor@…)` subsequently runs, OpenAPPA detects that `auditor@…` is not in the `internal` audience, blocks dispatch, and generates a human approval (`user`) remedy plan. Once approved, the email dispatches and the event is logged.

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
| **Requirement Check (`requires`)** | Drives the cast registered for the value, then checks the label it established; fails closed when no cast answers. |
| **Child Merge Boundary** | Unknown child returns merge like any read: unresolved identities cross while every known restriction holds. Registered casts resolve them where the return policy consumes the dimension. |

An Unknown label state does not halt execution until a tool contract's `requires` clause explicitly checks the value. At that point the engine drives the **cast** registered for the value — a component that assigns its complete label from a fixed declaration or an external classifier — and decides on the answer it establishes. A value no registered cast reaches stays Unknown, and the call that needed it is refused. This design allows deployments to start with a few high-risk tool annotations and incrementally expand policy coverage over time.

A deployment that would rather inspect the data before the model sees it declares the pending dimension on the tool itself, with `delta = { trust = "unknown" }`. The tool runs, the runtime holds its raw result back, and the cast reads the bytes the model has not: a non-restricting label releases the result, and a restricting one is offered to the agent as a narrowing to accept.

## Deploy at the gateway alone, or add components for full coverage

OpenAPPA's baseline host is an inference gateway: point every agent's model base URL at it and have the client carry one trajectory token — the only client-side changes. The engine checks each proposed tool call while it still holds the model's response, so a refused call never reaches the agent framework. The token ties each request to its conversation: the gateway mints one when a request starts a new trajectory, returns it in the response, and the client echoes it on every continuation. In this setup, input sanitizers work in full — the framework never sees pre-substitution arguments. Output sanitizers and pending casts keep raw results away from the model. Sub-agent spawning is governed by an ordinary contract on the spawn tool.

The pure gateway leaves some vectors open, and OpenAPPA's posture is to allow and declare rather than remove: tool execution is assumed faithful rather than enforced, raw results still exist on the framework's side, and sub-agent branching stays off. The deployment declaration names each open vector explicitly and auditably, so a technology leader can review exactly what remains. A policy construct that names a feature the deployment does not cover is refused at load with the missing coverage named, never degraded silently.

Each optional component closes a named vector:

| Feature | What it is and why | How it can be implemented |
|---|---|---|
| **Session identity** | Bind each request to its trajectory; labels accumulate per trajectory, so a wrong bind forgets restrictions | A harness hook that names the session on every event, or a gateway-minted trajectory token echoed by the framework |
| **Execution enforcement** | The executed call is the approved call, exactly once | A tool gateway that matches each call to a one-use grant (remote tools); pre-tool hooks (local tools); neither — execution stays assumed and is a declared open vector |
| **Raw withholding** | Output sanitizing and pending casts: the model — or nobody — sees the raw result | Gateway swap on the next request (the model never sees it; the framework's machine does); tool-gateway rewrite (the raw never reaches the framework); a post-tool hook replaces it before storage, though the framework process briefly held it |
| **Branching** | Children inherit the parent's restrictions and return only through a checked, sanitizable `submit_result` | Harness hooks plus an agent adapter; sub-agent traffic routed and registered through the gateway; neither — spawning is treated as egress and governed by contract |
| **Provider-run tools** | Tools the model provider executes inside the inference call — no pre-dispatch gate is possible | List them in the declaration: each result the response exposes is admitted like a tool result under the tool's declared label, and their outbound queries are a declared open vector. A surface whose results the response hides cannot be mediated — it is refused or declared open. Any surface the declaration does not list is stripped from the request or the request is refused — either way the vector is closed |

## Model guarantees depend on five explicit assumptions

OpenAPPA guarantees hold strictly within defined system boundaries. The engine assumes a benign but confusable agent, untampered logs, valid component definitions, a well-behaved harness where execution is not enforced, and that untrusted input arrives via ingested data. These five explicit assumptions bound the scope of automated policy enforcement:

| Assumption | Scope Boundary |
|---|---|
| **Benign but confusable agent** | Covert channels and intentional secret smuggling by the model are out of scope. |
| **Attacks arrive via ingested data** | Pre-trusted sources are trusted by definition; compromised trusted data is not caught. |
| **Registered components are correct** | Misconfigured authorities or permissive casts void their respective guarantees. |
| **Log is durable and strictly ordered** | Trajectory verification depends on total ordering and persistence of log entries. |
| **The harness executes faithfully where unenforced** | For tools without a tool gateway or hook, the framework is assumed to run approved calls unchanged, once, and to echo conversations honestly. A visible break is refused, with an operator alert recommended — not defended against. |

In short: declarative tool contracts set automatic bounds, while registered components — services or builtins — handle dynamic cases like human approvals or content scanners. As long as the execution log is persisted, OpenAPPA ensures every decision remains provable and auditable under your team's security mandates.

## Existing checks map onto registered engine components

Deployments migrate existing security controls into OpenAPPA by registering them as policy components with explicit mandates or ceilings:

| Existing Security Control | OpenAPPA Component |
|---|---|
| Human review / HITL prompts | `builtin = "hitl"` Authority |
| Custom approval webhooks / LLM evaluators | Authority Resolver |
| Content scanners & trust classifiers | Cast Resolver |
| Argument-aware trust, audience, and review classification | Tool-level Dynamic Resolver (an HTTP endpoint or an in-process builtin) |
| Regex / ML PII scrubbers & redactors | Sanitizer (`builtin = "redact-email"`, your own builtin module, or a resolver) |
| Directory / IAM group lookups | Membership Resolver |
| Imperative `if/else` access checks | Tool Contracts |

Registering security controls as OpenAPPA components prevents prompt injections from bypassing policy. Because the engine evaluates structured tool dispatches at the boundary rather than model output text, a compromised model cannot talk its way past policy rules.

Crucially, authorities, sanitizers, and casts are capped by registered mandates and ceilings: even if an ML classifier or third-party scanner makes a mistake, it cannot grant permissions beyond its pre-configured limit. Resolvers carry no mandate. A membership resolver's answers are trusted directory input, and a tool-level dynamic resolver's answers are trusted classifier input over attacker-influenced arguments — the engine validates both for shape and policy vocabulary only (literal reader IDs, declared trust ranks, attended attention marks), never against a ceiling. Register a resolver only for a service you trust as part of the deployment itself. Furthermore, declaring tool bounds in contracts removes scattered guardrail scripts and imperative `if` checks from your application code.

## Operational impact: How OpenAPPA simplifies security

Adopting OpenAPPA shifts your security model from manual code checks to formal algebraic guarantees:

| Dimension | Traditional Approach (Before) | OpenAPPA Model (After) |
|---|---|---|
| **Policy Verification** | Unverifiable `if/else` checks: impossible to prove whether manual rules cover all multi-step tool sequences. | **Mathematical provability**: deterministic label algebra guarantees information-flow safety across any chain of tool calls. |
| **Workflow Scalability** | **Combinatorial explosion**: writing explicit rules for every tool and data combination. | **Declarative contracts**: define bounds per tool once (`delta`, `requires`), and the engine derives allowed flows dynamically. |
| **Human Review** | Approving every sensitive tool call manually, leading to severe reviewer fatigue. | **Targeted approvals**: agents accept narrowings autonomously for internal work, consulting humans *only* when dispatches exceed bounds. |
| **Adoption Pace** | All-or-nothing requirement: every endpoint must be audited before deployment. | **Incremental rollout**: annotate high-risk tools on day one; `Unknown` labels handle unannotated tools safely. |

## Next steps

- [Reading a policy](/contracts): Guide to reviewing and writing policy configuration.
- [Benchmarks](/evaluation): Empirical paper results on multi-step workflows and bench-corp.
