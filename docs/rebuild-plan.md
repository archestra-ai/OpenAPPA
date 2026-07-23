# Plan v8: OpenAPPA two-component rebuild (Engine + Runtime) — confining agent-executor

Status: DRAFT for approval — then autonomous execution.

**Sources of truth: `docs/spec.md` + the initial task prompt.** All Rust code was dropped in #41;
from-scratch build. CLAUDE.md's earlier-prototype references are **disregarded** (removed by the
user). Spec vocabulary governs: **mandates, rulings, log records**, atomic plan execution, no
*reusable grant object* on any wire.

**Architecture decision (this revision): the runtime is a *confining agent-executor*.** It has three
faces — **north** the OpenAI-compatible `/v1/chat/completions` wire to a thin harness; **upstream**
Rig → the model; **south** it *executes* tool calls against registered tool backends. The harness
sends a user turn; the runtime drives inference + tool execution + policy mediation, looping
internally until the model yields a final assistant message, which it returns. The harness sees only
user turns and final answers — **never tool calls or results** — and owns the outer loop (turns) and
branching (child sessions via `X-APPA-Parent-Session`). Because the runtime executes tools, it *can*
withhold a raw result and surface only a sanitized derivative **on the bound-sanitizer/cast paths**
(confining), which makes output sanitization and quarantined branches sound by construction. (This is
confinement of the model's *reads* on those paths and of *tool sinks* — not universal sanitization;
the model's free final answer is a response sink this version does not mediate — acceptance #7.)

Confirmed user decisions: `X-APPA-Session` (trajectory id, server-minted) + `X-APPA-Parent-Session`;
`docs/contracts.md` rewritten to the spec dialect (S10); crate names `appa-engine` + `appa-runtime`;
in-mem store (durability deferred); Cast is a runtime admission mechanism, not a remedy; Redispatch
and Fork are prose recommendations, not engine-executable; naming — log version = **`Revision`** (not
"Frontier"), **no `CapabilitySnapshot`** (the immutable engine `Registry` is the static capability),
**no `result_status`** (the runtime observes real tool success); remedy computation = **"remedy
planning"** (`RemedyPlanner`). DE-SCOPED to follow-up: atomic **compiled composites**, the quarantine
`submit_result` **trust-raise attestation**, durable store, executor-side model-context compaction.

---

## Intent contract

**Goal.** A pure **`appa-engine`** (no IO/clock; a function of the event log; emits validated fact
batches) + an **`appa-runtime`** confining agent-executor that owns the in-mem event log (source of
truth per trajectory id), executes tools, resolves calls, holds the external implementations, and
serves the OpenAI wire.

**Inputs / outputs.**
- In: TOML policy config (spec §"configuration surface"); OpenAI `/v1/chat/completions` requests
  (non-streaming); tool/authority/sanitizer/cast/audience implementations (builtin/http).
- Out: a final assistant message per turn (tools executed + mediated internally); a replayable in-mem
  event log; an audit trail. Blocked tool calls are mediated internally: the model sees the block +
  remedy plan ids and runs `execute_remedy_plan(plan_id)`; on approval the runtime executes the exact
  canonical rendered call (tool + resolved arguments the engine checked and the authority approved).

**Acceptance criteria.**
1. **Pure engine.** Computes over the log's cached **views** + the engine's immutable `Registry` → a
   decision + a validated **`FactBatch`** with an expected-**`Revision`** basis; no IO/clock/mutation.
   Cold-replay reproduces every view and decision (tested).
2. **Synchronous dispatch lifecycle.** For each allowed tool call the runtime opens a dispatch,
   **executes the tool (south) and observes real success/failure**, then closes it: on **success**
   effects commit and the result **value is admitted** (`ValueAdmitted`) — the **label folds from
   `ValueAdmitted`, never from dispatch closure**; on **failure** nothing commits. `DispatchOpened`/
   `DispatchClosed` are logged for replay/audit.
3. **Two-fold check** per spec clocks: narrowing (committed label) → label requirements (committed
   label) → history requirements (log as it stands, evaluated **before** the call's own effects append
   — a call's `emits` never trips its own `no_prior`).
4. **Attention is first-class**: third `requires` kind; per-call; never satisfied by history;
   surfaces as a gap; routed **only** by `attends(mark)`; met only by a fresh ruling from an attending
   authority in atomic plan execution; one ruling may cover attention + a label/history gap.
5. **Remedy planning** yields **executable plans** (engine-side `Authorize`/`Sanitize`/`Accept`
   compositions, each with a `PlanId`, run atomically via `execute_remedy_plan`, each **clearing the
   whole block** — all gaps + any narrowing acceptance — re-checked through both gates) and **prose
   recommendations** (`Redispatch` "call tool X first then re-propose"; `Fork` "handle in a
   subagent"). `Fork` is **never curative** (a child starts at the same label) — advisory only,
   excluded from the proof. `Redispatch` counts toward curability only when the redispatched tool's
   own call is itself curable (**reachability** over a defined finite transition system with
   fixed-point/cycle handling and a completeness-derived bound). Empty of executable plans *and*
   curative recommendations = a proof **relative to the implemented remedy subset** (compiled
   composites are de-scoped, so this is *not* the spec's full global-unliftability proof — it is
   complete over what v1 implements); narrowing never terminal. Completeness vs an
   independently-implemented exhaustive reference planner over that same finite system.
6. **Rulings** are call-scoped over the canonical rendered call, carry an issuer, consumed by the
   dispatch they admit in one atomic batch, cannot outlive a boundary. **No end-user issuer covers a
   response-sink gap.** An authority is shown the **engine-rendered call** (tool + resolved-argument
   identities/secure references + canonical digest + exact gaps + provenance) — never raw secret
   bytes. **The ruling is a fact in the event log** (the mechanism); only a reusable grant on a wire
   is banned. A CAS-superseded ruling (`RulingSuperseded`) carries full evidence and is
   non-authorizing.
7. **Confinement is structural on the bound paths.** The runtime executes tools, so on a
   bound-sanitizer/cast dispatch it withholds the raw result and surfaces only the sanitized/cast
   derivative; the trajectory (event log per session id) is the sole source of truth for the label; the
   harness holds only user turns + final answers. Output sanitizers derive **once**
   (bound to the raw-result digest), admit under the **exact transformed label** (**trust preserved —
   never rises through a sanitizer; audience moved only by the mandate's `from → to`, and only if the
   raw satisfies `from`**), re-checked through both gates on the **pre-dispatch** clock; a sanitizer
   failure/deny/timeout **fails closed** (no raw surfaced, no value admitted; a fixed placeholder to
   the model), while any executed effects stand. **Confinement scope (honest bound):** the runtime
   controls what the model *reads* — a bound sanitizer/cast withholds raw; an *un-bound* tool result
   is admitted at its contract label and the model may read it. But the model's **final answer** is a
   **response sink this version does not fully mediate** (spec de-scopes response-sink mechanics beyond
   the no-self-approval bar), so an un-sanitized value the model read may appear in its answer to the
   harness. Tool sinks (send_email, http_post, …) *are* mediated. So "raw is confined" means "confined
   on the bound-sanitizer/cast paths and out of tool sinks", not "can never appear in the model's free
   final text".
8. **Unknown resolution is a runtime admission mechanism, not a remedy.** A check returns
   `Unresolved(facts)`; the runtime applies the registered `Cast` — **the engine validates the cast
   answer against `may_cast` and emits the `CastApplied`/`ValueAdmitted` batch** (impl in the runtime,
   validation in the engine, exactly like a sanitizer). **A cast may resolve *either* dimension —
   trust OR audience — an Unknown (unlike a sanitizer, which is audience-only); it fills only the
   Unknown dimension, preserving the known dimension and the content.** A per-value output Unknown
   resolves after the result exists (a bound output cast: commit effects → confine raw → resolve +
   validate against `may_cast` → admit; no cast/timeout/invalid → effects stand, no value). No cast →
   fail closed.
9. **In-mem store**: append-only log behind a narrow module boundary with **conditional append
   against an expected `Revision`** (concurrent-branch double-consume protection). Replay reconstructs
   state. Not crash-durable; a file/DB backend is the first follow-up.
10. **External impls abstain by default**; permissive/YOLO explicit; builtins + http with per-kind
    payloads (authority ← rendered call + refs + digest + gaps + provenance; sanitizer/cast ← content
    or secure reference; audience ← recipient). **Tool backends** (south) are registered too
    (builtin/http; MCP later) and executed by the runtime.
11. **Branching**: the harness opens a child by `X-APPA-Parent-Session` (a **trusted-host** input);
    the runtime **seeds the child at the parent's current label** (`seed_child`), immutably binds the
    parent, and rejects reparenting/cross-family merges; merge references a **logged child return
    value by id** and the server derives its label (∩/min) — never client-supplied. Quarantined
    branches are sound (the child's raw content stays in the child trajectory; only the returned value
    crosses). One shared family log; label views are branch-local, revision/effects shared.
12. **Session minting**: absent `X-APPA-Session` → the runtime mints a trajectory id, returns it in a
    response header; ids server-minted.
13. Validation green: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D
    warnings && cargo fmt --check`.

**Scope boundaries (NOT included).** Two crates only; no `check`/`edge`/`dojo`/`demo`. DE-SCOPED:
atomic compiled composites, quarantine `submit_result` trust-raise attestation, durable store, MCP
tool backend (builtin+http first), truly-async human-authority queue (v1 uses bounded http timeouts;
a slow/absent authority fails closed). No streaming. Response-sink mechanics beyond the
no-self-approval bar out of scope.

---

## Public API (to approve — surface, not full bodies)

### `appa-engine` (pure, sync)

```rust
pub enum Dim<T> { Known(T), Unknown }                       // Unknown = "not established", never a rung
pub struct Trust(u8);  pub struct Audience(/* groups | ids | Public */);
pub struct Label { pub trust: Dim<Trust>, pub audience: Dim<Audience> }
impl Label { pub fn combine(&self, other: &Label) -> Label; }        // min trust, ∩ audience

pub struct CanonicalDigest([u8; 32]);   pub struct DispatchId(/* trajectory + digest + occurrence */);
pub struct ResolvedCall { /* tool, resolved arg tree, resolution snapshot id, digest, arg refs */ }
pub struct LabeledValue { /* value + Label, inseparable */ }

pub struct ToolContract { /* name, tags, delta, emits, requires */ }
pub struct Mandate; pub struct Authority; pub struct Sanitizer; pub struct Cast;   // Cast validated here, impl in runtime
pub struct Registry;                                               // immutable after build = static capability
impl Registry { pub fn build(cfg: RegistryConfig) -> Result<Registry, LoadError>; }

pub enum Fact { ValueAdmitted(..), AssistantMessage(..), DispatchOpened(..), DispatchClosed(..),
                Ruling(..), RulingSuperseded(..), Acceptance(..), SanitizerApplied(..), CastApplied(..),
                PlansOffered(..), BlockFeedback(..), ChildReturn(..), Boundary(..) }
// every fact carries a branch attribution (which trajectory in the family); Boundary ∈ {TurnEnd,Fork,Merge}
// the model-transcript view (RP1) is a projection over AssistantMessage + admitted results + BlockFeedback + user ValueAdmitted
pub struct FactBatch { pub basis: Revision, pub facts: Vec<Fact> }  // runtime appends via CAS on Revision
pub struct Views<'a>;                                              // cached projections + current Revision

pub struct Engine { /* owns Registry */ }
impl Engine {
    pub fn check(&self, v: &Views, call: &ResolvedCall) -> CheckOutcome;
    pub fn open_dispatch(&self, v: &Views, call: &ResolvedCall) -> FactBatch;
    pub fn admit_result(&self, v: &Views, id: &DispatchId, r: ResultAdmission) -> FactBatch; // close+value(+sanitize)
    pub fn admit_cast(&self, v: &Views, target: &UnresolvedFact, answer: CastAnswer) -> Result<FactBatch, CastError>;
    pub fn execute_plan(&self, v: &Views, plan: PlanId, call: &ResolvedCall, rulings: &[Ruling])
        -> Result<FactBatch, PlanError>;
    pub fn plan(&self, v: &Views, raw: &RawBlock) -> PlannedBlock;    // fills plans + recommendations, bound to Revision
    pub fn seed_child(&self, parent: &Views) -> FactBatch;
    pub fn merge(&self, parent: &Views, child_return: &ChildReturnId) -> FactBatch;   // by id; server-derived label
}
pub enum CheckOutcome { Allow, Block(RawBlock), Unresolved(Vec<UnresolvedFact>) }   // check finds gaps; plan() solves
pub struct RawBlock { pub requirement_gaps: Vec<Gap>, pub narrowing: Option<Narrowing> }
pub struct PlannedBlock { pub raw: RawBlock, pub plans: Vec<RemedyPlan>,            // executable (ids bound to Revision)
                          pub recommendations: Vec<Recommendation> }                // prose (surfaced to the model too)
pub enum Gap { Trust(..), Includes(..), Cap(..), Prior(..), NoPrior(..), Attention(Mark) }
pub struct RemedyPlan { pub id: PlanId, pub steps: Vec<RemedyStep> }
pub enum RemedyStep { Authorize(..), Sanitize { point: SanitizerPoint, .. }, Accept }
pub enum Recommendation { Redispatch { tool: ToolName, reason: String }, Fork { reason: String } } // advisory
```

### `appa-runtime` (owns state + IO, async/tokio)

```rust
pub struct Config; impl Config { pub fn from_toml_str(s: &str) -> Result<Config, ConfigError>; }
pub struct Runtime; impl Runtime { pub fn new(cfg: Config) -> Result<Runtime, InitError>;
                                   pub async fn serve(self, addr: SocketAddr) -> Result<(), ServeError>; }
// Closed enum backends (no dyn): each is a `builtin` xor `http` variant with an inherent async method.
enum ToolBackend      { Builtin(..), Http(..) }  // invoke -> ToolOutcome (south execution)
enum AuthorityBackend { Builtin(..), Http(..), Hitl }  // rule -> Approve|Deny|Abstain
enum SanitizerBackend { Builtin(..), Http(..) }  // derive -> SanitizerAnswer
enum CastBackend      { Http(..) }               // resolve -> CastAnswer (bounded by may_cast)
// AudienceResolver (dynamic group membership, `john ∈ hr`) is de-scoped from v1: no acceptance case
// exercises it and concrete audiences resolve engine-side. A registered, timeout-bounded resolver is
// a follow-up. (See the follow-up ledger.)
```

### HTTP wire
- `POST /v1/chat/completions` — OpenAI-compatible, **non-streaming**; own serde structs. Headers:
  `X-APPA-Session` (absent → minted + returned in a response header), `X-APPA-Parent-Session`
  (fork/subagent parent, a trusted-host input). **Server-owned reserved tools, pinned from session
  creation (cannot be injected or removed via the north request):** every session pins
  `execute_remedy_plan(plan_id: string)`; **child sessions additionally pin `submit_result(value)`**
  (RP6). Their schemas are server-owned.

---

## Cross-cutting mechanisms

**CC1 — Synchronous dispatch lifecycle.** Driving a turn, per allowed tool call the runtime:
`open_dispatch` (record `DispatchOpened{DispatchId, digest, proposed_label, proposed_effects,
rulings?, bound_output_sanitizer?}`) → **invoke the tool (south)** → on the tool's real **success**:
`admit_result` closes the dispatch, commits effects, and admits the value — **raw** (contract label)
or, if a sanitizer is bound, the **sanitized derivation** (derive once, bound to the raw-result
digest; the label folds only from `ValueAdmitted`); on **failure**: close with no effects, no value; a
sanitizer failure fails closed (fixed placeholder, no raw, no value; executed effects stand). No
cross-request matching — the runtime holds the result in-process.

**CC2 — Trajectory is the sole source of truth.** The event log per session id is authoritative; the
harness holds only user turns + final answers, so nothing it does to its transcript affects the label
or reintroduces tool data. Each turn the runtime takes the new user input from the request (trust
boundary) and drives from its own log. (No transcript reconciliation, no compaction defense, no
dispatch-id map, no turn cursor — the confining executor removes the channel those defended.)

**CC3 — Branching (harness-initiated, server-seeded) = non-raising branch isolation.** The harness
opens a child via `X-APPA-Parent-Session` (trusted-host input); the runtime mints the child id,
`seed_child`s it at the parent's *current* label (a `Fork` boundary fact), immutably binds the parent,
and rejects reparenting/cross-family merge. **The child returns only through a structured
`submit_result` — a logged `ChildReturn` fact carrying the **returned value's own label** (the child
fold for a raw return, or a mandate-validated audience sanitizer's exact output label for a sanitized
one — trust never rises, attestation de-scoped, RP6); the child's free final assistant answer does NOT
cross to the parent or harness** (else the child model, having read raw suspicious content, could quote
it back — the confinement hole). Merge references that `ChildReturn` by id, **once**, into the direct
parent only; the parent absorbs `parent.combine(returned label)` — never client-supplied; a `Merge`
boundary lands in the shared family log. Because the
**trust-raise attestation is de-scoped**, a child that read suspicious content can only return
suspicious-or-lower trust (audience-only derivations and discarded work are the useful cases) — this
is *non-raising* isolation, not the spec's trusted-extraction quarantine (that needs the attestation,
a follow-up). An unbound (raw-policy) child's narrowing return soft-blocks with return plans instead
of merging silently, and an explicit void return (`value: null`) crosses nothing. Family log: one
shared revision + shared effect/history views; label folds are
branch-local (attributed facts). APPA only *recommends* forking (prose).

**CC4 — Static registry vs dynamic abstention.** The engine owns the immutable `Registry` (static
capability). Remedy planning is over it (a registered-may-decline external still yields a plan;
statically absent → none). Dynamic denial/timeout/abstention happens only at **execution** and fails
closed.

**CC5 — Stale-after-approval (Revision CAS across concurrent branches).** A ruling+`DispatchOpened`
batch that loses the conditional-append CAS (a concurrent branch advanced the shared `Revision`) is
discarded + audited (`RulingSuperseded`, full evidence); re-evaluate at the new revision, fresh
ruling if a gap stands. A `DispatchClosed` batch that loses re-reads and retries (no double-close).
A **sanitizer/cast derivative is sealed once** (runtime-produced, bound to the raw-result digest); a
CAS retry re-runs **only pure engine validation** on the sealed derivative — it never re-invokes the
nondeterministic external. A **`DispatchClosed`/finalization append is serialized per family** (a
family-scoped critical section on append), so a close lands in **bounded** steps even under continuous
concurrent `Revision` advance — it cannot livelock a re-read/retry loop (RP2 cancellation depends on
this).

---

## Runtime protocols (the S13 state machines — specified before coding)

**RP1 — North admission profile + model-context builder.** The north request is OpenAI-shaped but
accepted under a **strict profile applied to new AND existing sessions**: exactly **one new trailing
`user` message**; inbound `assistant` / `tool` / `tool_calls` / **any client `system`·`developer`** /
multiple user messages are **rejected (400)**. The **`system`/`developer` preamble and the tool set
come from server configuration only** — never the client — pinned at session creation, immutable
after. Every user input is assigned a **server-configured boundary label** (a policy default or a
registered cast), never a client-supplied or request-inferred label, recorded as a user-origin
`ValueAdmitted`. The runtime **never trusts the request's history**: every internal inference's context
is built **solely from server-held log facts** — the **model-transcript view** (pinned
system/developer → per turn: user input, within-turn (assistant tool-call, admitted tool result)
pairs, block-feedback, prior final answers). **Session isolation is authenticated:** `X-APPA-Session`
and `X-APPA-Parent-Session` are bound to an **authenticated caller (the trusted host / tenant)** — a
foreign session or parent id is rejected, not just namespaced. Absent `X-APPA-Session` → mint an id
bound to the caller, build the preamble from server config, return the id in a response header.

**RP2 — Turn-drive state machine + budgets + cancellation.** States: `Infer → Inspect(completion) →
[per tool call, SERIALLY, re-projecting Views after each committed batch: Check → Allow(OpenDispatch→
Invoke→Admit) | Unresolved(Cast→recheck) | Block(Offer→feedback)] → Infer | Final`. Multiple tool
calls in one completion are processed **serially on a fresh label/history clock** each. Budgets
(config + defaults): max inference rounds/turn, max tool invocations/turn, max remedy attempts/gap,
per-external timeout, **whole-turn wall-clock deadline**. Any exhaustion → **typed terminal**: emit a
fixed policy-stop assistant message + turn-end boundary; deterministic and replayable. **Cancellation
is defined from every state** (during inference, authority wait, block feedback, or before dispatch):
each transitions to the same typed terminal with a guaranteed `TurnEnd` boundary; the request holds no
resources past that. With an **open dispatch** (a south tool may have run), close is a **shielded
critical section**: record the tool outcome (or `Indeterminate`) and the `TurnEnd` boundary in one
**per-family serialized finalization** (see CC5) so it lands in bounded steps despite `Revision`
contention — never orphaning an open dispatch, never starving.

**RP3 — Tool outcome + sealed safe feedback.** `ToolOutcome::{ Success{ body:
BodyDisposition::Available(bytes) | RejectedTooLarge }, Failure(reason), Indeterminate }`. HTTP:
2xx→Success; timeout/connection→Indeterminate; non-2xx→Failure. A **2xx whose body exceeds the cap is
still Success** with `RejectedTooLarge` — **effects commit** (the tool reported success, so `prior(k)`
is satisfied) but **no value is admitted** (never truncated/attacker-controlled bytes) and the model
sees a sealed token. `Success{Available}` → admit the value (raw, or sanitized/cast per RP4/RP5);
effects commit. `Failure`/`Indeterminate` → close, **no effects, no value**, sealed token. Backend
error/oversized bytes are **never forwarded raw**; a deployment that wants an error body shown must
admit it as a labeled value (Unknown→cast).

**RP4 — Output-sanitizer two-phase admission (single tool + sanitizer).** The plan declares a
**result-label bound** (the sanitizer's declared output label). Phase 1: invoke the tool; on `Success`
effects commit and the raw is **confined** (never surfaced). Phase 2: derive the sanitized value
**once** (sealed, bound to the raw-result digest); compute its **actual** label (trust preserved;
audience via the mandate's `from→to` only if raw satisfies `from`); require actual **≤ the declared
bound** and re-run both gates on the **pre-dispatch clock**; **admit only if it passes**, else discard
the value (sealed placeholder to the model). **Executed effects stand either way.** Multi-step
composites (>1 acquisition) stay de-scoped.

**RP5 — Output-cast lifecycle + binding.** For a tool whose output dimension is Unknown: Phase 1
invoke; on `Success` effects commit, raw confined. Phase 2: resolve the Unknown via the registered
cast; the **engine validates** against `may_cast` and emits `CastApplied + ValueAdmitted` (fills only
the Unknown dimension — trust OR audience — preserving the known one + content). No cast/timeout/
invalid → effects stand, no value, sealed token. `admit_cast` binds to value id + raw-result digest +
resolved dimension + cast identity + `Revision` (anti-replay); deterministic cast selection
(registration order) when several match.

**RP6 — Branch return/merge protocol + family-log views.** Child creation: harness opens a child via
`X-APPA-Parent-Session`; the runtime mints the child id, records a `Fork` boundary with **immutable
parent binding**, `seed_child` at the parent's current label. Return: the child model calls the
reserved `submit_result(value)` tool — a string, or an explicit `value: null` void that records and
merges nothing — on the path the fork's immutable **return policy** binds: a raw return (narrowing
soft-blocked with return plans; non-narrowing merges silently) carries the child fold; a value a
**mandate-validated audience sanitizer** produced in the child carries the sanitizer's **exact
declared output label** (audience relabeled, **not** re-intersected with the child fold — else the
legitimate relabel is erased, spec §Branching). Trust **cannot rise** (the trust-raise attestation is
de-scoped, so a return's trust ≤ the child fold's). The child's free final text is **not** propagated. Merge: the parent consumes a specific
`ChildReturn` **by id, once, into the direct parent only** (reparenting / cross-family / double-merge
rejected); the server admits it as `parent.combine(returned-value label)` → a parent `ValueAdmitted` +
a `Merge` boundary. Family log: one shared append-only log + one
shared `Revision`; **effect/history views are family-wide** (a child egress fails a parent
`no_prior`); **label-fold views are branch-local** (fold only the branch's own ancestry via
branch-attributed facts); a boundary scopes pending work to its branch.

---

## Slices (atomic, independently buildable, exact validation)

`V(engine)` = `cargo test -p appa-engine && cargo clippy -p appa-engine --all-targets -- -D warnings
&& cargo fmt --check`; `V(all)` = the workspace form.

**S0 — Workspace skeleton.** Members `appa-engine`, `appa-runtime`; `dead_code="deny"`; no `baton`
identifiers; CI matrix adjusted. V: `cargo check --workspace`.

**S1 — Labels.** `Trust` (ordered chain) + `Audience` (symbolic sets, nested groups, `public`),
`Dim::Unknown` distinct (tests generate as an error, never a rung); `combine` (min trust, ∩ audience);
adequacy (floor/includes/cap). Invariant: comm/idempotent/assoc, restrictive-only, no permissive
delta representable, Unknown never silently folds. V: proptest laws + `V(engine)`.

**S2 — LabeledValue, provenance, ResolvedCall, DispatchId.** Inseparable `LabeledValue`;
argument-level `Provenance`; `ResolvedCall` (+ canonical digest + arg refs); `DispatchId`; raw-result
digest type. V: digest determinism + id-uniqueness tests + `V(engine)`.

**S3 — Event log + Fact model + views.** Facts (the enum above; **branch-attributed**); append-only
`EventLog` + monotone family `Revision`. Views (homomorphisms): **label fold from `ValueAdmitted`
only, branch-local** (RP6); seen-effect-kinds + history **family-wide** (RP6); boundary positions;
open-dispatch set; offered-plan records; **the model-transcript view** (RP1 — the ordered
model-visible messages, a projection over `AssistantMessage`/admitted results/`BlockFeedback`/user
inputs). History checks `prior`/`no_prior`. Invariant: append never gated; every view recomputable by
replay; label folds from admitted values (branch-local), effects/history family-wide, from success. V:
proptest (append monotonicity; view = homomorphism; cold-replay equivalence incl. model-transcript +
branch-local fold vs family-wide effects) + failure-commits-nothing unit tests + `V(engine)`.

**S4 — Contracts + mandates + registry.** `ToolContract{name,tags,delta,emits,requires}`; `Requires`
= label + history + **attention**; `Mandate` (cover ceilings, named waivers, `attends`) + `Scope`
(tags only); `Authority`; `Sanitizer{on,can_reduce:{from,to} audience-only}`;
`Cast{constant XOR resolver, may_cast}` (declaration + ceiling validated here). Immutable `Registry`;
duplicates refused; **no-empty-mandate = loud load error**; trust-raising sanitizer rejected.
Invariant: mandate powers name only their currency; scope tags only. V: unit tests (construction, dup
refusal, empty-mandate, trust-raise rejected) + `V(engine)`.

**S5 — Raw evaluation (private) + apply-as-batch.** `evaluate(views, call) -> Verdict` (crate-private):
narrowing → label reqs → history reqs (spec clocks); attention → gaps. Apply emits `DispatchOpened`
(proposed label/effects, folds nothing). Invariant: narrowing before label reqs; own emits never trip
own precondition; delta/effects/value deferred to admission. V: worked-example unit tests
(search_and_share; get_ticket; attention gap; clock ordering) + `V(engine)`.

**S6 — Remedy primitives + result/cast admission.** acceptance (plan id + canonical dispatch, atomic);
ruling coverage (mandate covers each gap incl. attention; authority request = rendered call + refs +
digest + gaps + provenance); **sanitizer** — input (substitute into rendered args) and output (bind
to dispatch; `admit_result` derives once bound to raw-digest; **exact transformed label** — trust
preserved, `from→to` audience only if raw satisfies `from`; **two-phase declared-bound admission per
RP4**; re-check both gates pre-dispatch clock; fail closed on failure); **cast admission** (`admit_cast`
validates against `may_cast`, emits `CastApplied`/`ValueAdmitted`, bound to value/digest/dimension/
cast/`Revision` per RP5; input-value vs bound output-Unknown paths). Invariant:
acceptance+dispatch atomic; ruling within mandate; **sanitizer audience-only; cast fills an Unknown
dimension (trust OR audience) preserving the known one**; ceilings checked; label folds only from
`ValueAdmitted`. V: per-primitive unit tests incl. swapped-call/replay
rejection, sanitizer `from`-fail + trust-preserved, cast may_cast-violation rejected + `V(engine)`.

**S7 — Remedy planning + empty-proof + reference planner.** `RemedyPlanner`: executable plans
(`Authorize`/`Sanitize`/`Accept` compositions clearing the whole block, re-checked both gates, routed
by currency incl. attention via `attends`) + prose recommendations (`Redispatch` for `prior(k)`/cap,
`Fork`). **Curability is reachability over a defined finite transition system**: states =
(label, history) reachable from the block via one executable step or one `Redispatch`-then-recheck
(a `Redispatch` counts only when the named tool's *own resolved call* is itself curable); transitions
computed against the static registry; the bound is the **fixed point** of the reachable set (finite:
labels descend, effect-history grows monotonically, both over finite domains), so cycles terminate.
**`Fork` is advisory — excluded from the proof.** Empty of executable plans + curative recommendations
at the fixed point = a **proof over the implemented remedy subset** (composites de-scoped). The
**reference planner is an independently-implemented exhaustive fixed-point search** over the same
finite system (not the production planner re-run). Invariant: each executable plan clears the whole
block; no trust-raise; static registry (CC4). V: proptest (planner == reference oracle on full-block
curability; no trust-raise; Fork excluded from terminality; cyclic-prerequisite terminates);
attention mismatched-tags; sanitizer-only plan only when it clears the block; `V(engine)`.

**S8 — Atomic plan execution + issuer bar.** `execute_plan`: render exact call, verify mandate
coverage per gap, land plan id + ruling(s) + `DispatchOpened` as one atomic batch on the current
`Revision`; pending can't outlive a boundary; CC5. Response-sink bar: `Sink` +
`Issuer::{EndUser,Authority(name)}`; end-user never covers a response-sink gap. Invariant: ruling
call-scoped over digest; consumed once; ruling stored only as a log fact. V: unit tests (swapped-call
rejected; two-eyes; attention fresh-ruling on repeat; boundary kills pending; end-user can't
self-approve response-sink; superseded ruling not reused) + `V(engine)`.

**S9 — Branching label semantics + family log.** `seed_child` (parent current label + Fork boundary,
immutable parent bind); `merge` by child-return-id → server-derived label (∩/min) + Merge boundary;
one shared family log with branch-local label views + shared revision/effects; reject
reparenting/cross-family merge. Invariant: child never at L0; ∩ can't widen; abandoned-branch egress
visible via shared log; no client label. V: unit tests (fork label = parent current; merge can't
widen; abandoned egress visible; reparent rejected) + `V(engine)`.

*Checkpoint A (S0–S9): pure engine. `V(all)` + external `REVIEW(diff)` + internal gate + draft PR.
Autonomous — continue.*

**S10 — Config loader + docs reconciliation.** Parse spec §config surface into the registry
(sanitizers with **exact tokens** `on = ["tool_input","tool_output"]`); load-time lints
(no-empty-mandate, constant-XOR-resolver, operator-required, unknown-field-loud, every legacy key
rejected). **Rewrite `docs/contracts.md` to the single spec dialect** — removing the deleted proxy dialect
(`appa-proxy`, tool passthrough, "the harness executes", client tool-result ingress); port k8s-ops +
worked examples. V: golden-parse tests (worked + k8s; input+output sanitizers); unmatched-tool +
omitted-field + absent-`requires` + legacy-key-rejection + malformed-config tests; a doc-grep test
asserting no `appa-proxy`/passthrough/harness-execution language survives; `V(all)`.

**S11 — In-mem store + session store + conditional append.** Concrete in-mem append-only log with
**conditional append against an expected `Revision`** (narrow module boundary, one impl; durable
backend later). `SessionStore` keyed by server-minted trajectory id **bound to an authenticated caller/tenant (RP1)**,
immutable parent links (family), **per-family serialized finalization append (CC5/RP2)**. Replay
reconstructs state. Invariant: append atomic per batch; conditional append is the serialization point;
ids server-minted + caller-bound; a close lands in bounded steps under contention; replayable not
crash-durable. V: unit tests (append→replay; concurrent double-consume rejected; **bounded close under
continuous competing appends**; session mint; **foreign session/parent id rejected**; fork/subagent
inheritance; reparent rejected) + `V(all)`.

**S12 — External implementations + tool execution (south).** Closed enum backends `ToolBackend`,
`AuthorityBackend`, `SanitizerBackend`, `CastBackend` (no `dyn`; `AudienceResolver` de-scoped from v1,
a follow-up); **default abstain** (authorities/sanitizers/casts); tool backends
builtin/http (MCP later). **Tool invocation returns `ToolOutcome::{Success|Failure|Indeterminate}`
(RP3)** with HTTP classification (2xx/timeout/non-2xx) + body-size cap; a failure's backend bytes are
**never forwarded raw** — the model gets a **sealed token**. YOLO explicit. Builtins: test tools,
`approve` (cover-free only), `redact-email`, constant casts. Http clients, per-kind payloads
(acceptance #10). Invariant: resolvers are the trusted base; builtin `approve` rejected for
cover-bearing mandates; no raw bytes to authorities; a cast never exceeds `may_cast`; a failed tool
commits no effects/value. V: unit tests (abstain-excluded vs dynamic-decline-fails-closed; YOLO
approves w/ audit; builtin redact; cast resolves Unknown, unresolved-without-cast fails closed;
success/failure/indeterminate outcomes; error-body never forwarded raw; http payloads vs a local test
server = true process boundary) + `V(all)`.

**S13 — OpenAI wire + turn-drive loop (axum + Rig) + branching.** Own serde wire structs
(non-streaming). Rig `0.40.0` client-only for upstream OpenRouter
(`CompletionModel::completion_request(..).tools(..).send()` → `AssistantContent::ToolCall`; never
`Agent::prompt`; map wire `role:"tool"` → `UserContent::ToolResult` **upstream-only**; **north rejects
inbound assistant/tool/tool_calls/history — RP1**). This slice **implements RP1–RP6** (north admission
profile + model-context builder; turn-drive state machine + budgets + cancellation; tool outcome +
sealed feedback; output-sanitizer two-phase; output-cast lifecycle; branch return/merge). Per request:
load/mint the trajectory (RP1); admit the new user turn; **drive the turn (RP2, serial per tool call,
fresh Views each)**: infer (context built solely from server-held facts) → per proposed call
`engine.check`; **allow** → `open_dispatch`, **invoke (south, RP3)**, `admit_result` (CC1/RP4/RP5);
**unresolved** → `admit_cast` (RP5) + re-check; **block** → persist `PlansOffered`, feed the block
(gaps + executable plan ids **+ prose recommendations**) back to the model so it runs
`execute_remedy_plan(plan_id)` or redispatches → route to the impl (bounded timeout; whole-turn
deadline; slow/absent = fail closed), on approval **execute the exact canonical call**; loop to a final
assistant message or a **typed policy-stop on budget exhaustion**; turn-end boundary; return.
**Branching (CC3/RP6):** child via `X-APPA-Parent-Session` → `seed_child`; child `submit_result` →
`ChildReturn`; parent merges by id once → server-derived label. Invariant:
the harness never sees a tool call/result; a bound-sanitizer/cast dispatch never surfaces raw to the
model; effects commit on real tool success; stale/foreign/replayed rulings refused. (The model's final
answer is an un-mediated response sink this version — acceptance #7 — not a "no raw ever escapes"
claim.) A later identical proposal is a new dispatch under a fresh check, not a re-issue. V: handler
unit tests for each RP1–RP6 state machine (north rejects, context-from-log-only, serial multi-call,
budget policy-stop, cancellation with open dispatch, sealed failure feedback, two-phase
sanitizer/cast, branch return/merge) + `V(all)`.

**S14 — End-to-end (real Runtime, only upstream stubbed).** Spawn the real `appa-runtime` over HTTP;
a local canned-completion server = the model (true process boundary); test tools registered south.
Assert: session mint (id bound to the caller); a turn drives tool calls and returns a final answer;
**north rejects a client assistant/tool/system/developer message and a foreign session/parent id
(RP1)** and the model context is built from the log (a reshaped request history changes nothing); an
**oversized 2xx commits effects but admits no value (RP3)**; a **child's audience-sanitized
`submit_result` reaches the parent with the sanitizer's audience (not re-intersected to the child
fold), trust not raised (RP6)**; a south test tool returning attacker-controlled content is labeled per contract and
blocks a downstream public sink; a blocked call surfaces plan ids + recommendations and an internal
remedy cycle executes the exact canonical call, effects landing **once** and only on real success;
**output sanitizer — the model/harness never receive raw, admitted label is the exact transformed
label under the declared bound, sanitizer failure fails closed while effects stand (RP4)**; a
**failing tool commits no effects** (so `prior(k)` stays unmet) and its **error body is not forwarded
raw (RP3)**; Unknown → cast resolves (trust or audience) or fails closed (RP5); attention freshness;
**budget exhaustion → typed policy-stop; cancellation with an open dispatch closes it (RP2)**; **a
quarantined child processes untrusted content, `submit_result` returns a server-derived-label value,
the child's free text does NOT reach the parent, merge is once-only (RP6)**; stale-after-approval
across two concurrent branches. Update README; document engine semantics in `appa-engine/src/lib.rs`.
V: `V(all)`.

---

## Cadence (autonomous — no user pauses)
Implement S0→S14 in order. Per checkpoint: `V(all)`, external `REVIEW(diff)` + internal gate, address
findings, open/update a draft PR, continue. Checkpoints: **A** S0–S9 (pure engine) · **B** S10–S12
(runtime state + config + impls + tools) · **C** S13–S14 (wire + turn-drive + branching + e2e).
Escalate only if scope, an observable behavior, an API/data contract, or an acceptance criterion must
change.

## Follow-up ledger (recorded, not built)
- Atomic compiled composites; quarantine `submit_result` trust-raise attestation (deferred again
  after landing once — see the tokenmaxxer follow-up ledger for the derived-cast design sketch and
  the provenance-bound attestor-input requirement any future landing must meet).
- MCP tool backend; truly-async human-authority queue (v1: bounded http timeout, fail closed).
- Durable (non-mem) store behind the S11 boundary; invoke/append crash-gap outbox.
- **`AudienceResolver`** — dynamic reader-group membership resolution (`john ∈ hr`), de-scoped from
  v1 (concrete audiences resolve engine-side; no acceptance case needs it). A registered,
  timeout-bounded resolver behind the same backend boundary when a deployment requires it.

## Risks
- The turn-drive loop (S13) + CC1 sanitizer admission are the subtlest parts; proven by S14
  (sanitizer-fail, failing-tool-no-effects, quarantine, internal remedy cycle, stale).
- Checker stays free of ad-hoc conditionals (spec §Implementation shape): every decision reduces to
  label arithmetic or a log query; imperative judgment lives only in registered externals.
- Paper theorems (monotone descent+settlement, merge confinement, remedy completeness) are proptest
  oracles — completeness vs the independent reference planner.
