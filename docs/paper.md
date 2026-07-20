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
contracts declare a label action, emitted world events, and requirements;
every proposed call is checked by label arithmetic and log predicates alone.
Two design moves distinguish APPA from classical IFC transplants. First,
*state acquisition is priced, not just release*: a call that would narrow the
trajectory's future release frontier is soft-blocked with executable remedy
plans, shifting the remedy choice to before the data is fetched — and an
empty remedy set is a proof of unremediability, enumerated from the declared
registry, not a search timeout. Second, *judgment is atomic*: every
discretionary act is an authority ruling delivered inside an indivisible
render–rule–dispatch step over the engine-rendered request — never the
agent's paraphrase — with call-scoped release as the default, so the label
never widens ambiently and no grant object exists to steal, replay, or
misbind. We prove the core soundness properties (raise-free monotone
descent, merge confinement, eviction soundness), describe an implementation
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
(restrictive reads intersect and take minima; ruling-carried raises union
and take maxima), and the trajectory label is the ordered composition of the
actions of the calls that actually dispatched. An append-only **event log**
travels with the run and is *recorded, never approved*: world events
(egress, mutation) appended at dispatch, and governance events (rulings and
the dispatches that consume them, boundaries, sanitizer applications,
Unknown casts). Every policy decision is a label comparison or a log
predicate; anything imperative — approval, sanitization, ACL resolution —
lives in registered components outside the engine. Non-commutativity is
load-bearing: a restricted read *after* a granted widening re-narrows the
state and provably evicts the added readers from everything read
subsequently, and order matters exactly where authority entered.

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
agent can act on without wasting turns. **Second, judgment is atomic and
call-scoped by default.** Every discretionary act is an authority ruling
delivered inside one indivisible step: the engine renders the pending call
(tool plus resolved arguments, with provenance, never value bytes and never
the agent's paraphrase), puts it to the authority, and on approval
dispatches it — the plan id, the ruling, and the dispatch land in the log
together. Approval and dispatch coincide, so no grant object exists between
them to swap, replay, or misbind; the grant discipline of earlier drafts of
this model (issue, bind, spend, boundary-death) is demoted to an internal
invariant of this step. And the default ruling
releases *one dispatch* while the label stays put — the widened state
classical declassification would commit simply never exists; a persistent
widening is a distinct, explicitly requested ruling reviewed over the
trajectory state's provenance. Together with remedy-set soundness — every
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
3. Atomic, call-scoped-by-default rulings over engine-rendered requests: no
   grant objects, no ambient elevation, robust declassification for
   benign-but-confusable agents (§5).
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
  (∩A, min r); permissive action = (∪B, max up to ceiling), ruling-carried
  only, and only in the explicitly requested epoch-wide variant. Fold =
  composition; non-commutative by design; order encodes where authority
  entered (restrictive actions are meets and meets commute —
  non-commutativity enters exactly at ruled raises).
- **Prop. 1 (collapse).** Any composite of audience actions equals a
  keep-then-add pair `s ↦ (s ∩ A) ∪ B` — the pair is the balance, the
  actions are the transactions. Implementation theorem, not model: no replay
  needed. [Proof: appendix, straightforward induction.]
- **Prop. 2 (raise-free descent + termination).** Without ruled raises every
  step is a meet; both carriers finite ⇒ the state descends monotonically
  and settles — no oscillation, termination for free. [Trivial; powers §4's
  completeness claim, so state it.]
- **The log monoid.** Free monoid on events under concatenation; appended,
  never checked. World events at dispatch — one append point, deliberately
  off any pre/post axis; fail-closed for `no_prior` (attempt-on-record even
  if the call fails); a positive `prior(k)` proves dispatch, not
  outer-world success (outcome-sensitive events future work, consistent
  with outer-system outages being out of scope). Governance events: rulings
  and the dispatches that consume them, boundaries (turn end / fork /
  merge — punctuation, not decisions), sanitizer applications, casts.
  Consulted exactly three ways: history predicates, ruling validity, audit.
  Views (e.g. seen-event-kinds) are monoid homomorphisms, cached; the
  "effects dimension" of earlier designs is precisely such a projection,
  demoted from the label — an effect is about what the run *did*, not what
  the data *is*. Folding events into the label is possible as notation (a
  product of monoids is a monoid) and destructive as semantics: per-value
  vs per-run merge, predicate polarity, and consumability would all
  re-emerge as special cases inside the "unified" object.
- **World events as the sanctioned pressure-release valve**: bespoke gating
  rituals encode as event vocabulary plus a dynamic authority (a
  `finance.spend` magnitude summed as a log view deciding auto-approve vs
  page-a-human) — a named event in an auditable log, not a distortion of
  the label algebra; ordered magnitudes in the checked algebra deliberately
  out of scope.
- **Contracts.** One contribution per monoid — `delta` (checked before
  applied) + `emits` (applied, never checked) — plus `requires`: label
  requirements (floor, cover `⊇ recipients`, source bound `⊆ C` /
  `strict`) and history predicates (`no_prior` persistent-negative,
  waivable; `prior` persistent-positive, remedy = make the event happen;
  `confirmed_by` consumable-positive, delivered inside atomic plan
  execution). Placeholders; dynamic resolvers as trusted base. Surface
  conventions (requires-first reading; source deltas derivable from ACL
  resolvers): presentation, never the slots — a delta is what the run
  *learns*, a requirement is what the run *exposes*.
- **Unknown.** Not a carrier element (`trusted < unknown < suspicious` does
  not exist); an unresolved dimension keeps the value out of the algebra
  entirely — composition stays total, monoid laws unharmed. Checks report
  *which* facts are unresolved. Casts are explicit, audited governance
  events; YOLO-vs-paranoid cast policy is deployment config.
  Gradual-typing framing.
- **Threat model** (may move before §3): benign-but-confusable actor; malice
  only via suspicious/unknown content; authorities/sanitizers/resolvers +
  config are the trusted base; serialized durable log assumed
  (crash-atomicity and branch concurrency are enforcement-layer
  obligations; history predicates sound per-line under concurrency); covert
  channels from a malicious model out of scope.

## 4. Enforcement: the two checks (bullets)

- **Check 1 — tool requirement compatibility** (classic IFC sink checking;
  sink-side adequacy): trust floor by chain order; audience cover
  `readers(fold) ⊇ recipients(args)`; recipient derivation via
  placeholders/resolvers. Noninterference discipline; nothing novel, claim
  inheritance not invention.
- **Check 2 — state acquisition** (least privilege, CaMeL-adjacent): order
  S by restrictiveness (`s' ≤ s` iff readers ⊆ and trust ≤); every unruled
  contribution is a meet — down or stays, both axes, no exceptions;
  soft-block iff `fold(s, delta(call)) < s`; repeats (fixpoints) pass. The
  *release frontier* is the theorem, not the checked object: release-side
  requirements are monotone in s ⇒ descent only shrinks the
  ruling-free-satisfiable set; ⊆-bounds are anti-monotone (descent
  *enables* them) — exactly why they sit outside the frontier story.
- **Sign purity.** A single transition never mixes signs: a raise and a
  narrowing are always two transitions, rendered and checked separately
  (`share_doc(doc, outsider)` = fetch composed with grant-access) — the
  lattice comparison always faces a pure descent or a pure raise, never an
  incomparable product.
- **Two clocks.** Label requirements on the prospective state
  `fold(L, delta(call))` — the state the dispatch would commit. The attack
  otherwise: `search_and_share` with `requires: {audience: public}`,
  `delta: {audience: internal}` — passes on the current state while the
  bytes it shares *are* the internal data its own dispatch commits. History
  predicates on the log as-is (= pre-state by construction; a call's emits
  can't trigger its own precondition). ⊆-bounds default prospective (the
  fetch itself evicts; evicted readers provably receive no post-read
  content); `strict` adds the pre-state bound — the clean room must already
  exist, established by a separate, deliberately accepted prior narrowing;
  channels that physically show every message to fixed readers regardless
  of the fold are out of scope (agentic trajectories, not generic channel
  IFC).
- **Thm (remedy completeness, scoped).** The remedy space is finite and
  enumerable from the registry: ruled raises whose mandate covers the gap;
  sanitizer-backed composites producing an admissible relabeled value
  (confining only); for failed `prior(k)`, registered tools whose `emits`
  include k; declared waiver/confirmation mandates. For release-side
  failures completeness follows from monotonicity: unruled steps only
  descend, sanitizer/branch results merge by ∩/min ⇒ nothing outside the
  enumeration can cure the gap. Empty ⇒ proof of unremediability. **Weak
  direction stated honestly:** nonempty = a plan exists relative to
  registry + resolver answers at check time. (Depends on the
  no-empty-mandate rule — an authority whose mandate covers nothing is a
  loud load error, §7.)
- **Eviction soundness (prop.).** A post-widening restricted read's
  intersection removes every reader not in the new content's own reader
  set; survivors are entitled by construction; evicted readers receive no
  post-read content.
- **Remedy-plan delivery**: plans are executable objects with ids behind
  one stable agent-facing tool (`execute_remedy_plan(plan_id)`, present
  from run start — mid-conversation tool injection breaks prompt caches);
  id, ruling, dispatch all land in the log.
- FixMe: open question — wire shape for demanded-but-not-failed predicates
  (`confirmed_by` on an otherwise-passing call) under the binary allow/block
  outcome.
- FixMe: open question — the shape of a `prior(k)` plan: plan execution
  spans two transitions (the event-minting call, then the original one);
  per-transition checking inside one plan execution is unresolved.

## 5. Atomic rulings and robust declassification (bullets)

- One judgment format: *authority A authorizes engine-rendered transition
  T*. No grant kinds, no grant objects in config or on any wire — public
  vocabulary is **mandates** (declarations), **rulings**, **log records**.
  One dispatch rule: a transition dispatches iff every failed or demanded
  predicate, and every raising component of its rendered delta, is covered
  by the ruling that admits it — and the issuer's mandate covers what was
  admitted.
- **Atomic plan execution.** One indivisible step on a suspended line:
  render → put to authority (provenance, never value bytes) → on approval,
  dispatch; plan id + ruling + dispatch land in the log together. The grant
  discipline of earlier drafts (issue, bind, spend, boundary-death) is
  demoted from public concept to internal invariant: approval and dispatch
  coincide, so nothing intervenes; no swapped call (the ruling names the
  rendered transition); no replay (consumed by the dispatch it admitted —
  one review is one review); log-reconstructible. [Wadler 1990 framing: the
  consumable resource, now spent in the same breath it is minted.]
- **Call-scoped release is the default**: the ruling covers one dispatch and
  the fold does not change — the release is recorded in the log (ruling +
  world event) while the label keeps describing what the data *is*. This is
  the branch-confined construction promoted to default semantics: the
  trajectory never holds a widened state at all. The **epoch-wide raise**
  is the explicitly requested variant: the raising action is part of the
  rendered transition's delta (including composition order — never
  ambient), reviewed over the trajectory state's provenance; later unruled
  deltas re-lower it; raising again takes a fresh ruling.
