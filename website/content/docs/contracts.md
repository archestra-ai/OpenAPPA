---
title: Policy reference
category: Deep Dive
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads its policy from a single TOML file. In practice, most of the configuration is generated automatically from tool descriptions, argument schemas, and existing system ACLs before being reviewed by a human auditor.

This document is a reference guide for writing and reviewing OpenAPPA policy TOML files. It covers global settings, audience lists and conditions, contract declarations (`[[tool]]`, `[[authority]]`, `[[sanitizer]]`, `[[cast]]`), and policy review red flags.

```toml
version = 1

# Optional. The trust chain, least-trusted first; the rank names are yours,
# except `unknown`, which is reserved. Omitted, it defaults to `suspicious < trusted`.
trust_chain = ["suspicious", "trusted"]
```

### Audience lists and conditions

An audience is a list of readers. Where a policy states the audience a value carries, it writes the list itself: `delta = { audience = ["support"] }`. The list `["public"]` is the unrestricted audience. The same bare list form sets `[boundary].audience`, `starting_label.audience`, and a cast's `constant.audience`.

Where a policy checks the current audience or the trajectory's history, it names the condition:

| Key | Under | Meaning | Example |
|---|---|---|---|
| **`contains`** | `requires.audience` | The current audience must include these readers. A `$arg` placeholder is allowed only here. | `audience = { contains = ["$recipient"] }` |
| **`within`** | `requires.audience` | The current audience must be a subset of these readers. | `audience = { within = ["internal"] }` |
| **`contains`** | `requires.effects` | The trajectory already recorded this effect. | `effects = { contains = ["backup.completed"] }` |
| **`excludes`** | `requires.effects` | The effect is neither recorded in the trajectory nor reserved by an unsettled dispatch. | `effects = { excludes = ["migration.applied"] }` |

Any other key under `requires.audience` or `requires.effects` is a policy load error.

### Groups

A reader list can name a **group**, written `@name`. An argument placeholder (`contains = ["$recipient"]`) can also resolve to a group. When OpenAPPA evaluates an operation, the registered `[membership]` resolver converts the group name into its literal list of readers. A name without `@` is a literal reader ID. Using `@` in a literal reader ID or referencing a group without registering `[membership]` causes a policy load error.

```toml
[membership]                    # one per deployment; every @group resolves here
name = "corp-directory"         # registration only; the deployment binds the endpoint

[[tool]]
name     = "post_audit_note"
requires = { audience = { within = ["finance", "@auditors"] } }  # a group in a `within` list
delta    = {}
```

Each tool call resolves group membership freshly, then pins that reader set for the duration of the call (including checks, remedy plans, dispatch, and logging). Directory updates apply to new calls immediately without reloading the policy. Execution records store the resolved reader IDs, never the group name. The reserved word `public` cannot be a group member.

If a membership resolver fails (timeout, network error, or invalid payload), OpenAPPA halts the check with an operational error and records nothing to the log. An empty reader list is a valid response. The resolver endpoint receives a JSON POST request (`{"version": 1, "resolver": "...", "group": "..."}`) and returns `{"version": 1, "readers": [...]}`.

### Ordered tool contracts

A policy can declare several contracts for one tool. OpenAPPA tests them in declaration order and uses the first matching contract.

```toml
[[tool]]
name = "Bash(command:cargo test*)"
requires = { trust = "trusted" }
delta = {}

[[tool]]
name = "Bash"
requires = { trust = "trusted", attention = ["hitl"] }
delta = {}
```

Text inside parentheses selects one top-level string argument. `*` matches any text. Use `\*`, `\)`, and `\\` for literal characters.

A missing or non-string argument does not match. A bare tool name matches every argument object and is typically the fallback.

OpenAPPA selects the contract before it validates that contract's `parameters` schema. A schema failure does not continue to a later contract. A `tool_input` rewrite cannot move the call to another ordered contract. Provider-run tools cannot use argument selectors.

### Dynamic resolvers

A dynamic resolver classifies a proposed tool call before the engine checks it. It returns selected fields of the tool's contract: output-label values for `delta`, and call-time constraints for `requires`. It does not resolve `@group` membership.

