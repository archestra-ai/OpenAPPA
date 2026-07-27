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
| `appa-runtime` | outer | the canonical mediation assembly: owns policy, engine, trajectory families, and the durable log |
| `appa-gateway` | host | OpenAI-compatible `/v1/chat/completions` adapter |
| `appa-sdk` | host | narrow facade for frameworks that own their own inference and tool execution |
| `appa-agent` | host | a provider and serial agent loop over the runtime's `Mediator` |

A harness author embeds `appa-runtime` with whatever store they already run.
They implement neither layer.

## Where each rule family lives

| family | module |
|---|---|
| `LBL` | `appa-engine/src/label.rs`, `value.rs` |
| `CHK` | `check.rs`, `admit.rs` |
| `RMD` | `plan.rs`, `execute.rs` |
| `AUT`, `RUL` | `authority.rs` |
| `SAN` | `authority.rs`, `admit.rs` |
| `LOG` | `fact.rs`, `projection.rs` |
| `BRN` | `branch.rs` |
| `UNK` | `admit.rs`, `label.rs` |
| `CFG` | `contract.rs`, `registry.rs`, `names.rs` |

## Invariants the code carries that prose cannot

Some spec rules are enforced by Rust's type system rather than by a runtime
check, and a second implementation in another language has to find its own
way to hold them:

- **Linearity of values.** No `Clone`, no `Deserialize`, no public
  constructor, consumed by value. Ownership and omitted derives do the
  enforcement — there is no typestate anywhere.
- **Lifecycle ordering** — no double release, no completion before release —
  is refused at event admission, which is the single choke point. Encoding
  it as typestate would infect every signature with generics, and that trade
  was made once and stands.
- **`ValueStore` mutators stay `pub(crate)`**, and read-only audit and
  projection types are never hoisted into the root re-exports.

## What is not implemented

`spec.md` marks these inline; collected here for anyone diffing code against
the document.

| item | spec |
|---|---|
| compiled composites | §10.2, deferred |
| input sanitizers | `SAN-3`, refused at load |
| quarantine-exit attestation | §10.1, design direction |
| membership resolvers for named groups | `LBL`, design direction |
| leaf-level provenance in the staged review | `RUL-9`, design direction |
| external interface protocols | §13, placeholder |
