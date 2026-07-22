# CLAUDE.md

OpenAPPA (formerly **Baton**; APPA = Agentic Permissions Policy Algebra)
is a value-granular information-flow policy engine for LLM agents. It sits
between the agent and its tools/inference and answers one question before
every proposed flow: *can this value, derived from these sources, legally flow
into this sink?* It is declarative and algebraic — no guardrails, no prompt
filtering, no bespoke `if`s; any imperative judgment lives in registered
external authorities and transformers, never in the engine.

## Naming

- Use the `appa` prefix for new OpenAPPA-owned crates, binaries, environment
  variables, and protocol identifiers. Existing unprefixed names are
  deliberate, not violations: core's internal module names (`engine`, `plan`,
  `turn`, …), `DOJO_MODEL`, the demo-owned `notify-mcp`, the reserved
  `assistant.response` sink. Never introduce new `baton`-named
  identifiers: `baton` was the earlier name and survives only in stale spots
  (e.g. `website/content/docs/agentdojo.md` still says `baton-dojo` where the
  package is `appa-dojo`).
- "Engine", "Trajectory", "Value", "Label", "Dimension", "Authority",
  "Transformer", "Remedy plan" are defined terms — use them as the glossary in
  `docs/spec.md` and `core/src/lib.rs` define them, not colloquially.
- **Agentic terminology first, IFC/security names as anchors.** In comments,
  docs, and identifiers, lead with the agentic vocabulary — *trajectory* (not
  execution trace / session history), *flow* (not information transfer /
  operation), *turn*, *tool call*, *emission*, *actor/agent*, *harness* — and
  reference the classical IFC or security term alongside where it grounds the
  concept: "the flow's label (the taint fold)", "the trajectory (the agent
  run's append-only history)", "a sink's requirements (sink-side adequacy)",
  "declassification via a registered transformer". The IFC lineage
  (Sabelfeld/Myers, taint, sink, label, noninterference, declassification) is
  the anchor readers map onto — cite it, but never let it displace the
  agentic term as the primary name for a concept that has one.
- Do not invent new terms, especially when working with spec. Try to use 
  existing definitions. If you want to introduce a new one - ask a user and 
  explain why.

## Document precedence

1. `docs/spec.md` — the specification draft: the two-monoid model as the
   product, written for engineers. Takes priority over implementation details
   where they conflict.
2. `docs/paper.md` — the paper skeleton: the same model with its formal
   claims, theorem scoping, and citation anchors.

