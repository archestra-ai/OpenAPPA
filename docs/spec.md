# APPA: Agentic Permissions Policy Algebra — Specification Draft

**Status: draft.** This document describes the APPA model for engineers
building agents and integrations. It is self-contained: no background in
security theory is assumed. A companion paper covers the formal foundations.

## Overview

APPA is a policy engine that sits between an AI agent and its tools. Every
tool call the agent proposes is checked before dispatch. It tracks where
information came from and decides whether it may flow into tools and external
recipients.

APPA never works via guardrails or other imperative techniques. It is
declarative: it infers the run's state from the run itself and checks it
against registered tool contracts and authorities — the only sources of truth
for its decisions. Everything reduces to comparisons against exactly two
pieces of state:

- the **label** — travels with the data. It answers "what is this
  information: who may read it, how trusted is it." Every change to it is
  checked before it commits.
- the **log** — travels with the run: an append-only record of what
  happened — effects, authority rulings, the agent's acceptances.
  Checks consult it (history
  requirements, ruling validity), but appends themselves are never gated:
  history is recorded, not approved.

Nothing else carries state. Anything imperative — an approval flow, an ML
model that vets content, a lookup that resolves a recipient to a set of
readers — lives in components registered with the engine, never inside it.
External identity machinery (OAuth, SAML) does not belong to APPA; it trusts
those inputs.

## Where APPA runs

APPA is a small kernel, framework-agnostic, but not free-floating. The engine
must **see the full trajectory** and **control tool execution**. That pins
where it can live:

- the harness itself;
- an inference proxy paired with a tool gateway;
- an inference proxy alone, if it intervenes in the model's output
  aggressively enough to stop a call.

A pure MCP gateway cannot host it: it sees tool calls but never the
trajectory that labels them. Product-wise this is a feature, not a
constraint — unlike naive rule-based approaches, APPA sits where the context
is, and is exactly as smart as the context it holds.

One capability further divides deployments: a layer either can hold a raw
tool result out of model context — run the call, keep the bytes back, surface
a sanitized derivative or nothing — or it cannot. Call a deployment that can
**confining**. The capability is just that — holding bytes back; the
confinement constructions (quarantined branches, compiled composites) are its
consequences, and exist only in confining deployments, because once the agent
has observed a value, it is in its context for good. Checks and label
propagation hold everywhere.

### Architecture

```mermaid
flowchart LR
    Harness --> Mediator
    subgraph Mediator["Mediator (harness / proxy + gateway)"]
        subgraph Engine["APPA Engine"]
            Contract["Tool Contracts"]
        end
    end
    Mediator --> Inference
    Config["Contracts, authorities,<br/>sanitizers, casts"] --> Contract

    classDef blue fill:#cfe2f3,stroke:#333,color:#000
    classDef yellow fill:#ffff66,stroke:#333,color:#000
    classDef green fill:#93c47d,stroke:#333,color:#000
    class Harness,Inference,Config,Mediator blue
    class Engine yellow
    class Contract green
```

- **Blue** — not part of APPA. This spec describes how to integrate it;
  example implementations exist.
- **Yellow** — APPA itself: the label arithmetic, the log, the checks.
- **Green** — the interface you configure APPA through: tool contracts and
  registered externals (authorities, sanitizers, casts — any of them
  optionally implemented by a dynamic **resolver**, an external service
  answering at decision time).

The mediation loop, on each round-trip: the run's turns accumulate into the
trajectory; every tool call the model proposes is checked before dispatch; an
allowed call runs and, on success, its declared contributions land (label
delta and effects); a blocked call never runs — the model receives exactly what
failed — requirement gaps or a narrowing — and the available remedy plans
instead, and can execute an engine-side plan by id (see The check).

## What APPA protects against

- The agent is **benign but confusable**: it can be steered by injected
  instructions, but it is not itself adversarial. A malicious model
  constructing covert channels — encoding secrets into its choice of
  actions — is out of scope.
- Malice enters **exclusively through content labeled suspicious** (or
  Unknown, until resolved). Content labeled trusted — including internal
  systems of record — is trusted by definition; APPA offers no protection
  when a trusted source is malicious, just as no policy survives a malicious
  operator.
- Authorities, sanitizers, casts — with the dynamic resolvers implementing
  them — and their **configuration** are the deployer's trusted base. A YOLO
  configuration
  (auto-approve authorities, a constant Unknown → trusted cast) is legitimate
  and voids the corresponding guarantees explicitly and auditably; that trade
  is the deployer's to make.
- APPA assumes a **serialized, durable event log**. Serializing appends
  across concurrent branches is the host's obligation, not the engine's: a
  history check is only as sound as the log it has seen — an effect a
  concurrent line has not yet appended is invisible to it. The invoke/append
  crash gap is accepted, not defended (see Implementation shape).
- The core trajectory is **linear**. Branching is a host capability governed
  by the confinement profile (below); its guarantees hold only in confining
  deployments.
- Deployment realities — approval UX (approval fatigue is the real attack
  surface of any human-in-the-loop scheme) and bootstrapping
  contract/sanitizer coverage — are adoption concerns, out of scope here.
  APPA is exactly as good as the authorities and contracts registered into
  it.

## The data model

All data flows as a `LabeledValue`. Value and label are never separated and
never operated on directly.

- **value** — an agentic turn: tool call + result, a part of the trajectory.
  The natural unit of existing agentic workflows.
- **label** — who may read this information and how trusted it is: the
  product of the two label dimensions.
- **label delta** — what running the tool would bring into the trajectory
  label, affecting all next steps. Contracts declare it as `delta`.
- **label requirement** — what the trajectory label must satisfy before a
  tool may run. Contracts declare it as `requires`.

Tool calls are transactional: once a tool call succeeds, its delta is folded
into the trajectory. Sanitizing after the fact cannot clear a trajectory.

## Labels

