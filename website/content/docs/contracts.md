---
title: Policy reference
category: Deep Dive
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads a root TOML file. The root can compose policy fragments with `include = ["battery.toml"]`. Root declarations run first. Included declarations follow in list order. An included file cannot include another file or replace root-wide settings. Duplicate external names within one kind are an error.

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
name = "corp-directory"         # registration only; the deployment binds the directory

[[tool]]
name     = "post_audit_note"
requires = { audience = { within = ["finance", "@auditors"] } }  # a group in a `within` list
delta    = {}
```

Each tool call resolves group membership freshly, then pins that reader set for the duration of the call (including checks, remedy plans, dispatch, and logging). Directory updates apply to new calls immediately without reloading the policy. Execution records store the resolved reader IDs, never the group name. The reserved word `public` cannot be a group member.

The deployment binds the registered name under `[externals.membership.<name>]` to an HTTP endpoint or a local command; no builtin serves a directory. The consult is the common envelope described under [Externals](#externals), with an empty declaration and the group name as the artifact:

```toml
[externals.membership.corp-directory]         # the deployment binds the directory
url = "https://directory.corp/members"
```

```json
{ "version": 1, "kind": "membership", "name": "corp-directory", "declaration": {}, "artifact": { "group": "auditors" } }
```

The answer is `{"version": 1, "answer": {"readers": ["cfo", "audit-lead"]}}`. An empty reader list is a valid answer. If a membership resolver fails (timeout, network error, or invalid answer), OpenAPPA halts the check with an operational error and records nothing to the log.

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

Text inside parentheses selects top-level string arguments. Write one or more `argument:pattern` clauses and separate them with commas. The contract matches only when every clause matches.

```toml
[[tool]]
name = "mcp__github__fork_repository(owner:archestra-ai,repo:website)"
requires = { trust = "trusted" }
delta = {}
```

`*` matches any text. Four escapes match a literal character: `\*`, `\)`, `\,`, and `\\`. A backslash before any other character is an error, and the policy does not load.

TOML reads backslashes too, so a selector escape passes through two layers. A basic string, in double quotes, needs every selector backslash doubled: `\\*`, `\\)`, `\\,`, `\\\\`. A literal string, in single quotes, passes backslashes through unchanged, so write the selector escape directly.

```toml
name = "search(query:a\\,b)"   # a basic string doubles the backslash
name = 'search(query:a\,b)'    # a literal string does not
```

A missing or non-string argument does not match. Clause order does not change the result: `tool(owner:x,repo:y)` and `tool(repo:y,owner:x)` are the same contract. A selector can name each argument only once. A bare tool name matches every argument object and is typically the fallback.

OpenAPPA selects the contract before it validates that contract's `parameters` schema. A schema failure does not continue to a later contract. A `tool_input` rewrite is judged by the contract its rewritten arguments select: a rewrite that stays in its contract keeps the resolver answers of the call last consulted; one that selects another contract is a new call under that contract, and its resolvers are consulted for the rewritten arguments. Provider-run tools cannot use argument selectors.

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

The resolver receives this value as the consult's `artifact.args`:

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

A resolver either carries its implementation or leaves it to the deployment. A resolver that carries a stock model builtin names it on its declaration with `builtin = "claude-code"` or `builtin = "llm"` and takes no `[externals.dynamic]` binding. Every other resolver is bound by name under `[externals.dynamic.<name>]` to an HTTP endpoint or a Unix command. [Externals](#externals) has the binding rule, the transports, and the consult every kind shares. `builtin` under `[externals.dynamic.<name>]` is a configuration error. A registered resolver without a binding, a binding no `[[dynamic_resolver]]` registers, a binding for a resolver that carries a builtin, and a declared builtin the deployment cannot serve — `llm` without `[externals.llm]`, `claude-code` where no Unix process group exists — refuse the deployment when it opens and when it reloads.

```toml
[[dynamic_resolver]]
name    = "classify-call"
builtin = "claude-code"
returns = ["delta.trust"]
```

A dynamic resolver has no `permits` and no ceiling. Its answer is trusted classifier evidence, whichever transport serves it, and every transport passes the same exact-shape, policy-vocabulary, audience, and pin validation. A pinned recheck and a replay never consult it again, and neither does a later proposal under the same contract with the same resolver inputs while an offer or an approval prepared for the pinned call stands in the trajectory.

The consult's declaration is the resolver's vocabulary; its artifact is `args`. For the one-argument example above:

```json
{
  "version": 1,
  "kind": "dynamic",
  "name": "classify-customer",
  "declaration": {
    "returns": ["delta.trust", "delta.audience"],
    "trust_ranks": ["suspicious", "trusted"],
    "attention_marks": ["privacy-review"]
  },
  "artifact": { "args": { "subject": "cust-7" } }
}
```

| Key | Meaning |
|---|---|
| `declaration.returns` | The results the resolver declared. The answer holds exactly these. |
| `declaration.trust_ranks` | The policy's trust chain, least-trusted first. A trust result must name one of these. |
| `declaration.attention_marks` | The attended marks. An attention result must name these only. |
| `artifact.args` | The data the tool's input mapping selected, under the resolver's declared input names. Without a mapping, the complete call: `name`, `description` when declared, and `arguments`. |

The consult carries nothing about the trajectory: no current label, no rank, no reader ids, no history. A resolver with mapped inputs that needs the tool name or its description reads it as an input.

Response, from an endpoint or a command:

```json
{
  "version": 1,
  "answer": {
    "delta.trust": "trusted",
    "delta.audience": ["finance"]
  }
}
```

`version` must match the consult. `answer` holds every result the resolver declared, keyed by the result's own name — including a result this tool does not read. A model builtin answers the same object without the envelope.

OpenAPPA rejects a missing result, an extra result, and a `null` result. It rejects an extra key beside `version` and `answer`. Trust and attention values must come from the declaration, whether or not the tool reads them: a result no field reads establishes nothing, but the record keeps it, so it answers to the same vocabulary. `delta.audience` is `"public"` or a list of reader names, and never a group. `requires.audience` is an object with `contains`, `within`, or both: `{"contains": [...], "within": [...]}`. An empty reader list is a valid, maximally restrictive answer. An empty attention list is valid, and it is the only valid attention answer when `attention_marks` is empty. A model transport's dynamic answer may name only readers that appear in `args`: the artifact is the only input a model has, so any other reader is invented. Command and endpoint resolvers answer from directories of their own and are not held to this.

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
| **Input sanitizer over a resolver** | A `tool_input` sanitizer whose `tags` cover a resolver-backed tool. | Tag the tool so no `tool_input` sanitizer covers it, unless you accept its rewrite under the earlier resolver answer. | A rewrite that stays in its contract keeps the classification of the call last consulted. One that selects another ordered contract is judged as a new call under it, with that contract's resolvers consulted. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **What an authority permits** | A wide `permits` table, such as `audience_missing = ["public"]`. | Restrict the authority's `permits` and `tags` to the minimum the desk needs. | An authority cannot rule beyond its `permits`, but a wide `permits` weakens the review gate. |
| **Auto-Approval Wiring** | `builtin = "approve"` behind a wide `permits` — an automated yes across everything it permits. | Keep what an auto-approval authority permits narrow; reserve wide `permits` for `hitl` or a reviewed resolver. | `builtin = "approve"` creates an automated open gate for all matching actions. Keep its `permits` and `tags` minimal. |
| **Model Judge Wiring** | `builtin = "claude-code"` or `builtin = "llm"` behind a wide `permits`, or on a sanitizer with a wide transition. | Keep a model authority's `permits` narrow and its `hint` exact; give a model sanitizer the narrowest transition its job needs. | `permits` caps what a model ruling clears and what a model derivation claims, not how well the model judged. The model sees only the declaration and the artifact, never the trajectory. |
| **Hint Accuracy** | A `hint` describing a power the `permits` or `may_cast` does not hold, or content the sanitizer does not remove. | Restate what the component permits in your own words: say what the entity covers, strips, or labels, and nothing more. | A hint reaches the agent with every plan naming the entity, reaches a model implementation as its charter, and grants nothing. A misleading one steers plan choice wrongly and misleads review. |

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
`[externals]` (see [Externals](#externals) for the binding rule):

```toml
[externals.authorities.finance-officer]
url       = "https://approver.corp/rule"
token_env = "APPA_APPROVER_TOKEN"            # sent as a bearer token
# Other bindings:
# command = ["/usr/local/bin/rule", "--json"]  # A local program, Unix only
# builtin = "hitl"                             # Human-in-the-loop elicitation
# builtin = "approve"                          # In-process auto-approval
# builtin = "claude-code"                      # A model rules, within `permits`
# builtin = "llm"                              # A model rules through [externals.llm]
```

A missing authority binding does not stop the deployment. That authority
returns no answer, so a remedy that names it cannot release the call.

### Authority implementation modes

| Implementation | Description | Audit Properties |
|---|---|---|
| **`builtin = "hitl"`** | Prompts a human reviewer in the loop. | Highest audit fidelity; presents the exact call and the requirements the ruling would cover to a person. |
| **`builtin = "approve"`** | Auto-approves matching gaps in-process. | Intentionally opens an automated policy bypass within what the authority `permits`. |
| **`builtin = "claude-code"`**, **`builtin = "llm"`** | A model rules from the authority's `hint` and `permits`, the call, and its unmet requirements. | `approve` or `deny` only, capped by `permits` like any implementation. The model never sees the trajectory; a wide `permits` is the review concern, not the model. |
| **`builtin = "<module name>"`** | A deployer builtin module from `--modules-dir`: your own compiled code, loaded by the runtime at startup and called in-process. | Bound by the same `permits` as any implementation; the module is deployer trusted code with the runtime's own privileges. |
| **`url = ...`**, **`command = [...]`** | Queries a privileged external service or a local program. | Receives the declaration and the call with its unmet requirements; the ruling is logged. |

An authority consult's declaration is `{"hint": …, "permits": …}` as the policy wrote it. Its artifact is the call — `tool` and canonical `arguments` — and `requirements`: the unmet requirements this ruling would cover, each `{"kind": "trust", "required": "trusted"}`, `{"kind": "audience", "required": "public"}` or `{"kind": "audience", "required": 2}` (the number of readers the call requires), `{"kind": "effect", "excludes": "…"}`, or `{"kind": "attention", "mark": "…"}`. It names no actual rank, reader, or label state. The answer is `{"ruling": "approve" | "deny", "reason"?: "…"}`; `reason` is logged at debug level and never persisted.

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
# builtin = "claude-code"                      # A model rewrites the value, within `permits`
# builtin = "llm"                              # A model rewrites it through [externals.llm]
```

