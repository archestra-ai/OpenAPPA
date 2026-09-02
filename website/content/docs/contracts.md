---
title: Policy reference
category: Deep Dive
order: 3
description: Declarations, syntax, and rules for OpenAPPA policy TOML files.
---

OpenAPPA reads a root TOML file. The root can compose policy fragments with `include = ["battery.toml"]`. Root declarations run first. Included declarations follow in list order. An included file cannot include another file or replace root-wide settings. Duplicate external names within one kind are an error.

This document is a reference guide for writing and reviewing OpenAPPA policy TOML files. It covers global settings, audience lists and conditions, contract declarations (`[[tool]]`, `[[annotator]]`, `[[authority]]`, `[[sanitizer]]`), and policy review red flags.

```toml
version = 2

# Optional. The trust chain, least-trusted first; the rank names are yours.
# Omitted, it defaults to `suspicious < trusted`.
trust_chain = ["suspicious", "trusted"]
```

### Audience lists and conditions

An audience list is the union of its entries. An entry is one of four spellings:

| Entry | Meaning |
|---|---|
| `"public"` | The unrestricted audience. It stands alone: `public` is the whole universe and combines with nothing. |
| `"self"`, `"internal"` | A built-in symbolic audience (see [the chain](#audiences)). At most one per list — the union of two chain levels is the outer one, so writing both is a mistake the policy refuses. |
| `"@name"`, `"@provider:selector"` | A mention of a configured named audience, or of a source collection directly. |
| anything else | A literal reader ID, compared exactly. |

Where a policy states the audience a value carries, it writes the list itself: `delta = { audience = ["support"] }`. The same bare list form sets `[boundary].audience` and `starting_label.audience`. Symbolic entries stay symbolic: a label holds `internal` or `@finance` as written, and the log records it that way.

Where a policy checks the current audience or the trajectory's history, it names the condition:

| Key | Under | Meaning | Example |
|---|---|---|---|
| **`contains`** | `requires.audience` | The current audience must include these readers. A `$arg` placeholder is allowed only here. | `audience = { contains = ["$recipient"] }` |
| **`within`** | `requires.audience` | The current audience must sit within this audience. | `audience = { within = ["internal"] }` |
| **`contains`** | `requires.effects` | The trajectory already recorded this effect. | `effects = { contains = ["backup.completed"] }` |
| **`excludes`** | `requires.effects` | The effect is neither recorded in the trajectory nor reserved by an unsettled dispatch. | `effects = { excludes = ["migration.applied"] }` |

Any other key under `requires.audience` or `requires.effects` is a policy load error.

### Audiences

OpenAPPA ships a built-in audience chain:

**`self` ⊆ `internal` ⊆ `public`**

`self` is the deployment's configured operating principal — whoever its credentials represent, which need not be a person. `internal` is the organization. `public` is the unrestricted state. The chain is fixed — a policy maps sources into its levels but never adds levels. Beyond the chain, `[[audience.group]]` declares named audiences (`@finance`), composed from the same sources.

A symbolic audience is a named reader set, and it stays symbolic: a label holds `internal ∩ [alice]` or `internal ∩ @finance` without expanding either name. When a decision needs actual membership — a `contains` or `within` comparison — OpenAPPA consults the configured sources for exactly the sets that decision reads, pins the answers to that one act, and records them with it. An act accepts only that evidence: an answer neither inherited from the record the act continues nor requested by its own reads is refused, live and at replay. Replay reads the pinned answers and never consults a source. A pinned answer is inherited by the acts that continue the same record — the remedy plan or approval it opened — so directory changes apply to the next independent decision; they never rewrite a label.

#### Audience sources

This build ships one audience source per provider, each with a fixed set of selector templates. A `from` list picks collections out of them; a provider enters the policy identity only when a selector references it:

| Provider | Selectors |
|---|---|
| `google-workspace` | `viewer`, `full-members`, `group/<group-address>` |
| `slack` | `viewer`, `full-members`, `user-group/<handle>` |
| `github` | `viewer`, `org/<org>/members`, `org/<org>/team/<team>` |

`viewer` names the requesting principal and feeds only `self`. The full-membership collections — and, for GitHub, one explicitly selected organization's members — feed `internal`: membership in unrelated, open-source, or personal organizations never implies `internal`. The named and full-membership collections feed `[[audience.group]]` entries; `viewer` does not. A selector that does not fit its level refuses the policy at load.

```toml
[audience.self]
from = ["google-workspace:viewer", "slack:viewer", "github:viewer"]

[audience.internal]
from = [
  "google-workspace:full-members",
  "slack:full-members",
  "github:org/archestra-ai/members",   # only this organization; selected explicitly
]

[[audience.group]]
name   = "finance"
within = "internal"                    # a trusted policy assertion: @finance ⊆ internal
from   = ["google-workspace:group/finance@corp.com", "github:org/archestra-ai/team/finance"]
```

Multiple sources feeding one audience are unioned. `within` asserts containment in a built-in audience (`self` or `internal`), and the engine trusts the assertion as policy. If the finance source reports an externally addressed member, that reader belongs to `@finance` and therefore to `internal`: OpenAPPA never second-guesses a configured source by inspecting a reader's email domain.

A mention is `@` followed by the one selector grammar: `@finance` names a configured `[[audience.group]]`, and `@slack:user-group/oncall` reads a source collection directly, without a group declaration. A direct source mention still needs its provider referenced by some `from` list — a provider enters the registered sources exactly when a selector picks from it. A mention written in the policy is validated at load: `@finacne` with no declaration refuses the policy. A dynamically supplied mention no source serves fails the call operationally — the call does not run and nothing is recorded.

#### Identity

Provider identities differ: `google-workspace:alice@corp.com`, `slack:U012345`, and `github:alice` may be the same person. Before any exact reader comparison, each member a source reports is canonicalized to one principal by the deployment's identity implementation:

```toml
[identity]
implementation = "verified-email"      # the shipped default
```

`verified-email` is deterministic and network-free. A member with a verified email becomes that address; a member without one keeps its provider-qualified ID. The address is the principal itself, so a reader written as an address — in a policy, in a tool argument, or in an annotation — is the same reader the directory's verified claim resolves to, and an ordinary email recipient matches the directory member who holds that address. It trusts only claims the source marks verified and applies only conservative normalization (domain case only — no dot removal, no `+suffix` stripping, no alias folding), so a personal GitHub email and a corporate Workspace email stay distinct principals. Explicit aliasing is what a custom implementation is for:

```toml
[identity]
implementation = "corp-identity"       # bound under [externals.identity.corp-identity]
```

A custom implementation answers one principal per member, is pinned per act like any membership answer, and must be deterministic. If it fails or answers an invalid principal, the act fails operationally and records nothing.

#### Bindings and failure

The deployment binds each referenced provider under `[externals.audience.<provider>]`, and a custom identity implementation under `[externals.identity.<name>]`, each to an HTTP endpoint or a local command; no builtin serves either kind. [Externals](#externals) has the envelope. An audience consult's declaration is the provider's selector templates; its artifact is one selector (`{"selector": "user-group/oncall"}`, answered `{"members": [{"id": "slack:U1", "verified_email": "a@corp.com"}, ...]}` — an empty list is a complete answer) or one member lookup (`{"member": "slack:U1"}`, answered `{"claims": {...}}` or `{"claims": null}`). An identity consult's artifact is one member's claims, answered `{"principal": "a@corp.com"}`.

Any source or identity failure — timeout, network error, invalid answer — halts the act with an operational error and records nothing to the log. A failed consult is never a policy decision.

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

Every released tool call carries one complete annotation: the `delta` its result contributes, the `requires` it must meet, and the effects it emits. A `[[tool]]` entry usually writes that annotation statically. Where the right contract depends on the call itself — a file path, a recipient, a command line — the entry names a registered **annotator** instead, and the annotator answers the complete annotation for each proposed call. An annotator's answers name literal readers only — never a symbolic audience.

An `[[annotator]]` declares two things: the `inputs` it reads from a proposed call, and its **mandate** — the closed vocabulary its answers may use. A `[[tool]]` routes through it with `annotator = "<name>"`; that entry then writes no `delta`, `requires`, or `effects` of its own, because the annotator produces all three. Annotator names are opaque non-empty strings and can contain dots.

#### Example: pass the complete call

Omit `inputs` on the annotator to pass the complete tool call: its name, its description when the tool declares one, and its arguments.

```toml
[[annotator]]
name  = "classify-command"
ranks = ["suspicious", "trusted"]              # The trust ranks its answers may use

[[tool]]
name        = "Bash"
description = "Runs one shell command and returns its output."
annotator   = "classify-command"
```

The annotator receives this value as the consult's `artifact.args`:

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
[[annotator]]
name      = "classify-customer"
inputs    = { subject = "$tool_call.arguments.customer_id" }
ranks     = ["suspicious", "trusted"]
audiences = ["finance", "support"]             # The readers a restricted audience answer may name

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
audiences = ["support"]

[[tool]]
name      = "*"
annotator = "classify-anything"
```

A call no declaration and no wildcard covers is refused before it runs. That refusal is operational, not a policy denial.

#### The mandate

The mandate is the vocabulary an annotator's answers may use. Every bound is optional; an omitted bound admits the whole policy vocabulary, so a reviewed mandate is written, not implied.

| Key | Bounds | Omitted |
|---|---|---|
| `ranks` | The trust ranks an answer may write in `delta.trust` and `requires.trust`. | Every rank in the trust chain. |
| `audiences` | The literal readers a restricted audience answer may name. `public` is always admissible and is never listed as a reader; a symbolic audience — `self`, `internal`, or a mention — is never admissible. An empty list closes the mandate to `public` answers only. | Every reader the policy writes. |
| `marks` | The attention marks an answer may require. | Every mark an authority names under `permits.attention`. |
| `effects` | The effect kinds an answer may emit or check in history. | Every effect kind the policy declares. |

#### Rules

- An annotator declares its inputs and its mandate. A tool routes through at most one, with `annotator`; that replaces the static `delta`, `requires`, and `effects`, and writing it beside any of them is a load error.
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

An annotator either carries its implementation or leaves it to the deployment. An annotator that carries a stock model builtin names it on its declaration with `builtin = "claude-code"` or `builtin = "llm"` and takes no `[externals.annotators]` binding. Every other annotator is bound by name under `[externals.annotators.<name>]` to an HTTP endpoint or a Unix command. [Externals](#externals) has the binding rule, the transports, and the consult every kind shares. `builtin` under `[externals.annotators.<name>]` is a configuration error. A registered annotator without a binding, a binding no `[[annotator]]` registers, a binding for an annotator that carries a builtin, and a declared builtin the deployment cannot serve — `llm` without `[externals.llm]`, `claude-code` where no Unix process group exists — refuse the deployment when it opens and when it reloads.

```toml
[[annotator]]
name    = "classify-call"
builtin = "claude-code"
```

The mandate is the ceiling policy review relies on, whichever transport serves the annotator: every transport passes the same exact-shape and mandate validation before an annotation is admitted.

The consult's declaration is the annotator's resolved mandate and input names; its artifact is `args`. For the one-argument example above:

```json
{
  "version": 1,
  "kind": "annotation",
  "name": "classify-customer",
  "declaration": {
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
| `declaration.inputs` | The declared input names. Empty when the annotator reads the complete call. |
| `declaration.trust_ranks` | The mandate's trust ranks, least-trusted first. A trust value must name one of these. |
| `declaration.audiences` | The mandate's readers. A restricted audience value may name these only. |
| `declaration.attention_marks` | The mandate's attention marks. An attention value must name these only. |
| `declaration.effects` | The mandate's effect kinds. An `emits` or history value must name these only. |
| `artifact.args` | The data the input mapping selected, under the declared input names. Without a mapping, the complete call: `name`, `description` when declared, and `arguments`. |

The consult carries nothing about the trajectory: no current label, no rank, no reader ids, no history. An annotator with mapped inputs that needs the tool name or its description reads it as an input.

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

OpenAPPA rejects a `null` anywhere, an unknown key anywhere, an empty `audience` object, a duplicate `emits` kind, and any value outside the mandate. `delta.audience` and the audience leaves of `requires` are `"public"` or a list of the mandate's readers, and never a symbolic audience. `requires.audience` is an object with `contains`, `within`, or both. Each `history` entry is one object with one key: `{"contains": "<effect>"}` or `{"excludes": "<effect>"}`. A rejected answer is no answer: the call does not run, nothing is recorded, and the call can be proposed again.

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
| **Dynamic Recipients** | Static readers when an ACL depends on an argument. | Use a placeholder for a recipient the call names — a literal reader, `public`, a built-in audience, or an `@` mention — or an annotator for an argument-derived contract. | Static readers can ignore the proposed argument; membership answers and annotations pin to the call. |
| **Annotator beside statics** | A `[[tool]]` that names an `annotator` and also writes `delta`, `requires`, or `effects`. | Give the contract one producer: a static declaration, or an annotator that answers all three. | It does not load: `annotator` replaces the static semantic fields. |
| **Unbounded wildcard mandate** | `name = "*"` routed through an annotator that declares no `ranks`, `audiences`, `marks`, or `effects`. | Bound the wildcard annotator's mandate to the vocabulary the long tail actually needs. | An omitted bound admits the whole policy vocabulary; the mandate is the ceiling review relies on, not the annotator's judgment. |
| **Combined Read & Release** | Single tool `share_doc(doc, recipient)` fetching and releasing in one step. | Split into `fetch_doc` (read) and `grant_doc_access` (release). | Combined tools force authorities to approve releases before content is fetched. |
| **What an authority permits** | A wide `permits` table, such as `audience_missing = ["public"]`. | Restrict the authority's `permits` and `tags` to the minimum the desk needs. | An authority cannot rule beyond its `permits`, but a wide `permits` weakens the review gate. |
| **Auto-Approval Wiring** | `builtin = "approve"` behind a wide `permits` — an automated yes across everything it permits. | Keep what an auto-approval authority permits narrow; reserve wide `permits` for `hitl` or a reviewed resolver. | `builtin = "approve"` creates an automated open gate for all matching actions. Keep its `permits` and `tags` minimal. |
| **Model Judge Wiring** | `builtin = "claude-code"` or `builtin = "llm"` behind a wide `permits`, or on a sanitizer with a wide transition. | Keep a model authority's `permits` narrow and its `hint` exact; give a model sanitizer the narrowest transition its job needs. | `permits` caps what a model ruling clears and what a model derivation claims, not how well the model judged. The model sees only the declaration and the artifact, never the trajectory. |
| **Hint Accuracy** | A `hint` describing a power the `permits` does not hold, or content the sanitizer does not remove. | Restate what the component permits in your own words: say what the entity covers, strips, or labels, and nothing more. | A hint reaches the agent with every plan naming the entity, reaches a model implementation as its charter, and grants nothing. A misleading one steers plan choice wrongly and misleads review. |

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
- **Dynamic argument placeholders (`$arg`)**: `requires.audience = { contains = ["$recipient"] }` evaluates `$recipient` against the actual call argument at runtime. The argument value can be a literal reader ID, the reserved word `public`, a built-in audience (`self`, `internal`), or an `@` mention read from the configured sources. Placeholders are supported only inside `contains`. The argument must be declared as a required top-level string in the tool's `parameters` schema. A dynamically supplied mention no source serves fails the call operationally, never as a policy denial.
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

At `tool_input`, the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes. This substitution can satisfy an unmet `contains` audience requirement, but cannot clear a `within` or trust requirement (`within` bounds the trajectory's own reach, and rewriting arguments does not change the decision to invoke the tool). A rewritten call is judged by the ordered contract its rewritten arguments select: the sanitizer's `tags` must reach that contract too, and its effects and requirements apply. An annotation binds the exact call, so a rewrite of an annotator-backed tool is annotated afresh, whichever contract it selects; membership answers are pinned to the act, so the substituted call is judged under the same pinned evidence.

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

[externals.audience.slack]
url = "https://audience.corp/slack"

[externals.identity.corp-identity]
url = "https://identity.corp/resolve"
```

An entry is `[externals.<kind>.<name>]`, with `<kind>` one of `authorities`, `sanitizers`, `annotators`, `audience`, or `identity`. An authority or sanitizer entry takes exactly one of `url`, `command`, or `builtin`. An annotator, audience, or identity entry takes exactly one of `url` or `command`; `builtin` there is a configuration error. An annotator that names `builtin = "claude-code"` or `builtin = "llm"` on its `[[annotator]]` declaration takes no entry, and neither does the reserved `attest-schema` sanitizer or the shipped `verified-email` identity implementation. An entry whose name no declaration registers refuses the deployment when it opens, and so does a registered sanitizer or annotator, a referenced audience source, or a custom identity implementation without its entry. An authority may stay unbound; it then returns no answer, so a remedy that names it cannot release the call. An included fragment can add entries, and it can declare an annotator with a builtin: every deployment that includes it then serves that builtin — `[externals.llm]` for `llm`, a Unix host for `claude-code`. The root-wide settings (`timeout_ms`, `max_body_bytes`, `review_timeout_ms`, `[externals.claude_code]`, `[externals.llm]`) stay in the root, and the same name in two files is an error.

### Transports

| Binding | Serves | Notes |
|---|---|---|
| `url = "…"` | every kind | HTTPS anywhere; cleartext `http` only on loopback; no credentials in the URL. `token_env` names an `APPA_*` variable whose value is sent as a bearer token, and never an `APPA_PROVIDER_*` one. |
| `command = ["…", …]` | every kind | Unix only. One JSON consult on standard input, one JSON answer on standard output; no shell; the working folder is that of the file that declares it; bounded by `timeout_ms` and `max_body_bytes`. At most eight run at once per runtime. The child inherits no `APPA_*` variable except the one `APPA_PROVIDER_*` credential its own `token_env` names. |
| `builtin = "hitl"` | authorities | The harness asks a person. |
| `builtin = "approve"` | authorities | Approves within `permits`. |
| `builtin = "redact-email"` | sanitizers | Replaces email addresses with a placeholder. |
| `builtin = "claude-code"` | authorities, sanitizers; an annotator names it on its declaration | Unix only. One isolated `claude -p` process per consult, tuned in `[externals.claude_code]`. |
| `builtin = "llm"` | authorities, sanitizers; an annotator names it on its declaration | The API-key profile in `[externals.llm]`. |
| `builtin = "<module>"` | authorities, sanitizers | A deployer module from `--modules-dir`, called in-process. |

`APPA_*` is the runtime's own environment namespace: its wiring and every secret a `url` binding's `token_env` names. A child process inherits none of it.

A `command` binding takes a `token_env` of its own, and it means the opposite of a URL's: the runtime sends nothing, it forwards that one variable to the child that reads it. The variable must be in the `APPA_PROVIDER_*` namespace — a credential belonging to the provider, such as a battery's Slack or GitHub token, which the runtime never reads. A `url` binding's `token_env` may not name a variable there, so the forwarding cannot carry a secret this runtime holds, and a command receives only the credential its own binding names, never another command's. The value is not read when the policy loads, so a policy stays loadable and describable on a machine that holds no provider credential; a missing one surfaces as the source's own refusal to answer. A `claude-code` consult inherits nothing of the namespace at all.

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
| `kind` | `authority`, `sanitizer`, `annotation`, `audience`, or `identity`. |
| `name` | The registered name, for one service that answers for several. |
| `declaration` | The registered half: the component's `hint` and `permits`, an annotator's mandate vocabulary, or an audience source's selector templates. The agent never writes it. |
| `artifact` | The value under judgment: the call and its unmet requirements, the body to rewrite, an annotator's `args`, a selector or member to read, or the member claims to canonicalize. |

| Kind | `declaration` | `artifact` | `answer` |
|---|---|---|---|
| `authority` | `hint`, `permits` | `tool`, `arguments`, `requirements` | `ruling` (`approve` or `deny`), optional `reason` |
| `sanitizer` | `hint`, `on`, `permits`, `parameters` (for `tool_input`) | `tool` (when known), `body` | `body` |
| `annotation` | `inputs`, `trust_ranks`, `audiences`, `attention_marks`, `effects` | `args` | `delta`, `requires`, `emits` |
| `audience` | `templates` | `selector`, or `member` for a lookup | `members`, or `claims` for a lookup |
| `identity` | empty | the member's claims: `id`, `verified_email` when present | `principal` |

The consult never carries the trajectory: no current label, no rank, no reader ids, no history, no user turn. A component judges the artifact against its own declaration and nothing else.

An endpoint or a command answers `{"version": 1, "answer": { … }}`. `version` must be `1`, `answer` must hold exactly the keys its kind defines, and no other key may appear. Anything else — an error status, a non-zero exit, a timeout, an oversized body, a malformed answer — is no answer: nothing is recorded, and the flow that asked stays where it was (a blocked call, a withheld result, an unannotated call that never runs). A failed consult is never a denial.

### Model transports

`claude-code` and `llm` render the same consult for a model: a fixed per-kind preamble and the `declaration` JSON as the system prompt, the `artifact` JSON as the only user turn, and an output schema built from the declaration — the `ruling` enum, an annotation's mandate vocabulary. The model answers the bare per-kind object; the artifact is treated as data, never as instructions. The prompt and the raw model output are never persisted; only the validated answer is.

A model answer can do what the kind allows any implementation: an authority's ruling stays within `permits`, an annotation within its annotator's mandate, and a sanitizer's derivation carries exactly the `permits` transition. A model sanitizer deserves a second look: `permits` caps the label the derivation claims, not the bytes the model leaves in it, so keep its transition narrow and its `hint` exact.

`[externals.claude_code]` tunes the subscription transport: `command` sets the executable, `model` pins the model, and `timeout_ms` gives a consult its own budget. Each consult is one `claude -p` process in safe mode with no tools, no project settings, no session persistence, a fresh temporary working directory, the CLI's optional background traffic disabled, and every `APPA_*` variable removed from its environment. At most four run at once per runtime.

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
