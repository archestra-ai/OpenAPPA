---
title: Policy reference
category: Deep Dive
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

### Groups

A reader list may name a **group**, written `@name`, and so may the actual argument an `includes($arg)` placeholder reads. The registered membership resolver turns the name into its literal reader set at the moment the engine first reads it for an operation — at the admission of an exposed provider-run result, at the pre-dispatch check, at mandate validation, at sanitizer application, at cast selection and the validation of a cast's answer. A name without the mark is a literal reader ID. A reader ID starting with `@` is a load error, as is a group mention in a policy with no `[membership]` registration.

```toml
[membership]                    # one per deployment; every @group resolves here
name = "corp-directory"         # registration only; the deployment binds the endpoint

[[tool]]
name     = "post_audit_note"
requires = { audience = { cap = ["finance", "@auditors"] } }  # group in a cap
delta    = {}
```

Resolution is fresh per operation, and pinned within one: the set resolved at a call's check is the set its block, its plan offers, its release and its admission use, and a written list means its literal readers plus the current members of each group it names. Directory updates take effect on the next policy check without a reload. Once resolved for a specific tool call, reader sets stay pinned to that call. Records keep the resolved readers, never the group name. `public` is reserved and never a group member; a directory answer containing it is malformed.

A resolver that produces no answer — a timeout, an error, malformed or oversized data — resumes nothing: the check does not complete and no engine fact is appended. An empty reader set is a successful answer. The endpoint accepts a versioned JSON POST request: `{version:1,resolver,group}`, with `group` the name without its `@` mark. It returns `{version:1,readers:[...]}`.

### Dynamic resolvers

A dynamic resolver classifies a proposed tool call before the engine checks it. It returns selected fields of the tool's contract: output-label values for `delta`, and call-time constraints for `requires`. It does not resolve `@group` membership.

A `[[dynamic_resolver]]` declares two things: the `inputs` a tool must supply, and the `returns` it always answers with. A `[[tool]]` attaches one with `uses`, maps each declared input from the proposed call, and points its own fields at the results it wants.

#### Example: pass one argument

```toml
[[dynamic_resolver]]            # registration only; the deployment binds the implementation endpoint
name    = "classify-customer"
inputs  = ["subject"]
returns = ["delta.trust", "delta.audience"]

[[tool]]
name       = "get_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
uses       = [{ resolver = "classify-customer", inputs = { subject = "$tool_call.arguments.customer_id" } }]

# This tool uses only the resolver's trust result.
delta    = { trust = "resolver.classify-customer.trust" }
requires = { trust = "trusted" }
```

The resolver receives only `customer_id`, under the name `subject`. It returns both of its results. This tool reads one.

The result path is `resolver.<resolver name>.<result name>`. It is a string because an unquoted dotted value is not valid TOML. The field it sits in supplies the scope, so `delta.trust` and `requires.trust` spell the same reference and read different results.

#### Example: pass the complete tool call

Omit `inputs` on the resolver to pass the complete call:

```toml
[[dynamic_resolver]]
name    = "classify-command"
returns = ["delta.trust", "delta.audience"]

[[tool]]
name        = "Bash"
description = "Runs one shell command and returns its output."
uses        = [{ resolver = "classify-command" }]
delta       = { trust = "resolver.classify-command.trust", audience = "resolver.classify-command.audience" }
```

The resolver receives this value in `args`:

```json
{
  "name": "Bash",
  "description": "Runs one shell command and returns its output.",
  "arguments": {
    "command": "git push origin main",
    "timeout": 60000
  }
}
```

This form does not need a tool parameter schema. It does need a tool `description`: the value carries one.

#### Example: use several resolvers

```toml
[[dynamic_resolver]]
name    = "trust-classifier"
inputs  = ["subject"]
returns = ["delta.trust"]

[[dynamic_resolver]]
name    = "record-acl"
inputs  = ["record"]
returns = ["delta.audience"]

[[tool]]
name       = "get_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
uses = [
  { resolver = "trust-classifier", inputs = { subject = "$tool_call.arguments.customer_id" } },
  { resolver = "record-acl", inputs = { record = "$tool_call.arguments.customer_id" } }
]
delta = { trust = "resolver.trust-classifier.trust", audience = "resolver.record-acl.audience" }
```

