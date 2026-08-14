# CLAUDE.md

OpenAPPA (APPA = Agentic Permissions Policy Algebra)
is a value-granular information-flow policy engine for LLM agents. It sits
between the agent and its tools/inference and answers one question before
every proposed flow: *can this value, derived from these sources, legally flow
into this sink?* It is declarative and algebraic — no guardrails, no prompt
filtering, no bespoke `if`s; any imperative judgment lives in registered
external authorities and transformers, never in the engine.

## IMPORTANT
The golden set is `docs/` plus three website files:
`website/content/docs/how-it-works.md`,
`website/content/docs/contracts.md` and `website/lib/terms.ts`.
Golden
files must agree with each other in every commit: a change that alters
what another golden file also states lands together with the matching
update, and a commit that leaves two golden files contradicting each
other is not allowed. Non-golden
files — the code and the rest of the website, for now — can be harshly
outdated.

## Naming

- Use the `appa` prefix for new OpenAPPA-owned crates, binaries, environment
  variables, and protocol identifiers. Existing unprefixed names are
  deliberate, not violations: core's internal module names (`engine`, `plan`,
  `turn`, …), `DOJO_MODEL`, the demo-owned `notify-mcp`, the reserved
  `assistant.response` sink. Never introduce new `baton`-named
  identifiers: `baton` was the earlier name and can happen only in stale spots.
- "Engine", "Trajectory", "Value", "Label", "Dimension", "Authority",
  "Transformer", "Remedy plan" are defined terms — use them as the glossary in
  `docs/spec.md` and `appa-engine/src/lib.rs` define them, not colloquially.
- **Agentic terminology first.** In comments, docs, and identifiers, lead with
  the agentic vocabulary: *trajectory* (not execution trace or session
  history), *flow* (not information transfer or operation), *turn*, *tool
  call*, *emission*, *actor/agent*, and *harness*. Use the classical IFC or
  security term when it adds precision or establishes lineage, and gloss it
  for public readers at first use. Never let it displace the agentic term as
  the primary name for a concept that has one.
- Do not invent new terms, especially when working with spec. Try to use 
  existing definitions. If you want to introduce a new one - ask a user and 
  explain why.

## Document precedence

1. `docs/spec.md` — normative. Every rule carries a stable id by family
   (`POS`, `LBL`, `CHK`, `RMD`, `AUT`, `RUL`, `SAN`, `LOG`, `BRN`, `UNK`,
   `CFG`, `EXT`, `IMP`, `THR`). Cite ids from code comments, tests and
   issues; they outlive section numbers. Takes priority over implementation
   details where they conflict.
2. `appa-engine/src/lib.rs` — concepts and semantics of the engine as
   implemented.

The rest of the golden set is not normative and must not be cited as if it
were; non-normative still means consistent — normativity decides who wins
an interpretive dispute, never which file may lag.
`website/content/docs/how-it-works.md` is the reader-facing introduction
(golden, though it lives with the website), `rationale.md` records why
decisions
went the way they did and settles nothing, `glossary.md` splits the
vocabulary into surface terms (typed by users) and model terms,
`website/lib/terms.ts` restates glossary entries as the website's
term-popover definitions,
`website/content/docs/contracts.md` is the policy-review guide (golden,
and likewise living with the website), `engine.md` maps the spec onto
the crates. `docs/README.md` is the map.

Edits flow one way: spec → how-it-works/contracts/website. A change that
starts in
a downstream doc has to land in the spec first; the flow decides where a
change starts, not when the rest catches up — within the golden set, the
downstream update lands in the same commit.

## Pre-public: no history, no compatibility

Until explicitly declared public, APPA owes nothing to its own past. Docs
describe the current model only — no retired rules, no "formerly", no
migration notes. Rule ids may be renumbered and freed numbers reused; keep
each family contiguous. Config and wire surfaces may break without shims
or deprecation paths.

## Collaboration

Applies to discussion and work in this repository.

- Lead with the result, finding, or decision the user needs. Report progress
  when it exposes a discovery, tradeoff, or blocker rather than narrating
  routine tool use.
- Assume the reader writes software but may not know this codebase or have a
  complete AI or security background. Explain repository and domain context
  needed to evaluate a decision; do not explain standard programming concepts
  unless asked.
