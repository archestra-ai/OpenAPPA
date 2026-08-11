# appa-runtime-v2

The process that gates a harness's flows. `docs/runtime.md` is the
contract for this crate; read it first. Where this crate and that
contract conflict, the contract wins.

## Mock engine status

This crate targets the engine boundary that `docs/engine.md` sketches:
`Engine::handle(&EngineView, EngineEvent) -> EngineDecision`. The real
engine does not expose this API yet — the engine team is building it.
`src/mock_engine.rs` holds a temporary copy of the boundary types and
the `MockEngine` with two modes:

- **Test mode.** A test enqueues the exact decision for each event; the
  queue is the behavior. The mock has no decision logic.
- **Default mode.** The binary's mode. The mock permits every call and
  admits every result. `Runtime::open` warns loudly: a mock engine is
  deciding, and no policy is enforced.

## Integration plan

When the engine team publishes the boundary types: delete
`src/mock_engine.rs`, import their types, and let the compiler drive
the fixes. Differences from this copy are expected; the planned cost is one
module plus compiler-led fixes. Only the `Session` event handlers in
`src/api/` call the boundary — adapters and the store never do (`IMP-1`;
a test enforces this) — so the swap stays inside `api` and
`mock_engine`.

## Mock-era openness

The boundary types — `ValidatedFactBatch` included — are plainly
constructible. This is by design, not an oversight: in test mode the
enqueued decision IS the engine's behavior, so tests must build
decisions and batches freely. The sealed-batch property of `IMP-4`
belongs to the real engine's published types and arrives with them at
integration. Nothing outside this crate consumes the library surface.

## Crate rules

- Adapters carry content, never labels: no label
  type and no engine type in a public signature.
- The store sees a fact batch as bytes plus a revision; the runtime never reads fact contents and stores no view
  and no transcript.
- Appends compare-and-swap on the log revision; a decision
  from a stale view never acts.
- rusqlite sits behind a std Mutex, never held across an await.
- The `/hook` listener trusts loopback: harness binding accepts the
  harness's events by construction, and every same-user
  loopback process sits inside that trusted host boundary. Isolating
  the wire further (a unix socket, a shared token) is a deployment
  hardening decision, not taken yet.
- External calls fail closed: no answer grants no permission.
