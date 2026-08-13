# appa-runtime-v2

The process that gates a harness's flows. The architecture — layers,
principles, the engine boundary, event semantics, the builtin-module
contract — lives in `docs/runtime.md`, part of the golden set and the
contract for this folder; read it first. Where these crates and that
contract conflict, the contract wins. This file only says what lives
where.

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
  store, the externals, the builtin modules (`builtins.rs` — stock
  implementations plus the `--modules-dir` loader over the
  `appa-builtin` ABI crate at the repo root), and the engine boundary
  (`runtime/src/engine.rs`, the one module that names `appa-engine`).

The Claude Code plugin, the marketplace manifest, and the example
policies are not code and live in `integrations/claude-code/` at the
repository root. The tests here still run those shipped files
(`runtime/tests/plugin_hooks.rs`, `runtime/tests/examples_load.rs`).
