//! appa-runtime — the process that gates a harness's flows.

pub mod api;
pub mod config;
pub mod hooks;
pub mod mcp;
pub mod tls;

mod builtins;
mod consult;
mod elicit;
mod engine;
mod external;
mod llm;
