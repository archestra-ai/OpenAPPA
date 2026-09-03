# appa-runtime

The process that gates a harness's flows. This file says what lives
where.

The log itself is not here: `appa-eventlog` at the repository root owns the
trajectory log and the stored policy files, and with them the record encoding,
the database, and the conditional append. This folder names no SQL.

Three crates, one binary, one process. Two of them are siblings of this
one at the repository root:

- `../appa-runtime-api/` — the vocabulary the runtime and its adapters
  share: `HookEvent`, `HookDecision`, the `Codec` of two plain fn
  pointers, and the content types. Pure types, deps serde/serde_json
  only.
- `../appa-adapter-claude-code/` — the Claude Code codec: hook JSON to
  `HookEvent`, `HookDecision` to hook wire JSON. It depends only on
  `appa-runtime-api`, so the boundary is compiler-enforced: an adapter
  cannot call the runtime, hold state, or see a dispatch id.
- this crate (package and binary `appa`) —
  the native lifecycle/description CLI and everything else: the runtime API and internal `Session` event model,
  the `hooks` dispatcher, the HTTP server, the MCP endpoint, the
  externals, the builtin modules (`builtins.rs` — stock implementations
  plus the `--modules-dir` loader over the `appa-builtin` ABI crate at
  the repo root), and the engine boundary (`src/engine.rs`, which
  translates and presents every engine decision; `api/mod.rs` and
  `api/session.rs` also name `appa-engine`), and `appa replay`
  (`src/replay.rs`: trace files parsed into typed hook events and run
  through the dispatcher over an in-memory log; the shipped traces live
  in `examples/tests/` at the repository root). It keeps no durable
  state of its own beside the log.

The Claude Code plugin, the marketplace manifest, and the example
policies are not code and live in `integrations/claude-code/` at the
repository root. The tests here still run those shipped files
(`tests/plugin_hooks.rs`, `tests/examples_load.rs`).
