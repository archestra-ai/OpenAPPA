//! Gateway assembly over the canonical runtime mediator and agent loop.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use appa_agent::Agent;
pub use appa_runtime::TranscriptHead;
use appa_runtime::store::SessionStore;
use appa_runtime::tool::{BuiltinTool, DEFAULT_BODY_CAP_BYTES};
use appa_runtime::wire::WireTool;

use appa_runtime::{Config, Limits, Mediator, ToolName};

use crate::inference::Inference;

pub use appa_runtime::InitError;
pub use appa_runtime::tool::{EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub max_inference_rounds: u32,
    pub max_tool_invocations: u32,
    pub max_blocked_proposals_per_call: u32,
    pub per_external_timeout: Duration,
    pub turn_deadline: Duration,
    pub body_cap_bytes: usize,
}

const DEFAULT_MAX_FORKS: u32 = 8;
const DEFAULT_MAX_FORK_DEPTH: u32 = 4;

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            max_inference_rounds: 16,
            max_tool_invocations: 32,
            max_blocked_proposals_per_call: 2,
            per_external_timeout: Duration::from_secs(30),
            turn_deadline: Duration::from_secs(120),
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
        }
    }
}

impl From<Budgets> for Limits {
    fn from(budgets: Budgets) -> Self {
        Limits {
            max_inference_rounds: budgets.max_inference_rounds,
            max_tool_invocations: budgets.max_tool_invocations,
            max_blocked_proposals_per_call: budgets.max_blocked_proposals_per_call,
            per_external_timeout: budgets.per_external_timeout,
            run_deadline: budgets.turn_deadline,
            body_cap_bytes: budgets.body_cap_bytes,
            max_forks: DEFAULT_MAX_FORKS,
            max_fork_depth: DEFAULT_MAX_FORK_DEPTH,
        }
    }
}

pub struct Runtime {
    mediator: Arc<Mediator>,
    inference: Inference,
    budgets: Budgets,
}

impl Runtime {
    /// `head` is this host's transcript head — the system/developer messages opening
    /// every model request. It is host configuration, so the gateway takes it from its embedder
    /// rather than reading it out of the policy.
    pub fn new(
        config: Config,
        inference: Inference,
        builtin_tools: BTreeMap<ToolName, BuiltinTool>,
        head: TranscriptHead,
    ) -> Result<Runtime, InitError> {
        Ok(Runtime {
            mediator: Arc::new(Mediator::new(config, builtin_tools)?.with_transcript_head(head)),
            inference,
            budgets: Budgets::default(),
        })
    }

    pub fn config(&self) -> &Config {
        self.mediator.config()
    }

    pub fn store(&self) -> &SessionStore {
        self.mediator.store()
    }

    pub fn mediator(&self) -> &Arc<Mediator> {
        &self.mediator
    }

    pub fn inference(&self) -> &Inference {
        &self.inference
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Compatibility tool surface for a root or direct child role.
    pub fn advertised_tools(&self, is_child: bool) -> Vec<WireTool> {
        let minimum_depth = u32::from(is_child);
        let can_fork = DEFAULT_MAX_FORKS > 0 && minimum_depth < DEFAULT_MAX_FORK_DEPTH;
        self.mediator.advertised_tools(is_child, can_fork)
    }

    pub(crate) fn max_fork_depth(&self) -> u32 {
        DEFAULT_MAX_FORK_DEPTH
    }

    pub(crate) fn agent(&self) -> Agent {
        Agent::new(self.mediator.clone(), self.inference.provider(), self.budgets.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime::tool::HttpClient;

    #[test]
    fn compatibility_surface_includes_fork_for_default_root_and_child_roles() {
        let runtime = Runtime::new(
            Config::from_toml_str("version = 1\n").expect("config parses"),
            Inference::new(
                "http://127.0.0.1:1",
                "key",
                "model",
                Duration::from_secs(1),
                HttpClient::new(),
            ),
            BTreeMap::new(),
            TranscriptHead::none(),
        )
        .expect("runtime assembles");
        let names = |is_child| {
            runtime
                .advertised_tools(is_child)
                .into_iter()
                .map(|tool| tool.function.name)
                .collect::<Vec<_>>()
        };

        assert_eq!(names(false), [EXECUTE_REMEDY_PLAN, FORK]);
        assert_eq!(names(true), [EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT]);
    }
}
