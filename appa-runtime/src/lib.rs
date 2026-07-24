//! # appa-runtime — the OpenAPPA confining agent-executor
//!
//! The runtime owns everything the pure [`appa_engine`] refuses to. It has three faces:
//! **north** an OpenAI-compatible `/v1/chat/completions` wire to a thin harness; **upstream**
//! an inference client to the model; **south** it *executes* tool calls against registered tool
//! backends. The harness sends a user turn; the runtime drives inference, tool execution, and
//! policy mediation, looping internally until the model yields a final assistant message.
//!
//! Concretely it reads the TOML policy config and builds the engine registry, resolves a
//! proposed call into an immutable `ResolvedCall`, owns the in-memory append-only event log
//! (the sole source of truth per trajectory id) with serialized conditional append, and holds
//! the external authority/sanitizer/cast/audience/tool implementations.
//!
//! Because the runtime executes tools, it can withhold a raw result and surface only a
//! sanitized/cast derivative on the bound paths — confinement of the model's reads on those
//! paths and of tool sinks. See `docs/rebuild-plan.md` for the full architecture.

pub mod admission;
pub mod config;
pub mod drive;
pub mod external;
pub mod feedback;
pub mod inference;
pub mod runtime;
pub mod server;
pub mod store;
pub mod tool;
pub mod transcript;
pub mod wire;
