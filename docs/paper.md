# Paper skeleton — AI security workshop

**Working title:** *APPA: An Information-Flow Policy Algebra for LLM Agents*

Alternates: *Down Is Free, Up Needs Authority: Information-Flow Policy for
Agentic Workloads* · *Two Monoids Suffice: Declarative Permission Policy for
LLM Agents*

**Target shape:** 6–8 page workshop paper (AI-security / LLM-safety workshop
at a major venue), formal material in appendices. Sections 3–6 are the
contribution; evaluation (§8) is honest-but-thin, framed as a case study of
the open-source implementation.

**Thesis sentence** (every section must serve it): a two-monoid algebra — a
checked monoid of policy actions on the label state and a free monoid of
events — suffices to give benign-but-confusable LLM agents robust
declassification: untrusted input can steer what is *requested*, but nothing
is declassified without trusted review of the exact rendered request, its
provenance, and the trajectory state it exposes.

---

## Abstract (draft)

LLM agents are confused deputies by construction: they wield their operator's
authority while consuming attacker-influenced text, and prompt-injection
attacks routinely convert read access into exfiltration. Existing defenses
are either imperative guardrails — brittle, unauditable, bypassable by
rephrasing — or hard restriction, which destroys the agent's utility. We
present APPA, a declarative information-flow policy engine for agentic
workloads whose entire run state is two monoids: a *label* (audience × trust)
acted on by tool-declared deltas, and an append-only *event log*. Tool
contracts declare a label action, emitted effects, and requirements;
every proposed call is checked by label arithmetic, log predicates, and
declared per-call attention demands — never content.
Two design moves distinguish APPA from classical IFC transplants. First,
*state acquisition is priced, not just release*: a call that would narrow the
trajectory's future release frontier is soft-blocked with executable remedy
plans, shifting the remedy choice to before the data is fetched — and an
empty remedy set is a proof of unremediability, enumerated from the declared
registry, not a search timeout. Second, *judgment is atomic*: every
discretionary act is an authority ruling delivered inside an indivisible
render–rule–dispatch step over the engine-rendered request — never the
agent's paraphrase — with every ruling call-scoped, so the label
never widens at all and no grant object exists to steal, replay, or
misbind. We prove the core soundness properties (monotone
descent and settlement, merge confinement, remedy completeness), describe an implementation
split into a pure decision core and a state-owning shell, and evaluate on
AgentDojo. [TODO: one sentence of results.]

## 1. Introduction (draft)

An LLM agent with tools is a confused deputy in the textbook sense: it acts
with the authority of its principal while taking instructions, in the same
channel, from whatever text its tools return. Prompt injection is not a bug
in any one model but a structural property of this arrangement: give a
capable agent private data, attacker-influenced content, and an egress
channel, and read access becomes exfiltration.

Deployed defenses cluster at two poles. At one pole, imperative guardrails:
classifiers, regex filters, prompt hygiene. These judge *content*, so they
are bypassed by paraphrase and give no account of what they guarantee. At
the other pole, hard restriction: allowlists, human approval on every call,
no-tools-after-untrusted-read policies. These judge *structure* but do so
statically and coarsely, so they tax every benign action to stop a rare
malicious one, and agents lose the utility that justified their deployment.
The literature has begun carrying information-flow control (IFC) into agent
runtimes — labels propagated through the trajectory, sinks checked against
them [2505.23643; CaMeL]. We take this line to what we argue is its natural
fixed point, and then diverge from it in two places where agentic workloads
break the classical assumptions.

The fixed point first. APPA's entire run state is two monoids. A **label** —
audience (a reader set) × trust (a rank in a finite chain) — travels with
the data and is *checked*; tool contracts declare label *actions*
(restrictive reads intersect and take minima — in v1 the only action kind:
rulings cover gaps check-transiently and never act on the label), and the
trajectory label is the composition of the
actions of the calls that actually dispatched. An append-only **event log**
travels with the run and is *recorded, never approved*: effects
(egress, mutation) appended on success, and governance events (rulings and
the dispatches that consume them, boundaries, sanitizer applications,
casts). Every policy decision is a label comparison or a log
predicate, or a declared per-call attention demand; anything imperative —
approval, sanitization, ACL resolution —
lives in registered components outside the engine. With no permissive
action the fold is a bag of meets — commutative, monotone, settling; the
non-commutative algebra (a restricted read *after* a granted widening
re-narrowing the state and evicting the added readers) moves wholesale
into the epoch-wide-raise extension — expressible, deliberately not
taken (§9).