The label has exactly two dimensions, **fixed in shape, configurable in
instance**. Configuration is data — an ordered list of names, a family of
sets — never code.

### Trust

A finite ordered chain of ranks. The default instance is
`suspicious < trusted`; a deployment may supply its own, e.g.
`unvetted < vendor < internal < reviewed < verified`.

An unreviewed read contributes the *minimum*: one suspicious value makes the
whole trajectory suspicious. A tool requiring rank `r` accepts anything at
or above `r`. A **ruling** — one recorded act of judgment by a registered
authority, defined under Rulings — may *cover* an unmet floor for one
dispatch, up to the ceiling the issuing authority's mandate declares; the
label itself never rises.

### Audience

A set of readers drawn from a fixed per-deployment universe, with named,
possibly nested groups; `public` means the whole universe. A customer's
secrecy taxonomy embeds as nested groups
(`level-3-readers ⊂ level-2-readers ⊂ everyone`) — classification schemes
are audience *configuration*, not a new dimension.

Reader sets are symbolic — domains, named groups, explicit id-lists.
Containments among named groups are configuration the engine decides as
data; membership of a raw id (`john ∈ hr`) is a dispatch-time question for a
registered **resolver**. A ruling naming a group binds the group
symbolically: every check resolves membership fresh at decision time — a
removed member is excluded from all later checks, but nothing looks back
between checks; mid-flight revocation is the directory's concern, not the
engine's.

Reading restricted data **shrinks** the reader set (intersection: only
people cleared for every input may read the combination); the set never
grows. A recipient outside the reader set is reached only through a ruling
covering one dispatch, never by widening the label. A tool's requirement can constrain
the reader set from either side:

- an **`includes`** (`audience ⊇ recipients`) — the trajectory's readers
  must include the concrete recipients the call would expose the data to;
- a **cap** (`audience ⊆ C`) — the reader set the dispatch would commit
  must stay inside the tool's declared set: "do not fetch me into a context
  outsiders can read."

### How the label moves

A contract's `delta` describes what a successful call does to the
trajectory label — and every delta is *restrictive*: intersect the
audience, take the minimum trust. There is no permissive delta. A ruling
covers a requirement gap for one dispatch while the label stays put (see
Rulings), so the label only ever moves down, and settles — no oscillation,
and no replay needed to compute it: the whole history collapses to a
running intersection and minimum.

This is a design direction, not a deferral: the label never widens. A task
that seems to need a persistent widening — an ongoing exchange with an
external recipient — is served by **branching**: a child carries the
exchange and dies with it, and the parent's label never holds the
recipient (see Branching). The durable alternative (the *epoch-wide
raise*, a ruling-carried permissive delta) was considered and rejected in
favor of branching; the companion paper states the trade and the algebra
it would drag back in.


## The event log

Everything historical lives in a single append-only log. Nothing about it is
ever checked before appending: history is recorded, not approved. Two
species of records share it:

- **Effects** — what the run did outside: `egress`, `mutation`, drawn
  from a configurable vocabulary. Declared by contracts as `emits` and
  appended when the call succeeds — one append point, deliberately. A call
  that dispatched but failed appends nothing; the window this opens for
  `no_prior(egress)` — a failed send may still have reached an inbox — is
  accepted for simplicity, as is the invoke/append crash gap (see
  Implementation shape). A positive `prior(k)` proves the tool reported
  success, nothing more about the outer world.
- **Governance events** — what was decided: authority rulings and the
  dispatches that consume them, the agent's narrowing acceptances, boundary
  events, sanitizer applications,
  casts. A **boundary event** is not a decision — it is punctuation:
  a mark in the log that pending plan executions — approval requests not
  yet ruled — cannot outlive. The engine appends
  one at the end of each assistant turn, at fork, and at merge.

The log is consulted in exactly three ways: **history requirements** in
contracts, **ruling validity** (rulings, their consumption, boundaries), and
audit. Useful summaries — e.g. "the set of effect kinds seen so far" —
are views computed from the log, cached by the engine, never independent
state.

The typical pattern — a convention, not a rule: integrity via trust floors
on mutating tools, confidentiality via audience `includes` on publishing
tools;
any contract may combine any requirements. History requirements gate a
dispatch like any other requirement — the log is not advisory — but on a
different question: the label answers *what the information is*, the log
answers *what has already happened*. They stay separate pieces of state
because their algebra differs — the label folds down and settles (minimum,
intersection), the event set only grows — and because branching treats
them differently: the log is one, shared across lines in realtime, while a
label is copied at fork and comes back only through the returned value
(see Branching). Folding events into the label would silently make
history line-scoped.

Effects are also the model's sanctioned pressure-release valve —
deliberately. A deployment can encode almost any bespoke gating ritual as
effect vocabulary plus a dynamic authority: a `finance.spend` effect whose
accumulated magnitude — a log view, summed by a registered authority —
decides between auto-approving and paging a human. That is better than the
alternatives: the hack is a named effect in an auditable log, not a
distortion of the label rules, and the guarantees on everything else stand.
(Budget as a label dimension is deliberately out of scope; a deployment
whose effect vocabulary sprawls is signaling it wants a workflow engine on
top, not a bigger policy engine.)

## Tool contracts

A contract declares one contribution per piece of state — `delta` for the
label, `emits` for the log — plus `requires` and routing-only `tags`:

- **`delta`** — the label action. Checked *before* it is applied: the state
  the call would commit must pass, so a delta can be blocked, remedied, or
  require a ruling.
- **`emits`** — the effects a successful call appends, an unordered
  batch recorded as one step. Applied, never checked: you cannot fail an
  append; history is not up for approval.
