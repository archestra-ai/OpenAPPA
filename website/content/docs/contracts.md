---
title: Policy reference
category: Deep Dive
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads a root TOML file. The root can compose policy fragments with `include = ["battery.toml"]`. Root declarations run first. Included declarations follow in list order. An included file cannot include another file or replace root-wide settings. Duplicate external names within one kind are an error.

This document is a reference guide for writing and reviewing OpenAPPA policy TOML files. It covers global settings, audience lists and conditions, contract declarations (`[[tool]]`, `[[annotator]]`, `[[authority]]`, `[[sanitizer]]`), and policy review red flags.

```toml
version = 1

# Optional. The trust chain, least-trusted first; the rank names are yours.
# Omitted, it defaults to `suspicious < trusted`.
trust_chain = ["suspicious", "trusted"]
```

### Audience lists and conditions

An audience is a list of readers. Where a policy states the audience a value carries, it writes the list itself: `delta = { audience = ["support"] }`. The list `["public"]` is the unrestricted audience. The same bare list form sets `[boundary].audience` and `starting_label.audience`.

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
[[tool]]
name = "search(query:a\\,b)"   # a basic string doubles the backslash

[[tool]]
name = 'search(title:a\,b)'    # a literal string does not
```

A missing or non-string argument does not match. Clause order does not change the result: `tool(owner:x,repo:y)` and `tool(repo:y,owner:x)` are the same contract. A selector can name each argument only once. A bare tool name matches every argument object and is typically the fallback.

OpenAPPA selects the contract before it validates that contract's `parameters` schema. A schema failure does not continue to a later contract. A `tool_input` rewrite is judged by the contract its rewritten arguments select. An annotation binds the exact call, so a rewrite of an annotator-backed tool is annotated afresh, whichever contract it selects. Provider-run tools cannot use argument selectors.

### Annotators

Every released tool call carries one complete annotation: the `delta` its result contributes, the `requires` it must meet, and the effects it emits. A `[[tool]]` entry usually writes that annotation statically. Where the right contract depends on the call itself — a file path, a recipient, a command line — the entry names a registered **annotator** instead, and the annotator answers the complete annotation for each proposed call. An annotator does not resolve `@group` membership.

An `[[annotator]]` declares three things. Its optional `hint` explains the policy vocabulary. Its `inputs` select call data. Its **mandate** bounds every answer. A `[[tool]]` routes through it with `annotator = "<name>"`. That tool entry writes no `delta`, `requires`, or `effects` because the annotator produces all three. Annotator names are opaque non-empty strings and can contain dots.

#### Example: pass the complete call

Omit `inputs` to pass the complete tool call: its name, its description when the tool declares one, and its arguments.

```toml
[[annotator]]
name  = "classify-command"
ranks = ["suspicious", "trusted"]              # The trust ranks its answers may use
hint  = "Use suspicious for output from network or unvetted sources. Use trusted only for local computation over trusted inputs."

[[tool]]
name        = "Bash"
description = "Runs one shell command and returns its output."
annotator   = "classify-command"
```

The annotator receives this consult artifact:

```json
{
  "args": {
    "name": "Bash",
    "description": "Runs one shell command and returns its output.",
    "arguments": {
      "command": "git push origin main",
      "timeout": 60000
    }
  }
}
```

This form does not need a tool parameter schema. It does not need a `description`: a tool without one sends `name` and `arguments` only.

#### Example: pass one argument

```toml
[[annotator]]
name      = "classify-customer"
inputs    = { subject = "$tool_call.arguments.customer_id" }
ranks     = ["suspicious", "trusted"]
audiences = ["finance", "support"]             # The readers a restricted audience answer may name
hint      = "finance may read billing records. support may read records assigned to a support case."

[[tool]]
name       = "get_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator  = "classify-customer"
```

The annotator receives only `customer_id`, under the name `subject`. Its answer is still the complete contract for the call.

#### Example: cover the long tail with a wildcard

The wildcard entry `name = "*"` covers every call the policy does not name. It must name an `annotator` and nothing else — no static fields, no metadata, no argument selector — and a policy writes at most one. An exact declaration always wins over it.

```toml
[[annotator]]
name      = "classify-anything"
ranks     = ["suspicious"]                     # The long tail never earns trust
audiences = ["internal"]