The two divergences are the contribution. **First, acquisition is a policy
event.** Classical IFC prices release: reads are free, and the taint bill
arrives at the sink. For an agent this is backwards — by the time the sink
check fails, the agent has already fetched the data, burned the turns, and
narrowed its own future. APPA orders label states by restrictiveness and
soft-blocks any call whose prospective state strictly descends: down is free
*but deliberate*, up needs authority. The block carries remedy plans, and
because raise-free steps only descend and every remedy is statically
declared — an authority's mandate, a sanitizer's transition, a tool emitting
the missing event — the remedy space is finite and enumerable from the
registry: an empty remedy set is a *proof* that no unlock exists, which the
agent can act on without wasting turns. (Acquisition blocks always carry
one extra, registry-free remedy — the agent's own acceptance of the
narrowing — so emptiness-as-proof concerns requirement-side gaps.) **Second, judgment is atomic and
call-scoped.** Every discretionary act is an authority ruling
delivered inside one indivisible step: the engine renders the pending call
(tool plus resolved arguments, with provenance, never value bytes and never
the agent's paraphrase), puts it to the authority, and on approval
dispatches it — the plan id, the ruling, and the dispatch land in the log
together. Approval and dispatch coincide, so no grant object exists between
them to swap, replay, or misbind; the grant discipline of earlier drafts of
this model (issue, bind, spend, boundary-death) is demoted to an internal
invariant of this step. And every ruling
releases *one dispatch* while the label stays put — the widened state
classical declassification would commit simply never exists; a persistent
widening (the epoch-wide raise) remains expressible in the algebra, but
for the applied problem it targets — an ongoing external exchange — we
prefer branching, and say so (§9). Together with remedy-set soundness — every
offered plan is individually sound, so *which* plan a possibly-tainted agent
picks is security-irrelevant — this is robust declassification
[Zdancewic–Myers] in agentic form.

Confinement completes the picture where the deployment allows it: sub-runs
fork at the parent's fold (never a fresh slate — that would be a laundering
primitive), and merge by construction cannot widen the parent — the returned
value's label is absorbed by intersection, while history needs no merging at
all: branches append to one shared log in realtime, because an egress that
happened in a branch happened in the world. Multi-step remedies compile into
single composite invocations executed inside the confining layer, so a
tainted agent selects plans whose steps it never holds — approval stays with
the plan's authority.

Contributions:

1. A two-monoid model of agentic information flow — label actions checked,
   events recorded — with policy as pure data (contracts, mandates) and all
   imperative judgment in registered externals (§3).
2. The acquisition check: soft-blocking of restrictive state descent with
   sound remedy plans, finitely enumerable from the registry; "empty remedy
   set is a proof" via raise-free monotonicity (§4).
3. Atomic, call-scoped rulings over engine-rendered requests: no
   grant objects, no ambient elevation, no label widening at all, robust
   declassification for benign-but-confusable agents (§5).
4. Branch semantics (fork/merge/quarantine) and compiled composites under
   which no branch can widen its parent, with honest world-history
   propagation through one shared log (§6).
5. An open-source implementation — a pure decision core under a state-owning
   shell — evaluated on AgentDojo (§7–8). [TODO: soften/sharpen once numbers
   exist.]

## 2. Related work (bullets)

- **Classical IFC.** Lattice model [Denning, CACM 1976]; noninterference
  [Goguen–Meseguer, 1982]. Position: we inherit the sink discipline; agentic
  workloads add the acquisition side.
- **Decentralized labels / reader sets.** DLM [Myers–Liskov, SOSP 1997] —
  audience-as-reader-sets follows it; rulings bind groups symbolically,
  with membership resolved fresh at every check.
