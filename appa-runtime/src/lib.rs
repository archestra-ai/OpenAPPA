//! appa-runtime — the process that gates a harness's flows.

#[cfg(feature = "daemon")]
mod agent_scan;
#[cfg(feature = "daemon")]
mod annotate;
pub mod api;
pub mod batteries;
#[cfg(feature = "daemon")]
pub mod claude_files;
pub mod config;
#[cfg(feature = "daemon")]
mod default_config;
pub mod describe;
#[cfg(feature = "daemon")]
pub mod file_ledger;
#[cfg(feature = "daemon")]
pub mod hook_client;
pub mod hooks;
#[cfg(feature = "daemon")]
pub mod init;
#[cfg(feature = "daemon")]
pub mod installation;
#[cfg(feature = "daemon")]
mod loopback_http;
pub mod managed_files;
#[cfg(feature = "daemon")]
mod management;
#[cfg(feature = "daemon")]
pub mod mcp;
#[cfg(feature = "daemon")]
pub mod replay;
#[cfg(feature = "daemon")]
#[path = "main.rs"]
pub mod runtime_cli;
#[cfg(feature = "daemon")]
pub mod runtime_start;
#[cfg(feature = "daemon")]
pub mod runtime_url;
#[cfg(feature = "daemon")]
pub mod session_context;
#[cfg(feature = "daemon")]
pub mod statusline;
pub mod tls;
pub mod tool_validation;

mod batteries_layout;
#[cfg(feature = "daemon")]
mod batteries_staging;
mod builtins;
mod consult;
mod elicit;
mod engine;
mod events;
mod external;
mod llm;
mod recorder;
pub mod yell;