- Surface decisions when reasonable interpretations would produce materially
  different work. Recommend one option and state the tradeoff; decide routine
  implementation details without asking.
- Match depth to the task. Keep routine implementation updates concise, but
  show the reasoning behind changes to the model, spec, architecture, or
  security guarantees.
- Use concrete references such as paths, rule ids, types, and commands. Mark
  uncertainty as uncertainty rather than smoothing it into confident prose.

## Writing (public docs)

Applies to `docs/` and other material written for readers outside the project.
Except for correctness, defined terminology, and claim scope, these are
defaults rather than lint rules; depart from them when the document reads
better as a result.

**Audience and purpose**

- Write for senior engineers and technology leaders. Assume technical
  fluency, but not complete or current knowledge of both AI and security.
- Do not require prior training in information-flow control. Introduce the
  minimum specialized vocabulary needed to state the idea accurately.
- Match the presentation to the document. The spec is normative and precise;
  guides build a usable mental model; glossaries and references optimize for
  lookup. Product framing belongs in introductions and guides, not in
  normative or reference material.
- In guides, show how remedy plans and narrowing keep an agent productive when
  those behaviors are relevant. Do not force the value proposition into every
  section.

**Vocabulary**

- Use APPA's defined terms consistently. Terms that readers must type in TOML
  or use through an API, including `delta`, `requires`, `emits`, `attention`,
  `exactly`, and `may_add`, must be taught rather than paraphrased away.
- Prefer plain technical English and the shortest accurate term. Specifications and normative documentation MUST follow ASD-STE100 (Simplified Technical English): short sentences (maximum 20–25 words), active voice, precise technical vocabulary, zero conversational fluff or oversimplification, and strict preservation of technical and modal nuances (such as capability `can` vs action `does`, `MUST`, `MAY`, `MUST NOT`). Gloss specialized IFC or security vocabulary at first use; omit it when it adds no precision.
- Reader-facing prose may explain a wire term in ordinary language. Show the
  exact wire term where readers need to recognize or type it.
- Name concrete behavior and cost instead of relying on broad category words.

**Claims**

- Lead with the strongest consequence the spec supports. Do not weaken a
  guarantee with `helps`, `aims to`, or `is designed to` when APPA actually
  enforces or proves the property.
- Headlines and introductions may compress formal scope for clarity, provided
  nearby text names the exact property and its boundary. "APPA makes declared
  flow decisions deterministic" is stronger and more accurate than a broad
  claim that APPA makes agents safe.
- Use `proven`, `provable`, and `deterministic` only for a specific property
  with support in the spec or its cited proof. Translate the formal property
  into its operational consequence rather than relying on the adjective.
- State a claim's assumptions and limits once, plainly and nearby. Do not
  repeat caveats until they obscure the guarantee.
- Prefer mechanical, falsifiable comparisons over claims about entire product
  categories. Name what APPA checks, prevents, or preserves.
- State guarantees at guide level and keep the rules or proof that support
  them in the spec.

**Style and structure**

- Prefer direct, complete sentences and paragraphs with one clear job. A
  one-sentence paragraph is fine when another sentence would be filler.
- Use headings that make the document easy to scan. Assert invariants in
  explanatory material; use topic headings where lookup is the purpose.
- Avoid repeated antithesis, sentence fragments used only for emphasis, and
  phrases that announce importance instead of showing it.
- Prefer code, tables, or lists when they communicate the same information
  with lower reading cost. Treat rhythm as an editing judgment, not a quota.

## Rust guidelines

**Spend the cleverness budget on the domain model, not the type machinery —
make invalid states unrepresentable with boring tools.** "Boring Rust"
constrains the mechanism vocabulary (no trait acrobatics, no `dyn`, no
type-level programming); type-first design constrains the data vocabulary
(invariants live in the shape of data). They compose: `Label::combine` only
ever narrows, `CastResolution` is an enum, `ResolvedCall` derives its digest
instead of storing it, and `CanonicalArguments` derives its RFC 8785 bytes
from the one validated value — so a permissive delta, a cast that is both
constant and resolver-backed, a digest belonging to different arguments, and
a payload disagreeing with its own canonical bytes are each unrepresentable.
Enums,
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
  out of core (`appa-runtime-v2 -v`/`-vv` selects the level).
- Public domain structs own their data; cloning small IDs/config is fine,
  cloning hot-path buffers is not.