Spec vs paper: the spec is the human-readable account, the paper the
academic one; both are skeletons with placeholders today (the spec's
interfaces need substantial work, the paper's prose is still slop). Develop
them together and keep them in sync — the paper may be a *subset* of the
spec in essence: corner cases that don't fit the paper elegantly still must
land in the spec, and nothing in the paper may contradict it.
3. `core/src/lib.rs` — concepts and semantics of the engine as implemented.
4. The "Core engine invariants" subsections of "Gotchas" below — the
   invariants a core edit must not silently break. **Read them before
   touching `core/`.**

The "Mental model" and "Gotchas" sections below describe the engine as
implemented; where they diverge from `docs/spec.md`, the spec is design
direction and the sections below remain the reference for editing `core/`
today.

## Workspace map

Cargo workspace members: `core`, `check`, `contracts`, `dojo`, `edge`, `proxy`.
Workspace lint: `dead_code = "deny"` — unused code fails the build.

- `core/` — `appa-core`, the engine. Prototype, `publish = false`, edition
  2024. Runtime deps are only `tracing` (facade), `serde` (derive), and
  `thiserror`; `tracing-subscriber`, `criterion`, `proptest`, `clap` are
  dev-only. Everything else is an integration around it.
- `check/` — `appa-check`, a stateless JSON oracle over appa-core: one request
  (contracts + episode so far + proposed call) on stdin, one decision on
  stdout. Used as a subprocess by the Python harness. Its wire format is
  deliberately narrower than the engine: no audience, no caller-configurable
  authorities — trust, a limited effects surface, and fixed internal
  authorities implementing the legacy `unknown_policy` knob
  (`deny` / `allow_with_audit` / `escalate`).
- `contracts/` — `appa-contracts`, translates the declarative TOML policy
  dialect (`docs/contracts.md`) into appa-core `ToolContract`s. Shared by the
  proxy and the gateway demo — one canonical dialect, don't fork it.
- `edge/` — `appa-edge`, the session/mediation layer the proxy drives: it
  owns the engine, replays admitted context into a `Trajectory`, settles
  each proposed call through `pursue`/`apply_approval` (webhook-resolved
  external rulings), and reads the audit projection for its decision log.
  Trimming core's API must account for edge as a first-class consumer.
- `proxy/` — `appa-proxy`, the **inference-layer** integration: an
  OpenAI-compatible HTTP proxy over appa-edge. Stateless — on every
  `/v1/chat/completions` response it replays the supported conversation
  history into a fresh `Trajectory` (system/developer roles and
  uncontracted tool results are skipped) and rewrites blocked tool calls
  into stop explanations before the harness sees them.
- `dojo/` — `appa-dojo`, a Rust-native AgentDojo-style benchmark substrate
  with the engine linked in-process (full audience/effects/authority access,
  unlike the appa-check wire format). Scenario catalog in `src/scenarios/`,
  one file per case.
- `harness-agentdojo/` — Python (uv) harness running the real AgentDojo
  benchmark with appa-check as the tool-call-veto defense. Contracts as data
  in `contracts/<suite>.toml`.
- `demo/gateway/` — `appa-demo`, the **tool-layer** gateway: an rmcp MCP
  server that owns a live trajectory per session, soft-blocks as ordinary
  tool results, escalates to a human via MCP elicitation, and dispatches only
  the canonical request the engine checked. **Deliberately outside the root
  workspace** (keeps heavy agent-framework deps out of the workspace build) —
  `cargo test --workspace` does not cover it; test it separately.
- `demo/kagent/` — appa-proxy as a kagent sidecar on kind, end-to-end
  prompt-injection demo.
- `website/` — Next.js (pnpm), not part of the cargo workspace. Content under
  `website/content/docs/` drifts from the crates — verify against crate
  READMEs before trusting it.

LLM-backed demos and benchmark runs (dojo, AgentDojo, the gateway and kagent
demos) need `OPENROUTER_API_KEY` (environment or repo-root `.env`);
`DOJO_MODEL` picks the dojo model. Core's criterion benches need no key.

## Mental model

- **Value-granular, causal.** A trajectory is an append-only log of scoped
  facts; values, effects, lifecycle, grants, and audit are projections over
  it. A flow is checked against `L_flow = combine(L_args, L_control)` — the
  fold of exactly the request's argument-tree leaves plus its control
  dependencies, **never the whole conversation**. A raw secret elsewhere in
  the trajectory does not taint an unrelated sink, but it taints everything
  derived from it, including the *choice* to act (implicit flows).
- **Propagation and checking are strictly separate operations**, per
  dimension. Propagation is the taint fold (`combine`): trust keeps the worst
  evidence, audience intersects reader sets, effects union. Checking is the
  adequacy relation at the sink (holds / fails(witness) / unprovable). Never
  describe the fold as "declassification" — declassification only ever
  happens through an explicit transformer or authority.
- **Binary outcomes, fail-closed.** Every well-formed flow — a tool call
  or an assistant emission, same pipeline — settles as **AllowedNow** (a
  linear permit) or **Blocked** with the exact failed predicates plus the
  frontier of predicted plans, where an **empty frontier is a proof** of
  unremediability (the search is uncapped, so terminal is never "nothing
  found within budget"; `terminal` then names the disposition). Stale,
  foreign, or conflicting proposals are refusals on a separate channel,
  outside the outcome, touching nothing.
- **Two remedy kinds only.** *Reduce* (derive a value through a registered
  transformer) and *Authorize* (an exact typed delta at an exact scope). The
  soft block is the product thesis: it forces the actor to choose — preserve
  outer-world capability or enter a restricted context — *before* fetching
  data ("shift the reasoning left").
- **Remedy-set soundness is the security boundary.** Every plan the engine
  returns must be individually sound, so which plan a (possibly
  suspicious-tainted) actor picks is security-irrelevant. Selection immunity
  comes from plan soundness, not from policing the selector — this is what
  lets the tainted planner keep driving.
- **Authorities rule on engine-supplied typed facts, never on the actor's
  paraphrase alone.** A `PendingApproval` carries the exact authorization
  targets with labels and the transitive provenance closure — never bytes;
  when the gateway quotes the model's escalation reason, it is explicitly
  marked *unverified*. A tainted model summarizing "may I email the
  compliance archive?" while omitting that the address is attacker-derived is
  the social-engineering channel this closes. Dispatch completes the
  guarantee: release renders the one canonical request from the exact checked
  tree, so nothing drifts between what was ruled on and what runs — the model
  never re-issues an approved call.
- **Registration is a trust decision, not verification.** A transformer wears
  a declared transition bound at registration (a mandate); audit wording is
  "admitted under the transition declared by registered transformer X", never
  "verified as clean". Content robustness belongs to the harness/authority,
  not the engine.
- **Unknown is a first-class label, fail-closed — in core, no policy knob.**
  The NaN metaphor: an unprovable flow is never accepted implicitly and
  routes through the same authority chain as a breach; clearing it is an
  explicit, audited `acknowledge_unknown` (the `fillna`). Annotate the risky
  few tools, leave the rest unknown, still catch the obvious flows.
  (`appa-check`'s `unknown_policy` knob — `deny` / `allow_with_audit` /
  `escalate` — is legacy integration-level configuration implemented as fixed
  authorities over the same machinery, not an engine exception.)

## Gotchas

### Algebra and terminology traps

- **Two orders on the same dimension — never conflate them.** The taint fold
  (`core/src/dimension.rs::combine`, `ValueLabel::combine`) is per dimension
  a commutative, idempotent semilattice where `Unknown` has a *definite*
  position (absorbing for audience/effects; between Trusted and Suspicious
  for trust). The adequacy relation (`covers` / `at_least` / `avoids`,
  returning `Adequacy<W>`: `Holds` / `Fails(witness)` / `Unprovable`) is the
  sink-side proof — there `Unknown` is **incomparable / bottom →
  `Unprovable`**; trust is the only dimension where the two orders disagree
  on `Unknown`. The operation is `combine`; do not call it a join.
  `widening_over` is a third *derived* relation (the dual of adequacy, on
  trust and audience only), not a third order: it powers the no-widening
  invariant, which those two dimensions enforce at admission by construction
  (the conservative fold absorbs a wider declaration —
  `debug_assert`-guarded, test-pinned). Effects are trajectory state:
  recorded at release, consulted by `forbid_prior_effects` — applied, never
  checked for growth (the v2 alignment removed the effects-acquisition
  gate). `Requirements::check_flow` is a thin
  *ordered* composition over the adequacy relations — the emission order
  (trust, audience, attention, effects) is observable; preserve it (there is
  a typed-order test).
- **Audience folds by intersection, not union** — a deliberate deviation from
  early notes (union would make the sink check vacuous; see
  `dimension::Audience`). Declassification (growing the reader set) is only
  ever an explicit authority act, never a fold outcome.
- **Audience models bounded reader identities, not destinations.** A fixed
  reader list or a `"$.args.<argument>"` extraction gives the sink whatever
  identities that argument carries — meaningful only when those identities
  bound the actual readers. Never model an arbitrary destination (a free-form
  URL, an open channel) as a reader set — nobody can bound who reads it; that
  sink is `requires = { audience = "public" }`.
- **"More restrictive" is not "unsafe" — and the cost lands later.**
  Committing a narrower label voluntarily shrinks the trajectory's future
  action space; that is the vision's state-acquisition concern, and it looks
  backwards under textbook IFC. In current core the restrictive pure read
  itself is `AllowedNow` — the narrowing binds when a *dependent* flow later
  hits a wider sink. No acquisition gate exists in current core (the
  effects-growth soft-ban was removed with the v2 alignment); the v2
  label-descent pre-acquisition gate is vision-ahead-of-code (see below),
  not current behavior.
- **Attention is a requirement, not a label dimension.** An explicit
  confirmation demand (`AttentionRule::ExplicitConfirmation`) is satisfiable
  ONLY by a competent authority's check-scoped stand-in ruling
  (`confirms = true` in its mandate; generic routing, not yet v2's named
  `confirmed_by(A)`), and fails closed with no competent authority. The
  structural confirming-user-turn path was removed in the v2 alignment. It
  was never built as a dimension — do not add one.

### Core engine invariants: values, flows, admission

- Values are **immutable**: body, label, and provenance fixed at admission. A
  transformer derives a *new* value; nothing mutates or relabels a source.
  Durable authority raises mint a new `Provenance::Endorsed` value via the
  raise helpers, never `combine`.
- Checks fold **exactly a flow's dependencies**:
  `L_flow = combine(L_args, L_control)` from the request's argument-tree
  leaves plus its mandatory control set — never the whole trajectory.
  Requests carry control *dependency sets*, never a caller-supplied control
  label (that would be a relabeling hole).
- **Admission is engine-owned.** `Trajectory::ingress` is the only
  caller-labeled path (the explicit trust boundary). Model outputs fold their
  mandatory read+control sets; tool outputs fold
  `combine(intrinsic, args, control)` where the contract's intrinsic label can
  only worsen the fold; only a validated transformer admission may sit below
  the conservative fold, and only under its *declared* output label.
  Caller-labeled assistant ingress does not typecheck — the response is a
  mediated emission sink like any tool. `ValueStore` mutators stay
  `pub(crate)` — never add a public `insert(bytes, label)`.
- Effects are **monotone trajectory state**, committed at release (a
  may-effect commitment fact: failure appends and removes nothing); the
  committed past is a projection over commitment facts. Labeling is
  transactional: a tool call that dispatched has its label in the trajectory
  forever — no post-hoc sanitizing can clear the log; facts only grow. Audit
  is **control-plane history** (`AuditEvent`), never a label field; failed
  transitions audit an event and create no value or action.

### Core engine invariants: the event log, revisions, linear capabilities

- The `EventSet` is the authoritative state: every public `Trajectory`
  mutation prevalidates, then appends **one atomic batch** of facts;
  lifecycle contradictions (double release, completion-before-release) are
  refused at admission — the single enforcement point. Authorization replay
  is NOT an admission concern: no stored grant exists, so double use is
  prevented entirely by the linear capabilities.
- **One build path.** Every derived read model — labels, provenance, turns,
  committed effects, audit, both pending slots — is a `TrajectoryProjection`
  of the log, rebuilt in full by `Trajectory::commit` after each batch. Never
  add a second, incremental fold over `Fact` (that is what this design
  deleted: a parallel `apply` plus a parity suite to police it, whose state
  half was tautological — it rebuilt with the same `apply` it was checking).
  Full reprojection per mutation is deliberate. Its cost is O(dependency
  edges), not O(events): `value_labels` refolds every historical value's whole
  dependency set, so a trajectory whose values cite many predecessors is cubic
  over its life, not quadratic (the old admission-time fold paid each value's
  fold once). If that ever matters, make the *one* path incremental, never add
  a second. The `ValueStore` holds **bodies only**: a label lives in the
  projection, and `ValueRef` composes the two for reading.
- `Revision` digests the event frontier; every appended batch advances it.
  Plans live in a side cache bound to their basis and append nothing — the
  per-evaluation `CheckPerformed` fact is what preserves cross-evaluate
  staling.
- Capabilities — `ExecutionToken`, `DispatchReceipt`, `StepCapability`,
  `PendingApproval` — are **non-`Clone`, `Serialize`-only, no public
  constructor**, spent on use. All but the receipt bind trajectory + revision
  (+ action/plan/step); a `DispatchReceipt` is deliberately lifecycle-bound
  instead (trajectory + action in Released phase) — it records a dispatch
  that already happened, so unrelated later mutations must not wedge the
  action. Plans, step capabilities, and pending approvals additionally bind the
  `EngineId` whose registries produced them — a capability never resolves
  against another engine's registries. Never add `Deserialize`: deserializing
  one forges the linearity. `Trajectory` itself is not serde at all.
- Two-phase dispatch: `release` commits may-effects, renders the **one**
  canonical request from the exact checked tree, and mints the receipt; `record_output`/`record_failure` consume the
  receipt and close the action. There is deliberately no one-call shortcut
  that skips the canonical request — do not add one. Binding failures
  (stale/foreign) refuse *without* touching state; the capability is consumed
  either way. Receipts are lifecycle-bound, not revision-bound: a receipt
  closes a dispatch that already happened, so unrelated mutations after
  release (a checked emission, a new value) never wedge the released action —
  only foreign, wrong-action, or already-closed receipts refuse. Tokens,
  step capabilities, and approvals authorize *future* changes and stay
  revision-bound. The pending action's proposed effects are the single
  source of truth for what release commits.
- A confirmation stand-in is check-transient like every lift: one ruling
  admits exactly one dispatch, and a repeat proposal demands a fresh ruling
  — nothing stored can be replayed. A one-off (`PolicyCheck`-scoped)
  authorization lands as a single `AuthorizationApplied` fact — approval and
  application coincide in one batch, so no stored grant object exists
  between them to consume twice; replay and double-use protection live
  entirely on the linear capabilities.

### Core engine invariants: pending action, plans, remedies

- At most one `PendingAction`; it keeps the **immutable original** proposal
  (identity basis for idempotent re-entry) and the **current** reduced form
  (what is checked and dispatched). A different proposal while one is
  pending is refused, never queued. Terminal blocks (empty plan frontier)
  clear the slot; remediable blocks (non-empty frontier) keep it.
- Every checked flow — a tool dispatch or an assistant emission — settles in
  one binary `FlowOutcome`: `AllowedNow(permit)` or `Blocked { violations,
  plans, terminal }` where `plans.is_empty() == terminal.is_some()` is an
  engine-construction invariant (blocks are built only by the
  remediable/terminal helpers, never assembled field-by-field). Invalid,
  stale, foreign, or conflicting proposals are `FlowRefusal`s on a separate
  channel, outside the outcome, touching no state. The two pending slots (action, emission) are
  independent and per-kind single-slot; a blocked emission never clears a
  pending action. Plans are predictions, not permits: plain serializable
  data, revision-bound, recomputed after every applied step; only the head
  step is executable; each step is a `PlannedRemedy` (the remedy plus its
  competent routes and the violations the authority is shown), and applying
  any remedy triggers the full re-evaluation as an execution invariant, never
  a plan-step object.
- The two-kind remedy vocabulary enforces conservation laws. **Reduce**
  answers to registered reduction relations: a value derivation
  (`ReductionTarget::DeriveValue`) cannot touch actions or past effects and
  wears its transformer's declared output label; its registered relation
  (the declared label precondition) is the one gate — the planner filters
  candidates with it and the applier rechecks it live against the current
  registries, so a planner/applier disagreement returns
  `TransitionFailure::ReductionRefused` on a step the planner promised.
  (Registered action-narrowing transitions were removed in the v2
  alignment; the v2 remedy vocabulary is sanitizer derivations and
  compiled composites.)
  **Authorize** grants an exact `AuthorizationDelta` at an exact
  `AuthorizationScope`: a check-scoped lift (excepting a prior effect,
  standing in for a confirmation, releasing a control dep, acknowledging an
  unprovable fact) changes no stored state; a durable raise
  (`AuthorizationScope::DerivedValue`) mints a *new* value like a transform —
  the authority raises `source`'s label with the raise helpers
  (`raised_to`/`admitting`, never `combine`), and the new value carries the
  raised label under `Provenance::Endorsed`, the source untouched. So raising
  trust or audience is durable and scoped to the derived value, never a
  check-transient lift. An `Authorization` is proposal data, not a
  capability; a product delta carries every atomic coordinate it asks for,
  so `AuthorityMandate::authorizes` requires `acknowledge_unknown` to clear
  an unknown even when the lift coordinates alone are covered. Authority
  comes from competence routing + the fail-closed recheck
  (`PostconditionFailed`, or a re-evaluation that re-routes the residual,
  blocks rather than permitting an under-covered flow).
- Registration is an operator trust decision, not content correctness: audit
  wording says "admitted under the transition declared by registered
  transformer X", never "verified as clean". Registries are populated at
  construction, duplicates refused, never silently replaced. Authorities
  (`Authority { name, mandate, mode: Inline(fn) | External }`) share one
  registry and name space; a grant routes to competent authorities inline-first
  then external, each in registration order, and an inline abstention (`None`)
  falls through to the next competent authority. Routing is resolved **live at
  application** against the current registry (a minted plan no longer pins its
  authority), so the construction-time-only rule is load-bearing for *safety*,
  not merely determinism: registering an authority between minting a plan and
  applying its step would change which authority rules it. The rule is
  mechanical: the first evaluation freezes the registries, and any later
  registration is refused (`RegistryFrozen`).

### Layer differences (the same engine, different guarantees)

- Two enforcement profiles exist and they are not equal. A mediator that owns
  tool dispatch (the **gateway**, tool layer) has confinement in reach:
  labels come from the contracts of the dispatches that produced them and
  approvals are engine state — though today a permitted call's raw result
  still returns to the model (confined `run_tools`-style wrappers are future
  work, see below). A mediator that only observes inference traffic (the
  **proxy**) gets sink checking and taint propagation but can never keep a
  value out of context. Know which profile you are editing.
- **The gateway's provenance is conservative and prompt-blind**: it never
  sees the LLM's context, so every argument is assumed derived from every
  prior tool output, and anything that never entered through a mediated call
  (user and system prompts) is invisible — implicitly public and trusted. A
  secret pasted into the prompt is outside the policy at that layer;
  value-granular and prompt-aware reads need the inference layer.
- **Unregistered-tool defaults differ by integration — this is per-layer
  configuration, not engine behavior.** In appa-proxy and the contracts
  dialect, a tool with *no contract at all* passes through unevaluated
  ("annotate the risky few"). In the gateway demo, a catalog tool without a
  contract is served but unregistered — calling it is unprovable and
  fail-closed through the authority chain. Inside a present contract,
  omissions always fail closed: absent `requires` means *unknown
  requirements* (escalates), `requires = {}` means "considered, nothing
  required"; omitted output trust/audience is unknown; only `effects`
  defaults to none.
- The proxy is stateless (replay per request) and conservative at dependency
  discovery: it supplies its entire admitted context as each request's
  control set. Core's causal `L_flow` semantics are exact; the proxy's
  approximation of the dependency sets is the coarse upper bound at that
  layer. The gateway owns a live trajectory per MCP session with one pending
  action per session — a different call while one is soft-blocked abandons
  the blocked one, and a completed action's identical retry is a *new* action
  through policy.
- The gateway elicits a human **once per authority** a remedy routes to, and
  applies that ruling to every grant the same authority must rule on — never
  to another authority's. No ruling at all (timeout, dismissal) fails closed
  without recording a decision.

### Vision-ahead-of-code (do not invent ad hoc)

- **Branching / quarantined branches** (a child enters a restricted state
  without tainting the parent; only an explicitly labeled result crosses
  back; structured Dual-LLM-style quarantine with a typed `submit_result`)
  is load-bearing for the vision but is harness territory, not appa-core,
  and is not implemented yet.
- **Pass-by-reference labels** (a byte-identical, never-retyped argument
  keeping its own label instead of the actor fold) is an acknowledged future
  extension, not current semantics: today a value the actor authors after
  observing restricted data inherits the full causal fold.
- Confined composition (`run_tools`-style wrappers where a raw read never
  joins the agent-visible trajectory) is likewise future/harness work.
- **Pre-acquisition gating of restrictive reads** — soft-blocking the read
  that would narrow the trajectory's future action space *before* fetching,
  so the actor chooses a remedy early — is a vision thesis; core currently
  allows the read and prices the narrowing at dependent sinks.

## Validation (every pass)

```sh
cargo test --workspace \
  && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
(cd demo/gateway && cargo test --all-features \
  && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check)
cargo test -p appa-check --test cli
(cd harness-agentdojo && uv run pytest)
```

`demo/gateway` is outside the workspace — the first line does not cover it.

## Rust guidelines

**Spend the cleverness budget on the domain model, not the type machinery —
make invalid states unrepresentable with boring tools.** "Boring Rust"
constrains the mechanism vocabulary (no trait acrobatics, no `dyn`, no
type-level programming); type-first design constrains the data vocabulary
(invariants live in the shape of data). They compose: linearity in core is
built entirely from Rust's plainest features — no `Clone`, no `Deserialize`,
no public constructor, consumed by value. Ownership, visibility, and
*omitted* derives do the enforcement; no typestate generics anywhere.

Where an invariant should live:

- **Structural invariants → types, because the encoding is boring.** "Can't
  be cloned", "can't be empty" (`NonEmptyVec`), "can't pair this scope with
  that coordinate" (validated constructor), "can't be built by callers"
  (`pub(crate)`). Zero cleverness, removes whole test categories.
- **Temporal/stateful invariants → one runtime choke point, never
  typestate.** Lifecycle ordering (no double release, no
  completion-before-release) is refused at event admission — the single
  enforcement point — because encoding it as
  type-state would infect every signature with generics. This is a
  deliberate standing decision, not a gap.
- **The budget test: type-level enforcement is worth it only while it stays
  out of caller signatures.** The moment an invariant needs a type
  parameter, lifetime, or trait bound on the public API to express, prefer
  the runtime refusal at one choke point plus a proptest law.

Mechanics:

- Plain functions, concrete structs, enums for closed states, newtypes over
  primitives (no raw strings, boolean flags, or long positional lists);
  pattern matching over if-chains.
- No `dyn`/`Box` in engine state; no trait without at least two real
  implementations or a real boundary. Transformers are plain `fn` pointers
  (`TransformerFn`) beside a serializable descriptor — no capturing closures.
- Minimize the public API surface: a few coarse operations over many tiny
  exported helpers. In core, keep `ValueStore` mutators `pub(crate)` and
  never hoist read-only audit/projection types into the root re-exports.
- Treat all external input as untrusted; validate at public entry points and
  convert immediately to native types.
- `Result` with domain error enums (`thiserror`) in library code. No
  unchecked failure on recoverable paths or external input; a documented
  `expect` on an invariant already established by prevalidation is house
  style in core (the message names the invariant, e.g. "plans reference only
  registered transformers"). Free `unwrap` belongs in CLI entrypoints and
  tests only.
- Never hold a lock across `.await` — with one deliberate, documented
  exception: the gateway holds its per-session mutex across the
  human-elicitation await to serialize the session.
- Observability is `tracing` only (decision path at `debug!`, algebra at
  `trace!`), borrow-only and never behavior-changing; exporter wiring stays
  out of core (`appa-gateway -- -v`/`-vv` in `demo/gateway` selects the
  level).
- Public domain structs own their data; cloning small IDs/config is fine,
  cloning hot-path buffers is not.

## Testing conventions

- The algebra **laws** are real `proptest` properties
  (`core/src/test_strategies.rs`), not fixture loops.
- Core semantic tests assert typed values — never `Display` output, doc
  text, or prose; those pin wording, not behavior. Integration tests (the
  proxy's block explanations, gateway narration) may pin stable user-visible
  text when that text *is* the observable behavior.
- No mocks. The dojo/harness compare defended vs undefended runs of real
  models; appa-check tests drive the real binary.