- **`requires`** — in three kinds:
  - **label requirements**, checked against the trajectory label: a trust
    floor (`trust = "trusted"`), an `includes` (`audience ⊇ recipients` —
    the recipient set derived from the actual arguments via placeholders, or
    declared statically), a cap (`audience ⊆ C` — see check timing).
  - **history requirements**, checked against the log, in two species:
    - `no_prior(egress)` — no matching effect in the log. Not consumed
      by checking; waivable for one dispatch by a ruling whose issuer's
      mandate covers the waiver.
    - `prior(backup_ran)` — a matching effect exists ("delete the
      database only after the backup ran"). Nothing to waive — the remedy is
      to make the effect happen. (The effect proves the backup tool reported
      success, nothing more about the outer world.)
  - **attention demands** — `attention = ["finance_signoff"]`: named marks
    drawn from a shared vocabulary. An attention demand is *per-call* and
    never satisfied by history — where an effect is a durable fact
    (`prior(k)` holds forever once the event lands), attention demands a
    fresh look at every dispatch. It is met only by a ruling from an
    authority that *attends* the mark (see Mandates), delivered inside the
    atomic plan execution — approval and dispatch coincide; a repeat
    dispatch takes a fresh ruling. Tool and authority never name each
    other: both reference the mark. The ruling is over the exact rendered
    call — tool plus resolved arguments; attending to `transfer(A, $1)`
    must not authorize `transfer(B, $100)`.
- **`tags`** — names with no algebraic life: they do not fold, never enter
  a check, and commit nothing to the log. Their sole job is routing — an
  authority's scope names the tags it has jurisdiction over (see
  Mandates).

Contracts can be static, static with placeholders, or dynamic. A placeholder
contract:

```toml
[[tool]]
name     = "send_email"        # send_email(to: $recipient)
requires = { trust = "trusted", audience = { includes = ["$recipient"] } }
effects  = ["egress"]          # emits
```

Naive placeholders do not solve real-world ACLs, so delegating resolution
(recipient → reader set) to external services is a legitimate choice for
complex deployments. Such dynamic resolvers must be registered in advance;
like sanitizers, they are trusted components.

Two surface conventions, both presentation rather than model. Contract
language leads with `requires` — a delta reads best as a stated consequence
(`read_hr: makes the conversation HR-only`); "this tool changes the label"
is the expert's view of the same fact. And source deltas are derivable: a
dynamic resolver mapping a document to its ACL's reader set auto-generates
`audience ∩ readers(doc)`, so humans hand-write the sinks they care about
and inherit the sources for free. The two slots stay distinct underneath —
the delta is what a successful call *commits* to the label, the requirement is
what the label must *satisfy* — and they are independent: a contract may
carry either, both, or neither, and a call with both is checked on both.

The concrete configuration surface is drafted in "The configuration
surface" below; examples throughout this document are written in that
dialect.

## The check

Before every tool call the contract is evaluated. The outcome is binary —
allow, or block with exactly what failed: requirement gaps, a narrowing,
or both. The check itself is two-fold:

**1. Tool requirement compatibility.** Never widen the audience, never act
on worse trust than the tool requires: the trajectory label satisfies the
contract or the call is blocked. Where the required audience comes from
placeholders, it is derived from the actual arguments — the trajectory's
readers must include the concrete recipients of *this* call; a static contract
simply declares its recipients.

**2. Narrowing.** Do not touch more secrets than the task really
needs. Every contribution moves the label down or leaves it in place — on
both axes, no exceptions: v1 has no permissive deltas, and rulings cover
gaps without touching the label. A call whose committed state would
strictly narrow is deliberately soft-blocked; a repeat that leaves the
state unchanged is not. The point: committing to restricted data
voluntarily shrinks the **release frontier** — what the agent may still
release, and to whom, without a further ruling. APPA makes that a
conscious, remediable choice *before* the data is fetched, instead of a
silent ratchet discovered three steps later. Accepting the narrowing is the
*agent's own* plan step — free, on the record, and involving no authority:
no security power is exercised; what makes the stop deliberate is that the
agent chooses the plan (see Rulings for how this composes with rulings).

Deltas never raise — the only sign rule v1 needs. A call may carry a
restrictive delta and a requirement gap at once
(`search_and_share` is exactly that); then both gates apply: the agent
accepts the narrowing, a ruling covers the gap, and neither substitutes
for the other. A tool whose *action* is itself a grant of access
(`share_doc(doc, outsider)`: fetch, then open the ACL) is still modeled
as a composite of a fetch and a release, so each transition stays simple
to rule on.

Together the two checks are a pragmatic middle ground between the two
failure modes of agent security: YOLO agents that ingest and leak
everything, and hard-restricted agents that cannot do anything useful.
Check 1 makes the dangerous flows impossible; check 2 makes the restricting
ones a deliberate, remediable choice.

The central thesis: **down is free, up needs authority** — and APPA asks the
agent to choose between preserving its release frontier and entering a
restricted context *before* fetching the data. The soft block shifts the
reasoning left. Spelled out: a requirement that fails because the state is
too *low* — an unmet `includes`, an unmet trust floor — is cured only by
a ruling covering the gap; no sequence of unruled steps can ever cure
it, because unruled steps only narrow. A requirement that fails because the
state is too *high* — a cap with outsiders in the context — is
cured by narrowing: free, modulo the agent's acceptance. ("Free" means no
security power is exercised — not frictionless.)

### Check timing

Ordered checks, each with its clock:

- **The narrowing check** runs first, on the state the dispatch would
  *commit* — the current label with the call's own `delta` applied. A
  strict narrowing is soft-blocked (see The check), and
  dispatch waits for the agent's acceptance of exactly that narrowing.
- **Label requirements** then evaluate on the current state — which, with
  an accepted narrowing in force, *is* the state the dispatch commits. The
  order is load-bearing: checked before the narrowing, a call could outrun
  its own consequences. The attack: `search_and_share` with
  `requires = { audience = { includes = ["public"] } }` and
  `delta = { audience = { exactly = ["internal"] } }` — on
  the pre-narrowing label the call passes as public, but the bytes it
  shares *are* the internal data its own dispatch commits; with the
  narrowing in force the `includes` fails, and the release takes a ruling.