### Sanitizer implementation modes

| Implementation | Description | Audit Properties |
|---|---|---|
| **`builtin = "redact-email"`** | In-process redactor: replaces email addresses with a fixed placeholder. | Deterministic and offline; guarantees exact string transformation without external calls. |
| **(reserved name `attest-schema`)** | The quarantine-exit sanitizer, registered by name alone: the engine applies it itself, so it takes no `[externals.sanitizers.attest-schema]` entry, and binding one is a load error. | Derives the return unchanged; claims instruction-cleanliness only. |
| **`builtin = "claude-code"`**, **`builtin = "llm"`** | A model rewrites the value from the sanitizer's `hint`, `on`, and `permits`, and for `tool_input` the tool's parameter schema. | `permits` caps the label the derivation claims, not the bytes the model leaves in it: what the model keeps is what crosses. Keep the transition narrow and the `hint` exact. |
| **`builtin = "<module name>"`** | A deployer builtin module from `--modules-dir`: your own compiled scrubber, loaded at startup and called in-process. | Bound by the same `permits` as any implementation; deployer trusted code. |
| **`url = ...`**, **`command = [...]`** | A scrubbing service behind an endpoint, or a local program. | The derivation is re-validated against the declared transition before admission. |

A sanitizer consult's declaration is `{"hint": …, "on": "tool_input" | "tool_output", "permits": …}` — `on` is the one point this consult applies at — plus `parameters`, the tool's argument schema, when the sanitizer rewrites `tool_input`. Its artifact is `{"tool": …, "body": …}` — the tool whose value it is, when known, and the bytes to rewrite. The answer is `{"body": …}`: the derivation, which OpenAPPA labels from `permits`.

