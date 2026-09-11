//! appa-runtime — the process that gates a harness's flows.

mod agent_scan;
pub mod api;
pub mod batteries;
pub mod config;
mod default_config;
pub mod describe;
pub mod hook_client;
pub mod hooks;
pub mod init;
pub mod installation;
mod loopback_http;
mod management;
pub mod mcp;
pub mod replay;
#[path = "main.rs"]
pub mod runtime_cli;
pub mod runtime_start;
pub mod runtime_url;
pub mod session_context;
pub mod statusline;
pub(crate) mod telemetry;
pub mod tls;
pub mod tool_validation;

mod batteries_layout;
mod builtins;
mod consult;
mod elicit;
mod engine;
mod events;
mod external;
mod llm;
pub mod yell;