A `[[dynamic_resolver]]` declares two things: the `inputs` a tool must supply, and the contract destinations it owns through `returns`. A `[[tool]]` attaches one with `uses` and maps each declared input from the proposed call. Attaching it assigns every destination in `returns` to that resolver. Resolver names are opaque non-empty strings and can contain dots.

#### Example: pass the complete call

Omit `inputs` on the resolver and its `uses` entry to pass the complete tool call: its name, its description when the tool declares one, and its arguments.

```toml
[[dynamic_resolver]]
name    = "classify-command"
returns = ["delta.trust", "delta.audience"]

[[tool]]
name        = "Bash"
description = "Runs one shell command and returns its output."
uses        = [{ resolver = "classify-command" }]
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

This form does not need a tool parameter schema. It does not need a `description`: a tool without one sends `name` and `arguments` only.

#### Example: pass one argument

```toml
[[dynamic_resolver]]
name    = "classify-customer"
inputs  = ["subject"]
returns = ["delta.trust", "delta.audience"]

[[tool]]
name       = "get_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
uses       = [{ resolver = "classify-customer", inputs = { subject = "$tool_call.arguments.customer_id" } }]

requires = { trust = "trusted" }
```

The resolver receives only `customer_id`, under the name `subject`. Its declaration owns both output-label fields. The static `requires.trust` field does not overlap.

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
```

OpenAPPA sends one request to each resolver. Both requests use the same current state.

#### Rules

- A resolver declares its inputs and all contract destinations it owns through `returns`.
- A tool can use zero or more resolvers. Omit `uses` when it uses none.
- A resolver always returns every declared result. Attaching it assigns all declared destinations to it.
- A tool field has one owner: a static policy value or one attached resolver. Static and resolver ownership cannot overlap. Two resolvers cannot own the same destination.
- If a resolver fails or returns an invalid result, OpenAPPA does not run the tool.

A tool maps each declared input from the proposed call. `$tool_call` is the only special source.

| Value | Meaning |
|---|---|
| `$tool_call` | Complete tool call: `name`, `description` when the tool declares one, and `arguments` |
| `$tool_call.name` | Tool name |
| `$tool_call.description` | Tool description from the policy |
| `$tool_call.arguments` | Complete argument object |
| `$tool_call.arguments.<name>` | One top-level argument |

A resolver with no `inputs` receives `$tool_call` as its `args`. `$tool_call.description` needs a tool `description`; `$tool_call` does not, and omits the key when there is none. A single argument needs a tool parameter schema, and the schema must mark that top-level argument as required. The resolver then receives whatever JSON value the call carries under that name.

A result is named for the one contract field it fills. These five names are the whole vocabulary:

| Result | Value |
|---|---|
| `delta.trust` | A rank from the trust chain |
| `delta.audience` | `"public"` or a list of literal readers |
| `requires.trust` | A rank from the trust chain |
| `requires.audience` | `contains`, `within`, or both |
| `requires.attention` | Marks from the attended list |

Ownership, pinning, and what a `tool_input` sanitizer can do to a pinned answer are in [How it works](/how-it-works).

#### Implementing a resolver

One `[externals.dynamic]` HTTP endpoint serves every resolver that does not declare a builtin; each request carries the resolver name. A resolver can instead name an in-process builtin on its declaration — the one implementation choice the policy itself may carry. The builtin available today is the Claude Code classifier (`builtin = "claude-code"`); it is one implementation, not the definition of how resolvers work.

```toml
[[dynamic_resolver]]
name    = "classify-call"
builtin = "claude-code"
returns = ["delta.trust"]
```

A resolver with `builtin = "claude-code"` never uses the endpoint. The builtin starts one isolated `claude` process per consult: non-interactive safe mode, no tools, no project settings, no session persistence, a fresh temporary working directory, and an environment with every `APPA_*` variable removed. The process receives the same request the HTTP wire carries on stdin and answers under a strict structured-output schema derived from `returns`, the trust chain, and the attended marks; the request is explicitly treated as untrusted data, never as instructions. Claude answers have no separate ceiling: they are trusted classifier evidence and pass the same exact-shape, policy-vocabulary, audience, and pin validation as HTTP answers. The prompt and the raw model output are never persisted — only the validated answer is.

