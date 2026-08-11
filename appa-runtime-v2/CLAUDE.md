# appa-runtime-v2

The process that gates a harness's flows. `docs/runtime.md` is the
contract for this folder; read it first. Where these crates and that
contract conflict, the contract wins.

Three crates, one binary, one process:

- `api/` (package `appa-runtime-api`) — the vocabulary the runtime and
  its adapters share: `HookEvent`, `HookDecision`, the `Codec` of two
  plain fn pointers, and the content types. Pure types, deps
  serde/serde_json only.
- `adapters/claude-code/` (package `appa-adapter-claude-code`) — the
  Claude Code codec: hook JSON to `HookEvent`, `HookDecision` to hook
  wire JSON. It depends only on `appa-runtime-api`, so the boundary is
  compiler-enforced: an adapter cannot call the runtime, hold state,
  or see a dispatch id.
- `runtime/` (package `appa-runtime-v2`, binary `appa-runtime-v2`) —
  everything else: the runtime API and internal `Session` event model,
  the `hooks` dispatcher, the HTTP server, the MCP endpoint, the
  store, the externals, and the engine boundary.

The plugin (`plugin/`) and the marketplace manifest stay at this
folder's top level, beside the crates.

## Mock engine status

The runtime crate targets the engine boundary that `docs/engine.md`
sketches: `Engine::handle(&EngineView, EngineEvent) -> EngineDecision`.
The real engine does not expose this API yet — the engine team is
building it. `runtime/src/mock_engine.rs` holds a temporary copy of the
boundary types and the `MockEngine` with three modes:

- **Test mode.** A test enqueues the exact decision for each event; the
  queue is the behavior. The mock has no decision logic.
- **Permissive mode.** The binary's default. The mock permits every
  call and admits every result. `Runtime::open` warns loudly: a mock
  engine is deciding, and no policy is enforced.
- **Offer mode** (`--mock offer`). The mock first blocks every call
  with a narrowing offer — exactly the proposed call — and authorizes
  it when the model executes the offer through `execute_remedy_plan`.
  It exercises the deny wire, the remedy round trip, and the fact-log
  append path, with still no policy enforced. Its state is
  its own fact batches; the runtime reads none of them.

## Integration plan

When the engine team publishes the boundary types: delete
`runtime/src/mock_engine.rs`, import their types, and let the compiler
drive the fixes. Differences from this copy are expected; the planned
cost is one module plus compiler-led fixes. Only the `Session` event
handlers in `runtime/src/api/` call the boundary — the dispatcher, the
adapters, and the store never do (`IMP-1`; a test enforces this) — so
the swap stays inside `api` and `mock_engine`.

## Mock-era openness

The boundary types — `ValidatedFactBatch` included — are plainly
constructible. This is by design, not an oversight: in test mode the
enqueued decision IS the engine's behavior, so tests must build
decisions and batches freely. The sealed-batch property of `IMP-4`
belongs to the real engine's published types and arrives with them at
integration. Nothing outside the runtime crate consumes the boundary:
the module is private, and the crate's public surface is
`Runtime::open`/`open_offer_mode`, the `hooks` dispatcher, `config`,
and the MCP service.

## Crate rules

- Adapters carry content, never labels: no label
  type and no engine type in the vocabulary. The dependency direction
  enforces the rest: an adapter sees only `appa-runtime-api`.
- Adapters carry no correlation state and no control-tool knowledge:
  the runtime matches outcomes to dispatches by canonical bytes
 and recognizes its own `execute_remedy_plan` wire names
 itself — in the `hooks` dispatcher before any
  session lookup, and in `Session` for callers that reach it directly.
  Dispatch identity never leaves the runtime crate.
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
