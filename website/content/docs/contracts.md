---
title: Policy reference
category: Get started
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads its policy from a single TOML file. In practice, most of the configuration is generated automatically from tool descriptions, argument schemas, and existing system ACLs before being reviewed by a human auditor.

This document is a reference guide for writing and reviewing OpenAPPA policy TOML files. It covers global settings, set operators, contract declarations (`[[tool]]`, `[[authority]]`, `[[sanitizer]]`, `[[cast]]`), and policy review red flags.

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

## What to check when reviewing

A tool contract is typically four lines long. Use this checklist during policy review to catch common syntax and structural red flags:

| Review Area | Red Flag / Misconfiguration | Safe / Correct Pattern | Spec Invariant & Risk |
|---|---|---|---|
| **`delta` Accuracy** | Tool reads sensitive customer data but declares `delta = {}` or omits `delta`. | Declare explicit restriction, e.g. `delta = { audience = { exactly = ["support"] } }`. | Undermines downstream checks; over-restricting is safe (costs reach, doesn't leak). |
| **Unannotated Tools** | Omitting `delta` while declaring `requires`. | Use `delta = {}` if output carries no labels, or separate unannotated tools. | Loader refuses `requires` on unannotated tools; unannotated output enters as `Unknown`. |
| **`effects` Completeness** | Mutation or deployment tool omits `effects`. | Declare all side effects, e.g., `effects = ["migration.applied", "mutation"]`. | Under-declared effects pass `no_prior` checks silently without triggering history constraints. |
| **Dynamic Recipients** | Static list `includes = ["user@corp"]` when recipient comes from a tool argument. | Use dynamic argument placeholder `includes = ["$recipient"]`. | Static lists pass unauthorized recipient arguments without checking argument values. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **Authority Mandates** | Overly permissive mandates like `can_cover_readers = { may_add = ["public"] }`. | Restrict authority `mandate` and `scope.tags` to the minimum necessary desk. | Authorities cannot exceed mandates, but overly broad mandates weaken review gates. |

## Tools

A `[[tool]]` entry defines what permissions its result restricts (`delta`), what global side effects it emits (`effects`), and what conditions must hold before it dispatches (`requires`).

```toml
[[tool]]
name  = "fetch_support_ticket"
tags  = ["support"]                                    # Scope tag for authority routing
# CRM is trusted infrastructure; the ticket body is customer-written text
delta = { trust = "suspicious", audience = { exactly = ["support"] } }

[[tool]]
name     = "apply_db_migration"
requires = { trust     = "trusted",
             effects   = { has    = ["backup.completed"],    # prior(k) check
                           has_no = ["migration.applied"] }, # no_prior(k) check
             attention = ["sre-signoff"] }             # Fresh per-call demand
effects  = ["migration.applied", "mutation"]           # Emitted side effects
delta    = {}                                          # Status string carries no label
```

### Key contract rules

- **`delta` is strictly restrictive**: A tool's delta can only narrow the audience or lower trust. Within an annotated `delta`, an omitted dimension defaults to identity.
- **Pending-cast deltas (`delta = { trust = "unknown" }`)**: Holds a label dimension pending resolution by a registered `[[cast]]` at admission. Declaring both `requires` and `unknown` delta on the same dimension is a load error.
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { includes = ["$recipient"] }` evaluates `$recipient` against actual call arguments at runtime. Placeholders are valid only inside `includes`.
- **History checks (`requires.effects`)**: `has` verifies `prior(k)`; `has_no` verifies `no_prior(k)` against the append-only log.
- **Attention demands (`requires.attention`)**: Forces fresh authority sign-off on *every* call, never satisfied by execution history.
- **Dual-gate contracts**: When a contract defines both a restrictive `delta` and a `requires` check (e.g., `search_and_share`), the engine evaluates both gates.

## Authorities

An `[[authority]]` provides dynamic judgment to clear specific requirement gaps for a single tool call. An authority approval clears the gap for that call, but **never raises the overall trajectory label**.

```toml
[[authority]]
name = "finance-officer"

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
on   = ["tool_output"]                         # Applies to child sub-execution returns

[sanitizer.mandate]
# Source label must satisfy `from`; output receives exact `to` label
audience = { from = { includes = ["finance"] }, to = { exactly = ["public"] } }

[sanitizer.implementation]
builtin = "redact-email"
```

To enforce automated return sanitization across all child sub-executions, policies can bind a default return sanitizer:

```toml
[child]
return_sanitizer = "pii-redactor"   # Forces all child sub-execution returns through pii-redactor
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

Applicable casts — matched by scope tags — evaluate in registration order. Register constant casts last: a cast placed after a constant that covers it can never run, and the loader refuses it. The engine validates every resolver response against its declared `may_cast` ceiling before admitting the value.
