---
title: Policy reference
category: Get started
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads its policy from a single TOML file. In practice, most of the configuration is generated automatically from tool descriptions, argument schemas, and existing system ACLs before being reviewed by a human auditor.

This document is a reference guide for writing and reviewing OpenAPPA policy TOML files. It covers global settings, set operators, contract declarations (`[[tool]]`, `[[authority]]`, `[[sanitizer]]`, `[[cast]]`), and policy review red flags.

<!-- appa:example -->
```toml
version = 1

# Optional. The trust chain, least-trusted first; the rank names are yours.
# Omitted, it defaults to `suspicious < trusted`.
trust_chain = ["suspicious", "trusted"]
```

### Set operators

Every set specification in a policy requires an explicit **operator** to prevent ambiguity between exact matches and lower/upper bounds:

| Operator | Meaning | Example Use |
|---|---|---|
| **`exactly`** | Fixes the set to these exact members. | `delta = { audience = { exactly = ["support"] } }` |
| **`includes`** | Requires at least these members (`audience ⊇ recipients`). | `requires = { audience = { includes = ["$recipient"] } }` |
| **`cap`** | Bounds the allowed audience from above (`audience ⊆ C`). | `requires = { audience = { cap = ["internal"] } }` |
| **`may_add`** | Bounds the readers an authority is permitted to cover. | `can_cover_readers = { may_add = ["public"] }` |

A set declaration without an explicit operator causes a policy load error.

### Groups

A reader list may name a **group**, written `@name`. The registered membership resolver turns the name into its literal reader set at the moment the engine first reads it for an operation — at the pre-dispatch check, at mandate or cast-ceiling validation, at sanitizer application. A name without the mark is a literal reader ID. A reader ID starting with `@` is a load error, as is a group mention in a policy with no `[membership]` registration.

```toml
[membership]                    # one per deployment; every @group resolves here
resolver = { url = "https://directory.corp/members", timeout_ms = 5000 }

[[tool]]
name     = "post_audit_note"
requires = { audience = { cap = ["finance", "@auditors"] } }  # group in a cap
delta    = {}
```

Resolution is fresh per call, and pinned within one: the set resolved at a call's check is the set its admission folds. A member added to the directory reaches the next call without a policy reload; removal reaches only future resolutions — a set already resolved stands. `public` is reserved and never a group member; a directory answer containing it is malformed.

### Dynamic resolvers

A dynamic resolver maps one top-level string argument to literal reader IDs. It does not resolve `@group` membership.

<!-- appa:example-fragment -->
```toml
[[dynamic_resolver]]
name = "crm-acl"
resolver = { url = "https://acl.corp/readers", timeout_ms = 5000 }

[[dynamic_resolver]]
name = "channel-members"
resolver = { url = "https://chat.corp/readers", timeout_ms = 5000 }

[[tool]]
name = "lookup_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
delta = { audience = { resolver = "crm-acl", argument = "customer_id" } }

[[tool]]
name = "send_message"
parameters = { type = "object", properties = { channel = { type = "string" } }, required = ["channel"] }
requires = { audience = { includes = { resolver = "channel-members", argument = "channel" } } }
delta = {}
```

The tool's parameter schema must declare the argument as a required top-level string. A call that omits the argument or supplies a non-string value fails schema validation as an `InvalidCall` before any resolution runs. Answers may contain many readers or none. They may not contain `public` or an `@group`.

Resolution occurs when a proposed call first checks. Its answer remains pinned through rechecks, remedy plans, rulings, dispatch, and admission. A new proposal resolves again. The dispatch record stores the answer. Blocks and rulings store the resulting literal readers.

A resolver that produces no answer — a timeout, an error, an abstention, malformed or oversized data — resumes nothing: the check does not complete, no recipient gap or Unknown delta is created, and no engine fact is appended. Runtime may retry or show operational feedback outside the log. A successful answer with an empty reader set is ordinary evidence, distinct from no answer.

The endpoint accepts a versioned JSON POST request: `{version:1,resolver,tool,argument,value}`. It returns `{version:1,readers:[...]}`. Non-2xx responses, timeouts, malformed responses, and oversized responses fail closed.

### Deployment coverage

The deployment declares what it covers when it opens the engine — which tools have enforced execution, where raw results can be withheld, whether child branches are controlled. The policy loader validates the file against that declaration, and a construct that names an engine behavior the deployment cannot perform is a load error naming the missing coverage: a `tool_output` sanitizer with no covered application point, a pending-cast `delta` on a tool whose raw result the model would see anyway, a `[child]` section without child-context control, a `requires`, dynamic `delta`, or pending-cast `delta` on a provider-run tool. A weaker executor class is not a construct — it loads, and its weakness is the open vector. Writing a policy therefore starts from the deployment's coverage, not from the full feature list. What stays uncovered is an open vector the deployment names explicitly and auditably — nothing is removed or silently degraded.