[[tool]]
name      = "*"
annotator = "classify-anything"
```

A call no declaration and no wildcard covers is refused before it runs. That refusal is operational, not a policy denial.

#### The mandate

The mandate is the vocabulary an annotator's answers may use. Every bound is optional; an omitted bound admits the whole policy vocabulary, so a reviewed mandate is written, not implied.

The optional `hint` is a trusted deployer instruction. It can define ranks, audiences, marks, and effects, as well as specify evidence rules and examples. The hint is advisory and cannot expand the mandate. A hint cannot exceed 512 characters.

| Key | Bounds | Omitted |
|---|---|---|
| `ranks` | The trust ranks an answer may write in `delta.trust` and `requires.trust`. | Every rank in the trust chain. |
| `audiences` | The literal readers a restricted audience answer may name. `public` is always admissible and is never listed as a reader; a group is never admissible. An empty list closes the mandate to `public` answers only. | Every reader the policy writes. |
| `marks` | The attention marks an answer may require. | Every mark an authority names under `permits.attention`. |
| `effects` | The effect kinds an answer may emit or check in history. | Every effect kind the policy declares. |

#### Rules

- An annotator declares its optional hint, inputs, and mandate. A tool routes through at most one with `annotator`. That replaces static `delta`, `requires`, and `effects`; writing both forms is a load error.
- The answer is one complete annotation. An omitted leaf is the identity: no restriction on that dimension, no requirement in that slot.
- The answer is pinned to the exact call it annotated. A pinned recheck and a replay never consult the annotator again, and a `tool_input` rewrite is annotated afresh for its own bytes.
- If the annotator fails or answers outside its mandate, the call does not run. The refusal is operational — the call was never judged — and nothing is appended: the call can be proposed again.

An annotator maps each declared input from the proposed call. `$tool_call` is the only source.

| Value | Meaning |
|---|---|
| `$tool_call` | Complete tool call: `name`, `description` when the tool declares one, and `arguments` |
| `$tool_call.name` | Tool name |
| `$tool_call.description` | Tool description from the policy |
| `$tool_call.arguments` | Complete argument object |
| `$tool_call.arguments.<name>` | One top-level argument |

An annotator with no `inputs` receives `$tool_call` as its `args`. `$tool_call.description` needs a tool `description`; `$tool_call` does not, and omits the key when there is none. A single argument needs a tool parameter schema, and the schema must mark that top-level argument as required. The annotator then receives whatever JSON value the call carries under that name.

#### Implementing an annotator

An Annotator either carries an inline builtin implementation or delegates its execution to the deployment. An Annotator using a stock model builtin specifies `builtin = "claude-code"` or `builtin = "llm"` on its declaration and requires no `[externals.annotators]` binding. Every other Annotator is bound by name under `[externals.annotators.<name>]` to an HTTP endpoint or a Unix command. The [Externals](#externals) section details the binding rules, transports, and shared consult structure. Specifying `builtin` under `[externals.annotators.<name>]` is a configuration error.

The deployment refuses to open or reload if any of the following occur:
- A registered Annotator lacks a binding.
- A binding references an unregistered Annotator.
- A binding is defined for an Annotator that already specifies a `builtin`.
- A declared `builtin` cannot be served (for example, `llm` without `[externals.llm]`, or `claude-code` where no Unix process group exists).

```toml
[[annotator]]
name    = "classify-call"
builtin = "claude-code"
hint    = "Use internal for company-only data and suspicious for data from unvetted sources."
```

The mandate is the ceiling policy review relies on, whichever transport serves the annotator: every transport passes the same exact-shape and mandate validation before an annotation is admitted.

The consult declaration carries the hint, input names, and resolved mandate. Its artifact is `args`. For the one-argument example above:

```json
{
  "version": 1,
  "kind": "annotation",
  "name": "classify-customer",
  "declaration": {
    "hint": "finance may read billing records. support may read records assigned to a support case.",
    "inputs": ["subject"],
    "trust_ranks": ["suspicious", "trusted"],
    "audiences": ["finance", "support"],
    "attention_marks": [],
    "effects": []
  },
  "artifact": { "args": { "subject": "cust-7" } }
}
```

| Key | Meaning |
|---|---|
| `declaration.hint` | The deployer's optional instruction for policy-specific classification. It grants nothing outside the mandate. |
| `declaration.inputs` | The declared input names. Empty when the annotator reads the complete call. |
| `declaration.trust_ranks` | The mandate's trust ranks, least-trusted first. A trust value must name one of these. |
| `declaration.audiences` | The mandate's readers. A restricted audience value may name these only. |
| `declaration.attention_marks` | The mandate's attention marks. An attention value must name these only. |
| `declaration.effects` | The mandate's effect kinds. An `emits` or history value must name these only. |
| `artifact.args` | The data the input mapping selected, under the declared input names. Without a mapping, the complete call: `name`, `description` when declared, and `arguments`. |

The consult carries nothing about the trajectory: no current label, no rank, no reader ids, and no history.

Response, from an endpoint or a command:

```json
{
  "version": 1,
  "answer": {
    "delta": { "trust": "suspicious", "audience": ["finance"] },
    "requires": { "history": [], "attention": [] },
    "emits": []
  }
}
```

`version` must match the consult. `answer` is exactly one object with exactly three keys: `delta`, `requires`, and `emits`. `requires` always carries its `history` and `attention` arrays, even empty; every other leaf is optional and means the identity when omitted. A model builtin answers the same object without the envelope.

OpenAPPA rejects a `null` anywhere, an unknown key anywhere, an empty `audience` object, a duplicate `emits` kind, and any value outside the mandate. `delta.audience` and the audience leaves of `requires` are `"public"` or a list of the mandate's readers, and never a group. `requires.audience` is an object with `contains`, `within`, or both. Each `history` entry is one object with one key: `{"contains": "<effect>"}` or `{"excludes": "<effect>"}`. A rejected answer is no answer: the call does not run, nothing is recorded, and the call can be proposed again.

### Deployment coverage

The `[deployment]` table declares the capabilities of your hosting environment—such as starting security labels, enforced execution points, raw output withholding, and child branch isolation.

During policy load, OpenAPPA validates that all declared constructs are supported by the deployment. If a policy requires a capability the deployment lacks, loading fails with an explicit error:
- A `tool_output` sanitizer requires an application point the deployment can withhold: a tool listed in `confined_results`, or the child-return crossing (`confined_child_return = true`).
- A tool listed in `confined_results` requires a deployment that can withhold raw results. A provider-run tool cannot be listed: its result reaches the model inside the inference call, before any host could withhold it. A coverage entry must name a tool the policy covers; with a wildcard, every name qualifies.
- A `[child]` section requires child context isolation.
- Provider-run tools (tools executed directly inside a provider inference call) may declare only a static `delta`: no `requires`, no `annotator`, and no argument selectors.

Uncovered vectors are explicitly declared in the deployment configuration so security leaders can audit them, rather than silently degrading guarantees.

## What to check when reviewing

A tool contract is short: a name, a `delta`, and often `effects` and a `[tool.requires]` table of one key per line. Use this checklist during policy review to catch common syntax and structural red flags:

| Review Area | Red Flag / Misconfiguration | Safe / Correct Pattern | Spec Invariant & Risk |
|---|---|---|---|
| **`delta` Accuracy** | Tool reads sensitive customer data but declares `delta = {}` or omits `delta`. | Declare explicit restriction, e.g. `delta = { audience = ["support"] }`. | Undermines downstream checks; over-restricting is safe (costs reach, doesn't leak). |
| **Expecting late classification** | Omitting `delta` in the belief that something will classify the result later. | Omit `delta`, or write `delta = {}`, for a result that carries no restriction; name an `annotator` when the restriction depends on the call. | An unwritten dimension is the identity, not a pending state; nothing classifies it later. |
| **`effects` Completeness** | Mutation or deployment tool omits `effects`. | Declare all side effects, e.g., `effects = ["migration.applied", "mutation"]`. | Under-declared effects pass `excludes` checks silently without triggering history constraints. |
| **Dynamic Recipients** | Static readers when an ACL depends on an argument. | Use a placeholder for a recipient the call names — a literal reader, `public`, or an `@group` — or an annotator for an argument-derived contract. | Static readers can ignore the proposed argument; placeholder groups and annotations pin their answer to the call. |
| **Annotator beside statics** | A `[[tool]]` that names an `annotator` and also writes `delta`, `requires`, or `effects`. | Give the contract one producer: a static declaration, or an annotator that answers all three. | It does not load: `annotator` replaces the static semantic fields. |
| **Unbounded wildcard mandate** | `name = "*"` routed through an annotator that declares no `ranks`, `audiences`, `marks`, or `effects`. | Bound the wildcard annotator's mandate to the vocabulary the long tail actually needs. | An omitted bound admits the whole policy vocabulary; the mandate is the ceiling review relies on, not the annotator's judgment. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **What an authority permits** | A wide `permits` table, such as `audience_missing = ["public"]`. | Restrict the authority's `permits` and `tags` to the minimum the desk needs. | An authority cannot rule beyond its `permits`, but a wide `permits` weakens the review gate. |
| **Auto-Approval Wiring** | `builtin = "approve"` behind a wide `permits` — an automated yes across everything it permits. | Keep what an auto-approval authority permits narrow; reserve wide `permits` for `hitl` or a reviewed resolver. | `builtin = "approve"` creates an automated open gate for all matching actions. Keep its `permits` and `tags` minimal. |
| **Model Judge Wiring** | `builtin = "claude-code"` or `builtin = "llm"` behind a wide `permits`, transition, or Annotator mandate. | Keep `permits` and transitions narrow. Bound each Annotator mandate and give it an exact `hint`. | The declaration caps model output. It does not prove that the model classified the artifact correctly. The model never sees the trajectory. |
| **Hint Accuracy** | A `hint` describes unavailable authority powers, incomplete sanitizer behavior, or Annotator vocabulary incorrectly. | State what the component covers, removes, or classifies. For an Annotator, define policy-specific values and the evidence that selects them. | A hint grants nothing. A misleading hint directs model judgment incorrectly and misleads policy review. |

## Tools

A `[[tool]]` entry defines its name and `description`, then its output restrictions (`delta`), side effects (`effects`), and dispatch conditions (`requires`) — or the `annotator` that produces that whole contract per call.

```toml
[[tool]]
name = "fetch_support_ticket"
tags = ["support"]                                     # Authorities and sanitizers select tools by tag

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
- **Omitted is identity**: An omitted `delta` and `delta = {}` say the same thing — the result carries no restriction. An omitted `requires` slot asks nothing. There is no pending state: what the annotation does not restrict, nothing restricts later.
- **Annotators (`annotator`)**: Routes every call of the tool through one registered `[[annotator]]`, which answers the complete contract — `delta`, `requires`, and effects — for that call, inside its declared mandate. The answer is pinned to the exact call and holds on replay.
- **One producer per contract**: A contract's semantics have one source — the static fields, or the named annotator. `annotator` beside `delta`, `requires`, or `effects` is a load error.
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { contains = ["$recipient"] }` evaluates `$recipient` against the actual call argument at runtime. The argument value can be a literal reader ID, the reserved word `public`, or an `@group` expanded by the membership resolver. Placeholders are supported only inside `contains`. The argument must be declared as a required top-level string in the tool's `parameters` schema.
- **Wildcard tool (`name = "*"`)**: Covers every call the policy does not name. It must name an `annotator` and nothing else — no static fields, no metadata, no argument selector — and a policy writes at most one. An exact declaration always wins over it. A call no declaration and no wildcard covers is refused before it runs.
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
[[tool]]
name  = "fetch_support_ticket"
tags  = ["support"]
delta = { trust = "suspicious", audience = ["finance"] }

[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]                         # Tool results and child sub-execution returns
# on = ["tool_input"]                          # Whole-argument substitution at dispatch
hint = "Removes personal details from a finance record."  # Advisory; grants nothing
tags = ["support"]                             # Applies only to values from tools with these tags

[sanitizer.permits]
# `from`: the source audience must contain these readers; `to`: the output gets exactly this audience
audience = { from = ["finance"], to = ["public"] }

[deployment]
confined_results = ["fetch_support_ticket"]    # The host can withhold this tool's raw result
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
[[sanitizer]]
name = "vouch-fetched-text"
on   = ["tool_output"]

[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" } # Instead of `audience`, never alongside it

[deployment]
confined_child_return = true                    # The child-return crossing is an application point
```

