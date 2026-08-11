//! appa-runtime-v2 — the process that gates a harness's flows.

pub mod api;
pub mod config;
pub mod hooks;
pub mod mcp;

mod external;
#[cfg_attr(not(test), allow(dead_code))]
mod mock_engine;
mod store;
