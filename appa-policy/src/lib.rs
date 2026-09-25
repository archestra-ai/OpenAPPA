//! The spec's policy-dialect compiler: the configuration dialect (TOML) → the engine's
//! [`RegistryConfig`](appa_engine::registry::RegistryConfig) for the runtime.

mod annotator;
mod audience;
mod config;
mod convert;
mod error;
mod raw;
#[cfg(test)]
mod tests;

pub use annotator::{AnnotatorBinding, AnnotatorBuiltin, InputSource, ToolCallSource};
pub use audience::{SelectorDeclaration, declare_templates, declared_sources};
pub use config::Config;
pub use convert::parse_delta;
pub use error::ConfigError;