When a tool result would narrow the trajectory label, OpenAPPA checks if a registered `tool_output` sanitizer can improve the label. If selected, the host withholds the raw result and runs the sanitizer. If the cleaned derivation prevents narrowing, it enters the trajectory label. If residual narrowing remains, the agent can accept the residual or apply another compatible sanitizer. A sanitizer whose declared transition cannot improve the label is never offered.

Like an authority, a sanitizer can name `tags`: it then applies only to values whose originating tool carries one of them. A child sub-execution return originates from no tool, so only a sanitizer without `tags` applies at that crossing.

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. This substitution can satisfy an unmet `contains` audience requirement, but cannot clear a `within` or trust requirement (`within` bounds the trajectory's own reach, and rewriting arguments does not change the decision to invoke the tool). A rewritten call is judged by the ordered contract its rewritten arguments select: the sanitizer's `tags` must reach that contract too, and its effects and requirements apply. An annotation binds the exact call, so a rewrite of an annotator-backed tool is annotated afresh, whichever contract it selects; a group membership answer survives only when the rewrite stays in its contract and the argument naming the group is unchanged.

To enforce automated return sanitization across all child sub-executions, policies can bind a default return sanitizer:

```toml
[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]

[sanitizer.permits]
audience = { from = ["finance"], to = ["public"] }

[deployment]
context_control       = true        # [child] needs child context isolation
confined_child_return = true

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

[deployment]
confined_child_return = true
```

Registering `name = "attest-schema"` is sufficient; OpenAPPA applies it natively without an `[externals]` entry (binding one is a load error).

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

[externals.annotators.classify-customer]
url = "https://classifier.corp/label"

[externals.membership.corp-directory]
url = "https://directory.corp/members"
```

An entry is `[externals.<kind>.<name>]`, with `<kind>` one of `authorities`, `sanitizers`, `annotators`, or `membership`. An authority or sanitizer entry takes exactly one of `url`, `command`, or `builtin`. An annotator or membership entry takes exactly one of `url` or `command`; `builtin` there is a configuration error. An annotator that names `builtin = "claude-code"` or `builtin = "llm"` on its `[[annotator]]` declaration takes no entry, and neither does the reserved `attest-schema` sanitizer. An entry whose name no declaration registers refuses the deployment when it opens, and so does a registered sanitizer, annotator, or membership resolver without its entry. An authority may stay unbound; it then returns no answer, so a remedy that names it cannot release the call. An included fragment can add entries, and it can declare an annotator with a builtin: every deployment that includes it then serves that builtin — `[externals.llm]` for `llm`, a Unix host for `claude-code`. The root-wide settings (`timeout_ms`, `max_body_bytes`, `review_timeout_ms`, `[externals.claude_code]`, `[externals.llm]`) stay in the root, and the same name in two files is an error.

### Transports

| Binding | Serves | Notes |
|---|---|---|
| `url = "…"` | every kind | HTTPS anywhere; cleartext `http` only on loopback; no credentials in the URL. `token_env` names an `APPA_*` variable whose value is sent as a bearer token. |
| `command = ["…", …]` | every kind | Unix only. One JSON consult on standard input, one JSON answer on standard output; no shell; the working folder is that of the file that declares it; bounded by `timeout_ms` and `max_body_bytes`. At most eight run at once per runtime. |
| `builtin = "hitl"` | authorities | The harness asks a person. |
| `builtin = "approve"` | authorities | Approves within `permits`. |
| `builtin = "redact-email"` | sanitizers | Replaces email addresses with a placeholder. |
| `builtin = "claude-code"` | authorities, sanitizers; an annotator names it on its declaration | Unix only. One isolated `claude -p` process per consult, tuned in `[externals.claude_code]`. |
| `builtin = "llm"` | authorities, sanitizers; an annotator names it on its declaration | The API-key profile in `[externals.llm]`. |
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
| `kind` | `authority`, `sanitizer`, `annotation`, or `membership`. |
| `name` | The registered name, for one service that answers for several. |
| `declaration` | The policy-authored half: a component's `hint` and `permits`, or an Annotator's hint, input names, and mandate. The agent never writes it. |
| `artifact` | The judged value: a call and its unmet requirements, a body to rewrite, an Annotator's `args`, or a group name. |

| Kind | `declaration` | `artifact` | `answer` |
|---|---|---|---|
| `authority` | `hint`, `permits` | `tool`, `arguments`, `requirements` | `ruling` (`approve` or `deny`), optional `reason` |
| `sanitizer` | `hint`, `on`, `permits`, `parameters` (for `tool_input`) | `tool` (when known), `body` | `body` |
| `annotation` | `hint`, `inputs`, `trust_ranks`, `audiences`, `attention_marks`, `effects` | `args` | `delta`, `requires`, `emits` |
| `membership` | empty | `group` | `readers` |

The consult never carries the trajectory: no current label, no rank, no reader ids, no history, no user turn. A component judges the artifact against its own declaration and nothing else.

An endpoint or a command answers `{"version": 1, "answer": { … }}`. `version` must be `1`, `answer` must hold exactly the keys its kind defines, and no other key may appear. Anything else — an error status, a non-zero exit, a timeout, an oversized body, a malformed answer — is no answer: nothing is recorded, and the flow that asked stays where it was (a blocked call, a withheld result, an unannotated call that never runs). A failed consult is never a denial.

### Model transports

`claude-code` and `llm` render the same model consult. The system prompt contains a fixed preamble and the `declaration` JSON. The `artifact` JSON is the only user turn. The output schema comes from the declaration, including an Annotator's mandate vocabulary. The model answers the bare per-kind object. The artifact is data, never instructions. OpenAPPA does not persist the prompt or raw model output. It persists only the validated answer.

A model answer can do what the kind allows any implementation: an authority's ruling stays within `permits`, an annotation within its annotator's mandate, and a sanitizer's derivation carries exactly the `permits` transition. A model sanitizer deserves a second look: `permits` caps the label the derivation claims, not the bytes the model leaves in it, so keep its transition narrow and its `hint` exact.

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