- **History requirements** ask what has already happened: they evaluate on
  the log as it stands at check time — so a call's own `emits` can never
  trigger its own precondition.

Neither label check is a configuration entity: both derive from the
contract's own `requires` and `delta`; the surface exposes no timing
knobs.

Caps (`audience ⊆ C`) follow the same clock as every label requirement —
the call's own narrowing counts: a read that itself narrows into the cap
passes, surfacing as the standard narrowing soft block ("this
fetch drops these readers"), and the dropped readers provably receive no
post-read content. (Deployments whose channel physically shows every
message to fixed readers regardless of the label are out of scope: APPA
governs agentic trajectories, not generic channels.)

The delta commits and the effects append only when the tool call succeeds.

### Remedy plans

A block carries `remedy_plans`: the sound remedies available under the
current configuration and deployment capability (a confinement plan exists
only in a confining deployment). Plans are executable objects, not prose:
each carries an id, and the engine exposes **one agent-facing tool for
every engine-side plan, present from the start of the run** —
`execute_remedy_plan(plan_id)` — so the tool set stays stable (injecting
tools mid-conversation breaks prompt caches). On execution the id, the
ruling where the plan carries one, and the dispatch all land in the log;
for an acceptance plan the plan id *is* the record. A `prior(k)` plan has
no engine-side step and no id-execution path (see below).

```ts
type CheckOutcome =
  | { outcome: "allow" }
  | { outcome: "block";
      requirement_gaps: RequirementGap[];  // unmet entries of `requires`
      narrowing?: Narrowing;               // present when the call's own delta fired check 2
      remedy_plans: RemedyPlan[] };
```

An attention demand on an otherwise-passing call surfaces through this
same block shape: the unmet demand is a requirement gap like any other —
attention is the third kind of `requires` — and the remedy plan is the
atomic ruling by an attending authority. A narrowing is reported in its
own slot, never as a requirement gap: nothing in `requires` failed, and
the acceptance plan, not a ruling, answers it. A call like
`search_and_share` fills both.

Two facts about the list:

- **Nonempty is the weak direction**: a plan *exists* relative to the
  registered configuration and, where dynamic resolvers contribute, their
  answers at check time; succeeding still takes the authority granting and
  the world cooperating.
- **An empty list is a proof, not a shrug.** The remedy space is finite and
  enumerable from the registry: the in-scope (tag-routed) ruled covers
  whose declared mandate
  ceiling reaches the gap; the input-sanitizer substitutions that would
  produce an admissible derived argument (any deployment); the
  output-sanitizer-backed composites (confining deployments only); for a failed
  `prior(k)`, the registered tools whose `emits` include `k` — the plan is
  to make the effect happen; for waivers and attention demands, the declared
  mandates that cover them; for a narrowing soft block, the acceptance
  plan — always available, from no registry entry at all, because it
  grants nothing (so a narrowing block is never terminal; the
  empty-list proof concerns requirement gaps). For a gap the state is too
  *low* for — an unmet floor, an unmet `includes` — nothing outside
  that enumeration can ever cure it, because unruled steps only narrow;
  the history and attention cures — a `k`-emitting tool, a waiving or
  attending mandate — are registry entries by definition. (A cap gap —
  the state too *high* — is the one species cured by narrowing itself,
  free modulo acceptance; see The check.) The
  agent provably should not spend turns on an unliftable restriction.

Plans divide by who executes them. A plan whose steps are engine-side
acts — a ruling, a sanitizer application, an acceptance — executes
atomically via `execute_remedy_plan` (see Atomic plan execution). A plan
for a failed `prior(k)` carries no engine-side step: it names a
registered tool whose `emits` include `k`; the agent dispatches that tool
as an ordinary, separately-checked call, then re-proposes the original
one — two transitions, each under its own check, nothing atomic between
them.

## Rulings

Authorities are the single home of judgment in APPA — every act of human or
policy discretion is an authority **ruling**, one format, appended to the
log. Two halves of one principle bound what a ruling can do:

- **A ruling admits a dispatch despite a requirement gap; it never edits
  the trajectory.** The trajectory changes only through what the admitted
  call itself commits — its `delta` and its `emits`. An authority never
  rewrites the label directly; a ruling over a call with no delta and no
  emits changes nothing but the log.
- **A ruling cannot substitute for the agent's acceptance.** A dispatch
  whose delta would shrink the release frontier needs the *agent's*
  explicit acceptance of that narrowing as a plan step (see The check) —
  no security power is exercised, so no authority is involved. The two
  gates compose independently: one call may need both a ruling (for its
  gap) and an acceptance (for its narrowing), and neither covers the
  other — an authority approving the dispatch does not accept the frontier
  loss on the agent's behalf, and the agent's acceptance clears no
  requirement.

Every ruling is **call-scoped**: it admits a specific pending call and
covers exactly the engine-rendered call it names — tool plus resolved
arguments, never the agent's paraphrase — for one dispatch, and **the label
does not change**. The release is recorded where run history belongs: the
ruling and the effect land in the log, while the label keeps
describing what the data *is*. A widening that genuinely should persist —
an ongoing external thread, many sends under one review — is served by
branching, never by the label: fork a child to carry the exchange; each
send is ruled inside it, and the widening structurally cannot outlive the
branch or cross back (see Branching). The durable alternative (the
**epoch-wide raise**) was considered and rejected in favor of branching.
One review is one review.

### Atomic plan execution

The mechanism is the remedy plan; every plan with an engine-side step is
**atomic**. Executing a
ruling-carrying plan is
one indivisible step on a suspended line: the engine renders the call, puts
it to the authority — with provenance, never value bytes — and on approval
dispatches it; the plan id, the ruling, and the dispatch land in the log
together. (An acceptance plan is atomic trivially: accept and dispatch,
one step, no authority round trip — the plan id and the dispatch are the
whole record.) Consequences, by construction rather than bookkeeping:

- nothing can intervene between approval and dispatch;
- an approval cannot cover a swapped call — it names the rendered call;
- an approval cannot be replayed — it is consumed by the dispatch it
  admitted; one review is one review, a repeat takes a fresh ruling;
- the decision trail stays reconstructible from the log alone.

No grant object appears in configuration or on any wire: the public
vocabulary is **mandates**, **rulings**, and **log records**.

### Mandates

There are no ruling kinds at runtime. One engine rule instead: **a call
dispatches iff every requirement gap is covered by the
rulings that admit it — each issuer's mandate covering what it admitted;
the rulings bind the same rendered call and are consumed together in one
atomic step.** (Usually that is one ruling; two-eyes configurations
collect several.) The typology lives where typology belongs — in **mandates**,
declarations of what an authority's ruling may cover, each power naming
the currency it acts on:

- a **cover up to a ceiling** — admitting a dispatch over an unmet trust
  floor (endorsing up to a rank — e.g. a human reviewed the fetched page
  and ruled the content safe) or over an unmet `includes` (vouching
  readers, up to a declared set). The label does not move; the ceiling
  bounds the gap one ruling may cover;
- a **named waiver** — covering a failed `no_prior` for the admitted
  dispatch only, naming the event kinds it may waive;
- **attends** — the attention marks whose demands this authority's ruling
  satisfies. Deliberate consequence: a single ruling by an attending
  authority over a call covers both a label or history gap and an attention
  demand on the same call — one review is one review; a deployer who wants
  two eyes declares two marks attended by different authorities. What no
  ruling ever satisfies is the agent's acceptance of a narrowing.

Accepting a narrowing is deliberately *not* a mandate
power: it is the agent's own free plan step (see the two-gate principle
above). A deployer who wants a human on expensive narrowings anyway
attaches an attention mark to the narrowing tools — opt-in, never a
default authority.