A sanitizer permits one dimension. For trust, `from` is the rank the source must meet or exceed, and `to` is the rank the derivation carries — this is how a scrubber vouches untrusted fetched text back up:

```toml
[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" } # Instead of `audience`, never alongside it
```

When a tool result would narrow the trajectory label, OpenAPPA checks if a registered `tool_output` sanitizer can improve the label. If selected, the host withholds the raw result and runs the sanitizer. If the cleaned derivation prevents narrowing, it enters the trajectory label. If residual narrowing remains, the agent can accept the residual or apply another compatible sanitizer. A sanitizer whose declared transition cannot improve the label is never offered.

Like a cast or an authority, a sanitizer can name `tags`: it then applies only to values whose originating tool carries one of them. A child sub-execution return originates from no tool, so only a sanitizer without `tags` applies at that crossing.

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. This substitution can satisfy an unmet `contains` audience requirement, but cannot clear a `within` or trust requirement (`within` bounds the trajectory's own reach, and rewriting arguments does not change the decision to invoke the tool). A rewritten call that stays in its contract keeps the resolver answers pinned to the call last consulted (the proposal, or an earlier rewrite that selected this contract), and preserves a group membership answer only if the argument naming that group is unchanged. A rewritten call that selects another ordered contract is judged as a new call under that contract: the sanitizer's `tags` must reach that contract too, its resolvers are consulted for the rewritten arguments, and its effects and requirements apply.

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

Registering `name = "attest-schema"` is sufficient; OpenAPPA applies it natively without an `[externals]` entry (binding one is a load error).

## Casts

Unannotated tools return data in an `Unknown` label state. A `[[cast]]` resolves the whole value at once, using static rules or external classifiers: its answer is one complete label that preserves every dimension already established and makes every unresolved dimension concrete, admitted atomically or not at all.

A block lists each Unknown source by value under `unestablished`, together with the dimensions no applicable cast reaches. No remedy plan clears that slot; only an admitted cast does. While any source in a block is unestablished, the block offers no executable plan.

```toml
[[cast]]
name     = "content-classifier"
hint     = "Labels a support ticket by how far its text can be trusted."
                                              # What the classifier is for, in the deployer's words
tags     = ["support"]                        # Applies only to values from tools with these tags
resolver = { may_cast = { trust = ["suspicious"], audience = ["public"] } }
                                              # The answer must be one of these ranks and within
                                              # these readers; the ceiling is policy, the classifier deployment

[[cast]]
name     = "paranoid-default"
constant = { trust = "suspicious", audience = ["public"] }
                                              # Complete label; fallback without tags, registered last
```

```toml
[externals.casts.content-classifier]           # the deployment binds the classifier
url = "https://classify.corp/label"
# builtin = "claude-code"                      # A model labels the value, within `may_cast`
# builtin = "llm"                              # A model labels it through [externals.llm]
```

A resolver-backed cast binds under `[externals.casts.<name>]` to an HTTP endpoint, a local command, or a model builtin, always under its `may_cast` ceiling. A constant cast is defined directly in policy, so it takes no `[externals]` binding, and binding one is a load error.

When resolving an `Unknown` value, OpenAPPA evaluates applicable casts (selected by `tags`) in registration order until one returns an answer. If a resolver is unreachable or fails, OpenAPPA moves to the next registered cast. Place fallback constant casts last, as any cast registered after a constant cast without `tags` will never be reached.

OpenAPPA validates every resolver answer against the declared `may_cast` ceiling before admitting the value. An answer outside the ceiling is refused, and the cascade continues with the next applicable cast in registration order; the first answer OpenAPPA admits stands. A `may_cast` audience of `["public"]` allows a resolver to lift audience restrictions entirely; review such ceilings carefully.

A classifier can be consulted again for the same value after a runtime restart or a concurrent-write retry, so a cast implementation must be idempotent.

The runtime consults a resolver-backed cast with the common envelope. The comments explain each key; they are not sent.

```jsonc
{
  // WHY: Identifies the consult shape.
  "version": 1,

  // WHY: Says which external kind is consulted.
  "kind": "cast",

  // WHY: Selects the cast when one service answers for several.
  "name": "content-classifier",

  // WHY: What the policy declared: the deployer's hint, the ceiling the answer
  // must stay within, and the tool whose result the value is (absent for a
  // subagent's return).
  "declaration": {
    "hint": "Labels a support ticket by how far its text can be trusted.",
    "may_cast": { "trust": ["suspicious"], "audience": "public" },
    "tool": { "name": "read_ticket", "description": "Reads one support ticket." }
  },

  // WHY: The value's bytes, and nothing else — no current label, no history.
  "artifact": { "body": "the ticket text" }
}
```

The response is `{"version": 1, "answer": {"trust": "suspicious", "audience": "public"}}`, where `audience` is `"public"` or an array of literal reader ids; a model builtin answers the inner object alone. Anything else — an error status, a timeout, a malformed body, or an empty answer — is no answer: nothing is recorded, the next applicable cast is consulted, and when none answers the call stays undecided and can be proposed again.

A tool contract can also declare a pending dimension with `delta = { trust = "unknown" }`. When configured in `confined_results`, the runtime withholds raw results from the model until the cast evaluates the value. If the resolved label restricts the trajectory, OpenAPPA presents a narrowing prompt to the agent before delivering the data.

## Externals

The policy registers components by name. The deployment binds each name in the `[externals]` table:

```toml
[externals]
timeout_ms     = 2000        # one machine consult: endpoint or command
max_body_bytes = 65536       # the largest answer accepted

[externals.authorities.finance-officer]
url       = "https://approver.corp/rule"
token_env = "APPA_APPROVER_TOKEN"     # an APPA_* variable; its value is sent as a bearer token

[externals.sanitizers.pii-redactor]
builtin = "redact-email"

[externals.casts.content-classifier]
command = ["/usr/local/bin/classify", "--json"]

[externals.dynamic.classify-customer]
url = "https://classifier.corp/label"

[externals.membership.corp-directory]
url = "https://directory.corp/members"
```

An entry is `[externals.<kind>.<name>]`, with `<kind>` one of `authorities`, `sanitizers`, `casts`, `dynamic`, or `membership`. An authority, sanitizer, or cast entry takes exactly one of `url`, `command`, or `builtin`. A dynamic or membership entry takes exactly one of `url` or `command`; `builtin` there is a configuration error. A dynamic resolver that names `builtin = "claude-code"` or `builtin = "llm"` on its `[[dynamic_resolver]]` declaration takes no entry, and neither do a constant cast and the reserved `attest-schema` sanitizer. An entry whose name no declaration registers refuses the deployment when it opens, and so does a registered sanitizer, cast, dynamic resolver, or membership resolver without its entry. An authority may stay unbound; it then returns no answer, so a remedy that names it cannot release the call. An included fragment can add entries, and it can declare a dynamic resolver with a builtin: every deployment that includes it then serves that builtin — `[externals.llm]` for `llm`, a Unix host for `claude-code`. The root-wide settings (`timeout_ms`, `max_body_bytes`, `review_timeout_ms`, `[externals.claude_code]`, `[externals.llm]`) stay in the root, and the same name in two files is an error.

### Transports

| Binding | Serves | Notes |
|---|---|---|
| `url = "…"` | every kind | HTTPS anywhere; cleartext `http` only on loopback; no credentials in the URL. `token_env` names an `APPA_*` variable whose value is sent as a bearer token. |
| `command = ["…", …]` | every kind | Unix only. One JSON consult on standard input, one JSON answer on standard output; no shell; the working folder is that of the file that declares it; bounded by `timeout_ms` and `max_body_bytes`. At most eight run at once per runtime. |
| `builtin = "hitl"` | authorities | The harness asks a person. |
| `builtin = "approve"` | authorities | Approves within `permits`. |
| `builtin = "redact-email"` | sanitizers | Replaces email addresses with a placeholder. |
| `builtin = "claude-code"` | authorities, sanitizers, casts; a dynamic resolver names it on its declaration | Unix only. One isolated `claude -p` process per consult, tuned in `[externals.claude_code]`. |
| `builtin = "llm"` | authorities, sanitizers, casts; a dynamic resolver names it on its declaration | The API-key profile in `[externals.llm]`. |
| `builtin = "<module>"` | authorities, sanitizers | A deployer module from `--modules-dir`, called in-process. |

### The consult

Every transport receives one JSON object per consult:

```json
{
  "version": 1,
  "kind": "authority",
  "name": "finance-officer",
  "declaration": { "hint": "…", "permits": { "trust_below": "trusted" } },
  "artifact": { "tool": "wire_funds", "arguments": { "amount": 5000 }, "requirements": [{ "kind": "trust", "required": "trusted" }] }
}
```

| Key | Meaning |
|---|---|
| `version` | The consult shape. It is `1`. |
| `kind` | `authority`, `sanitizer`, `cast`, `dynamic`, or `membership`. |
| `name` | The registered name, for one service that answers for several. |
| `declaration` | The policy's own words for this component: its `hint`, its `permits` or `may_cast`, its `returns` and vocabulary. The policy author wrote it; the agent never can. |
| `artifact` | The value under judgment: the call and its unmet requirements, the body to rewrite or label, the resolver's `args`, or the group name. |

| Kind | `declaration` | `artifact` | `answer` |
|---|---|---|---|
| `authority` | `hint`, `permits` | `tool`, `arguments`, `requirements` | `ruling` (`approve` or `deny`), optional `reason` |
| `sanitizer` | `hint`, `on`, `permits`, `parameters` (for `tool_input`) | `tool` (when known), `body` | `body` |
| `cast` | `hint`, `may_cast`, `tool` (when known) | `body` | `trust`, `audience` |
| `dynamic` | `returns`, `trust_ranks`, `attention_marks` | `args` | one value per declared result |
| `membership` | empty | `group` | `readers` |

The consult never carries the trajectory: no current label, no rank, no reader ids, no history, no user turn. A component judges the artifact against its own declaration and nothing else.

An endpoint or a command answers `{"version": 1, "answer": { … }}`. `version` must be `1`, `answer` must hold exactly the keys its kind defines, and no other key may appear. Anything else — an error status, a non-zero exit, a timeout, an oversized body, a malformed answer — is no answer: nothing is recorded, and the flow that asked stays where it was (a blocked call, a withheld result, the next cast in the cascade). A failed consult is never a denial.

### Model transports

`claude-code` and `llm` render the same consult for a model: a fixed per-kind preamble and the `declaration` JSON as the system prompt, the `artifact` JSON as the only user turn, and an output schema built from the declaration — the `ruling` enum, the `may_cast` ranks, the declared results. The model answers the bare per-kind object; the artifact is treated as data, never as instructions. The prompt and the raw model output are never persisted; only the validated answer is.

A model answer can do what the kind allows any implementation: an authority's ruling stays within `permits`, a cast's label within `may_cast`, and a sanitizer's derivation carries exactly the `permits` transition. A dynamic resolver has no ceiling, so a model bound there is trusted classifier evidence, exactly as an endpoint is. A model sanitizer deserves a second look: `permits` caps the label the derivation claims, not the bytes the model leaves in it, so keep its transition narrow and its `hint` exact.

`[externals.claude_code]` tunes the subscription transport: `command` sets the executable, `model` pins the model, and `timeout_ms` gives a consult its own budget. Each consult is one `claude -p` process in safe mode with no tools, no project settings, no session persistence, a fresh temporary working directory, and every `APPA_*` variable removed from its environment. At most four run at once per runtime.

`[externals.llm]` is the API-key transport, one profile per deployment:

```toml
[externals.llm]
provider       = "anthropic"          # anthropic | openai | gemini | ollama
model          = "claude-sonnet-4-5"
token_env      = "APPA_LLM_TOKEN"     # required, except for ollama
# url          = "https://gateway.corp/v1"   # optional; validated like a `url` binding
timeout_ms     = 30000                # this profile's own consult budget
max_concurrent = 4                    # consults in flight at once
```

`openai` speaks the chat-completions API, so an OpenAI-compatible `url` works unchanged. `ollama` defaults to `http://localhost:11434` and takes no token.