- **Mandate typology** (where typology belongs — declarations, not runtime
  objects): acquisition block; raise up to a ceiling (only epoch-wide
  transitions need it — the call-scoped default widens nothing); named
  waiver; confirmation of named tools (`confirmed_by` as a requirement
  demanding a ruling even when nothing fails; one ruling satisfies both a
  soft block and `confirmed_by` on the same call; two eyes = two named
  authorities; the end user as the default confirms-authority).
- **Response-sink rule**: no ruling issued by the end user may cover any
  predicate or raising component of a response-sink release, whatever
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
  transitions; **tool results only** — never the trajectory (post-factum
  sanitization cannot clear a fold: corollary of the observation model, not
  a policy choice), never outgoing arguments (an input sanitizer fires
  after the leak; exact-plus-redacted views of one datum = a branching
  construction). Mandates bind transitions, not information (`remove_pii`
  sound for CRM tickets, laundering for all-PII data); information-type
  scoping = explicit future work. Registration is a trust decision about the
  transformer, not a verification of its output. **Unknown casts** as the
  second non-grant power (fillna).
- Sudo metaphor, honest sense: one command, one elevation, nothing ambient
  afterwards.
- FixMe: open question — the request channel for the epoch-wide variant:
  contract annotation vs a second plan offered alongside the call-scoped one
  vs harness configuration.
