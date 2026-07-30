# APPA specification

**Status: draft.** This is the normative account of the APPA model. It says
what an implementation must do and nothing about why — the arguments live in
`rationale.md`, and `../website/content/docs/how-it-works.md` is the
readable introduction.

Rules carry ids by family. Cite them from tests, issues and the paper —
ids, not section numbers. A rule keeps its id when it moves, so a family's
numbering need not run in document order. Every normative statement
carries an id. MUST, MUST NOT, SHOULD and MAY are used in the RFC 2119
sense.

Families: `POS` position and capability · `LBL` labels · `CHK` the check ·
`RMD` remedy plans · `AUT` authorities · `RUL` rulings · `SAN` sanitizers
and casts · `LOG` effects and history · `BRN` branching · `UNK` Unknown ·
`CFG` load-time rules · `EXT` external interfaces · `IMP` implementation
shape · `THR` threat model.

Sections that are not live carry a status. **Design direction** means agreed
but unspecified; **deferred** means specified but unimplemented.

## 1. Position and capability

APPA is a kernel that must see the full trajectory and control tool
execution. That admits three positions: the harness itself, an inference
proxy paired with a tool gateway, or an inference proxy alone if it
intervenes in model output aggressively enough to stop a call. A pure MCP
gateway cannot host it, since it sees tool calls but never the trajectory
that labels them.

- **[POS-1]** The engine MUST be positioned so that every tool call is
  checked before dispatch and every admitted value is folded into the label.
- **[POS-2]** A deployment is **confining** if it can run a tool call and
  keep the result out of the model's context. Pending-cast admission
  (§11.1) and quarantined branches (§10.1) depend on exactly that and
  exist only in confining deployments.
- **[POS-3]** Checks and label propagation MUST behave identically in
  confining and non-confining deployments. Capability affects which remedy
  plans exist, never which flows pass.
- **[POS-4]** A channel that already shows every message to a fixed reader
  set regardless of the label is out of scope: the label cannot un-show
  what the channel exposes.
- **[POS-5]** A deployment is **context-controlling** if it chooses what a
  child branch sees and takes delivery of what the branch returns.
  Branching (§10) exists only in context-controlling deployments. The
  capability is weaker than confinement: a host can bound a child's
  context while still showing its own model every tool result; quarantined
  branches need both capabilities.

- **[POS-6]** In a deployment that rebuilds model requests, the transcript
  head — the system and developer messages opening every request — is host
  configuration and MUST NOT be client input.

## 2. Labels — `LBL`

- **[LBL-1]** A label has exactly two dimensions: **trust** and
  **audience**. The shape is fixed; the instance is deployment
  configuration, expressed as data.
- **[LBL-2]** Trust is a finite ordered chain of ranks. The default instance
  is `suspicious < trusted`. A deployment MAY supply its own chain.
- **[LBL-3]** Audience is a set of readers drawn from a fixed per-deployment
  universe. `public` denotes the whole universe.
- **[LBL-4]** In the current dialect every audience reaching the algebra is
  an explicit id-list, so intersection and subset are exact. A customer
  classification scheme is expressed as audience configuration by writing
  each tier out as its member readers.
- **[LBL-5]** Folding a value's label into the run's intersects the audience
  and takes the minimum trust.
- **[LBL-6]** Every `delta` MUST be restrictive. No permissive delta exists,
  and no engine operation widens a label on either dimension.
- **[LBL-7]** The current label is the fold of the starting label with every
  admitted delta, and MUST be recomputable from the log alone. No replay of
  decisions is required.
- **[LBL-8]** The starting label is engine configuration. The neutral,
  least restrictive value is `{audience: public, trust: trusted}`.
- **[LBL-9]** A ruling MUST NOT change the label. See `RUL`.
- **[LBL-10]** All data flows as a **labeled value**: one tool call plus its
  result, carrying the label of the information it holds. Value and label
  MUST NOT be separated; an operation takes both or neither.
- **[LBL-11]** The label folds only from **admitted** values. A call that
  succeeds but admits no value — an oversized body, a refused derivation —
  appends its effects and folds nothing.
- **[LBL-12]** Deriving a cleaner value from an admitted one MUST NOT undo
  the fold. Sanitizing after the fact cannot clear a trajectory; the
  derivation carries its own label and the run keeps the one it took.

**Design direction.** Today an audience is a literal list of reader ids:
a group such as `finance` is written out member by member in configuration,
and a directory change reaches the engine only when that configuration is
reloaded. The intended future shape for directory-backed deployments is
named groups resolved against the directory at dispatch time, which keeps
membership fresh at the cost of making every subset and intersection
depend on a resolver's answer. Until that membership resolver exists, the
id-list dialect stands.

## 3. The check — `CHK`

- **[CHK-1]** Every proposed call MUST be checked before dispatch. The
  outcome is `allow`, or `block` carrying what stopped the call — the
  unmet requirements, the narrowing where one fired, the values whose
  needed dimension no registered cast could establish — and the remedy
  plans.

