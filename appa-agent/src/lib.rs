//! A concrete OpenAI-compatible provider and serial agent loop over [`appa_runtime::Mediator`].

mod agent;
mod provider;

pub use agent::{Agent, AgentError, Outcome};
pub use appa_runtime::TrajectoryId as SessionId;
pub use appa_runtime::store::TenantId;
pub use provider::{
    DEFAULT_COMPLETION_BODY_CAP_BYTES, Endpoint, ModelId, OpenAiCompatible, OpenAiConfig, ProviderError,
};