## What to check when reviewing

A tool contract is typically four lines long. Use this checklist during policy review to catch common syntax and structural red flags:

| Review Area | Red Flag / Misconfiguration | Safe / Correct Pattern | Spec Invariant & Risk |
|---|---|---|---|
| **`delta` Accuracy** | Tool reads sensitive customer data but declares `delta = {}` or omits `delta`. | Declare explicit restriction, e.g. `delta = { audience = { exactly = ["support"] } }`. | Undermines downstream checks; over-restricting is safe (costs reach, doesn't leak). |
| **Unannotated Tools** | Omitting `delta` while declaring `requires`. | Use `delta = {}` if output carries no labels, or separate unannotated tools. | Loader refuses `requires` on unannotated tools; unannotated output enters as `Unknown`. |
| **`effects` Completeness** | Mutation or deployment tool omits `effects`. | Declare all side effects, e.g., `effects = ["migration.applied", "mutation"]`. | Under-declared effects pass `no_prior` checks silently without triggering history constraints. |
| **Dynamic Recipients** | Static readers when an ACL depends on an argument. | Use a placeholder for a literal recipient, or a dynamic resolver for an argument-derived reader set. | Static readers can ignore the proposed argument; dynamic resolution pins the ACL answer. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **Authority Mandates** | Overly permissive mandates like `can_cover_readers = { may_add = ["public"] }`. | Restrict authority `mandate` and `scope.tags` to the minimum necessary desk. | Authorities cannot exceed mandates, but overly broad mandates weaken review gates. |
| **Hint Accuracy** | A `hint` describing a power the mandate does not hold, or content the sanitizer does not remove. | Restate the declared mandate in your own words: say what the entity covers or strips, and nothing more. | A hint reaches the agent with every plan naming the entity, and grants nothing. A misleading one steers plan choice wrongly and misleads review. |

## Tools

A `[[tool]]` entry defines what permissions its result restricts (`delta`), what global side effects it emits (`effects`), and what conditions must hold before it dispatches (`requires`).

<!-- appa:example-fragment -->
```toml
[[tool]]
name  = "fetch_support_ticket"
tags  = ["support"]                                    # Scope tag for authority routing
# CRM is trusted infrastructure; the ticket body is customer-written text
delta = { trust = "suspicious", audience = { exactly = ["support"] } }

[[tool]]
name     = "apply_db_migration"
effects  = ["migration.applied", "mutation"]           # Emitted side effects
delta    = {}                                          # Status string carries no label

[tool.requires]
trust     = "trusted"
effects   = { has = ["backup.completed"], has_no = ["migration.applied"] }  # prior(k) / no_prior(k)
attention = ["sre-signoff"]                            # Fresh per-call demand
```

### Key contract rules

- **`delta` is strictly restrictive**: A tool's delta can only narrow the audience or lower trust. Within an annotated `delta`, an omitted dimension defaults to identity.
- **Pending-cast deltas (`delta = { trust = "unknown" }`)**: Holds a label dimension pending resolution by a registered `[[cast]]` at admission. Declaring both `requires` and `unknown` delta on the same dimension is a load error.
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { includes = ["$recipient"] }` evaluates `$recipient` against actual call arguments at runtime. Placeholders are valid only inside `includes`.
- **Dynamic resolvers**: A `{ resolver = "name", argument = "arg" }` form maps a top-level string argument to literal readers. It is valid as an audience delta or as the value of `includes`.
- **History checks (`requires.effects`)**: `has` verifies `prior(k)` against appended effects; `has_no` verifies `no_prior(k)` against appended effects plus unsettled reservations — emits reserved at release and not yet observed to succeed or fail.
- **Attention demands (`requires.attention`)**: Forces fresh authority sign-off on *every* call, never satisfied by execution history.
- **Dual-gate contracts**: When a contract defines both a restrictive `delta` and a `requires` check (e.g., `search_and_share`), the engine evaluates both gates.

## Authorities

An `[[authority]]` provides dynamic judgment to clear specific requirement gaps for a single tool call. An authority approval clears the gap for that call, but **never raises the overall trajectory label**.

<!-- appa:example-fragment -->
```toml
[[authority]]
name = "finance-officer"
hint = "The desk that signs off spend. Consult it to release a payment."  # Advisory; grants nothing

[authority.mandate]
can_cover_trust_to = "trusted"                 # Cover unmet trust floor up to this rank
can_cover_readers  = { may_add = ["public"] }  # Cover an unmet `includes` check up to these readers
can_waive          = ["email.sent"]            # Waive a failed `no_prior` constraint
attends            = ["finance-signoff"]       # Satisfy fresh attention demands

[authority.scope]
tags = ["finance"]                             # Omitted scope matches all tools

[authority.implementation]
resolver = { url = "https://approver.corp/rule", timeout_ms = 30000 }
# Builtin options:
# builtin = "hitl"                             # Human-in-the-loop elicitation
# builtin = "approve"                          # In-process auto-approval
```

### Authority implementation modes

| Implementation | Description | Audit Properties |
|---|---|---|
| **`builtin = "hitl"`** | Prompts a human reviewer in the loop. | Highest audit fidelity; presents exact arguments and label context to a human. |
| **`builtin = "approve"`** | Auto-approves matching gaps in-process. | Intentionally opens an automated policy bypass within declared mandate limits. |
| **`resolver = { url = ... }`** | Queries a privileged external service. | Receives call digest, rendered payload, and review context; decision is logged verbatim. |

## Sanitizers

A `[[sanitizer]]` defines a formal label transition for data passed through a registered scrubbing pipeline (such as a PII redactor or HTML safety filter).

```toml
[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]                         # Tool results and child sub-execution returns
# on = ["tool_input"]                          # Whole-argument substitution at dispatch
hint = "Removes personal details from a finance record."  # Advisory; grants nothing

[sanitizer.mandate]
# Source label must satisfy `from`; output receives exact `to` label
audience = { from = { includes = ["finance"] }, to = { exactly = ["public"] } }

[sanitizer.scope]
tags = ["support"]                             # Applies only to values from tools with these tags

[sanitizer.implementation]
builtin = "redact-email"
```

A mandate binds exactly one dimension. Trust is declared on the same terms, as a floor the source must meet and the rank the derivation carries — this is how a scrubber vouches untrusted fetched text back up:

```toml
[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" } # Instead of `audience`, never alongside it
```

A registered `tool_output` sanitizer is offered at each narrowing its mandate can strictly improve. The deployment must be able to withhold the raw result at that point. Executing the plan binds the sanitizer to the dispatch and accepts no raw or guessed residual. On success, the host withholds the raw result and the engine validates the derivation. A derivation that no longer narrows enters the trajectory. Otherwise it remains confined and opens a new stage. That stage offers acceptance of the exact current residual and any further applicable, helpful sanitizers. A sanitizer whose declared transition cannot help is never offered.

Like a cast or an authority, a sanitizer may scope itself by tags: it applies only to values whose originating tool carries a covered tag. A child sub-execution return originates from no tool, so only unscoped sanitizers apply at that crossing.

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. The substitution can clear an `includes` gap — the derivation's `to` stands in for the argument contribution — but never a `cap` or trust gap: a cap bounds the run's own reach, and rewriting the bytes does not rewrite the decision to call.

To enforce automated return sanitization across all child sub-executions, policies can bind a default return sanitizer:

```toml
[child]
return_sanitizer = "pii-redactor"   # Forces all child sub-execution returns through pii-redactor
```

The reserved builtin `attest-schema` covers the quarantine exit without touching bytes. It raises trust on a structured child return when three conditions hold: every returned field is shape-bounded (values the schema declares and bounds — ranged numbers, booleans, closed enums, bounded formats, arrays under a length bound; no free text), the structure was bound at fork before the child read anything, and the parent's fork-time rank covers the mandate's `to`. It claims instruction-cleanliness only — a sink that acts on the returned value keeps its own gates.

```toml
[[sanitizer]]
name = "quarantine-exit"
on   = ["tool_output"]
hint = "Vouches a fork-bound structured return; shape only, no content claims."

[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[sanitizer.implementation]
builtin = "attest-schema"
```

## Casts

Unannotated tools return data in an `Unknown` label state. A `[[cast]]` resolves `Unknown` dimensions to concrete classifications using static rules or external classifiers.

```toml
[[cast]]
name     = "content-classifier"
resolver = { url = "https://classifier.corp/resolve", timeout_ms = 10000,
             may_cast = { trust = ["suspicious"] } } # Bounded resolver cast

[cast.scope]
tags = ["support"]                            # Applies only to values from tools with these tags

[[cast]]
name     = "paranoid-default"
constant = { trust = "suspicious" }           # Unscoped constant fallback, registered last
```

Applicable casts — matched by scope tags — evaluate in registration order. Register constant casts last: a cast placed after a constant that covers it can never run, and the loader refuses it. The engine validates every resolver response against its declared `may_cast` ceiling before admitting the value. A `public` audience cap is an open gate: it lets a single resolver answer resolve a value to `public` and lift its audience restriction entirely — review it like any covering mandate.