- FixMe: open question — response-sink mechanics: what contract governs the
  assistant's reply and how it enters the check pipeline; only the
  no-self-approval bar is specified.
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
  boundary kills anything pending (an in-flight ruling finds the boundary
  and dies — falls out of the boundary clause, no special rule).
- **Merge, per monoid**: returned value = data, absorbed ∩/min ⇒ **Prop.: a
  child-internal widening cannot cross back** (∩ cannot add readers).
  History needs no merging: one shared log, realtime appends — an egress in
  the child happened in the world the moment it was sent, not at merge
  time. A ruling issued in a branch is a record, not a token — consumed
  inside its own atomic plan execution, nothing for the parent to reuse;
  merge appends a boundary, killing anything still pending. Finalization
  trivial for every started branch (return/failure/abandonment): nothing
  was withheld, so nothing can be lost. Result label smaller than the
  child's fold only via a mandated sanitizer.
- **Quarantined branches** (Dual-LLM/CaMeL lineage): pre-declared
  `submit_result` schema + sanitizers; schema validation never raises —
  structure is not provenance; the raise is the mandated sanitizer's
  attestation-shaped claim ("the returned integer is a version number from
  the named source, carrying none of its free text" — not a mere parser).
- **Compiled composites**: plan → one synthesized invocation (`requires` =
  entry conditions + `confirmed_by` on the plan's authority); the ordered
  body is part of the rendered object the authority rules on, so the
  approval covers the enumerated internal transitions; body runs inside the
  confining layer, per-step checks against evolving internal state,
  intermediates never surface; composite delta = returned value's merge
  contribution (raw step-delta composition wrong in both directions);
  two-phase (declared result-label bound checked outer; actual result label
  checked before commit; on failure value discarded, executed steps' events
  stand — honest prefix, no undo promised); actor never holds steps ⇒
  cannot cherry-pick. Non-confining deployments cannot compile composites —
  documented trade.
- FixMe: open question — cross-line `no_prior` under the shared log: a
  child's egress fails a parent's `no_prior(egress)`; intended conservatism,
  or needs line scoping (cf. the parked line-scoped `prior(k)`, §9).

## 7. Implementation (bullets)

- Two layers. **Inner: the pure algebra** — `check(state, transition) →
  verdict`, `apply(state, transition) → state'`, no IO, no clock;
  semantically a function of the full event log, every decision replayable
  from the log alone; the wire passes homomorphic views (fold state,
  seen-event-kinds, pending-plan records, boundary positions), sound
  because every view is itself a fold. **Outer: owns state** — durable
  append with pluggable destinations (local file / database), serialization,
  the dispatch-atomicity obligations of the threat model. Harness authors
  implement neither.
- Type-invariant-first: transition invariants enforced via the type system;
  external labels and authority decisions are trusted inputs. Design
  guideline (not a model claim): the checker free of ad-hoc conditionals —
  label arithmetic and log predicates only; imperative logic in registered
  externals.
- FixMe: config surface deliberately undecided; constraints any surface must
  keep: mandates only — no grant objects anywhere; the **no-empty-mandate
  rule** (an authority whose mandate covers nothing is a loud load error — the
  empty-`remedy_plans` proof depends on it); block messages name the
  authority able to clear them.
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
- FixMe: the implemented engine predates this model (tri-state outcomes,
  effects-as-dimension, no call-scoped rulings): either rebuild to v2 before
  evaluating, or frame the evaluation honestly as a predecessor implementing
  the sink/taint half.

## 9. Limitations and future work (bullets)

- Benign-but-confusable is load-bearing: covert channels from a malicious
  model out of scope; trusted-source malice out of scope by definition.
- Approval fatigue: the real attack surface of any HITL scheme; adoption
  concern, out of scope — the algebra is only as good as the authorities
  and contracts registered into it.
- FixMe: **dispatch atomicity — one open ruling**: the crash gap between invoking
  a tool and appending its events. (a) durable outbox — invocation + events
  commit as one durable record before the invoke; storage-layer clause,
  invisible to contract authors, log stays *true* (recommendation on
  record); vs (b) polarity-split events (`attempted`/`done`; negatives gate
  on attempted, positives on done) — fail-closed both ways but
  reintroduces the pre/post axis into every contract and leaves permanent
  conservative lies in the log.
- FixMe (parked): outcome-sensitive completion events; line-scoped
  `prior(k)` (a steered quarantined child can mint a parent-gate event with
  a real, harmless dispatch).
- Mandate scoping by information type (declared future work);
  pass-by-reference labels (byte-identical unretyped arguments keep their
  own label — falls out of arguments being LabeledValues, deferred);
  partial-context handoff (forking a child onto a sanitized subset of the
  parent's history) out of scope.
- Vendor-shipped dimensions by engineering review, never registration
  (lawfulness is property-testable; security meaning is not): the
  spend-budget monoid as the canonical case (ordered, per-run, accumulating
  in the free direction — today a log view summed by a dynamic authority),
  independent attestations, value-specific purpose limitation.

## Appendices (candidates)

- A: Full definitions + proofs (Props 1–2, remedy completeness, eviction
  soundness, merge confinement).
- B: The worked example's full contracts and log.
- C: The rendered-transition format (what an authority actually sees) and
  the atomic plan-execution step.
