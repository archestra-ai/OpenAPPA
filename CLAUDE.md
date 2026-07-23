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

