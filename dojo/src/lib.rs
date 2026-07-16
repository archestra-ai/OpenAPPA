//! # appa-dojo
//!
//! A Rust-native substrate for an [AgentDojo]-style prompt-injection benchmark,
//! wired directly to the in-process [`appa_core`] information-flow policy engine.
//!
//! AgentDojo is built from four pieces: an **agent**, a **tools runtime**, a
//! mutable **environment** (the state tools read and write), and a generator of
//! **user tasks** (utility) and **injection tasks** (security). This crate
//! provides the reusable substrate — environment, tools, agent loop — plus a
//! direct OpenAPPA policy gate and the scoring abstractions, and deliberately
//! leaves the authored task catalog to a later slice.
//!
//! The vocabulary maps as follows:
//!
//! | AgentDojo | here |
//! |---|---|
//! | environment (`TaskEnvironment`) | your own `W` — any mutable struct |
//! | tools (functions over the environment) | [`Toolset<W>`] of [`Tool<W>`] |
//! | agent pipeline (the tool-calling loop) | [`Agent`] over a rig-core [`Model`] |
//! | a defense that gates tool calls | [`AppaGate`] (direct [`appa_core`]) |
//! | utility / security checkers | [`UtilityCheck`] / [`SecurityCheck`] |
//! | attack (indirect prompt injection) | [`Attack`] + [`InjectionVector`] |
//!
//! ## Three-step authoring
//!
//! ```no_run
//! # use appa_dojo::{Toolset, Agent, model};
//! # use serde_json::json;
//! # #[derive(Clone)] struct Mailbox { emails: Vec<String>, sent: Vec<String> }
//! # async fn demo() -> Result<(), appa_dojo::DojoError> {
//! let mut ws = Mailbox { emails: vec!["hi".into()], sent: vec![] };
//! let tools = Toolset::<Mailbox>::new()
//!     .tool("read_inbox", "List all emails", json!({"type": "object", "properties": {}}),
//!           |ws, _args| Ok(json!(ws.emails)))
//!     .finalize()?;
//! let model = model::from_env("anthropic/claude-3.5-sonnet")?;
//! let run = Agent::new(&model).run(&mut ws, &tools, "Summarize my inbox").await?;
//! println!("{}", run.final_text);
//! # Ok(())
//! # }
//! ```
//!
//! [AgentDojo]: https://github.com/ethz-spylab/agentdojo

pub mod agent;
pub mod error;
pub mod model;
pub mod policy;
pub mod scenarios;
pub mod scoring;
pub mod suite;
pub mod tool;

/// Re-exported so callers can construct OpenAPPA contracts ([`appa_core::ToolContract`],
/// [`appa_core::Requirements`], labels) for [`AppaGate`] without a separate dependency.
pub use appa_core;

pub use agent::{Agent, AgentRun, StopReason, ToolCallRecord, ToolOutcome};
pub use error::DojoError;
pub use model::Model;
pub use policy::{AppaGate, AppaGateBuilder, EmissionVerdict, GateVerdict};
pub use scoring::{
    Attack, EmptyCohort, Episode, ImportantInstructions, InjectionVector, Metrics, SecurityCheck, UtilityCheck,
    run_episode,
};
pub use suite::{Case, Scores, table};
pub use tool::{Tool, ToolError, Toolset};