```ts
type CheckOutcome =
  | { outcome: "allow" }
  | { outcome: "block";
      requirement_gaps: RequirementGap[];  // unmet entries of `requires`
      narrowing?: Narrowing;               // present when the call's own delta fired CHK-2
      unestablished?: ValueRef[];          // facts no registered cast could establish (CHK-16)
      remedy_plans: RemedyPlan[] };
```

- **[CHK-16]** APPA turns unknowns into knowns before it decides. When a
  check consumes an Unknown dimension, the runtime MUST attempt the
  registered casts on the unestablished values and re-check before
  returning an outcome; resolution is automatic, never an agent choice and
  never a remedy plan (§5.1). Casts are attempted in registration order
  and the first resolution that establishes the dimension stands, each
  re-validated against the cast's declaration (`SAN-8`) before admission.
  `unestablished` therefore names only values
  no registered cast could establish — none is registered, or its resolver
  abstained per `EXT-1` — and no ruling clears such an entry: a fact does,
  or the configuration changes. The pure core performs no IO; the runtime
  drives resolution and the engine admits the results (`UNK-8`). A runtime
  MAY attempt casts as early as admission, and a §11.1 pending-cast tool
  declares exactly that. *Deferred.*

### 3.1 Ordering and clocks

- **[CHK-2]** The **narrowing check** runs first, on the label the dispatch
  would commit — the current label with the call's own `delta` applied. Let
  `L' = L ⊓ delta(c)`. If `L' < L` strictly on either dimension, the engine
  MUST block `c` with `narrowing` populated and MUST NOT dispatch until an
  acceptance of exactly `L → L'` is recorded.
- **[CHK-3]** If `L' = L`, `CHK-2` does not fire. A repeat call that
  restricts nothing further passes without a narrowing block.
- **[CHK-4]** **Label requirements** evaluate on the current label, which
  with an accepted narrowing in force is the label the dispatch commits.
  This order is normative: evaluating requirements before the narrowing
  would let a call outrun its own consequences.
- **[CHK-5]** **History requirements** evaluate on the log as it stands at
  check time. A call's own `emits` MUST NOT satisfy its own precondition.
- **[CHK-6]** Neither label check is a configuration entity. Both derive
  from the contract's `requires` and `delta`, and the evaluation order of
  §3.1 is fixed: the configuration surface MUST NOT offer a way to
  reorder the checks or defer one.
- **[CHK-7]** Effects append when the call succeeds. The delta commits at
  admission of the result value or its registered derivation.

### 3.2 Requirement kinds

- **[CHK-8]** A **trust floor** (`trust = "r"`) accepts any rank at or above
  `r`.
- **[CHK-9]** An **`includes`** requirement holds when `audience ⊇
  recipients`. Where the contract uses placeholders, recipients are derived
  from the actual arguments of this call; a static contract declares them
  directly.
- **[CHK-10]** A **cap** requirement holds when `audience ⊆ C`. It follows
  the same clock as every label requirement, so a read that itself narrows
  into the cap passes and surfaces as an ordinary narrowing block.
- **[CHK-11]** `no_prior(k)` holds when no matching effect exists in the
  log. Checking does not consume it. It is waivable for one dispatch by a
  ruling whose issuer's mandate covers the waiver.
- **[CHK-12]** `prior(k)` holds when a matching effect exists. Nothing is
  waivable; the remedy is to make the effect happen. A positive `prior(k)`
  proves the emitting tool reported success and nothing more about the outer
  world.
- **[CHK-13]** An **attention demand** is per-call and MUST NOT be satisfied
  by history. It is met only by a ruling from an authority that attends the
  mark, delivered inside atomic plan execution. A repeat dispatch takes a
  fresh ruling.
- **[CHK-14]** A narrowing MUST be reported in its own slot and never as a
  requirement gap: nothing in `requires` failed, and an acceptance rather
  than a ruling answers it.
- **[CHK-15]** A single call MAY carry both a restrictive delta and a
  requirement gap. Both gates then apply, and neither substitutes for the
  other.

## 4. Tool contracts

A contract declares one contribution per piece of state, plus its
requirements and its routing tags.

- **[CFG-1]** `delta` is the label action, checked before it is applied.
- **[CFG-2]** `emits` are the effects a successful call appends, an
  unordered batch recorded as one step. They are applied and never checked.
- **[CFG-3]** `requires` carries label requirements, history requirements
  and attention demands.
- **[CFG-4]** `tags` have no algebraic life: they MUST NOT fold, enter a
  check, or reach the log. Their sole use is authority routing.
- **[CFG-5]** A contract MAY carry either of `delta` and `requires`, both,
  or neither. A call with both is checked on both.