OpenAPPA sends one request to each resolver. Both requests use the same current state.

#### Available call values

`$tool_call` is the only special source.

| Value | Meaning |
|---|---|
| `$tool_call` | Complete tool call |
| `$tool_call.name` | Tool name |
| `$tool_call.description` | Tool description from the policy |
| `$tool_call.arguments` | Complete argument object |
| `$tool_call.arguments.<name>` | One top-level argument |

Any read of the description needs a tool `description`. A single argument needs a tool parameter schema, and the schema must mark that top-level argument as required. The resolver then receives whatever JSON value the call carries under that name.

#### Results and their destinations

A result is named for the one contract field it establishes. These five names are the whole vocabulary:

| Result | Destination | Value |
|---|---|---|
| `delta.trust` | `delta.trust` | A rank from the trust chain |
| `delta.audience` | `delta.audience` | `"public"` or a list of literal readers |
| `requires.trust` | `requires.trust` | A rank from the trust chain |
| `requires.audience` | `requires.audience` | `includes`, `cap`, or both |
| `requires.attention` | `requires.attention` | Marks from the attended list |

#### Rules

- A resolver declares its inputs and all results it returns.
- A tool can use zero or more resolvers. Omit `uses` when it uses none.
- A resolver always returns every declared result. A tool can use any part of that result.
- A tool field holds one value: a value the policy wrote, or one resolver result. So a tool reads at most one result per field, and it uses at most five resolvers.
- A tool must read a result of every resolver it uses. A `uses` entry no field reads does not load.
- If a resolver fails or returns an invalid result, OpenAPPA does not run the tool.

#### Ownership and pinning

Every contract field has one owner. A field the policy writes and a field a resolver establishes are the same field, so the two never overlap and requirements do not combine across resolvers. A dimension no owner describes stays fail-closed `Unknown`. History requirements are always static.

Resolution occurs when a proposed call first checks. The validated answer is pinned to the exact `args` its resolver received. The pin holds through rechecks, remedy plans, rulings, dispatch, and admission. The dispatch record stores the answer and a digest of those `args`, not a second copy of them: the arguments and the `uses` entry are already on the record, so a replay rebuilds the value and compares. A new proposal resolves again. An answer given about an unrelated call is never evidence here.

An input-substitution remedy rewrites the arguments of a proposal through a registered sanitizer. The pins survive the rewrite and the resolver is not consulted again: the answer is about the proposal, and the proposal stays on the record for the rebuild to compare against. This holds for every read shape, including a use that reads the complete call, `$tool_call`, or `$tool_call.arguments`. So a resolver-backed tool is offered input-substitution remedies like any other.

A `tool_input` sanitizer therefore holds one power more than its declared transition names. Its rewrite keeps the classification a resolver gave about the arguments before the rewrite, even where the rewrite changes exactly what that resolver read. A rewrite that turns one recipient, path, or resource into another one dispatches under the answer given for the first.

A `tool_input` sanitizer replaces the whole argument object, and it declares no list of the arguments it writes. So the policy cannot state that a rewrite leaves one resolver input alone, and it has one control here: `scope`. Give a resolver-backed tool a tag no `tool_input` sanitizer covers, unless you accept that sanitizer's rewrite standing under the resolver's earlier answer. Scope permits the hop; it does not produce one. The engine offers it only where the call blocks on an `includes` gap the sanitizer's declared transition clears.

A resolver that produces no answer — an unbound or unreachable implementation, a timeout, a process failure, a malformed, out-of-policy, unsupported-version, or oversized response — resumes nothing: the call was not checked, no engine fact is appended, and the answer is not a policy denial. The hook fails operationally and the reason lands in the runtime's own log; the next proposal consults again.

#### Implementations

