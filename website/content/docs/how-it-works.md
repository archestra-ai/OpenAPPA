---
title: How it works
category: Deep Dive
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

A root configuration can include reusable policy fragments. Root declarations run first. Included declarations follow in the listed order. Included files add declarations and named external bindings; they cannot replace root-wide settings or include more files.

Several contracts can name the same tool. OpenAPPA checks them in declaration order and uses the first matching argument pattern. A bare name is the fallback.

Dynamic judgment—such as regex filters, ML classifiers, or human approval queues—lives in registered components. Authorities and sanitizers run as HTTP endpoints (`resolver`) or in-process modules (`builtin`). Each declares what it `permits`, and that declaration bounds its power. Casts declare a fixed label or use a resolver under a `may_cast` ceiling. An authority may omit its deployment binding. It then returns no answer, so a remedy that names it cannot release a call.

A tool can also use dynamic resolvers that classify each proposed call before dispatch. A resolver declares the inputs it reads and the contract fields it owns through `returns`: `delta.trust` and `delta.audience` for the output label, and `requires.trust`, `requires.audience`, and `requires.attention` for call-time constraints. Attaching the resolver through `uses` assigns every declared field to it. The tool maps each declared input from the proposed call. Without an explicit mapping, the resolver receives the complete call: the tool name, its description when the policy declares one, and the arguments object.

A resolver that carries the stock Claude Code classifier names it on its declaration with `builtin = "claude-code"`. Every other resolver is bound by name to an HTTP endpoint or a Unix command under `[externals.dynamic.<name>]`. Every request carries the policy's trust chain and the attention marks that authorities name under `permits.attention`. Trust answers must select a rank from that chain. Attention answers must select literal marks from that set, which preserves per-mark authority routing. A resolver has no `permits` or ceiling of its own, so its returned evidence is part of the trusted deployment boundary.

On Unix systems, a local command receives one JSON request on standard input and returns one JSON answer on standard output. OpenAPPA invokes its argument list directly, without a shell. The shared external timeout and byte limit bound the complete exchange. OpenAPPA rejects a command binding when the platform cannot run it safely. A command failure returns no answer, so the tool does not run.

### A resolver's answer is pinned to the call it classified

Each contract field has a single source of truth. A field written in policy and a field supplied by a dynamic resolver cannot overlap, and requirements do not combine across resolvers. An unannotated dimension remains fail-closed (`Unknown`). History requirements (`effects`) are always static.

When a tool call is proposed, OpenAPPA evaluates attached resolvers immediately. The validated answer is pinned to the exact inputs the resolver received. This pinned answer stays with the call through requirement checks, remedy evaluation, human approval, dispatch, and execution logging. Replaying an execution log verifies the recorded answer against the same resolver inputs rather than re-querying the resolver. New tool proposals always consult resolvers freshly.

If an input-substitution sanitizer rewrites the arguments of a tool call and the rewritten arguments select the same ordered tool contract, the resolver classification of the call last consulted — the proposal, or an earlier rewrite that selected this contract — remains pinned to the call; the resolver is not consulted again. If the rewritten arguments select a different ordered tool contract, the rewrite is judged as a new call under that contract: its resolvers are consulted for the rewritten arguments, and its effects and requirements apply.

Because a `tool_input` sanitizer rewrites the entire argument payload without specifying which fields changed, the rewrite retains the classification assigned to the call last consulted. For example, a changed path or recipient keeps the original classification when it remains in the same ordered contract. You can restrict which tools a sanitizer can modify with its `tags`.

If a resolver fails to return a valid answer (e.g. timeout, network failure, or invalid response format), OpenAPPA halts execution with an operational error. It does not record a policy denial, and the tool does not execute.

## Labels only move one way

A tool contract declares a `delta` to define how fetching its result restricts the agent's current security label. A `delta` can only restrict permissions—it intersects allowed readers, lowers trust levels, or leaves the label unchanged.

Because permissions only tighten over time, data cannot be laundered by passing it through intermediate steps or LLM calls. Reading internal system records permanently marks the execution context as internal, and ingesting unvetted web content permanently drops its trust level.

Restricting permissions doesn't mean blocking external work: an `authority` can approve a specific outbound call without changing the overall label, or the agent can spin off a child execution to isolate sensitive reads from its main workflow.

:::fig-label-fold:::

The current label is computed directly from all values admitted so far—combining tool result restrictions and sanitized derivations—eliminating the need to re-evaluate full trajectory history:

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
delta = { audience = ["internal"] }   # "internal" is a single reader id

[[tool]]
name       = "send_email"                  # send_email(body, recipient)
parameters = { type = "object", properties = { recipient = { type = "string" }, body = { type = "string" } }, required = ["recipient", "body"] }
requires   = { audience = { contains = ["$recipient"] } }   # $recipient reads the required string argument
delta      = {}                            # annotated: the result carries nothing
effects    = ["egress"]

[[tool]]
name     = "file_github_issue"
requires = { audience = { contains = ["public"] } }
delta    = {}
effects  = ["egress", "mutation"]

[[sanitizer]]                              # both sanitized routes cross this
name = "remove_pii"
on   = ["tool_output"]
hint = "Removes customer identities from a CRM record."   # advisory; grants nothing

[sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[[authority]]                              # who can approve the auditor mail
name    = "user"

[authority.permits]
audience_missing = ["public"]              # a call missing any reader, up to public
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

The trajectory begins at the deployment's starting label — `{public, trusted}` unless the `[deployment]` table declares otherwise — recorded once on the opening record; user prompts and other initial context introduce no additional label restrictions. When the agent calls `get_ticket_from_crm()`, OpenAPPA intercepts the dispatch before execution. The engine offers three operational paths, and the block names all of them:

| Execution Path | Trajectory Label Impact | Downstream Dispatch Impact |
|---|---|---|
| **Accept Narrowing** | Parent becomes `internal`. | `file_github_issue` is blocked; `send_email` requires authority approval. |
| **Sanitize the Result** | Parent remains `{public, trusted}`. | The raw ticket is withheld from the model; `remove_pii`'s derivation is admitted in its place. |
| **Child Branch + Sanitizer** | Parent remains `{public, trusted}`; child narrows to `internal`. | The child reads the raw ticket and returns the sanitized derivation across the merge. |

:::fig-two-endings:::

**Result Sanitization** keeps raw data out of model context by deriving a clean output before ingestion. **Child Branching** lets a sub-execution read and reason over raw content, sanitizing only what crosses back into the parent trajectory.

If emailing raw CRM data to an external auditor is required, the agent accepts the narrowing to `internal`. When `send_email(ticket, auditor@…)` subsequently runs, OpenAPPA detects that `auditor@…` is not in the `internal` audience, blocks dispatch, and generates a human approval (`user`) remedy plan. Once approved, the email dispatches and the event is logged.

## Engine refusals enumerate every valid remedy

When OpenAPPA refuses an action, it does not leave the agent stranded. If no remedy plans exist, the action is fundamentally unreachable under the current policy. When valid alternatives exist, OpenAPPA returns a structured refusal listing every available remedy: requesting authority approval, scrubbing data with a sanitizer, running a prerequisite tool, or accepting a narrowing prompt.

:::fig-remedy-plan:::

Because every remedy except narrowing acceptance derives from a registered component, the engine presents all valid options in a single refusal object:

```ts
{ outcome: "block",
  requirement_gaps: [...],  // unmet entries from `requires`
  narrowing: {...},         // present when the call's own delta narrows
  unestablished: [...],     // sources whose unresolved dimensions no registered cast reaches
  remedy_plans: [...] }     // valid remedy plans executable by id or tool call
```

A non-empty remedy list indicates that candidate paths exist, though external components may still decline a requested ruling. When an authority denies a request, that denial is appended to the log to prevent repeating the request for that specific call.

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

To inspect data before the LLM sees it, a tool contract can declare a pending dimension with `delta = { trust = "unknown" }`. When configured in `confined_results`, the runtime withholds the raw result while the cast evaluates the payload. If the cast resolves to a non-restricting label, the data is delivered directly; if it restricts the label, OpenAPPA offers the agent a narrowing choice before delivery.

## Deploy at the gateway alone, or add components for full coverage

OpenAPPA's baseline deployment is an inference gateway: configure your agent framework to route model requests through the gateway and pass a trajectory token on each request. The gateway intercepts tool calls before returning the model's response to the agent, preventing refused calls from ever executing.

The gateway mints a trajectory token at the start of a conversation, returns it in the response, and tracks accumulated labels across turns. In this mode, input sanitizers rewrite tool arguments transparently, while output sanitizers and pending casts withhold raw data from the model.

A standalone gateway provides immediate protection, while optional deployment components can close remaining exposure vectors:

| Feature | Purpose | Implementation Options |
|---|---|---|
| **Session identity** | Binds requests to an execution trajectory so accumulated labels are tracked accurately | Harness hook identifying the session, or a gateway-minted trajectory token |
| **Execution enforcement** | Ensures only approved tool calls execute, exactly once | Tool proxy validating one-time execution tokens (remote tools), or pre-tool execution hooks (local tools) |
| **Raw withholding** | Prevents the model from seeing unsanitized outputs or unclassified data | Gateway payload replacement on the next request, tool proxy rewrites, or post-tool hooks |
| **Branching** | Isolates sub-agent reads and controls returns via `submit_result` | Harness lifecycle hooks with an agent adapter, or gateway-routed sub-agent traffic |
| **Provider-run tools** | Mediates tools executed directly inside provider inference calls | Declare tools in policy: exposed results are labeled upon admission, and outbound provider queries are audited as declared open vectors |

## Model guarantees depend on five explicit assumptions

OpenAPPA's guarantees hold within defined system boundaries:
1. **Benign but confusable agent**: Protection focuses on preventing unintended data leaks and prompt injection exploits; intentional covert channel exfiltration by the model itself is out of scope.
2. **Attacks arrive via ingested data**: Pre-vetted trusted sources are trusted by configuration; compromised internal data is not caught.
3. **Registered components are correct**: What a component `permits`, or a cast's `may_cast` ceiling, bounds its power, but misconfigured endpoints void their specific guarantees.
4. **Log is durable and strictly ordered**: Security tracking relies on a persistent, append-only log.
5. **The harness executes faithfully where unenforced**: When tool proxies or hooks are not configured, the agent framework is expected to run approved calls as returned.

In short: declarative tool contracts set automatic bounds, while registered components — services or builtins — handle dynamic cases like human approvals or content scanners. As long as the execution log is persisted, OpenAPPA ensures every decision remains provable and auditable under your team's security policy.

## Existing checks map onto registered engine components

Deployments migrate existing security controls into OpenAPPA by registering them as policy components, each with an explicit `permits` table or `may_cast` ceiling:

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

Crucially, an authority or sanitizer can do only what its `permits` declares, and a cast only what its `may_cast` ceiling allows: even if an ML classifier or third-party scanner makes a mistake, it cannot grant permissions beyond its pre-configured limit. Resolvers carry no `permits`: a membership resolver supplies trusted directory data, and a dynamic resolver supplies trusted classification over the proposed call. OpenAPPA validates resolver answers for valid schema and policy vocabulary (literal reader IDs, declared trust ranks, attended attention marks). Register resolvers only for services you trust as part of your deployment. Declaring tool bounds in contracts replaces scattered guardrail scripts and manual `if` checks across your codebase.

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