- **[CFG-14]** Contracts may be **static**, **static with placeholders**, or
  **dynamic**. A dynamic resolver mapping an argument to a reader set — a
  document to its ACL's readers, a recipient to the readers behind it — MUST
  be registered in advance and is part of the deployer's trusted base.

## 5. Remedy plans — `RMD`

- **[RMD-1]** Every block MUST carry `remedy_plans`: the sound remedies
  available under the registered configuration and the deployment's
  capability.
- **[RMD-2]** Every plan with an **engine-side** step is an executable
  object with an id. The engine MUST expose `execute_remedy_plan(plan_id)`
  from the start of the run rather than injecting a tool when a block
  occurs. A plan with no engine-side step — `RMD-13`, `RMD-14` — names a
  call the agent makes for itself and carries no id, since there is nothing
  for the engine to execute.
- **[RMD-3]** On execution, the plan id, the ruling where the plan carries
  one, and the dispatch MUST all land in the log. For an acceptance plan the
  plan id is the record.
- **[RMD-4]** The list MUST enumerate every sound alternative. Each
  requirement gap independently chooses among its competent authorities; a
  choice combination groups into per-authority covers, a plan is its grouped
  assignment, and combinations whose groupings coincide are one plan.
- **[RMD-5]** Enumeration MUST be total. The alternative bound is enforced
  at load: a registry whose worst case would exceed the planner's cap is
  refused as a configuration-shape error. Runtime truncation is forbidden.
- **[RMD-15]** The list MUST be ordered least-mandate-first, so the agent
  reaches the least powerful entity that can help before a stronger one.
  Plans compare gap by gap on the mandate power assigned to each gap,
  within that gap's own currency: trust ceilings by rank order, audience
  ceilings and waiver sets by inclusion, attention by identity. Plan A
  precedes plan B when every gap's assigned power in A is at most B's and
  at least one is strictly less; plans incomparable under that order may
  appear in any relative order. Ordering is presentation only, and the
  enumeration stays total per `RMD-4`. *Deferred.*
- **[RMD-6]** An abstention consumes only the plan it was consulted for; a
  denial consumes every offered plan naming the denying authority for this
  rendered call (`RMD-16`). Plans naming other authorities stay offered,
  so an advertised alternative is always executable.
- **[RMD-7]** Re-proposal is bounded by the harness's blocked-proposal
  budget per rendered call, charged when a block's offers are minted. The
  budget is spam control on the agent, not a count of denials: a denial
  bites through `RMD-16`, by excluding the denying authority's plan, and
  never by shrinking the budget.
- **[RMD-16]** A denial is sticky for exactly its rendered call: once an
  authority has denied a plan for a rendered call — tool plus canonical
  digest — no later block of that same rendered call in the trajectory may
  offer a plan naming that authority. The denial is a recorded governance
  event (`LOG-3`), so the exclusion replays from the log. Plans naming
  other authorities stay offered per `RMD-6`; changed arguments change the
  digest and lift the exclusion. An abstention — including the timeout and
  error cases of `EXT-1` — is not a denial and does not stick. *Deferred.*
- **[RMD-8]** Pending offers die with their turn. A plan execution MUST be
  re-validated against the live state it lands in: an offer whose block
  re-derives unchanged executes, one the state has moved past is refused by
  value mismatch.

### 5.1 What the planner enumerates

| gap | enumerated remedy | status |
|---|---|---|
| unmet trust floor | in-scope authorities whose mandate ceiling reaches the rank | live |
| unmet `includes` | in-scope authorities whose mandate can vouch the readers | live |
| failed `no_prior(k)` | in-scope authorities whose mandate waives `k` | live |
| failed `prior(k)` | registered tools whose `emits` include `k` | live |
| failed cap | registered tools whose restrictive delta drops the offending readers | live |
| unmet attention mark | authorities attending the mark | live |
| narrowing | the acceptance plan | live |
| an Unknown a check consumes | registered casts, attempted by the runtime per `CHK-16` | live, never surfaced as a plan object |
| any gap curable by a redacted argument | input-sanitizer substitutions | design direction |

- **[RMD-9]** A **nonempty** list asserts that a plan exists relative to the
  registered configuration and, where dynamic resolvers contribute, their
  answers at check time. It does not assert that execution succeeds.
- **[RMD-10]** An **empty** list asserts that no plan exists — evaluated
  at the same instant as `RMD-9`, against the registered configuration,
  the resolver answers current at check time, and the denials recorded
  for this rendered call (`RMD-16`). A resolver answering differently
  later does not retroactively falsify it. The assertion concerns
  requirement gaps: an `unestablished` entry offers no plan by design,
  since a fact rather than a plan clears it (`CHK-16`).
- **[RMD-11]** The acceptance plan is always available for a narrowing, from
  no registry entry, because it grants nothing. A narrowing block is
  therefore never terminal, and the emptiness assertion of `RMD-10` concerns
  requirement gaps only.

### 5.2 Who executes a plan