The deployment tunes the builtin in `[externals.claude_code]`: `command` sets the executable path (a service environment often strips `PATH`), `model` pins the model, and `timeout_ms` gives the consult its own budget instead of the shared machine-consult `timeout_ms` — a model call is slower than an ordinary endpoint. At most four Claude consults run at once per runtime. Each consult has model latency and account cost; a pinned recheck and a replay never invoke it again.

Both implementations receive the same request and answer under the same validation.

Request, for the one-argument example above:

```json
{
  "version": 1,
  "resolver": "classify-customer",
  "args": { "subject": "cust-7" },
  "context": {
    "current_trust": "trusted",
    "current_trust_rank": 1,
    "current_audience": "public",
    "trust_unresolved": false,
    "audience_unresolved": false,
    "static_attention": []
  },
  "trust_ranks": ["suspicious", "trusted"],
  "attention_marks": ["privacy-review"]
}
```

| Key | Meaning |
|---|---|
| `version` | The request shape. It is `1`. |
| `resolver` | Which resolver answers, when one service handles several |
| `args` | The data the tool's input mapping selected, under the resolver's declared input names. Without a mapping, the complete call: `name`, `description` when declared, and `arguments`. |
| `context` | The state before this tool call: current trust name and rank, current readers, whether each dimension is still unknown, and the attention marks the tool policy already requires |
| `trust_ranks` | The policy's trust chain, least-trusted first. A trust result must name one of these. |
| `attention_marks` | The attended marks. An attention result must name these only. |

A resolver with mapped inputs that needs the tool name or its description reads it as an input.

Response:

```json
{
  "version": 1,
  "result": {
    "delta.trust": "trusted",
    "delta.audience": ["finance"]
  }
}
```

`version` must match the request. `result` holds every result the resolver declared, keyed by the result's own name — including a result this tool does not read.

OpenAPPA rejects a missing result, an extra result, and a `null` result. It rejects an extra key beside `version` and `result`. Trust and attention values must come from the lists in the request, whether or not the tool reads them: a result no field reads establishes nothing, but the record keeps it, so it answers to the same vocabulary. `delta.audience` is `"public"` or a list of reader names, and never a group. `requires.audience` is an object with `contains`, `within`, or both: `{"contains": [...], "within": [...]}`. An empty reader list is a valid, maximally restrictive answer. An empty attention list is valid, and it is the only valid attention answer when `attention_marks` is empty.

### Deployment coverage

The `[deployment]` table declares the capabilities of your hosting environment—such as starting security labels, enforced execution points, raw output withholding, and child branch isolation.

During policy load, OpenAPPA validates that all declared constructs are supported by the deployment. If a policy requires a capability the deployment lacks, loading fails with an explicit error:
- A `tool_output` sanitizer requires a deployment that can withhold raw tool results.
- A pending cast (`delta = { trust = "unknown" }`) requires raw results to be withheld until classified.
- A `[child]` section requires child context isolation.
- Provider-run tools (tools executed directly inside a provider inference call) cannot declare `requires`, dynamic resolvers, or pending casts.

Uncovered vectors are explicitly declared in the deployment configuration so security leaders can audit them, rather than silently degrading guarantees.

## What to check when reviewing

A tool contract is short: a name, a `delta`, and often `effects` and a `[tool.requires]` table of one key per line. Use this checklist during policy review to catch common syntax and structural red flags:

