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

## Engine boundary status

`runtime/src/engine.rs` is the one module that speaks to `appa-engine`.
It drives the real engine through today's composed operations — check,
plan, open-dispatch, execute-remedy, observe-success, admit-result,
seed-child, child-return — behind two functions the session calls:
decode-and-validate the persisted log, then decide one event. The
decode step runs `Engine::validate_replay` before any projection is
built, so it is the store-reopen trust gate. `T31`'s
published `handle` boundary replaces the composition when it lands;
the swap stays inside `api` and `engine` (`IMP-1`; a structural
source-scan test enforces that no other module names either).

`Runtime::open` compiles `[policy]` through the runtime-v2-only
`appa-policy` declaration compiler. Implementation bindings live in
`[externals]`, never inline; the compiler rejects every inline site.
Runtime open then refuses what this
runtime cannot honor: pending-cast deltas, `[[cast]]` declarations,
reserved control-tool names, and policy-named externals with no
`[externals]` binding.

Interims, each recorded in `docs/engine.md`: offers carry a durable
id→trajectory routing row, but the offered payload is process state —
a restart declines pending offers, and execution never trusts the
cache (live re-plan, value match, `RMD-8`); the fork seed is the
scalar interim (`T39`); no cast resolution is wired, so blocks on
unestablished dimensions are terminal feedback.

The seam's test variant (`EngineSeam::Test`, cfg(test)-only) returns
enqueued decisions so session-orchestration tests pin commit ordering,
conflict replay, and evidence loops without engine policy; the
real-engine behavioral tests beside the session pin policy behavior
against compiled fixtures. `FactBatch` remains publicly constructible
until `T31` seals it; nothing outside the runtime crate consumes the
boundary, and the crate's public surface is `Runtime::open`, the
`hooks` dispatcher, `config`, and the MCP service.

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