- **[RMD-12]** A plan whose steps are engine-side acts — a ruling, a
  sanitizer application, an acceptance — MUST execute atomically through
  `execute_remedy_plan`. See `RUL-5`.
- **[RMD-13]** A plan for a failed `prior(k)` carries no engine-side step.
  It names a registered tool whose `emits` include `k`; the agent dispatches
  that tool as an ordinary separately-checked call, then re-proposes.
- **[RMD-14]** A plan for a failed cap has the same shape: it names a
  registered tool whose restrictive delta drops the offending readers. The
  narrowing is accepted at that tool's own block, and the re-proposed call
  is checked afresh.

## 6. Authorities and mandates — `AUT`

- **[AUT-1]** Authorities are the single home of judgment. Every act of
  human or policy discretion is a ruling.
- **[AUT-2]** There are no ruling kinds at runtime. The typology lives in
  **mandates**, which declare what an authority's rulings may cover.
- **[AUT-3]** Mandate powers, each naming the currency it acts on:
  - a **cover up to a ceiling** — admitting a dispatch over an unmet trust
    floor up to a declared rank, or over an unmet `includes` up to a
    declared reader set. The label does not move; the ceiling bounds the gap
    one ruling may cover.
  - a **named waiver** — covering a failed `no_prior` for the admitted
    dispatch only, naming the event kinds it may waive.
  - **attends** — the attention marks whose demands this authority's rulings
    satisfy.
- **[AUT-4]** A single ruling by an attending authority MAY cover both a
  label or history gap and an attention demand on the same call. One
  reviewer therefore means one review: forcing a second, independent pair
  of eyes on the same call takes a second attention mark attended by a
  different authority.
- **[AUT-5]** Accepting a narrowing MUST NOT be a mandate power. A deployer
  wanting a human on expensive narrowings attaches an attention mark to the
  narrowing tool.
- **[AUT-6]** An authority whose mandate covers nothing MUST be a load
  error, never a no-op. The emptiness assertion of `RMD-10` depends on it.
- **[AUT-7]** Requirement gaps route by **tags, exclusively**. An
  authority's `scope` names the tags it covers; an authority with no
  declared scope covers every call.
- **[AUT-8]** Attention gaps route by their own currency: a demand reaches
  exactly the authorities that attend its mark, and scope tags are not
  consulted.
- **[AUT-9]** Trust, audience and effects are checked currencies and MUST
  NOT double as routing keys.
- **[AUT-10]** Tags cannot break soundness, only coverage. A mis-tagged
  catalog may route a gap to an authority that cannot help — who still
  cannot exceed their mandate — or route it to no one, so the worst a
  mis-tagging produces is a block reported terminal while a competent
  authority sits unconsulted.
- **[AUT-11]** **The response-sink bar.** Where tool credentials are
  broader than the end user's own read rights — an agent running on
  service-account credentials — the run's audience can exclude the user,
  and showing content to the user is then itself a release. No ruling
  issued by the end user may cover any requirement gap of that release,
  whatever mandate the user otherwise holds. Where tool credentials equal
  the user's rights, showing the user what its tools fetched releases
  nothing the user could not fetch alone, and the bar is vacuous.

## 7. Rulings — `RUL`

- **[RUL-1]** A ruling admits a dispatch despite a requirement gap and MUST
  NOT edit the trajectory. The trajectory changes only through what the
  admitted call commits — its `delta` and its `emits`.
- **[RUL-2]** A ruling MUST NOT substitute for the agent's acceptance of a
  narrowing, and an acceptance clears no requirement. The two gates compose
  independently.
- **[RUL-3]** Every ruling is **call-scoped**: it covers exactly the
  engine-rendered call it names — tool plus resolved arguments, bound by the
  canonical digest — for one dispatch.
- **[RUL-4]** A call dispatches iff every requirement gap is covered by the
  rulings that admit it, each issuer's mandate covering what it admitted,
  all rulings binding the same rendered call and consumed together in one
  atomic step.
- **[RUL-5]** **Atomic plan execution.** Executing a ruling-carrying plan is
  one indivisible step on the suspended branch: the engine renders the call,
  puts the staged review to the authority, and on approval dispatches. The
  plan id, the ruling carrying its review, and the dispatch land in the log
  together. Consequently nothing can intervene between approval and
  dispatch, an approval cannot cover a swapped call, an approval cannot be
  replayed, and the decision trail is reconstructible from the log alone.
[review] we need to finalize the branch vs atomic plan situation. it is blurry now 
- **[RUL-6]** An acceptance is **informed**: the agent must have seen the
  offer before accepting it. A plan carrying an acceptance therefore MUST
  execute in a later round than the one that surfaced its offer; an
  acceptance written in the same assistant response that triggered the
  block was authored before the offer existed and MUST be refused. Plans
  carrying only rulings are not gated this way — the deciding authority
  saw the staged review either way.
