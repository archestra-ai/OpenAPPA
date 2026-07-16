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
  `docs/spec.md` defines them, not colloquially.
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

## Document precedence

1. `docs/spec.md` — the normative integration spec.
2. `docs/authority-model-design.md` — the plan-of-record: the model as built,
   standing decisions, and (§4) superseded decisions. **Do not resurrect
   anything from §4 silently** — the five-kind remedy taxonomy, plan caps,
   bounded rescue, plan postures, mutable trajectory state, and the separate
   response pipeline are all deliberately gone.
3. `docs/declassifier-design.md` — foundation rationale, superseded in places
   (marked inline). Its code sketches are historical; the code is the reference.
4. `core/src/lib.rs` — concepts and semantics of the engine.
5. `core/CLAUDE.md` — the invariants a core edit must not silently break.
   **Read it before touching `core/`.**

An unfinished higher-level vision document (Baton/APPA first principles)
exists outside this repository; its key theses are folded into the "Mental
model" and "Gotchas" sections below. Treat those principles as design
direction when they conflict with incidental implementation details — the
sections below, not an unfindable file, are the reference.

## Workspace map

Cargo workspace members: `core`, `check`, `contracts`, `dojo`, `proxy`.
Workspace lint: `dead_code = "deny"` — unused code fails the build.

- `core/` — `appa-core`, the engine. Prototype, `publish = false`, edition
  2024. Everything else is an integration around it.
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
- `proxy/` — `appa-proxy`, the **inference-layer** integration: an
  OpenAI-compatible HTTP proxy. Stateless — on every `/v1/chat/completions`
  response it replays the supported conversation history into a fresh
  `Trajectory` (system/developer roles and uncontracted tool results are
  skipped) and rewrites blocked tool calls into stop explanations before the
  harness sees them.
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
- **Tri-state outcomes, fail-closed.** Every well-formed flow — a tool call
  or an assistant emission, same pipeline — settles as **AllowedNow** (a
  linear permit), **Remediable** (soft block with a non-empty frontier of
  predicted plans), or **Terminal** (a *proven* no-remedy claim — the search
  is uncapped, so Terminal is never "nothing found within budget"). Stale,
  foreign, or conflicting proposals are refusals on a separate channel,
  outside the tri-state, touching nothing.
- **Two remedy kinds only.** *Reduce* (derive a value through a registered
  transformer, or narrow the action through a registered transition verified
  never wider) and *Authorize* (an exact typed delta at an exact scope). The
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
  (`combine`) and the adequacy relation are different structures, and trust's
  `Unknown` sits in *opposite* positions: definite in the fold (between
  Trusted and Suspicious), incomparable/bottom → `Unprovable` in adequacy.
  The operation is `combine`; do not call it a join. `widening_over` is a
  third *derived* relation (dual of adequacy), not a third order.
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
  hits a wider sink. The effects axis is the one place acquisition is gated
  today: proposed effects growth soft-bans at the flow check. A
  pre-acquisition soft gate on restrictive *reads* is vision-ahead-of-code
  (see below), not current behavior.
- **Attention is a requirement, not a label dimension.** An explicit user
  confirmation is satisfied structurally by a confirming user turn and spent
  as a grant consumption at release. It was never built as a dimension — do
  not add one.

### Engine invariants (details in `core/CLAUDE.md` — read it before editing core)

- Values are immutable; a transformer derives a *new* value; nothing ever
  mutates or relabels a source. Durable authority raises mint a new
  `Provenance::Endorsed` value via the raise helpers, never `combine`.
- Admission is engine-owned; `Trajectory::ingress` is the only caller-labeled
  path. Requests carry control *dependency sets*, never a caller-supplied
  control label (that would be a relabeling hole). Caller-labeled assistant
  ingress does not typecheck — the response is a mediated emission sink like
  any tool.
- One build path: every read model is a full reprojection of the event log
  after each atomic batch. Never add a second incremental fold over `Fact`.
- Linear capabilities (`ExecutionToken`, `DispatchReceipt`, `StepCapability`,
  `PendingApproval`) are non-`Clone`, `Serialize`-only, no public
  constructor. **Never add `Deserialize`** — deserializing one forges
  linearity. Receipts are lifecycle-bound (they close a dispatch that already
  happened); everything else is revision-bound and staled by any appended
  fact.
- Plans are predictions, never permits: only the head step is executable,
  every applied step triggers full re-evaluation, and authority routing is
  resolved live at application — which is why registries freeze at the first
  evaluation (`RegistryFrozen`). That rule is load-bearing for *safety*, not
  determinism.
- One gate, one relation: `engine::planning::constrain_gate` is the whole
  narrowing check for both planner and applier. Never duplicate it.
- Transactional labeling: a tool call that dispatched has its label in the
  trajectory forever — release appends the may-effect commitment before
  dispatch, failure appends and removes nothing. No post-hoc sanitizing can
  clear the log; facts only grow.

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

### Benchmarks

- In `harness-agentdojo`, contracts label tools by *source type* only
  (readers of third-party text are suspicious, pure-state readers trusted) —
  never by whether a given result actually carries an injection. That is the
  benchmark's ground truth and peeking is cheating.
- The trust-only limitation is the experiment's honest premise, not a bug: a
  benign send and a poisoned one are identical in label space, so a
  trusted-only sink policy pays a utility price. Audience is the dimension
  that would split them; that is data-plus-protocol work, not a new engine.
- A suite's contract table must cover every tool (the harness cross-checks
  and fails on drift); the three unknown-policies only separate under sparse
  annotation.

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
  completion-before-release, no double grant consumption) is refused at
  event admission — the single enforcement point — because encoding it as
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
  beside a serializable descriptor — no capturing closures.
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
  out of core.
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