| Review Area | Red Flag / Misconfiguration | Safe / Correct Pattern | Spec Invariant & Risk |
|---|---|---|---|
| **`delta` Accuracy** | Tool reads sensitive customer data but declares `delta = {}` or omits `delta`. | Declare explicit restriction, e.g. `delta = { audience = ["support"] }`. | Undermines downstream checks; over-restricting is safe (costs reach, doesn't leak). |
| **Unannotated Tools** | Omitting `delta` while declaring `requires`. | Use `delta = {}` if output carries no labels, or separate unannotated tools. | Loader refuses `requires` on unannotated tools; unannotated output enters as `Unknown`. |
| **`effects` Completeness** | Mutation or deployment tool omits `effects`. | Declare all side effects, e.g., `effects = ["migration.applied", "mutation"]`. | Under-declared effects pass `excludes` checks silently without triggering history constraints. |
| **Dynamic Recipients** | Static readers when an ACL depends on an argument. | Use a placeholder for a recipient the call names — a literal reader, `public`, or an `@group` — or a dynamic resolver for an argument-derived reader set. | Static readers can ignore the proposed argument; placeholder groups and dynamic resolution pin their answer to the call. |
| **Overlapping resolvers** | Two `uses` entries whose `returns` include the same destination. | Give each destination one owner. | It does not load because a contract field cannot have two values. |
| **Input sanitizer over a resolver** | A `tool_input` sanitizer whose `tags` cover a resolver-backed tool. | Tag the tool so no `tool_input` sanitizer covers it, unless you accept its rewrite under the earlier resolver answer. | The rewrite keeps the original classification. OpenAPPA refuses it if the new arguments select another ordered contract. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **What an authority permits** | A wide `permits` table, such as `audience_missing = ["public"]`. | Restrict the authority's `permits` and `tags` to the minimum the desk needs. | An authority cannot rule beyond its `permits`, but a wide `permits` weakens the review gate. |
| **Auto-Approval Wiring** | `builtin = "approve"` behind a wide `permits` — an automated yes across everything it permits. | Keep what an auto-approval authority permits narrow; reserve wide `permits` for `hitl` or a reviewed resolver. | `builtin = "approve"` creates an automated open gate for all matching actions. Keep its `permits` and `tags` minimal. |
| **Hint Accuracy** | A `hint` describing a power the `permits` does not hold, or content the sanitizer does not remove. | Restate what the component permits in your own words: say what the entity covers or strips, and nothing more. | A hint reaches the agent with every plan naming the entity, and grants nothing. A misleading one steers plan choice wrongly and misleads review. |

## Tools

A `[[tool]]` entry defines its name and `description`, its output restrictions (`delta`), side effects (`effects`), dispatch conditions (`requires`), and the resolvers it `uses`.

```toml
[[tool]]
name = "fetch_support_ticket"
tags = ["support"]                                     # Authorities, sanitizers, and casts select tools by tag

[tool.delta]
trust    = "suspicious"                                # The ticket body is customer-written text
audience = ["support"]                                 # The record is for the support desk only

[[tool]]
name     = "apply_db_migration"
effects  = ["migration.applied", "mutation"]           # Emitted side effects
delta    = {}                                          # Status string carries no label

[tool.requires]
trust     = "trusted"
attention = ["sre-signoff"]                            # Fresh per-call demand

[tool.requires.effects]
contains = ["backup.completed"]                        # Already recorded in the trajectory
excludes = ["migration.applied"]                       # Neither recorded nor reserved by an unsettled dispatch
```

### Key contract rules

- **`delta` is strictly restrictive**: A tool's delta can only narrow the audience or lower trust. Within an annotated `delta`, an omitted dimension defaults to identity (unchanged).
- **Pending-cast deltas (`delta = { trust = "unknown" }` or `delta = { audience = "unknown" }`)**: Holds one label dimension pending resolution by a registered `[[cast]]` at admission. At most one dimension may be pending-cast. Declaring both `requires` and `unknown` delta on the same dimension is a load error. `"unknown"` is reserved: it can name neither a trust rank nor a reader.
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { contains = ["$recipient"] }` evaluates `$recipient` against the actual call argument at runtime. The argument value can be a literal reader ID, the reserved word `public`, or an `@group` expanded by the membership resolver. Placeholders are supported only inside `contains`. The argument must be declared as a required top-level string in the tool's `parameters` schema.
- **Dynamic resolvers (`uses`)**: Attaches registered resolvers to classify proposed calls. Each entry maps required inputs from `$tool_call`. Mapped arguments must be required top-level properties in the tool schema. Without an explicit input mapping, a resolver receives the complete tool call: `name`, `description` when declared, and `arguments`.
- **Single field ownership**: Each contract field has one source: a static policy value or an attached resolver whose `returns` includes that destination. Static and resolver ownership cannot overlap. Two attached resolvers cannot own the same destination.
- **History checks (`requires.effects`)**: `contains` passes when the trajectory already recorded the effect. `excludes` passes when the effect is neither recorded nor reserved by an unsettled dispatch (a dispatch released with that effect that has not yet succeeded or failed).
- **Attention demands (`requires.attention`)**: Forces fresh authority sign-off on *every* call; never satisfied by execution history.
- **Dual-gate contracts**: When a contract defines both a restrictive `delta` and a `requires` check (e.g., `search_and_share`), OpenAPPA evaluates both gates before dispatch.

## Authorities

An `[[authority]]` provides dynamic judgment to clear specific requirement gaps for a single tool call. Its `permits` table says which gaps its rulings can clear, and how far. An authority approval clears the gap for that call, but **never raises the overall trajectory label**.

```toml
[[authority]]
name = "finance-officer"
hint = "The desk that signs off spend. Consult it to release a payment."  # Advisory; grants nothing
tags = ["finance"]                             # The tools it can answer; omitted, every tool.
                                               # Attention routing ignores tags: a mark routes to every
                                               # authority whose `permits.attention` names it.

[authority.permits]
trust_below        = "trusted"                 # A call whose trust requirement is unmet, for requirements up to this rank
audience_missing   = ["public"]                # A call whose audience is missing required readers, up to these readers
effects_containing = ["email.sent"]            # A call although the trajectory already contains one of these effects
attention          = ["finance-signoff"]       # The marks its rulings satisfy
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
| **`builtin = "approve"`** | Auto-approves matching gaps in-process. | Intentionally opens an automated policy bypass within what the authority `permits`. |
| **`builtin = "<module name>"`** | A deployer builtin module: your own compiled code, loaded by the runtime at startup and called in-process. | Bound by the same `permits` as any implementation; the module is deployer trusted code with the runtime's own privileges. |
| **`url = ...`** | Queries a privileged external service. | Receives call digest, rendered payload, and review context; decision is logged verbatim. |

## Sanitizers

A `[[sanitizer]]` defines a formal label transition for data passed through a registered scrubbing pipeline (such as a PII redactor or HTML safety filter). Its `permits` table names the one transition its derivation can claim.

```toml
[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]                         # Tool results and child sub-execution returns
# on = ["tool_input"]                          # Whole-argument substitution at dispatch
hint = "Removes personal details from a finance record."  # Advisory; grants nothing
tags = ["support"]                             # Applies only to values from tools with these tags

[sanitizer.permits]
# `from`: the source audience must contain these readers; `to`: the output gets exactly this audience
audience = { from = ["finance"], to = ["public"] }
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
| **`builtin = "<module name>"`** | A deployer builtin module: your own compiled scrubber, loaded at startup and called in-process. | Bound by the same `permits` as any implementation; deployer trusted code. |
| **`url = ...`** | A scrubbing service behind an endpoint. | The derivation is re-validated against the declared transition before admission. |

A sanitizer permits one dimension. For trust, `from` is the rank the source must meet or exceed, and `to` is the rank the derivation carries — this is how a scrubber vouches untrusted fetched text back up:

```toml
[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" } # Instead of `audience`, never alongside it
```

When a tool result would narrow the trajectory label, OpenAPPA checks if a registered `tool_output` sanitizer can improve the label. If selected, the host withholds the raw result and runs the sanitizer. If the cleaned derivation prevents narrowing, it enters the trajectory label. If residual narrowing remains, the agent can accept the residual or apply another compatible sanitizer. A sanitizer whose declared transition cannot improve the label is never offered.

Like a cast or an authority, a sanitizer can name `tags`: it then applies only to values whose originating tool carries one of them. A child sub-execution return originates from no tool, so only a sanitizer without `tags` applies at that crossing.

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. This substitution can satisfy an unmet `contains` audience requirement, but cannot clear a `within` or trust requirement (`within` bounds the trajectory's own reach, and rewriting arguments does not change the decision to invoke the tool). The rewritten call keeps the resolver answers pinned to the proposal it replaces. It cannot select another ordered contract. It preserves a group membership answer only if the argument naming that group is unchanged.

To enforce automated return sanitization across all child sub-executions, policies can bind a default return sanitizer:

```toml
[child]
return_sanitizer = "pii-redactor"   # Forces all child sub-execution returns through pii-redactor
```

The reserved builtin `attest-schema` validates structured sub-agent returns without altering data bytes. It safely raises trust from `suspicious` to `trusted` when:
1. All returned fields are strictly shape-bounded (numbers, booleans, fixed enums, bounded formats; no free text).
2. The schema structure was bound before the sub-agent read untrusted data.
3. The parent agent had trusted status when spawning the sub-agent.

```toml
[[sanitizer]]
name = "attest-schema"
on   = ["tool_output"]
hint = "Verifies the sub-agent returned valid structured data matching the schema."

[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }
```

Registering `name = "attest-schema"` is sufficient; OpenAPPA applies it natively without requiring an `[externals]` entry (configuring one is a load error).

## Casts

Unannotated tools return data in an `Unknown` label state. A `[[cast]]` resolves the whole value at once, using static rules or external classifiers: its answer is one complete label that preserves every dimension already established and makes every unresolved dimension concrete, admitted atomically or not at all.

A block lists each Unknown source by value under `unestablished`, together with the dimensions no applicable cast reaches. No remedy plan clears that slot; only an admitted cast does. While any source in a block is unestablished, the block offers no executable plan.

```toml
[[cast]]
name     = "content-classifier"
tags     = ["support"]                        # Applies only to values from tools with these tags
resolver = { may_cast = { trust = ["suspicious"], audience = ["public"] } }
                                              # The answer must be one of these ranks and within
                                              # these readers; the ceiling is policy, the endpoint deployment

[[cast]]
name     = "paranoid-default"
constant = { trust = "suspicious", audience = ["public"] }
                                              # Complete label; fallback without tags, registered last
```

```toml
[externals.casts.content-classifier]           # the deployment binds the classifier
url = "https://classify.corp/label"
```

A dynamic cast requires a resolver bounded by a `may_cast` ceiling (`builtin` is not supported for casts). A constant cast is defined directly in policy, so it requires no `[externals]` binding.

When resolving an `Unknown` value, OpenAPPA evaluates applicable casts (selected by `tags`) in registration order until one returns an answer. If a resolver is unreachable or fails, OpenAPPA moves to the next registered cast. Place fallback constant casts last, as any cast registered after a constant cast without `tags` will never be reached.

OpenAPPA validates every resolver answer against the declared `may_cast` ceiling before admitting the value. An answer outside the ceiling is refused, and the cascade continues with the next applicable cast in registration order; the first answer OpenAPPA admits stands. A `may_cast` audience of `["public"]` allows a resolver to lift audience restrictions entirely; review such ceilings carefully.

A classifier can be consulted again for the same value after a runtime restart or a concurrent-write retry, so a cast implementation must be idempotent.

The runtime consults a resolver-backed cast with a JSON POST. The comments explain each key; they are not sent.

```jsonc
{
  // WHY: Identifies the request shape.
  "version": 1,

  // WHY: Says which external kind is consulted.
  "kind": "cast",

  // WHY: Selects the cast when one service answers for several.
  "name": "content-classifier",

  "payload": {
    // WHY: Gives the classifier the value's bytes.
    "body": "the ticket text",

    // WHY: Names the tool whose result the value is; null for a subagent's return.
    "tool": "read_ticket",

    // WHY: Gives the current state, in the shape a dynamic resolver receives.
    "context": {
      "current_trust": "trusted",
      "current_trust_rank": 1,
      "current_audience": "public",
      "trust_unresolved": true,
      "audience_unresolved": true,
      "static_attention": []
    }
  }
}
```

The response is `{"version": 1, "answer": {"trust": "suspicious", "audience": "public"}}`, where `audience` is `"public"` or an array of literal reader ids. Anything else — an error status, a timeout, a malformed body, or an empty answer — is no answer: nothing is recorded, the next applicable cast is consulted, and when none answers the call stays undecided and can be proposed again.

A tool contract can also declare a pending dimension with `delta = { trust = "unknown" }`. When configured in `confined_results`, the runtime withholds raw results from the model until the cast evaluates the payload. If the resolved label restricts the trajectory, OpenAPPA presents a narrowing prompt to the agent before delivering the data.