- **Declassification taxonomy.** [Sabelfeld–Sands, JCS 2009] what/who
  dimensions — sanitize (what) / approve/endorse (who) map onto it. Robust
  declassification [Zdancewic–Myers, CSFW 2001] — the property our ruling
  discipline targets; state the agentic analogue precisely.
- **Least privilege.** [Saltzer–Schroeder, 1975] — the state-acquisition
  check is its trajectory-level form.
- **Graded/parametric effect structure.** Graded monads [Katsumata, POPL
  2014] — the dimension interface (carrier, ordered action monoid, adequacy
  relation) is parametric in exactly this shape; deliberately not exposed as
  a runtime API (a lawful fold can be property-tested, the security meaning
  of a dimension cannot — new dimensions arrive by engineering review, not
  registration).
- **Gradual typing for security.** [Disney–Flanagan, STOP 2011;
  Fennell–Thiemann, CSF 2013] — Unknown as unadmitted-not-lattice-element;
  cast-at-boundary = fillna.
- **Linearity.** [Wadler 1990] — the ruling as a consumable resource in an
  append-only world, now spent in the same breath it is minted (atomic plan
  execution).
- **Agent security.** Dual LLM [Willison 2023]; CaMeL [Debenedetti et al.,
  arXiv:2503.18813] — quarantined branches are the structured form; IFC for
  agentic workloads [arXiv:2505.23643]; ADTs/monads as vehicle
  [arXiv:2603.00991]. Guardrail/prompt-filter literature as the imperative
  pole; approval-fatigue critiques of HITL.
- Positioning table (defense × {declarative, pre-acquisition, call-scoped
  release, provenance-faithful approval, confinement}). [TODO]

## 3. The model (bullets + formal statements)

- **Label state.** `S = P(U) × T`: reader set over finite universe U (named,
  possibly nested groups; symbolic, closed under ∩/∪; containments are
  configuration decided as data; raw-id membership resolved fresh at every
  check by registered resolvers — a removed member is excluded from all
  later checks, nothing looks back between checks), trust rank in finite
  chain T (deployment-configurable instance; both instances lawful by
  construction — chain + powerset need no deployer proof obligations).
- **The checked monoid is the monoid of policy actions on S** (composition,
  do-nothing identity — monoids in the literal sense). Restrictive action =
  (∩A, min r) — in v1 the *only* action kind: rulings cover gaps
  check-transiently and never act on the label. Fold = composition of
  meets — commutative, monotone. (The epoch-wide extension — expressible,
  not taken (§9) — adds the
  permissive action (∪B, max up to ceiling), ruling-carried; under it the
  fold turns non-commutative exactly where authority entered.)
- **Prop. 1 (collapse).** In v1 the fold collapses to a running meet — a
  single keep-set and minimum; no replay needed. (Under the
  extension: any composite of audience actions equals a keep-then-add pair
  `s ↦ (s ∩ A) ∪ B` — the pair is the balance, the actions are the
  transactions. [Proof: appendix, straightforward induction.])