One `[externals.dynamic]` HTTP endpoint serves every resolver that does not declare a builtin; each request carries the resolver name. A resolver can instead name an in-process builtin on its declaration — the one implementation choice the policy itself may carry. The builtin available today is the Claude Code classifier (`builtin = "claude-code"`); it is one implementation, not the definition of how resolvers work.

```toml
[[dynamic_resolver]]
name    = "classify-call"
builtin = "claude-code"
returns = ["delta.trust"]
```

A resolver with `builtin = "claude-code"` never uses the endpoint. The builtin starts one isolated `claude` process per consult: non-interactive safe mode, no tools, no project settings, no session persistence, a fresh temporary working directory, and an environment with every `APPA_*` variable removed. The process receives the same request the HTTP wire carries on stdin and answers under a strict structured-output schema derived from `returns`, the trust chain, and the attended marks; the request is explicitly treated as untrusted data, never as instructions. Claude answers have no separate ceiling: they are trusted classifier evidence and pass the same exact-shape, policy-vocabulary, audience, and pin validation as HTTP answers. The prompt and the raw model output are never persisted — only the validated answer is.

The deployment tunes the builtin in `[externals.claude_code]`: `command` sets the executable path (a service environment often strips `PATH`), `model` pins the model, and `timeout_ms` gives the consult its own budget instead of the shared machine-consult `timeout_ms` — a model call is slower than an ordinary endpoint. At most four Claude consults run at once per runtime. Each consult has model latency and account cost; a pinned recheck and a replay never invoke it again.

Both implementations receive the same request and answer under the same validation. A deployment upgrades OpenAPPA and its resolver endpoints together: the request keeps version `1`, so an endpoint cannot tell the two shapes apart.

#### Wire format

The comments below explain each key. They are not sent.

Request with one mapped argument:

```jsonc
{
  // WHY: Identifies the request shape.
  "version": 1,

  // WHY: Selects the resolver when one service handles several resolvers.
  "resolver": "classify-customer",

  // WHY: Contains only the data selected by the resolver input mapping.
  "args": {
    // WHY: Gives the resolver's subject input the selected customer ID.
    "subject": "cust-7"
  },

  // WHY: Gives the resolver the current state before this tool call.
  "context": {
    // WHY: Gives the current trust name.
    "current_trust": "trusted",

    // WHY: Gives the current trust position in the ordered trust list.
    "current_trust_rank": 1,

    // WHY: Gives the current readers.
    "current_audience": "public",

    // WHY: Says whether trust is still unknown.
    "trust_unresolved": false,

    // WHY: Says whether audience is still unknown.
    "audience_unresolved": false,

    // WHY: Lists attention marks already required by the tool policy.
    "static_attention": []
  },

  // WHY: Limits trust results to names defined by the policy.
  "trust_ranks": ["suspicious", "trusted"],

  // WHY: Limits attention results to names defined by the policy.
  "attention_marks": ["privacy-review"]
}
```

The request has no `tool`, `input`, `scope`, `returns`, or `expects` key. A resolver that needs the tool name or its description reads it as an input.

Response:

```jsonc
{
  // WHY: Identifies the response shape. It must match the request version.
  "version": 1,

  // WHY: Holds every result declared by this resolver, keyed by the result's own name.
  "result": {
    // WHY: Supplies the declared trust result.
    "delta.trust": "trusted",

    // WHY: Supplies the declared audience result, even if this tool does not use it.
    "delta.audience": ["finance"]
  }
}
```

OpenAPPA rejects a missing result, an extra result, and a `null` result. It rejects an extra key beside `version` and `result`. Trust and attention values must come from the lists in the request, whether or not the tool reads them: a result no field reads establishes nothing, but the record keeps it, so it answers to the same vocabulary. `delta.audience` is `"public"` or a list of reader names, and never a group. `requires.audience` is an object with an `includes` floor, a `cap` ceiling, or both. An empty reader list is a valid, maximally restrictive answer. An empty attention list is valid, and it is the only valid attention answer when `attention_marks` is empty.

### Deployment coverage

