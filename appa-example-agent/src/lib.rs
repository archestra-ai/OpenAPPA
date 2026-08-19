//! `appa-example-agent` — a protoagent on the runtime.
//!
//! The runtime does not run a loop and does not want one: the harness
//! owns inference, tool execution and the transcript, and the runtime
//! answers one question in front of every flow. This crate is a harness of that shape, for hosts that
//! embed the runtime rather than serving hooks to an editor.
//!
//! It drives the same public dispatcher a hook adapter reaches —
//! `appa_runtime::hooks::handle`, with typed events — so there is no
//! second event model and no wire between the two. What the runtime
//! deliberately does not hold, the agent does: the transcript, the tool
//! catalogue, the budget, and the parent/child stack.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use appa_example_agent::{Agent, ToolCatalogue, ToolShim, OpenAiCompatible};
//! # use appa_runtime::api::{Runtime, TrajectoryId};
//! # async fn example(runtime: Arc<Runtime>, tools: Vec<appa_example_agent::wire::WireTool>) {
//! let agent = Agent::new(
//!     runtime,
//!     OpenAiCompatible::openrouter("some/model", "sk-..."),
//!     ToolShim::new("http://127.0.0.1:9000/"),
//!     ToolCatalogue::new(tools),
//! );
//! let outcome = agent
//!     .run(
//!         TrajectoryId("run-1".to_string()),
//!         "Summarize this quarter's tickets.",
//!         Default::default(),
//!     )
//!     .await;
//! # let _ = outcome;
//! # }
//! ```

mod agent;
mod budget;
mod http;
mod provider;
mod record;
mod tools;
pub mod wire;

pub use agent::{Agent, ArgumentKey, Outcome, SpawnTool, StopReason, ToolName, Transcript, TranscriptHead};
pub use budget::Limits;
pub use http::HttpClient;
pub use provider::{
    DEFAULT_COMPLETION_BODY_CAP_BYTES, Endpoint, ModelId, OpenAiCompatible, OpenAiConfig, ProviderError,
};
pub use record::{CallId, Record, Recorded};
pub use tools::{DEFAULT_TOOL_BODY_CAP_BYTES, ToolCatalogue, ToolShim};