- **Prop. 2 (monotone descent + settlement).** Every v1 step is a meet;
  both carriers finite ⇒ the state descends monotonically and settles
  after finitely many strict descents — no
  oscillation, unconditionally. (Label stabilization, not run
  termination — identity deltas may repeat forever.) [Trivial; powers
  §4's completeness claim, so state it.]
- **The log monoid.** Free monoid on events under concatenation; appended,
  never checked. Effects append on success — one append point, deliberately
  off any pre/post axis; a call that dispatched but failed appends nothing
  (a failed send may still have reached an inbox — accepted for
  simplicity, with the invoke/append crash gap, §9); a positive `prior(k)`
  proves the tool reported success, nothing more about the outer
  world. Governance events: rulings
  and the dispatches that consume them, boundaries (turn end / fork /
  merge — punctuation, not decisions), the agent's descent acceptances,
  sanitizer applications, casts.
  Consulted exactly three ways: history predicates, ruling validity, audit.
  Views (e.g. seen-effect-kinds) are monoid homomorphisms, cached; the
  "effects dimension" of earlier designs is precisely such a projection,
  demoted from the label — an effect is about what the run *did*, not what
  the data *is*. Folding events into the label is possible as notation (a
  product of monoids is a monoid) and destructive as semantics: per-value
  vs per-run merge, predicate polarity, and consumability would all
  re-emerge as special cases inside the "unified" object.
- **Effects as the sanctioned pressure-release valve**: bespoke gating
  rituals encode as effect vocabulary plus a resolver-implemented authority (a
  `finance.spend` magnitude summed as a log view deciding auto-approve vs
  page-a-human) — a named effect in an auditable log, not a distortion of
  the label algebra; ordered magnitudes in the checked algebra deliberately
  out of scope.
- **Contracts.** One contribution per monoid — `delta` (checked before
  applied) + `emits` (applied, never checked) — plus `requires`: label
  requirements (floor, cover `⊇ recipients`, source bound `⊆ C` /
  `strict`), history predicates (`no_prior` persistent-negative,
  waivable; `prior` persistent-positive, remedy = make the effect happen),
  and **attention demands** — named per-call marks, never satisfied by
  history (durable-vs-per-call is precisely the effects/attention split);
  met only by a ruling from an attending authority inside atomic plan
  execution. Plus routing-only `tags` — names with no algebraic life, the
  exclusive currency of authority scope (§5).
  Placeholders; dynamic resolvers as trusted base. Surface
  conventions (requires-first reading; source deltas derivable from ACL
  resolvers): presentation, never the slots — a delta is what the run
  *learns*, a requirement is what the run *exposes*.
- **Unknown.** Not a carrier element (`trusted < unknown < suspicious` does
  not exist); an unresolved dimension keeps the value out of the algebra
  entirely — composition stays total, monoid laws unharmed. Checks report
  *which* facts are unresolved. Casts are explicit, audited governance
  events; a cast is constant XOR resolver-implemented under a declared
  ceiling of admissible targets — YOLO-vs-paranoid is the constant knob,
  and the ceiling keeps a dynamic classifier from becoming a laundering
  endpoint. Gradual-typing framing.
- **Threat model** (may move before §3): benign-but-confusable actor; malice
  only via suspicious/unknown content; authorities/sanitizers/casts — with
  the dynamic resolvers implementing them — plus
  config are the trusted base; serialized durable log assumed
  (append serialization across branches is an enforcement-layer
  obligation; a history check is only as sound as the log it has seen, and
  the invoke/append crash gap is accepted, not defended — §9); covert
  channels from a malicious model out of scope.

## 4. Enforcement: the two checks (bullets)

- **Check 1 — tool requirement compatibility** (classic IFC sink checking;
  sink-side adequacy): trust floor by chain order; audience cover
  `readers(fold) ⊇ recipients(args)`; recipient derivation via
  placeholders/resolvers. Noninterference discipline; nothing novel, claim
  inheritance not invention.
- **Check 2 — state acquisition** (least privilege, CaMeL-adjacent): order
  S by restrictiveness (`s' ≤ s` iff readers ⊆ and trust ≤); every
  contribution is a meet — down or stays, both axes, no exceptions;
  soft-block iff `fold(s, delta(call)) < s`; repeats (fixpoints) pass.
  Accepting the descent is the *agent's own* free plan step — no security
  power is exercised, so no authority is involved; the deliberateness stop
  *is* the agent selecting the plan. The two gates compose independently:
  a ruling never accepts a narrowing on the agent's behalf, and an
  acceptance clears no requirement. The
  *release frontier* is the theorem, not the checked object: release-side
  requirements are monotone in s ⇒ descent only shrinks the
  ruling-free-satisfiable set; ⊆-bounds are anti-monotone (descent
  *enables* them) — exactly why they sit outside the frontier story.
- **Sign rule.** Deltas never raise — the only sign invariant v1 needs. A
  call may carry a restrictive delta and a release-side requirement gap
  at once (`search_and_share`); then both gates apply independently: the
  agent accepts the narrowing, a ruling covers the gap. A tool whose
  action is itself an access grant
  (`share_doc(doc, outsider)` = fetch composed with grant-access) is
  modeled as a composite so each transition stays simple to rule on.
- **Two clocks.** Label requirements on the prospective state
  `fold(L, delta(call))` — the state the dispatch would commit. The attack
  otherwise: `search_and_share` with
  `requires = { audience = { includes = ["public"] } }`,
  `delta = { audience = { exactly = ["internal"] } }` — passes on the current state while the
  bytes it shares *are* the internal data its own dispatch commits. History
  predicates on the log as-is (= pre-state by construction; a call's emits
  can't trigger its own precondition). ⊆-bounds default prospective (the
  fetch itself narrows past the bound; readers dropped by the meet
  provably receive no post-read
  content); `strict` adds the pre-state bound — the clean room must already
  exist, established by a separate, deliberately accepted prior narrowing;
  channels that physically show every message to fixed readers regardless
  of the fold are out of scope (agentic trajectories, not generic channel
  IFC). ("Evict" language is reserved for the raise extension.)
- **Thm (remedy completeness, scoped).** The remedy space is finite and
  enumerable from the registry: in-scope (tag-routed) ruled covers whose
  mandate ceiling reaches the gap;
  input-sanitizer substitutions producing an admissible derived argument
  (any deployment); output-sanitizer-backed composites
  (confining only); for failed `prior(k)`, registered tools whose `emits`
  include k; declared waiver mandates and attended attention marks; for an
  acquisition soft block, the acceptance plan — always available from no
  registry entry, because it grants nothing (acquisition blocks are never
  terminal; emptiness-as-proof is a requirement-side claim). For release-side
  failures completeness follows from monotonicity: unruled steps only
  descend, sanitizer/branch results merge by ∩/min ⇒ nothing outside the
  enumeration can cure the gap. Empty ⇒ proof of unremediability. **Weak
  direction stated honestly:** nonempty = a plan exists relative to
  registry + resolver answers at check time. (Depends on the
  no-empty-mandate rule — an authority whose mandate covers nothing is a
  loud load error, §7.)
- **Eviction soundness (prop., extension only).** Belongs to the
  epoch-wide raise (§9): a post-widening restricted read's
  intersection removes every reader not in the new content's own reader
  set; survivors are entitled by construction; evicted readers receive no
  post-read content. Vacuous in v1 — no widening ever exists to evict.
- **Remedy-plan delivery**: plans are executable objects with ids; every
  engine-side plan runs behind one stable agent-facing tool
  (`execute_remedy_plan(plan_id)`, present from run start —
  mid-conversation tool injection breaks prompt caches); on execution the
  id, the dispatch, and — for ruling-carrying plans — the ruling all land
  in the log. A `prior(k)` plan carries no engine-side step (below).
- Resolved: demanded-but-not-failed predicates (an attention demand on an
  otherwise-passing call) surface through the same block shape — the unmet
  demand is the failed predicate; the remedy plan is the atomic ruling by
  an attending authority.
- Resolved: the shape of a `prior(k)` plan — no engine-side step: the plan
  names a registered tool whose `emits` include `k`, and the agent
  dispatches that tool as an ordinary, separately-checked call before
  re-proposing the original — two transitions, each under its own check,
  nothing atomic between them.

## 5. Atomic rulings and robust declassification (bullets)

- One judgment format: *authority A authorizes engine-rendered transition
  T*. No grant kinds, no grant objects in config or on any wire — public
  vocabulary is **mandates** (declarations), **rulings**, **log records**.
  One dispatch rule: a transition dispatches iff every failed or demanded
  predicate is covered
  by the rulings that admit it — each issuer's mandate covering what it
  admitted, all rulings bound to the same rendered call and consumed
  together in one atomic step (usually one; two-eyes collects several). Two-gate principle: a ruling admits a dispatch despite a gap
  and never edits the trajectory — the trajectory changes only through
  what the admitted call itself commits (`delta` + `emits`); the
  deliberateness stop on narrowing is the agent's own gate, which no
  ruling can substitute for (§4).
- **Atomic plan execution** (ruling-carrying plans; an acceptance plan is
  atomic trivially — accept + dispatch, no authority round trip). One
  indivisible step on a suspended line:
  render → put to authority (provenance, never value bytes) → on approval,
  dispatch; plan id + ruling + dispatch land in the log together. The grant
  discipline of earlier drafts (issue, bind, spend, boundary-death) is
  demoted from public concept to internal invariant: approval and dispatch
  coincide, so nothing intervenes; no swapped call (the ruling names the
  rendered transition); no replay (consumed by the dispatch it admitted —
  one review is one review); log-reconstructible. [Wadler 1990 framing: the
  consumable resource, now spent in the same breath it is minted.]
- **Call-scoped release is the only ruling**: it covers one dispatch and
  the fold does not change — the release is recorded in the log (ruling +
  effect) while the label keeps describing what the data *is*. This is
  the branch-confined construction promoted to the semantics: the
  trajectory never holds a widened state at all. The **epoch-wide raise**
  (a ruling-carried permissive delta persisting until the next narrowing
  evicts it) is algebraically coherent but deliberately not adopted: the
  applied need it targets is met by branching (§6), and §9 states that
  trade honestly.
- **Mandate typology** (where typology belongs — declarations, not runtime
  objects), each power naming the currency it acts on: cover up to a
  ceiling (trust floors up to a rank, recipient gaps up to a declared
  reader set — the label untouched); named waiver (the `no_prior` event
  kinds it may except); attends (the attention marks whose demands its
  ruling satisfies — tool and authority never name each other, both
  reference the mark; one ruling may cover a requirement gap and an
  attention demand
  on the same call, never the narrowing-acceptance gate; two eyes = two
  marks attended by different
  authorities). The acquisition block is deliberately absent: accepting a
  descent is the agent's free plan step, not a granted power. **Scope** is
  the jurisdiction axis, routed by `tags` exclusively — trust, audience,
  and effects are checked currencies and never routing keys; soundness is
  tag-independent (a mis-routed authority still cannot exceed its mandate
  and still rules on the rendered call), only completeness is
  tag-dependent.
- **Response-sink rule**: no ruling issued by the end user may cover any
  predicate of a response-sink release, whatever
  mandate the user otherwise holds — the approval request would arrive on
  the very channel being released; in-band self-confirmation is
  structurally not a check.
- **Canonical rulings** close the paraphrase channel (the tainted model
  summarizing "may I email the compliance archive?" omitting the
  attacker-derived address). **Remedy-set soundness** gives selection
  immunity: every plan individually sound ⇒ the tainted planner may keep
  driving.
- Map onto [Sabelfeld–Sands] what/who; state the robust-declassification
  property for the benign-but-confusable actor precisely (attacker controls
  suspicious content ⇒ controls requests, not releases).
- **Sanitizers**: registered transformers deriving new values under mandated
  transitions — never applied to the trajectory (post-factum
  sanitization cannot clear a fold: corollary of the observation model, not
  a policy choice). One transition, two application points: **output** —
  the derivation is admitted, the raw result stays confined (context
  protection; confining deployments only); **input** — the derivation is
  substituted into the engine-rendered call, so the harness dispatches the
  redacted bytes (sink protection, any deployment — but it cannot un-leak
  the context: the raw value was already observed); the sink check
  evaluates the derivation's declared label in place of the raw
  argument's contribution, the trajectory label untouched, the
  application logged. **Transitions move
  audience only — trust never rises through a sanitizer**: a mechanical
  transform can bound who may read its derivative; "now trustworthy" is
  judgment, i.e. a ruling or a cast; sole structured exception is the
  quarantine-exit attestation (§6). Mandates bind transitions, not
  information (`remove_pii`
  sound for CRM tickets, laundering for all-PII data); information-type
  scoping = explicit future work. Registration is a trust decision about the
  transformer, not a verification of its output. **Casts** as the
  second non-grant power (fillna): constant XOR resolver-implemented under
  a declared target ceiling.
- Sudo metaphor, honest sense: one command, one elevation, nothing ambient
  afterwards.
- Resolved: response-sink mechanics beyond the bar above — the contract
  governing the assistant's reply and its entry into the check pipeline —
  are explicitly out of scope for this version.
- FixMe: the precise formal statement of robust declassification under
  call-scoped release — the sink relation becomes "holds, or covered by a
  mandated ruling," so the label is a truthful description of the data, not
  an upper bound on releases; harder to state than
  noninterference-modulo-declassification and load-bearing for the paper.

## 6. Confinement (bullets)

- Deployment taxonomy: must see full trajectory + control execution
  (harness; proxy+gateway; aggressive proxy; *not* a pure MCP gateway —
  product-wise a feature: APPA is exactly as smart as the context it
  holds). **Confining** capability = can hold raw bytes back; the
  constructions are its consequences; checks/propagation hold everywhere.
- **Fork**: child starts at parent's fold (fresh slate = laundering
  primitive: "summarize what we know" into a public label); child appends
  to the **same shared log** — the parent's history is its prefix; the fork
  boundary kills anything pending (an in-flight plan execution — an
  approval request not yet ruled — finds the boundary
  and dies — falls out of the boundary clause, no special rule).