The deployment declares what it covers in the policy file's `[deployment]` table — the starting label every root opens at, which tools have enforced execution, where raw results can be withheld, whether child branches are controlled. The policy loader validates the file against that declaration, and a construct that names an engine behavior the deployment cannot perform is a load error naming the missing coverage: a `tool_output` sanitizer with no covered application point, a pending-cast `delta` on a tool whose raw result the model would see anyway, a `[child]` section without child-context control, a `requires`, dynamic `delta`, or pending-cast `delta` on a provider-run tool. A weaker executor class is not a construct — it loads, and its weakness is the open vector. Writing a policy therefore starts from the deployment's coverage, not from the full feature list. What stays uncovered is an open vector the deployment names explicitly and auditably — nothing is removed or silently degraded.

## What to check when reviewing

A tool contract is typically four lines long. Use this checklist during policy review to catch common syntax and structural red flags:

| Review Area | Red Flag / Misconfiguration | Safe / Correct Pattern | Spec Invariant & Risk |
|---|---|---|---|
| **`delta` Accuracy** | Tool reads sensitive customer data but declares `delta = {}` or omits `delta`. | Declare explicit restriction, e.g. `delta = { audience = { exactly = ["support"] } }`. | Undermines downstream checks; over-restricting is safe (costs reach, doesn't leak). |
| **Unannotated Tools** | Omitting `delta` while declaring `requires`. | Use `delta = {}` if output carries no labels, or separate unannotated tools. | Loader refuses `requires` on unannotated tools; unannotated output enters as `Unknown`. |
| **`effects` Completeness** | Mutation or deployment tool omits `effects`. | Declare all side effects, e.g., `effects = ["migration.applied", "mutation"]`. | Under-declared effects pass `no_prior` checks silently without triggering history constraints. |
| **Dynamic Recipients** | Static readers when an ACL depends on an argument. | Use a placeholder for a recipient the call names — a literal reader, `public`, or an `@group` — or a dynamic resolver for an argument-derived reader set. | Static readers can ignore the proposed argument; placeholder groups and dynamic resolution pin their answer to the call. |
| **Unread resolver** | A `uses` entry whose results no field of the tool reads. | Remove the entry, or point a field at one of its results. | It does not load. A consult that changes nothing but blocks the tool on failure is a policy mistake, and it costs a request on every call. |
| **Input sanitizer over a resolver** | A `tool_input` sanitizer whose `scope` tags cover a resolver-backed tool. | Tag the tool so no `tool_input` sanitizer covers it, unless that sanitizer's rewrite is one you accept under the resolver's earlier answer. | The rewrite dispatches under the answer given before it: a rewrite that changes a recipient, path, or resource carries the classification of the original one. A sanitizer declares no write set, so only `scope` controls this. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **Authority Mandates** | Overly permissive mandates like `can_cover_readers = { may_add = ["public"] }`. | Restrict authority `mandate` and `scope.tags` to the minimum necessary desk. | Authorities cannot exceed mandates, but overly broad mandates weaken review gates. |
| **Auto-Approval Wiring** | `builtin = "approve"` behind a wide mandate — an automated yes across everything the mandate covers. | Keep auto-approval mandates narrow; reserve wide mandates for `hitl` or a reviewed resolver. | Mandate powers do not depend on the implementation behind them: the open gate is legitimate, deliberate, and visible in review. |
| **Hint Accuracy** | A `hint` describing a power the mandate does not hold, or content the sanitizer does not remove. | Restate the declared mandate in your own words: say what the entity covers or strips, and nothing more. | A hint reaches the agent with every plan naming the entity, and grants nothing. A misleading one steers plan choice wrongly and misleads review. |

## Tools

A `[[tool]]` entry defines its name and `description`, its output restrictions (`delta`), side effects (`effects`), dispatch conditions (`requires`), and the resolvers it `uses`.

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
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { includes = ["$recipient"] }` evaluates `$recipient` against the actual call argument at runtime, as one audience expression: an ordinary string is one literal reader ID, `public` is the Public audience — only a Public trajectory includes it — and `@name` is a group the membership resolver expands, pinned to that proposed call. Placeholders are valid only inside `includes`, and `recipient` must be a required top-level string property of the tool's `parameters` — an omitted `parameters`, an optional, non-string, or nested property is a load error. A call that omits the argument or passes a non-string fails schema validation as an `InvalidCall` before the check runs.
- **Dynamic resolvers (`uses`)**: Each entry names a registered resolver and maps every input that resolver declares from `$tool_call`. The tool's own fields then read results by name. A mapped `$tool_call.arguments.<name>` must be a required top-level property of the tool's `parameters`, at any type; a resolver that declares no inputs reads the complete call and needs no `parameters`, but does need a `description`.
- **A field has one owner**: `delta.trust` holds a written value or one resolver result, never both — the TOML key takes one value, so the two cannot be spelled together. Write a value for a field no resolver reads: a concrete one (`delta = { trust = "trusted" }`), `delta = {}` to make the output neutral (pass-through), or omit `delta` to leave it `Unknown` (fail-closed — the safe default). Because each field takes one result, requirements do not combine across resolvers, and a tool uses at most five.
- **History checks (`requires.effects`)**: `has` verifies `prior(k)` against appended effects; `has_no` verifies `no_prior(k)` against appended effects plus unsettled reservations — emits reserved at release and not yet observed to succeed or fail.
- **Attention demands (`requires.attention`)**: Forces fresh authority sign-off on *every* call, never satisfied by execution history.
- **Dual-gate contracts**: When a contract defines both a restrictive `delta` and a `requires` check (e.g., `search_and_share`), the engine evaluates both gates.

## Authorities

An `[[authority]]` provides dynamic judgment to clear specific requirement gaps for a single tool call. An authority approval clears the gap for that call, but **never raises the overall trajectory label**.

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
```

The policy stops there. Who actually rules is a deployment question, bound in
`[externals]`:

```toml
[externals.authorities.finance-officer]
url = "https://approver.corp/rule"
# Builtin options:
# builtin = "hitl"                             # Human-in-the-loop elicitation
# builtin = "approve"                          # In-process auto-approval
```

### Authority implementation modes

| Implementation | Description | Audit Properties |
|---|---|---|
| **`builtin = "hitl"`** | Prompts a human reviewer in the loop. | Highest audit fidelity; presents exact arguments and label context to a human. |
| **`builtin = "approve"`** | Auto-approves matching gaps in-process. | Intentionally opens an automated policy bypass within declared mandate limits. |
| **`builtin = "<module name>"`** | A deployer builtin module: your own compiled code, loaded by the runtime at startup and called in-process. | Same mandate ceiling as any implementation; the module is deployer trusted code with the runtime's own privileges. |
| **`url = ...`** | Queries a privileged external service. | Receives call digest, rendered payload, and review context; decision is logged verbatim. |

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
```

```toml
[externals.sanitizers.pii-redactor]            # the deployment binds the scrubber
builtin = "redact-email"
```

### Sanitizer implementation modes

| Implementation | Description | Audit Properties |
|---|---|---|
| **`builtin = "redact-email"`** | In-process redactor: replaces email addresses with a fixed placeholder. | Deterministic and offline; guarantees exact string transformation without external calls. |
| **(reserved name `attest-schema`)** | The quarantine-exit sanitizer, registered by name alone: the engine applies it itself, so it takes no `[externals]` entry, and binding one is a load error. | Derives the return unchanged; claims instruction-cleanliness only. |
| **`builtin = "<module name>"`** | A deployer builtin module: your own compiled scrubber, loaded at startup and called in-process. | Same mandate ceiling as any implementation; deployer trusted code. |
| **`url = ...`** | A scrubbing service behind an endpoint. | The derivation is re-validated against the declared transition before admission. |

A mandate binds exactly one dimension. Trust is declared on the same terms, as a floor the source must meet and the rank the derivation carries — this is how a scrubber vouches untrusted fetched text back up:

```toml
[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" } # Instead of `audience`, never alongside it
```

A registered `tool_output` sanitizer is offered at each narrowing its mandate can strictly improve. The deployment must be able to withhold the raw result at that point. Executing the plan binds the sanitizer to the dispatch and accepts no raw or guessed residual. On success, the host withholds the raw result and the engine validates the derivation. A derivation that no longer narrows enters the trajectory. Otherwise it remains confined and opens a new stage. That stage offers acceptance of the exact current residual and any further applicable, helpful sanitizers. A sanitizer whose declared transition cannot help is never offered.

Like a cast or an authority, a sanitizer may scope itself by tags: it applies only to values whose originating tool carries a covered tag. A child sub-execution return originates from no tool, so only unscoped sanitizers apply at that crossing.

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. The substitution can clear an `includes` gap — the derivation's `to` stands in for the argument contribution — but never a `cap` or trust gap: a cap bounds the run's own reach, and rewriting the bytes does not rewrite the decision to call. The rewritten call keeps the resolver answers pinned to the proposal it replaces, and it keeps a membership answer only where the argument naming that group is unchanged.

To enforce automated return sanitization across all child sub-executions, policies can bind a default return sanitizer:

```toml
[child]
return_sanitizer = "pii-redactor"   # Forces all child sub-execution returns through pii-redactor
```

The reserved builtin `attest-schema` covers the quarantine exit without touching bytes. It raises trust on a structured child return when three conditions hold: every returned field is shape-bounded (values the schema declares and bounds — ranged numbers, booleans, closed enums, bounded formats, arrays under a length bound; no free text), the structure was bound at fork before the child read anything, and the parent's fork-time rank covers the mandate's `to`. It claims instruction-cleanliness only — a sink that acts on the returned value keeps its own gates.

```toml
[[sanitizer]]
name = "attest-schema"
on   = ["tool_output"]
hint = "Verifies the sub-agent returned valid structured data matching the schema."

[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }
```

Registering the reserved name is the whole wiring: the engine applies `attest-schema` itself, so the deployment binds no `[externals]` entry for it — an explicit binding, builtin or resolver, is a load error.

## Casts

Unannotated tools return data in an `Unknown` label state. A `[[cast]]` resolves the whole value at once, using static rules or external classifiers: its answer is one complete label that preserves every dimension already established and makes every unresolved dimension concrete, admitted atomically or not at all.

```toml
[[cast]]
name     = "content-classifier"
resolver = { may_cast = { trust = ["suspicious"],
                          audience = { cap = ["public"] } } } # Complete product ceiling;
                                              # the ceiling is policy, the endpoint deployment

[cast.scope]
tags = ["support"]                            # Applies only to values from tools with these tags

[[cast]]
name     = "paranoid-default"
constant = { trust = "suspicious",
             audience = { exactly = ["public"] } }  # Complete label; unscoped fallback, registered last
```

```toml
[externals.casts.content-classifier]           # the deployment binds the classifier
url = "https://classify.corp/label"
```

A resolver is the only implementation a cast takes: the answer is a label the `may_cast` ceiling has to bound, not a stock transform, so `builtin` is refused. A constant is answered from the policy, so it binds nothing and an `[externals]` entry for it is a load error.

Applicable casts — matched by scope tags — evaluate in registration order until one answers. A resolver that cannot answer — unreachable, timed out, malformed — is skipped, which is what makes a trailing constant the deployment's declared fallback. Register constant casts last: a cast placed after a constant that covers it can never run, and the loader refuses it.

The engine validates every resolver response against its declared `may_cast` ceiling before admitting the value. An answer over the ceiling is refused outright and falls through to nothing: a classifier answering wider than its policy allows is misbehaving, not silent, and the result stays withheld. A `public` audience cap is an open gate: it lets a single resolver answer resolve a value to `public` and lift its audience restriction entirely — review it like any covering mandate.

A tool that declares its own pending dimension — `delta = { trust = "unknown" }` — is held rather than annotated late: the deployment lists it in `confined_results`, the runtime keeps the raw result from the model, and the cast reads it first. A restricting answer reaches the agent as a narrowing offer, and the bytes are delivered only if it accepts.