**Requirement gaps route by tags, exclusively.** A mandate says what
an authority may
grant; its **scope** — the tags it covers — says over which calls; the two
questions never share a mechanism. An authority with no declared scope
covers every call — small configs stay small. Attention gaps are the one
exception, routed by their own currency: an attention demand reaches
exactly the authorities that attend its mark, scope tags not consulted —
the mark is both the demand and the route. Trust, audience, and effects are
checked currencies and must not double as routing keys: coupling
jurisdiction to the effect vocabulary would let an accounting rename
silently move an authority's reach. Tags can afford to route precisely
because they have no algebraic life. The consequence is a clean split:
**soundness is tag-independent, only completeness is tag-dependent** — a
mis-tagged catalog can route a gap to the wrong authority (who still
cannot exceed their mandate and still rules on the rendered call) or fail
to route it at all (a spuriously terminal block); a misdeclared effect or
audience, by contrast, perturbs the checks themselves. If a deployment
wants "every tool committing `finance.spend` carries the `finance` tag" as
an invariant, that is a load-time lint, not a semantic channel.

One structural bar concerns the assistant's own reply to the user (the
response sink): when the trajectory is restricted enough that even showing
content to the user is a release, that release takes a *distinct*
authority's ruling — **no ruling issued by the end user may cover any
requirement gap of a response-sink release**, whatever
mandate the user otherwise holds. The user cannot self-approve seeing
restricted content: the approval request would arrive on the very channel
being released, and an in-band self-confirmation is structurally not a check
at all.

The response sink's remaining mechanics — the contract governing the
assistant's reply and how it enters the check pipeline — are deliberately
out of scope in this version; only the structural bar above is normative.

Authorities can be automatic (up to auto-approve-everything) but stay
explicit: ML model, LLM-as-judge, regex, human in the loop, oncall page —
the implementation is up to you. Metaphorically, remedies are fine-grained
sudo — and with call-scoped rulings, sudo in its honest sense: one command,
one elevation, nothing ambient afterwards.

### Sanitizers and casts

Two further powers produce values and labels rather than admit calls:

- **Sanitize** — a registered transformer derives a new value under the
  label its mandate authorizes; the raw source keeps its own label. One
  declared transition, two application points:
  - **tool output** — the derivation is what the trajectory admits; the
    raw result stays confined. This protects the *context*, and only a
    confining deployment can offer it — once the agent has observed the
    raw value, it is in its context for good.
  - **tool input** — the derivation is substituted into the engine-rendered
    call, so the harness dispatches exactly the redacted bytes. This
    protects the *sink*, and works in every deployment — but it cannot
    un-leak the agent's context: the raw value the argument derived from
    was already observed. Fitting a flow under a sink's requirements is
    what it is for; guarding the context is what it cannot do. The
    substituted call is checked with the derivation's declared label
    standing in for the raw argument's contribution — the trajectory
    label untouched, the application logged as a governance event.
- **Cast** — resolve an Unknown dimension to a concrete state, making the
  value usable (see Unknown below). A cast is either **constant** (every
  Unknown on its dimension resolves to one declared state — the
  YOLO/paranoid knob, in process, no round trip) or
  **resolver-implemented** (decided per value by a registered dynamic
  resolver), never both. A dynamic cast declares the set of states it may
  cast to — the ceiling that keeps a sloppy or compromised classifier from
  becoming a laundering endpoint.

A sanitizer's mandate binds it to the transition it may claim
(`remove_pii`: audience internal → public); without mandates any registered
function could assert any label drop and the trust boundary would be
invisible to audit. **A sanitizer's transition moves audience only — trust
never rises through a sanitizer.** A mechanical transform can bound who may
read its derivative; "this content is now trustworthy" is judgment, and
judgment is a ruling or a cast. (The one structured exception is the
quarantine exit under Branching: a `submit_result` attestation whose
mandate covers a trust-bearing claim about extracted structure — the only
unruled trust up-move in the system, and registered accordingly.) A
mandate binds a transition, not the information it is claimed over —
`remove_pii` is a sound remedy for CRM tickets and a laundering machine
for data that is all PII; scoping mandates by information type is explicit
future work. Registration is a trust decision about the sanitizer, not a
verification of its output.