- **Merge, per monoid**: returned value = data, absorbed ∩/min ⇒ **Prop.: a
  child cannot widen its parent** (∩ cannot add readers; in v1 the child
  holds no widened state to begin with — the prop is load-bearing under
  the raise extension and for attestation-raised result values).
  History needs no merging: one shared log, realtime appends — an egress in
  the child happened in the world the moment it was sent, not at merge
  time. A ruling issued in a branch is a record, not a token — consumed
  inside its own atomic plan execution, nothing for the parent to reuse;
  merge appends a boundary, killing anything still pending. Finalization
  trivial for every started branch (return/failure/abandonment): nothing
  was withheld, so nothing can be lost. Result label less restrictive
  than the
  child's fold only via a mandated sanitizer or the quarantine-exit
  attestation.
- **Quarantined branches** (Dual-LLM/CaMeL lineage): pre-declared
  `submit_result` schema + sanitizers; schema validation never raises —
  structure is not provenance; the raise is the mandated sanitizer's
  attestation-shaped claim ("the returned integer is a version number from
  the named source, carrying none of its free text" — not a mere parser).
  This attestation is the system's only unruled trust up-move (§5) and is
  registered accordingly.
- **Compiled composites**: plan → one synthesized invocation (`requires` =
  entry conditions + an attention demand attended by the plan's
  authority); the ordered
  body is part of the rendered object the authority rules on, so the
  approval covers the enumerated internal requirement gaps, while
  executing the plan is the agent's recorded acceptance of the enumerated
  internal narrowings — each gate exercised by its own party; body runs inside the
  confining layer, per-step checks against evolving internal state,
  intermediates never surface; composite delta = returned value's merge
  contribution (raw step-delta composition wrong in both directions);
  two-phase (declared result-label bound checked outer; actual result label
  checked before commit; on failure value discarded, executed steps' events
  stand — honest prefix, no undo promised); actor never holds steps ⇒
  cannot cherry-pick. Non-confining deployments cannot compile composites —
  documented trade.
- Resolved: cross-line `no_prior` under the shared log is deliberately
  global — a child's egress fails a parent's `no_prior(egress)`; effects
  are facts about the world, not about a line. Line scoping (the
  once-parked line-scoped `prior(k)`) is rejected (§9 states the accepted
  edge).

## 7. Implementation (bullets)

- Two layers. **Inner: the pure algebra** — `check(state, transition) →
  verdict`, `apply(state, transition) → state'`, no IO, no clock;
  semantically a function of the full event log, every decision replayable
  from the log alone; the wire passes homomorphic views (fold state,
  seen-effect-kinds, pending-plan records, boundary positions), sound
  because every view is itself a fold. **Outer: owns state** — durable
  append with pluggable destinations (local file / database), serialization,
  the durability obligations of the threat model. Harness authors
  implement neither.
- Type-invariant-first: transition invariants enforced via the type system;
  external labels and authority decisions are trusted inputs. Design
  guideline (not a model claim): the checker free of ad-hoc conditionals —
  label arithmetic and log predicates only; imperative logic in registered
  externals.
- Config surface: a TOML dialect is drafted in the spec — still a draft,
  but every spec example is written in it. Constraints any shipped surface
  must keep: mandates only — no grant objects anywhere; the
  **no-empty-mandate rule** (an authority whose mandate covers nothing is
  a loud load error — the empty-`remedy_plans` proof depends on it); block
  messages surface the applicable remedy plans, naming eligible
  authorities where a plan carries a ruling; explicit set relations on
  every audience mention (includes / exactly / may-add); scope routed by
  tags only; casts constant XOR resolver-implemented.
- [TODO: LoC, dependency count — smallness as auditable-TCB argument.]

## 8. Evaluation (stubs — all TODO)

- **AgentDojo**: defended vs undefended across suites; security (injections
  blocked) vs utility (benign tasks completed) frontier; honest premise:
  contracts label tools by source type only, so a benign send and a
  poisoned one are identical in label space — trust-only sink policies pay
  a measurable utility price; audience is the dimension that splits them,
  framed as data-plus-protocol future work.
- **Sparse annotation**: annotate k risky tools, rest Unknown, sweep the
  cast policy — the gradual-security claim quantified.
- Case study: the CRM/auditor worked example end-to-end through the real
  engine (§ example or appendix).
- Non-goals stated: no content-level detection benchmark (we do not inspect
  content); latency microbenchmarks optional.
- FixMe: the implemented engine trails this model in places. Closed by the
  spec alignment: binary outcomes, effects as log state with an open
  declared vocabulary (`no_prior`, named per-dispatch waivers). Still
  predecessor-shaped: value-granular per-flow labels rather than the
  per-run label, check timing on the current rather than committed state,
  no atomic render→rule→dispatch plan objects, no `prior(k)`, no boundary
  events. Either close those before evaluating, or
  frame the evaluation honestly as a predecessor implementing the
  sink/taint half.

## 9. Limitations and future work (bullets)

- Benign-but-confusable is load-bearing: covert channels from a malicious
  model out of scope; trusted-source malice out of scope by definition.
- Approval fatigue: the real attack surface of any HITL scheme; adoption
  concern, out of scope — the algebra is only as good as the authorities
  and contracts registered into it.
- **Dispatch atomicity — resolved**: effects append when the tool call
  succeeds; the invoke/append crash gap and the
  failed-call-may-still-have-egressed window are accepted for simplicity
  in this version. The durable outbox (invocation + effects committed as
  one durable record before the invoke; storage-layer clause, invisible to
  contract authors) remains the hardening path; polarity-split
  `attempted`/`done` events are rejected — they would reintroduce the
  pre/post axis into every contract.
- FixMe (parked): outcome-sensitive completion events (an effect proves
  the tool reported success, nothing further about the outer world).
  Global history's accepted edge, stated honestly: a steered quarantined
  child can satisfy a parent's `prior(k)` gate with a real, harmless
  dispatch — line-scoped `prior(k)` was considered and rejected (§6).
- **Epoch-wide raise (expressible, not taken)**: the ruling-carried
  persistent widening, evicted by the next narrowing. The extension is
  algebraically coherent and comes as one package — the permissive
  action, non-commutative folds, the keep-then-add collapse (Prop 1),
  eviction soundness (§4) — plus a request-channel design (contract
  annotation vs a second offered plan vs harness config). We state a
  preference instead of deferring: for the applied problem — an ongoing
  exchange with an external recipient — **branching is the better
  construction**: the child carries the widening and dies with it, and
  merge-by-intersection makes leak-back structurally impossible, so APPA
  keeps the label narrow-only and spends its complexity budget there.
  Honest caveat: deployments will be tempted to amortize review with
  auto-approving authorities; such an authority must compare each rendered
  call against the one it reviewed — recreating epoch behavior without
  that check is strictly worse than the explicit extension would have
  been.
- Mandate scoping by information type (declared future work);
  pass-by-reference labels (byte-identical unretyped arguments keep their
  own label — falls out of arguments being LabeledValues, deferred);
  partial-context handoff (forking a child onto a sanitized subset of the
  parent's history) out of scope.
- Vendor-shipped dimensions by engineering review, never registration
  (lawfulness is property-testable; security meaning is not): the
  spend-budget monoid as the canonical case (ordered, per-run, accumulating
  in the free direction — today a log view summed by a
  resolver-implemented authority),
  independent attestations, value-specific purpose limitation.

## Appendices (candidates)

- A: Full definitions + proofs (Props 1–2, remedy completeness, merge
  confinement; eviction soundness under the raise extension).
- B: The worked example's full contracts and log.
- C: The rendered-transition format (what an authority actually sees) and
  the atomic plan-execution step.
