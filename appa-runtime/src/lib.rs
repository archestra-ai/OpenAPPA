//! appa-runtime — the process that gates a harness's flows.

pub mod api;
pub mod config;
mod default_config;
pub mod describe;
pub mod hook_client;
pub mod hooks;
pub mod init;
pub mod mcp;
pub mod plugin_bundle;
#[path = "main.rs"]
pub mod runtime_cli;
pub mod tls;

mod builtins;
mod consult;
mod elicit;
mod engine;
mod external;
mod llm;
mod plugin_layout;
