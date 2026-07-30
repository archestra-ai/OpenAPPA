# CLAUDE.md

OpenAPPA (formerly **Baton**; APPA = Agentic Permissions Policy Algebra)
is a value-granular information-flow policy engine for LLM agents. It sits
between the agent and its tools/inference and answers one question before
every proposed flow: *can this value, derived from these sources, legally flow
into this sink?* It is declarative and algebraic — no guardrails, no prompt
filtering, no bespoke `if`s; any imperative judgment lives in registered
external authorities and transformers, never in the engine.

## IMPORTANT
docs/ is a current state of truth. Code and website can be harshly outdated.

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
  `docs/spec.md` and `appa-engine/src/lib.rs` define them, not colloquially.
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

1. `docs/spec.md` — normative. Every rule carries a stable id by family
   (`POS`, `LBL`, `CHK`, `RMD`, `AUT`, `RUL`, `SAN`, `LOG`, `BRN`, `UNK`,
   `CFG`, `EXT`, `IMP`, `THR`). Cite ids from code comments, tests and
   issues; they outlive section numbers. Takes priority over implementation
   details where they conflict.
2. `paper/` — the LaTeX paper (AISec '26 draft, merged in #37): the same
   model with its formal claims, theorem scoping, and citation anchors.
   Coding work is grounded in the spec, not the paper — coding never
   changes the paper.
3. `appa-engine/src/lib.rs` — concepts and semantics of the engine as
   implemented.

The rest of `docs/` is not normative and must not be cited as if it were.
`guide.md` is the reader-facing introduction, `rationale.md` records why
decisions went the way they did and settles nothing, `glossary.md` splits
the vocabulary into surface terms (typed by users) and model terms,
`contracts.md` is the policy-review guide, `engine.md` maps the spec onto
the crates. `docs/README.md` is the map.

Edits flow one way: spec → guide/contracts/website. A change that starts in
a downstream doc has to land in the spec first.

## Pre-public: no history, no compatibility

Until explicitly declared public, APPA owes nothing to its own past. Docs
describe the current model only — no retired rules, no "formerly", no
migration notes. Rule ids may be renumbered and freed numbers reused; keep
each family contiguous. Config and wire surfaces may break without shims
or deprecation paths. The paper is the exception: it stays as published.

## Writing (reader-facing docs)

Applies to `docs/` and anything else written for readers outside the
project. `paper/` has its own register and these rules do not touch it.

**Vocabulary**

- Ordinary programming vocabulary stays — declarative, imperative,
  append-only, idempotent. Field vocabulary is glossed at first use or cut:
  semilattice, noninterference, taint, sink-side adequacy.
- Terms appearing in the TOML surface or the API are mandatory. `delta`,
  `requires`, `emits`, `attention`, `exactly`, `may_add` — teach them.
  Never route around a term the reader will have to type.
- Every term must be readable cold where it lands, or glossed by its own
  sentence. "A sanitizer's declared transition" assumes prior knowledge;
  "a sanitizer that can clean the data" carries itself.
- Deflate words carrying more fear than content. "Serialized and durable"
  reads as a platform project; "an append-only file" reads as an afternoon,
  and the afternoon is the truth for a single process. Name the cost, not
  the category.
- Reader-facing prose may diverge from wire vocabulary on purpose. `block`
  stays the wire tag; prose says the engine refuses the call and names what
  would make it pass. The glossary bridges the two once.

**Claims**

- Attach every claim to its subject. "APPA is declarative" is three claims —
  config, verdict, registered components — and only the first holds. The
  same trap waits at "provable" and "deterministic".
- Mechanical over comparative. "Without the second check the agent finds
  out three steps later" beats "in every other system that cost is
  invisible": same information, falsifiable, no swagger.
- State the guarantee at guide level; the proof that earns it lives in the
  spec.
- State limits flat. A limitation in promise cadence reads as ad copy.

**Rhythm**

- No sentence fragments for emphasis, and no one-sentence paragraphs.
  Paragraphs run three sentences or more.
- Never announce importance. `deliberately`, `load-bearing`, "The point:",
  "The central thesis:" all tell the reader that something matters instead
  of letting it matter — and in a spec everything is deliberate.
- Antithesis ("X, not Y") earns one instance per page. The 2026-07 spec
  draft had 25.

**Structure**

- Headings assert the invariant — "Labels only move one way", not "How the
  label moves" — so skimming collects the guarantees.
- A paragraph's best sentence is usually its last. Move it to the front and
  delete the warm-up.
- Perceived reading cost decides whether a section is read at all. Prefer a
  code line to a sentence describing code, a table to parallel prose, an
  asserting heading to a topic heading.

## Rust guidelines

**Spend the cleverness budget on the domain model, not the type machinery —
make invalid states unrepresentable with boring tools.** "Boring Rust"
constrains the mechanism vocabulary (no trait acrobatics, no `dyn`, no
type-level programming); type-first design constrains the data vocabulary
(invariants live in the shape of data). They compose: `Label::combine` only
ever narrows, `CastResolution` is an enum, `ResolvedCall` derives its digest
instead of storing it, and `AuthorityRequest` has private fields behind a
validating constructor — so a permissive delta, a cast that is both constant
and resolver-backed, a digest belonging to different arguments, and a
request naming a dangling reference are each unrepresentable. Enums,
visibility and validated constructors do the enforcement; no typestate
generics anywhere.

Where an invariant should live:

- **Structural invariants → types, because the encoding is boring.** "Can't
  hold both of these at once" (enum), "can't be inconsistent with its
  source" (derive it, don't store it), "can't be built with a bad reference"
  (validated constructor), "can't be built by callers" (`pub(crate)`). Zero
  cleverness, removes whole test categories.
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
  implementations or a real boundary. External backends are closed enums
  dispatched by match (`BuiltinSanitizer`, `SanitizerBackend`,
  `AuthorityBackend`) beside a serializable descriptor — no capturing
  closures, no registry of callbacks.
- Minimize the public API surface: a few coarse operations over many tiny
  exported helpers. In core, keep state mutators `pub(crate)` (as
  `admit_result` and `admit_cast` are) and never hoist read-only
  audit/projection types — `Projection`, `Views` — into the root re-exports.
- Treat all external input as untrusted; validate at public entry points and
  convert immediately to native types.
- `Result` with domain error enums (`thiserror`) in library code. No
  unchecked failure on recoverable paths or external input; a documented
  `expect` on an invariant already established by prevalidation is house
  style in core (the message names the invariant, e.g. "plans reference only
  registered transformers"). Free `unwrap` belongs in CLI entrypoints and
  tests only.
- Never hold a lock across `.await` inside a critical section — the store's
  methods are synchronous and never await under their mutexes. The
  deliberate exception is the turn lease: `Turn` holds an
  `OwnedMutexGuard<()>` for its whole lifetime, inference and tool awaits
  included, because a trajectory's turns are serialized by construction.
- Observability is `tracing` only (decision path at `debug!`, algebra at
  `trace!`), borrow-only and never behavior-changing; exporter wiring stays
  out of core (`appa-gateway -- -v`/`-vv` in `demo/gateway` selects the
  level).
- Public domain structs own their data; cloning small IDs/config is fine,
  cloning hot-path buffers is not.