- **[RUL-7]** Authorities MUST rule on the engine-rendered call plus
  provenance, never on the agent's paraphrase. Concretely, an approval UI
  or resolver payload presents the tool name and resolved arguments as the
  engine will dispatch them, with each referenced value's label and
  origin; the agent's own account of what it is doing — the sentence a
  confused agent writes under injected instructions — never reaches the
  authority as the thing to approve.
- **[RUL-8]** **The staged review.** What crosses to the authority is the
  call's identity — tool plus canonical digest, binding the resolved
  arguments — and its typed context: the trajectory label fold at review
  time, each referenced value's label and provenance, and the gaps. The
  context MUST be persisted verbatim on the resulting ruling, so the log
  replays the review rather than a hash of hidden state.
- **[RUL-9]** The staged review carries the rendered call's argument
  payload: an authority judging `send_email(text, recipient)` sees the
  text it is asked to release. Authorities sit in the deployer's trusted
  base per `THR-3`, so the review crossing to its authority is disclosure
  to a trusted judge, not a flow the algebra checks; a deployer unwilling
  to show an authority the bytes it judges should not register that
  authority over those calls. The recipients of a proposed release cross
  typed as the `Gap::Includes` subject of `RUL-8` in any case. The payload
  is persisted once: the ruling binds it by canonical digest, the
  dispatched call in the log carries the bytes, and the ruling's persisted
  context stays the typed context of `RUL-8`. *Deferred.*
- **[RUL-10]** No grant object appears in configuration or on any wire. The
  public vocabulary is mandates, rulings and log records.

## 8. Sanitizers and casts — `SAN`

- **[SAN-1]** A **sanitizer** is a registered transformer deriving a new
  value under the label its mandate authorizes. The raw source keeps its own
  label.
- **[SAN-2]** At **tool output**, the derivation is admitted by the context
  that would otherwise receive the raw value, and the raw value is withheld
  from it. That takes a host able to withhold — today the child-return
  crossing in a context-controlling deployment (`POS-5`), where the raw
  stays behind in the child and the derivation is what crosses to the
  parent.
- **[SAN-3]** At **tool input**, the derivation is substituted into the
  engine-rendered call, so the harness dispatches exactly the redacted
  bytes. The substituted call is checked with the derivation's declared
  label standing in for the raw argument's contribution; the trajectory
  label is untouched and the application is logged as a governance event.
  This protects the sink and cannot un-leak the context. *Design
  direction*: tool-input application is not implemented yet. Until it is,
  the loader MUST refuse a configuration registering `on = ["tool_input"]`
  rather than accept a sanitizer it would never apply.
- **[SAN-4]** A sanitizer's mandate binds the one transition it may claim,
  declared on one dimension as a `from` and a `to`. The raw value MUST satisfy
  the transition's `from` before the `to` applies. The `to` is fixed at
  registration: a sanitizer does not decide its derivation's label per
  value, as a resolver-implemented cast does under `SAN-8`, so the declared
  `to` is the transition's own ceiling. Trust and audience are bound on the
  same terms. The undeclared dimension is untouched: the derivation
  carries the raw value's label on it unchanged.
- **[SAN-5]** A mandate binds a transition, not the information it is
  claimed over. Scoping mandates by information type is open work.
- **[SAN-6]** Registering a sanitizer vouches for its implementation. It
  verifies nothing about its output.

Implementations are `builtin` or `resolver` per `CFG-15`. The builtins to
expect are the boring ones: a scrubber that drops API keys and tokens from
a fetched body, an email-address redactor. Registering either kind is the
vouching act of `SAN-6`.

### 8.1 Casts

- **[SAN-7]** A **cast** resolves an Unknown dimension to a concrete state.
  It is either **constant** or **resolver-implemented**, never both.
- **[SAN-8]** A resolver-implemented cast MUST declare the set of states it
  may cast to. The ceiling keeps a sloppy or compromised classifier from
  becoming a laundering endpoint.


## 9. Effects and history — `LOG`

- **[LOG-1]** Everything historical lives in one append-only log. Appends
  MUST NOT be gated by any check.
- **[LOG-2]** **Effects** are declared by contracts as `emits` and appended
  when the call succeeds — one append point. A call that dispatched and
  failed appends nothing.
- **[LOG-3]** **Governance events** are authority rulings and denials, the
  dispatches that consume rulings, acceptances, sanitizer applications,
  casts, and boundary events.
- **[LOG-4]** A **boundary event** is a mark the engine appends at the end
  of each assistant turn, at fork, and at merge. It never gates a flow
  itself; it lets later reads — offer expiry per `RMD-8`, the informed
  acceptance of `RUL-6`, audit — tell which events fell inside which turn
  and branch.
- **[LOG-5]** The log is consulted in exactly four ways: history
  requirements, ruling validity, **lifecycle validity** — whether a dispatch
  is still open, whether a child has already returned — and audit. Every
  other read is a projection rather than a consultation: the label and the
  model-visible transcript are views of the log per `IMP-2`, the log's own
  state in another shape. Lifecycle state is recomputable from the log
  exactly as a view is; it sits on the consultation side because its reads
  refuse admissions, and a projection never gates anything.