### Why remedies are safe to hand to the agent

The engine soft-blocks anything that does not pass as-is and suggests remedy
plans built from the registered configuration — and, for a narrowing
block, the always-available acceptance plan. Two invariants make that safe
even when the agent may already be steered by injected content:

- **Remedy-set soundness.** Every suggested plan is individually sound, so
  *which* plan the agent picks is security-irrelevant. Selection immunity
  comes from plan soundness, not from policing the selector.
- **Canonical rulings.** Authorities rule on the engine-rendered call plus
  provenance, never on the agent's paraphrase. A steered model summarizing
  "may I email the compliance archive?" while omitting that the address is
  attacker-derived is the remaining social-engineering channel; rendering
  the exact checked call closes it.

### Compiled composites (confining deployments)

A multi-step plan (e.g. confined acquisition feeding a sanitizer) is not
handed to the agent as steps — it is the same `execute_remedy_plan` object
with a body. In confining deployments the engine **compiles the plan into a
composite**: one synthesized invocation whose `requires` are the plan's
entry conditions plus an attention demand attended by the plan's
authority, and whose
ordered body is part of the rendered object the authority rules on — so the
plan's approval covers its enumerated internal *requirement gaps*, while
executing the plan is the agent's recorded acceptance of its enumerated
internal *narrowings*: the body is visible in the rendered object, so
both gates are exercised knowingly, each by its own party.

- The body executes step-by-step inside the confining layer, each step
  checked against the evolving internal state, intermediate values never
  surfacing.
- The composite's label `delta` is the *returned value's* contribution
  (intersect readers, min trust) — not the raw composition of the steps'
  deltas, which would be wrong in both directions: over-tainting the parent,
  or applying an internal sanitizer's raise to it.
- Execution is two-phase: the outer check runs against a declared bound on
  the result's label; the body executes while the confining layer holds the
  result; the actual result label is then checked against the bound, and the
  value commits only if it passes — otherwise it is discarded, while the
  executed steps' events stand.
- `emits` append per step, as steps succeed: a mid-body failure halts
  the composite with the successful prefix standing honestly in the log —
  never effects that did not happen. No undo is promised; compensating
  stranded effects is the deployer's affair, and plan-approving
  authorities rule knowing that.

Approving a plan is thus an ordinary confirmation of one rendered
invocation, and the agent cannot cherry-pick steps — it never holds them. A
non-confining deployment cannot compile composites: it can check and block,
but it cannot withhold. That is its documented trade.

## The configuration surface (draft dialect)

A draft, not final — but authoritative: every configuration example in
this document is written in this dialect, and every convention it shows
is normative (see the constraints under Implementation shape). Four
top-level kinds mirror the config box in the architecture diagram: tools,
authorities, sanitizers, casts.

```toml
version = 1

[[tool]]
name  = "fetch_ticket"
tags  = ["finance"]
delta = { trust = "suspicious", audience = { exactly = ["finance"] } }

[[tool]]
name     = "send_report"
tags     = ["finance", "external-comms"]
requires = { trust = "trusted",
             audience  = { includes = ["finance"] },
             effects   = { has = ["backup.completed"],     # prior(k)
                           has_no = ["email.sent"] },      # no_prior(k)
             attention = ["finance-signoff"] }
delta    = { trust = "trusted", audience = { exactly = ["finance"] } }
effects  = ["email.sent", "finance.spend"]                 # emits

[[authority]]
name = "finance-officer"

[authority.mandate]
can_raise_trust_to = "trusted"                # cover ceiling, trust
can_add_readers    = { may_add = ["public"] } # cover ceiling, audience
can_waive          = ["email.sent"]           # named waiver
attends            = ["finance-signoff"]      # attention marks

[authority.scope]
tags = ["finance"]           # jurisdiction; omitted scope = every call

[authority.implementation]
resolver = { url = "https://approver.corp/rule", timeout_ms = 30000 }
# resolver = { channel = "hitl" }  # same authority, human elicitation
# builtin  = "approve"             # in-process; cover-free mandates only

[[sanitizer]]
name = "pii-redactor"
on   = ["tool_input", "tool_output"]

[sanitizer.can_reduce]
# audience only, by construction: trust is never sanitizer territory
audience = { from = { includes = ["finance"] }, to = { exactly = ["public"] } }

[sanitizer.implementation]
builtin = "redact-email"

[[cast]]                     # constant XOR resolver, never both
name     = "paranoid-default"
constant = { trust = "suspicious" }

[[cast]]
name     = "content-classifier"
resolver = { url = "https://classifier.corp/resolve", timeout_ms = 10000,
             may_cast = { trust = ["suspicious"] } }   # ceiling
```

Load-bearing conventions, each argued elsewhere in this document:

- **Every set mention carries its operator** (`includes` / `exactly` /
  `may_add`) — a bare list is ambiguous between narrow and wide.
- **Surface names map onto model terms**: `effects = [...]` declares
  `emits`; `effects.has` / `effects.has_no` are `prior(k)` / `no_prior(k)`;
  `attention` marks are the per-call demands of Tool contracts.
- **Mandate powers name the currency they act on** (trust ceiling, reader
  set, event kinds, attention marks) and nothing else; `scope` is tags
  only; tool and authority never name each other.
- **Implementations are `builtin` (in-process) or `resolver` (dynamic)**;
  an in-process `builtin = "approve"` is legal only for a mandate with no
  cover ceilings — the one competence a policy may grant itself is
  clearing what it can fully see. HITL is a resolver channel, not a
  different kind of authority.
- **A sanitizer declares where it may apply** (`on`) and an audience-only
  transition (`can_reduce`); **a cast is constant xor
  resolver-implemented**, the dynamic form bounded by `may_cast`.

## Worked example

Two similar tasks:

- **Task A** — get a ticket from an internal CRM and send it to an external
  auditor's email.
