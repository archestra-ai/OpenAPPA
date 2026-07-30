# The reference implementation

For people working on the engine itself, or building a second one. Most
readers want `guide.md` or `spec.md` instead.

This file maps the spec onto the shipped code and stays thin. The crate docs
carry the detail, and duplicating them here would only produce drift.

## Layers

`spec.md` §14 defines two layers. The crates follow it:

| crate | layer | role |
|---|---|---|
| `appa-engine` | inner | the pure decision core. No IO, no clock, a function of the log's cached views |
| `appa-runtime` | outer | the canonical mediation assembly: owns policy, engine, trajectory families, and the log |
| `appa-gateway` | host | OpenAI-compatible `/v1/chat/completions` adapter |
| `appa-sdk` | host | narrow facade for frameworks that own their own inference and tool execution |
| `appa-agent` | host | a provider and serial agent loop over the runtime's `Mediator` |

A harness author embeds `appa-runtime` and implements neither layer. Note
that `IMP-3` describes the outer layer as owning durable append with
pluggable destinations; `appa-runtime` ships one in-memory store
(`store.rs`) behind that surface, and a durable backend is a follow-up. A
process that restarts loses its log, so history requirements start over.

## Where each rule family lives

| family | module |
|---|---|
| `LBL` | `appa-engine/src/label.rs`, `value.rs` |
| `CHK` | `check.rs`, `admit.rs` |
| `RMD` | `plan.rs`, `execute.rs` |
| `AUT`, `RUL` | `authority.rs` |
| `SAN` | `authority.rs`, `admit.rs`, `branch.rs` |
| `LOG` | `fact.rs`, `projection.rs` |
| `BRN` | `branch.rs` |
| `UNK` | `admit.rs`, `label.rs` |
| `CFG` | split three ways: `appa-runtime/src/config.rs` parses the TOML and refuses shape errors, `appa-engine/src/registry.rs` refuses the algebraic ones (empty mandate, unannotated tool with label requirements), `contract.rs` and `names.rs` hold the types. `CFG-13` block messages are runtime feedback |
| `EXT` | `appa-runtime/src/external.rs` |

## Invariants the code carries that prose cannot

`IMP-4` asks for structural enforcement: the type system where it reaches,
one refusal point where it does not. These are the places the shape of a
type carries a spec rule, so a second implementation in another language has
to find its own way to hold them. Most are covered by tests as well —
`combine_never_widens`, the digest tests, the dangling-reference test, the
stale-plan test — since a structural invariant in one crate still has to
survive the boundary to the next.

- **`LBL-6`, no permissive delta.** `Label::combine` (`label.rs`) takes the
  minimum trust and intersects the audience, and it is the only operation by
  which one label affects another. A raise is therefore not expressible as a
  fold: a sanitizer relabels a *new* derived value, a cast establishes a
  dimension that was never set, and a ruling never touches a label at all
  (`LBL-9` — `execute.rs` has no path that writes one).
- **`SAN-7`, constant xor resolver.** `CastResolution` is an enum
  (`authority.rs`), so a cast declaring both is unrepresentable *inside the
  engine*. The TOML surface still carries two optional fields, so
  `RawCast::convert` (`appa-runtime/src/config.rs`) rejects both-and-neither
  at load — the type ends the question one layer in, not at the boundary.
- **`RUL-3`, a ruling binds one rendered call.** `ResolvedCall` derives its
  `CanonicalDigest` on demand and never stores it (`value.rs`), so a value
  round-tripped through `serde` cannot carry a digest belonging to different
  arguments.
- **`RUL-8`, the staged review.** `AuthorityRequest` has private fields and a
  validating constructor (`external.rs`), so a request naming a dangling or
  foreign value reference cannot be built.
- **`RMD-8`, offers die with their turn.** Plans re-derive and match by
  value, so a stale handle mismatches rather than retargeting a live block.

Type-level enforcement stops at the caller's signature, so lifecycle
ordering is a runtime refusal instead — encoding it as typestate would put
generics on every public function. `IMP-4` asks for that refusal at one
choke point, and the crates do not yet manage it: at-most-once is checked in
both `submit_child_return` and `check_child_return` (`branch.rs`), and the
dispatch guard in both `observe_success` and `admit_result` (`admit.rs`).
The store's append point validates revisions, not admission.

## What is not implemented

`spec.md` marks these inline; collected here for anyone diffing code against
the document.

| item | spec |
|---|---|
| input sanitizers | `SAN-3`, refused at load |
| the two-outcome check with runtime-driven resolution — the shipped check still returns a third `unresolved` outcome and leaves casting to the host | `CHK-1`, `CHK-16` |
| a sanitizer mandate on the trust dimension — `Sanitizer::can_reduce` is audience-typed on both ends, and `admit.rs` copies the raw's trust through | `SAN-4` |
| quarantine-exit attestation | §10.1, design direction |
| membership resolvers for named groups | `LBL`, design direction |
| argument payload in the staged review — `AuthorityRequest` carries identity and typed context only | `RUL-9` |
| remedy-plan ordering and sticky denials | `RMD-15`, `RMD-16` |
| external interface protocols | §13, placeholder |
| a durable log — `SessionStore` is in-memory and `Mediator` owns it directly, so there is no pluggable destination either | `IMP-3`, `LOG-9` |
| a live HITL queue — `AuthorityBackend::Hitl` abstains on every request, fail-closed per `EXT-1` | `CFG-15` |
| dynamic contracts — `RecipientSpec` has only `Static` and `Placeholder`, and `RawTool` denies unknown fields, so the dialect cannot express a resolver-backed recipient | `CFG-14` |
| one admission choke point — at-most-once and the dispatch guard are each enforced at two call sites | `IMP-4` |

The inverse gap — code the spec no longer describes, removal pending: the
per-tool `output_sanitizer` binding, the `[[preamble]]` table (the
transcript head is host configuration per `POS-6`), the `can_reduce`
key the surface now spells `mandate` (`CFG-15`), and the
`resolver = { channel = "hitl" }` spelling HITL now writes as
`builtin = "hitl"` (`CFG-15`).