- **[LOG-6]** Every history check is **kind-containment only**. `prior(k)`
  and `no_prior(k)` ask whether a matching effect exists, never how many or
  how large.
- **[LOG-7]** The engine MUST NOT keep a magnitude view or feed one to any
  authority. Counting and summing are outside the model.
- **[LOG-8]** Summaries such as the set of effect kinds seen so far are
  views computed from the log and cached. They MUST NOT become independent
  state.
- **[LOG-9]** The host MUST serialize appends across concurrent branches and
  make them durable. A history check is only as sound as the log it has
  seen.

- **[LOG-10]** The effect vocabulary is deployment configuration. A
  deployment MAY encode a bespoke gating ritual as an effect plus a dynamic
  authority. An authority whose decision needs an accumulated magnitude
  keeps that account in its own systems.

## 10. Branching — `BRN`

- **[BRN-1]** The core trajectory is linear. Branching is a host capability
  requiring `POS-5`, because the snapshot of `BRN-4` and the single return
  channel are both bounds on a child's context. A host that branches MUST
  implement the rules of this section in full, or MUST NOT let branch
  results cross back. Quarantined branches (§10.1) additionally require
  confinement, since they withhold bytes.
- **[BRN-2]** **Fork.** The child starts at the parent's *current* label,
  never at the neutral starting label.
- **[BRN-3]** The child appends to the same shared log; the parent's history
  is its prefix. A fork appends a boundary event.
- **[BRN-4]** The model-visible handoff is a snapshot: the child receives
  every completed ancestor message through the fork plus its child task. It
  receives neither an ancestor's incomplete tool-call round, later ancestor
  activity, nor sibling activity. Nested children inherit the corresponding
  completed prefix from each ancestor. `submit_result` is the only channel
  carrying child-derived data back, and a host MUST NOT open another: the
  snapshot bounds what enters a child, the single return bounds what leaves
  it, and without both the merge rules govern nothing.
- **[BRN-5]** **Merge.** The returned value's label folds into the parent
  like any other read. Nothing the child did can widen the parent, since
  intersection cannot add readers.
- **[BRN-6]** History needs no merging. There is one shared log to which
  every branch appends in realtime, so a child's `egress` fails a parent's
  `no_prior(egress)`. There is no branch-scoped `prior(k)`.
- **[BRN-7]** A ruling issued in a branch is a record and not a token. It
  was consumed inside its own atomic plan execution, so its presence in the
  shared log gives the parent nothing to reuse.
- **[BRN-8]** A child returns **at most once**. The first value crossing
  consumes the return channel, and a later `submit_result` MUST be refused.
- **[BRN-9]** A **void return** — `submit_result` with no value — crosses no
  value, and so contributes nothing to the parent's label. The branch's own
  effects and governance events are in the shared log as any branch's are;
  what a void withholds is a label contribution, not a trace. It MUST NOT
  consume the return channel: at-most-once binds value crossings, not
  endings.
- **[BRN-10]** Finalization is trivial for every started branch whatever its
  fate. Nothing was withheld, so nothing can be lost; a dead branch means no
  value crossed.
- **[BRN-11]** A raw return that would **narrow** the parent MUST soft-block
  at the merge exactly like a narrowing call, carrying **return plans**.
- **[BRN-12]** Return plans are: the acceptance plan, always; and per
  applicable registered output sanitizer, either the sanitizer alone where
  its relabel fully clears the narrowing, or the sanitizer composed with
  acceptance of exactly the residual narrowing. A sanitizer whose relabel
  changes nothing about the merged outcome MUST NOT be offered.
- **[BRN-13]** A narrowing on a dimension that no applicable sanitizer's
  mandate transitions crosses only by acceptance.
- **[BRN-14]** A return whose label has an Unknown dimension resolves
  before it merges, per `CHK-16`: the runtime attempts the registered
  casts, and what cannot be established blocks the merge with the values
  named `unestablished` and no plans offered.
- **[BRN-15]** A policy-bound `return_sanitizer` crosses every return
  unconditionally and is not part of the plan choice.
- **[BRN-16]** A raw return that narrows nothing merges without a block.

### 10.1 Structured quarantined branches

**Design direction.** A sanitizer's transition is claimed over the bytes it
derives. The quarantine-exit **attestation** claims something else — that
extracted structure stands for what it was extracted from — and no
registered kind makes that claim today.

A child handles suspicious content and returns through `submit_result` with
a pre-declared structured output.

- **[BRN-17]** Schema validation alone MUST NOT raise a label: structure is
  not provenance. The raise is claimed by a mandated transformer and only
  within its mandate — one covering the specific attestation, not a mere
  parser.

## 11. Unknown — `UNK`