- **Task B** — get a ticket from an internal CRM and file a ticket in a
  public issue tracker.

```toml
[[tool]]
name     = "get_ticket_from_crm"
requires = { trust = "trusted" }
delta    = { audience = { exactly = ["internal"] } }

[[tool]]
name     = "send_email"        # send_email(body, to: $recipient)
requires = { trust = "trusted", audience = { includes = ["$recipient"] } }
effects  = ["egress"]

[[tool]]
name     = "file_github_ticket"
# we never post things that are not public
requires = { trust = "trusted", audience = { includes = ["public"] } }
effects  = ["egress", "mutation"]
```

Registered: `remove_pii` — a sanitizer with mandate audience
internal → public (barely working in practice, fine for the example) — and
`human_in_the_loop_approver`, an escalation authority with an
audience-cover mandate reaching the auditor and no declared scope, so it
covers every call.

The trajectory starts at the neutral, least restrictive label
`L0 = {audience: public, trust: trusted}` and an empty log (the default
starting label is engine configuration).

**The fetch.** `get_ticket_from_crm()` would fold in the `internal`
audience.
This leaks nothing — the stop is needed because committing to internal
voluntarily shrinks the release frontier: what the agent may still release,
and to whom, without a further ruling. The engine soft-blocks and suggests
remedy plans:

1. run `get_ticket_from_crm` through `remove_pii` as a compiled composite
   (confined; needs a confining deployment) → the sanitized value
   contributes a neutral delta, the label stays `L0`;
2. accept the restriction — the agent's own free plan step, no authority
   involved → `L1 = {audience: internal, trust: trusted}`.

Under plan 1 the raw ticket never joins the agent-visible trajectory: only
the sanitizer's output crosses the boundary.

**Task B** after plan 1: `file_github_ticket(ticket)` requires audience
`public` — compatible; the successful call appends `{egress, mutation}` to
the log.

**Task A** after plan 2: `send_email(ticket, auditor_email)` derives its
required audience from the actual argument: the readers must include the
auditor. Under `L1` they do not — so this is the **second, distinct
gate**: accepting the restriction never implies permission to disclose.
The ruling is call-scoped: the plan puts the rendered send to the approver
and dispatches on approval — the ruling lands in the log, the successful
send appends `egress`, the label stays at `L1`, and disclosing a second ticket takes its own
ruling. (A long auditor exchange belongs in a branch that carries it —
see Branching; in the main line each send is its own review.)

A smart agent chooses the right remedy from the task as early as possible —
the soft block shifts the reasoning left.

## Branching (the confinement profile)

The core trajectory is linear. A host that branches — subagents, quarantined
fetches, metatools — must implement this profile, or must not let branch
results cross back. It is the primary composition mechanism; the compiled
composites above are the engine-owned instance of the same semantics.

- **Fork.** The child starts at the parent's *current* label — never at the
  neutral `L0`: a fresh-slate child could "summarize what we know" into a
  public label, a laundering primitive. The child appends to the same shared
  log; the parent's history is simply its prefix. A fork appends a boundary
  event — which is why nothing pending survives into either line: an
  in-flight plan execution — an approval request not yet ruled — finds
  the boundary and dies; no special rule needed.
- **Merge.** Two things come back, each in its native way:
  - The **returned value** is data: the parent absorbs its label like any
    other read — intersect readers, min trust. Nothing the child did can
    widen the parent: intersection cannot add readers. (Load-bearing for
    attestation-raised result values — and for the rejected raise
    extension, were it ever revisited.)
  - **History needs no merging at all**: there is one shared log, and every
    line appends to it in realtime. An egress that happened in the child
    happened in the world — the email is in someone's inbox the moment it is
    sent, not at merge time. A ruling issued in a branch is a record, not a
    token: it was consumed inside its own atomic plan execution, so its
    presence in the shared log gives the parent nothing to reuse. The merge
    appends a boundary event; any plan execution still pending in the
    child dies
    there, like at any boundary.
  - Finalization is thereby trivial for every started branch, whatever its
    fate — return, failure, abandonment: nothing was withheld, so nothing
    can be lost. "The branch died" means no *value* crossed; history was
    already shared.

Cross-line history is deliberately global: with one shared realtime log,
a child's `egress` fails a parent's `no_prior(egress)`. That conservatism
is intended — effects are facts about the world, not about a line;
the email is in someone's inbox regardless of which line sent it. There
is no line-scoped `prior(k)`.

The child's own label may end maximally poisoned; the parent absorbs only
the returned value's label — *less restrictive* than the child's own fold
for exactly one
legitimate reason: a mandated sanitizer (including the quarantine-exit
attestation) relabeled it.

Example: an agent already working with internal data needs a one-off egress
to an external recipient mid-task. A call-scoped ruling covers the
rendered send and the label never contains the external recipient at
all — in the main line and in a branch alike. For an *ongoing* exchange
the branch is the construction: the child carries the thread, each send
is ruled inside it, and nothing about the exchange can outlive the branch
or widen the parent. This is APPA's answer to every "the widening should
persist" request — persistence lives in a branch, never in the label.

### Structured quarantined branches

The recommended way to work with untrusted sources without poisoning the
main run. The child handles the suspicious content and returns through a
`submit_result` tool with a pre-declared structured output and sanitizers,
e.g. `{format: {major_version: int}, sanitizers: [...]}`.

Example: a sensitive task first needs a third-party software version from
GitHub. Fetched directly, the page would fold `suspicious` trust into the
main trajectory. In a quarantined branch, only the extracted version crosses
back — entering the parent as trusted. Schema validation alone never raises
a label — structure is not provenance; the raise is claimed by the mandated
sanitizer, and only within its mandate: here, a transformer whose mandate
covers exactly this attestation — "the returned integer is a version number
extracted from the named source, carrying none of the source's free text" —
not a mere parser.

## Unknown as a first-class citizen