- **[UNK-1]** Both dimensions support **Unknown**, meaning "this label has
  not been established yet". Unknown is not a rank: `trusted < unknown <
  suspicious` does not exist.
- **[UNK-2]** Unknown is absorbing under the fold. One Unknown value makes
  the run's dimension Unknown.
- **[UNK-3]** A requirement that **consumes** an Unknown dimension MUST NOT
  pass. Resolution is attempted first per `CHK-16`; what remains
  unestablished is reported by value in the block's `unestablished` slot,
  never as a blanket Unknown result or a bare failure verdict: an
  unestablished dimension is a missing fact, and the report names it.
- **[UNK-4]** A call whose requirements consume no Unknown dimension
  proceeds. An Unknown run fails closed at the sinks that care and nowhere
  else.
- **[UNK-5]** A tool listed with **no `delta` key at all** is unannotated:
  its results are admitted at Unknown in both dimensions.
- **[UNK-6]** Within a *declared* delta, an omitted dimension contributes
  the fold identity. `delta = {}` is the explicit "this result carries
  nothing" annotation.
- **[UNK-7]** An unannotated tool MUST NOT declare label requirements, and
  the combination MUST be refused at load. Its unestablished contribution
  evaluates as identity at check time, so its own consequence could outrun
  its requirement. History and attention requirements compose fine.
- **[UNK-8]** A registered cast fills the dimension in, per `SAN-7`.

### 11.1 Pending-cast admission

- **[UNK-9]** A resolution whose admission strictly narrows the live label
  MUST NOT admit on its own. The narrowing offered is the **whole**
  admission's fold move against the live label, established dimensions
  included, because the fold may have moved since the pre-dispatch check.
- **[UNK-10]** The raw result stays confined while the offer is open.
  Acceptance lands the cast record, the acceptance, and the admitted value
  in one atomic batch.
- **[UNK-11]** The call's effects MUST append the moment success is
  observed, independent of the offer's fate, so a later call's history check
  sees them while the result stays confined.
- **[UNK-12]** An offer the turn ends without accepting **lapses**: the
  dispatch closes successfully, effects standing and nothing admitted. The
  close and the unaccepted resolution MUST be durable records.
- **[UNK-13]** An acceptance here is informed, per `RUL-6`.
- **[UNK-14]** A resolution that moves nothing admits directly. Acceptance
  is owed for narrowings, not for casts.

## 12. Configuration surface — `CFG`

A draft dialect, but authoritative: every configuration example in these
documents is written in it.

```toml
version = 1

[[tool]]
name  = "fetch_ticket"
tags  = ["finance"]
delta = { trust = "suspicious", audience = { exactly = ["finance"] } }

[[tool]]
name  = "scan_inbox"
delta = { trust = "unknown" }   # pending-cast; "unknown" is reserved, never a rank
                                # name, and at most one dimension

[child]
return_sanitizer = "pii-redactor"

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
# builtin = "hitl"                 # same authority, human elicitation
# builtin = "approve"              # in-process auto-approval

[[sanitizer]]
name = "pii-redactor"
on   = ["tool_output"]

[sanitizer.mandate]                       # one transition, keyed by dimension
audience = { from = { includes = ["finance"] }, to = { exactly = ["public"] } }
# trust  = { from = "suspicious", to = "trusted" }   # same terms, other dimension

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

Whatever surface ships MUST keep:

- **[CFG-6]** **Mandates only.** No grant object in configuration or on any
  wire.
- **[CFG-7]** **The no-empty-mandate rule** of `AUT-6`.
- **[CFG-8]** **Explicit set relations.** Every audience mention carries its
  operator — `includes`, `exactly`, `cap`, `may_add` — because a bare list
  is ambiguous between narrow and wide. A list without its operator is a
  load error.
- **[CFG-9]** **Scope routed by tags only**, per `AUT-7`.
- **[CFG-10]** **Casts declared constant xor resolver-implemented**, per
  `SAN-7`.
- **[CFG-11]** **Surface names map onto model terms.** `effects = [...]`
  declares `emits`; `effects.has` and `effects.has_no` are `prior(k)` and
  `no_prior(k)`; `attention` marks are the per-call demands of `CHK-13`.
- **[CFG-12]** **Mandate powers name the currency they act on** and nothing
  else. Tool and authority never name each other.
- **[CFG-13]** Block messages MUST surface the applicable remedy plans,
  naming the eligible authorities where a plan carries a ruling.
- **[CFG-15]** **Implementations are `builtin` or `resolver`**, a closed
  set: in-process, or dynamic behind a registered endpoint. A sanitizer
  declares where it may apply (`on`) and its transition (`mandate`). HITL
  is the reserved builtin `"hitl"` — the harness hosts the elicitation,
  and no channel concept exists. A mandate's powers do not depend on the
  implementation behind them: wiring `builtin = "approve"` to a covering
  mandate is an open gate the deployer chose, legitimate per `THR-3` and
  visible in review.