Real-world deployments are messy; APPA does not assume every tool is
annotated. Both label dimensions support **Unknown** — and it is not another
point on the scale: `trusted < unknown < suspicious` does not exist. It
means "this label has not been established yet": a value with an Unknown
dimension cannot be folded or checked at all until a registered cast fills
it in. A check that runs into Unknown inputs reports *which*
facts are unresolved, never a blanket Unknown result.

A registered cast fills the dimension in: **constant** (`unknown →
trusted` for YOLO deployments, `→ suspicious` for paranoid ones) or
**resolver-implemented** per value, under its declared ceiling of
admissible targets. Richer schemes — human in the loop on first use,
cached afterwards — live behind the resolver. This is fail-closed by
construction: annotate five high-risk tools, leave the rest Unknown, and
still catch the obvious flows.

## Implementation shape

The engine is two layers. The **inner layer is the pure decision core** —
`check(state, transition) → verdict`, `apply(state, transition) → state'`,
no IO, no clock: semantically a function of the full event log, so every
decision is replayable from the log alone. In practice the wire contract
passes the log's cached views — the label state, the seen-effect-kinds set,
pending-plan records, boundary positions — rather than the raw log; sound
because every view is recomputable by replay. The **outer layer owns
state**: durable append with pluggable destinations (a local file for a
single-host harness; a database where the filesystem is ephemeral),
serialization, and the durability obligations of the threat model. A
harness author implements neither — they embed the outer layer with whatever
store they already run; the decision core never sees IO.

The invoke/append crash gap is deliberately out of scope in this version:
effects append when the call succeeds, and a host that fails between a
successful invoke and the append may lose effects — accepted for
simplicity. Hardening (e.g. a durable outbox committing invocation and
effects as one record) is future work for the outer layer.

Transition invariants are enforced through the type system, under the
assumption that external labels and authority decisions are trusted inputs —
they, together with sanitizers and dynamic resolvers, form the trusted base.
A design guideline: the checker itself stays free of ad-hoc conditionals;
every decision reduces to label arithmetic or a log query, and anything
imperative belongs in a registered external.

The configuration surface remains a draft — every configuration example
in this document is written in it. Whatever surface ships must keep: **mandates only** — no grant objects in config or on any
wire; the **no-empty-mandate rule** — an authority whose mandate covers
nothing is a loud load error, not a no-op (the empty-`remedy_plans` proof
depends on it); block messages that surface the applicable remedy plans,
naming the eligible authorities where a plan carries a ruling; **explicit
set relations** — a bare reader list is ambiguous between narrow and
wide, so every audience mention carries its operator (includes / exactly
/ may-add); **scope routed by tags only**; and casts declared **constant
xor resolver-implemented**.

## Glossary

- **Trajectory** — one agent run: its label plus its event log.
- **LabeledValue** — the unit of data flow: a turn (tool call + result) with
  its label, never separated.
- **Label** — who may read the run's information (audience) and how trusted
  it is (trust).
- **Delta** — a contract's declared label action, applied when the call
  succeeds.
- **Emits** — a contract's declared effects, appended when the call
  succeeds.
- **Effect** — a recorded fact of what the run did outside (`egress`,
  `mutation`): appended to the log when the call succeeds, read back by
  history requirements.
- **Requires** — a contract's conditions: label requirements (checked
  against the state the call would commit), history requirements
  (checked against the log as it stands), and attention demands (per-call,
  never satisfied by history).
- **Requirement gap** — an unmet entry of a contract's `requires` — a
  label, history, or attention gap — reported in a block. Distinct from
  a narrowing, which fails no requirement.
- **Attention mark** — a named, per-call demand for a fresh ruling by an
  attending authority; the shared vocabulary through which tools demand
  review and authorities offer it, without naming each other.
- **Tag** — a routing-only name with no algebraic life: never folded,
  checked, or logged. The exclusive currency of authority scope.
- **Narrowing** — a strict restriction of the label (fewer readers, lower
  trust) that a call's delta would commit, shrinking the release frontier.
  Soft-blocked until the agent accepts it; the block always carries the
  acceptance plan, so it is never terminal.
- **Acceptance** — the agent's own free plan step acknowledging a narrowing
  before dispatch: no authority involved, no security power exercised,
  clears no requirement; the plan id in the log is the record.
- **Remedy plan** — an executable object with an id; every engine-side plan
  runs atomically via `execute_remedy_plan(plan_id)`: render, rule (when
  the plan carries a ruling), dispatch, log. A plan for a failed `prior(k)` carries
  no engine-side step: it names a registered tool whose `emits` include
  `k`, for the agent to dispatch as an ordinary checked call before
  re-proposing.
- **Authority / mandate / scope / ruling** — a registered judge; the
  declaration of what its rulings may cover; the tags it has jurisdiction
  over; one act of judgment, appended to the log. Every ruling is
  call-scoped and never touches the label.
- **Epoch-wide raise** — a considered-and-rejected durable widening.
  APPA's answer to persistent external exchanges is branching; the label
  never widens.
- **Sanitizer** — a registered transformer deriving a new value under a
  mandated, audience-only label transition; applied to a tool output (the
  derivation is admitted, the raw stays confined — confining deployments)
  or a tool input (the derivation is substituted into the rendered call).
- **Cast** — the registered resolution of an Unknown dimension to a
  concrete state: constant, or resolver-implemented under a declared
  ceiling of admissible targets.
- **Resolver** — the dynamic implementation kind of a registered external:
  serves authority rulings, cast decisions, sanitizer derivations, and
  membership /
  argument-to-reader-set questions at decision time.
- **Boundary event** — punctuation in the log (turn end, fork, merge) that
  pending plan executions cannot outlive.
- **Confining deployment** — one that can hold a raw tool result out of the
  model's context. Required for quarantined branches and compiled
  composites.
- **Unknown** — "label not established yet." Unusable until a registered
  cast resolves it; the cast policy is deployment configuration.