- **[CFG-16]** At most **one dimension** may be declared pending-cast
  (`delta = { trust = "unknown" }`), and a `requires` on that same dimension
  is a load error — the requirement would evaluate before the resolution
  that establishes it. `"unknown"` is reserved, so a trust rank of that name
  is refused.
- **[CFG-17]** A `[child] return_sanitizer` binding is validated at load:
  the named sanitizer MUST exist and MUST carry the `tool_output` point.

Contract language leads with `requires` as a surface convention; a delta
reads best as a stated consequence. Source deltas are derivable, so a
dynamic resolver mapping a document to its ACL's reader set can
auto-generate `audience ∩ readers(doc)`.

## 13. External interfaces — `EXT`

**Placeholder.** The wire protocols between the engine and its registered
externals are named here and specified nowhere. A third party implementing
an authority service, a classifier, or a directory lookup has nothing to
build against today, and closing that is a prerequisite for calling APPA an
open standard.

The interfaces that need specifying:

| interface | carries | today |
|---|---|---|
| authority resolver | a staged review (`RUL-8`), returns a ruling or an abstention | `url` + `timeout_ms` in config; payload unspecified |
| HITL elicitation | the same staged review, through human elicitation | `builtin = "hitl"`; transport unspecified |
| cast resolver | a value's identity and provenance, returns a state within `may_cast` | `url` + `timeout_ms`; payload unspecified |
| sanitizer resolver | a value, returns a derivation under the declared transition | unspecified |
| membership resolver | a recipient or group, returns a reader set | design direction; see `LBL` |

Each needs a request schema, a response schema, a versioning rule, and a
timeout. Failure semantics are already fixed:

- **[EXT-1]** An external that times out, answers with an error, or answers
  malformed MUST contribute no decision: an abstention for an authority, a
  failed derivation for a sanitizer, an unestablished dimension for a cast. The
  block or the Unknown stands. No external failure may be read as an
  approval, and an unreachable authority MUST be indistinguishable in effect
  from one that abstained.

## 14. Implementation shape

The engine is two layers.

- **[IMP-1]** The **inner layer** is the pure decision core —
  `check(state, call) → verdict` and `apply(state, call) → state'` — with no
  IO and no clock. It is semantically a function of the full event log, so
  every decision is replayable from the log alone.
- **[IMP-2]** In practice the wire contract passes the log's cached views —
  the label, the seen-effect-kinds set, pending-plan records, boundary
  positions — rather than the raw log. This is sound because every view is
  recomputable by replay.
- **[IMP-3]** The **outer layer** owns state: durable append with pluggable
  destinations, serialization, and the durability obligations of `LOG-9`. A
  harness author embeds the outer layer with whatever store they already
  run; the decision core never sees IO.
- **[IMP-4]** External labels, authority decisions, sanitizers and dynamic
  resolvers are trusted inputs and together form the trusted base.
  Invariants on state changes MUST be enforced structurally: by the
  implementation language's type system where it can express them, and
  otherwise by refusal at one admission choke point. Enforcement scattered
  across call sites satisfies neither branch.
- **[IMP-5]** The checker MUST stay free of ad-hoc conditionals. Registered
  contracts and authorities are the only sources of a decision, every
  decision reduces to label arithmetic or a log query, and anything
  imperative — an approval flow, a model that vets content, a lookup
  resolving a recipient to readers — lives in a registered external and
  never in the engine.

### 14.1 Accepted gaps

The invoke/append crash gap is out of scope in this version: effects append
when the call succeeds, and a host failing between a successful invoke and
the append may lose effects. Its confined-result cousin is accepted on the
same terms. Hardening — a durable outbox committing invocation and effects
as one record — is future work for the outer layer.

`rationale.md` lists these with their reasoning, alongside the failed-send
egress window.

## 15. Threat model

- **[THR-1]** The agent is **benign but confusable**: steerable by injected
  instructions, but not itself adversarial. A malicious model constructing
  covert channels is out of scope.
- **[THR-2]** Malice enters exclusively through content labeled suspicious,
  or Unknown until resolved. Content labeled trusted is trusted by
  definition, and APPA offers no protection when a trusted source is
  malicious.
- **[THR-3]** Authorities, sanitizers, casts, their dynamic resolvers, and
  the configuration are the deployer's trusted base. A permissive
  configuration is legitimate and voids the corresponding guarantees
  explicitly and auditably.
- **[THR-4]** APPA assumes a serialized, durable event log per `LOG-9`.
- **[THR-5]** Approval UX and the bootstrapping of contract coverage are
  adoption concerns and out of scope here. APPA is exactly as good as the
  authorities and contracts registered into it.
- **[THR-6]** External identity machinery — OAuth, SAML, the directory that
  says who sits behind an address — is outside APPA, which trusts what it
  returns. A reader id is an opaque atom to the algebra; establishing that
  the atom names the right person is the deployment's job.
