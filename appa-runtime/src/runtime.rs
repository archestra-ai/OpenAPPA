//! Runtime assembly: the long-lived server object that owns the pure [`Engine`], the session store,
//! the upstream inference client, and the per-name external backends built from the validated
//! [`Config`].

use std::collections::BTreeMap;
use std::time::Duration;

use appa_engine::engine::Engine;
use appa_engine::names::{AuthorityName, CastName, SanitizerName};
use appa_engine::value::ToolName;
use thiserror::Error;

use crate::config::{AuthorityImpl, CastImpl, Config, SanitizerImpl, ToolImpl};
use crate::external::{AuthorityBackend, CastBackend, SanitizerBackend};
use crate::inference::Inference;
use crate::store::SessionStore;
use crate::tool::{BuiltinTool, DEFAULT_BODY_CAP_BYTES, HttpClient, HttpTool, ToolBackend};
use crate::wire::{WireMessage, WireTool, WireToolSchema};

/// The server-owned reserved tool every session pins: run a remedy plan the engine offered for a
/// blocked call. Server-handled, never dispatched south, never client-injectable.
pub const EXECUTE_REMEDY_PLAN: &str = "execute_remedy_plan";
pub const SUBMIT_RESULT: &str = "submit_result";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InitError {
    #[error("registered tool {0} has no execution backend (no `http` impl and no builtin supplied)")]
    UncoveredTool(String),
    #[error("registered tool {0} collides with a server-owned reserved tool name")]
    ReservedToolConflict(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub max_inference_rounds: u32,
    pub max_tool_invocations: u32,
    pub max_remedy_attempts_per_gap: u32,
    pub per_external_timeout: Duration,
    pub turn_deadline: Duration,
    pub body_cap_bytes: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            max_inference_rounds: 16,
            max_tool_invocations: 32,
            max_remedy_attempts_per_gap: 2,
            per_external_timeout: Duration::from_secs(30),
            turn_deadline: Duration::from_secs(120),
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
        }
    }
}

pub struct Runtime {
    config: Config,
    engine: Engine,
    store: SessionStore,
    inference: Inference,
    tool_backends: BTreeMap<ToolName, ToolBackend>,
    authority_backends: BTreeMap<AuthorityName, AuthorityBackend>,
    sanitizer_backends: BTreeMap<SanitizerName, SanitizerBackend>,
    cast_backends: BTreeMap<CastName, CastBackend>,
    preamble: Vec<WireMessage>,
    budgets: Budgets,
}

impl Runtime {
    /// Assemble a runtime. `builtin_tools` supplies backends for registered tools that carry no `http`
    /// impl (test fixtures and self-contained deployments); production all-`http` policies pass an
    /// empty map. Fails closed if any registered tool is left without a backend or shadows a reserved
    /// name.
    pub fn new(
        config: Config,
        inference: Inference,
        builtin_tools: BTreeMap<ToolName, BuiltinTool>,
    ) -> Result<Runtime, InitError> {
        Self::with_options(config, inference, builtin_tools, Vec::new(), Budgets::default())
    }

    /// Assemble with an explicit server preamble and budgets — the wiring the north handler and tests
    /// drive through.
    pub fn with_options(
        config: Config,
        inference: Inference,
        mut builtin_tools: BTreeMap<ToolName, BuiltinTool>,
        preamble: Vec<WireMessage>,
        budgets: Budgets,
    ) -> Result<Runtime, InitError> {
        let client = HttpClient::new();
        let engine = Engine::new(config.registry().clone());

        let mut tool_backends = BTreeMap::new();
        for contract in engine.registry().tools() {
            let name = &contract.name;
            if is_reserved(name.as_str()) {
                return Err(InitError::ReservedToolConflict(name.as_str().to_string()));
            }
            let backend = match config.tool_impl(name) {
                Some(ToolImpl::Http { url, timeout_ms }) => ToolBackend::Http(HttpTool::new(
                    url.clone(),
                    Duration::from_millis(*timeout_ms),
                    client.clone(),
                )),
                None => match builtin_tools.remove(name) {
                    Some(builtin) => ToolBackend::Builtin(builtin),
                    None => return Err(InitError::UncoveredTool(name.as_str().to_string())),
                },
            };
            tool_backends.insert(name.clone(), backend);
        }

        let mut authority_backends = BTreeMap::new();
        for authority in engine.registry().authorities() {
            let backend = match config.authority_impl(&authority.name) {
                Some(AuthorityImpl::Builtin(b)) => AuthorityBackend::Builtin(*b),
                Some(AuthorityImpl::HttpResolver { url, timeout_ms }) => AuthorityBackend::Http {
                    url: url.clone(),
                    timeout: Duration::from_millis(*timeout_ms),
                    client: client.clone(),
                },
                // Every authority carries an impl by config construction; absence is fail-closed HITL.
                Some(AuthorityImpl::Hitl) | None => AuthorityBackend::Hitl,
            };
            authority_backends.insert(authority.name.clone(), backend);
        }

        let mut sanitizer_backends = BTreeMap::new();
        let mut cast_backends = BTreeMap::new();
        for sanitizer in &config.registry_config().sanitizers {
            if let Some(imp) = config.sanitizer_impl(&sanitizer.name) {
                let backend = match imp {
                    SanitizerImpl::Builtin(b) => SanitizerBackend::Builtin(*b),
                    SanitizerImpl::HttpResolver { url, timeout_ms } => SanitizerBackend::Http {
                        url: url.clone(),
                        timeout: Duration::from_millis(*timeout_ms),
                        client: client.clone(),
                    },
                };
                sanitizer_backends.insert(sanitizer.name.clone(), backend);
            }
        }
        for cast in &config.registry_config().casts {
            // Only resolver casts bind a backend; a constant cast is resolved engine-side.
            if let Some(CastImpl::HttpResolver { url, timeout_ms }) = config.cast_impl(&cast.name) {
                cast_backends.insert(
                    cast.name.clone(),
                    CastBackend::Http {
                        url: url.clone(),
                        timeout: Duration::from_millis(*timeout_ms),
                        client: client.clone(),
                    },
                );
            }
        }

        Ok(Runtime {
            config,
            engine,
            store: SessionStore::new(),
            inference,
            tool_backends,
            authority_backends,
            sanitizer_backends,
            cast_backends,
            preamble,
            budgets,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    pub fn inference(&self) -> &Inference {
        &self.inference
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    pub fn preamble(&self) -> &[WireMessage] {
        &self.preamble
    }

    pub fn tool_backend(&self, name: &ToolName) -> Option<&ToolBackend> {
        self.tool_backends.get(name)
    }

    pub fn authority_backend(&self, name: &AuthorityName) -> Option<&AuthorityBackend> {
        self.authority_backends.get(name)
    }

    pub fn sanitizer_backend(&self, name: &SanitizerName) -> Option<&SanitizerBackend> {
        self.sanitizer_backends.get(name)
    }

    pub fn cast_backend(&self, name: &CastName) -> Option<&CastBackend> {
        self.cast_backends.get(name)
    }

    /// The tools advertised to the model for a session: every registered policy tool plus the reserved
    /// server tools (`submit_result` only for a child session). Schemas are server-owned (RP1).
    pub fn advertised_tools(&self, is_child: bool) -> Vec<WireTool> {
        let mut tools: Vec<WireTool> = self
            .engine
            .registry()
            .tools()
            .map(|contract| reserved_schema(contract.name.as_str()))
            .collect();
        tools.push(reserved_schema(EXECUTE_REMEDY_PLAN));
        if is_child {
            tools.push(reserved_schema(SUBMIT_RESULT));
        }
        tools
    }
}

fn is_reserved(name: &str) -> bool {
    name == EXECUTE_REMEDY_PLAN || name == SUBMIT_RESULT
}

fn reserved_schema(name: &str) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: name.to_string(),
            description: None,
            parameters: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inference() -> Inference {
        Inference::new(
            "http://127.0.0.1:1",
            "k",
            "m",
            Duration::from_secs(1),
            HttpClient::new(),
        )
    }

    const ECHO_TOOL_CONFIG: &str = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "get_logs"
"#;

    const HTTP_TOOL_CONFIG: &str = r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "get_logs"
[tool.implementation.http]
url = "http://tools.internal/get_logs"
timeout_ms = 5000
"#;

    #[test]
    fn a_builtin_covers_a_backendless_tool() {
        let config = Config::from_toml_str(ECHO_TOOL_CONFIG).unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("get_logs"), BuiltinTool::Echo("ok".to_string()));
        let runtime = Runtime::new(config, inference(), builtins).unwrap();
        assert!(runtime.tool_backend(&ToolName::new("get_logs")).is_some());
    }

    #[test]
    fn an_http_tool_binds_its_backend() {
        let config = Config::from_toml_str(HTTP_TOOL_CONFIG).unwrap();
        let runtime = Runtime::new(config, inference(), BTreeMap::new()).unwrap();
        assert!(matches!(
            runtime.tool_backend(&ToolName::new("get_logs")),
            Some(ToolBackend::Http(_))
        ));
    }

    #[test]
    fn a_tool_without_any_backend_is_a_load_error() {
        let config = Config::from_toml_str(ECHO_TOOL_CONFIG).unwrap();
        match Runtime::new(config, inference(), BTreeMap::new()) {
            Err(InitError::UncoveredTool(name)) => assert_eq!(name, "get_logs"),
            _ => panic!("expected UncoveredTool"),
        }
    }

    #[test]
    fn a_policy_tool_may_not_shadow_a_reserved_name() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "submit_result"
[tool.implementation.http]
url = "http://tools.internal/x"
timeout_ms = 1000
"#,
        )
        .unwrap();
        match Runtime::new(config, inference(), BTreeMap::new()) {
            Err(InitError::ReservedToolConflict(name)) => assert_eq!(name, "submit_result"),
            _ => panic!("expected ReservedToolConflict"),
        }
    }

    #[test]
    fn advertised_tools_pin_reserved_and_gate_submit_result_on_child() {
        let config = Config::from_toml_str(HTTP_TOOL_CONFIG).unwrap();
        let runtime = Runtime::new(config, inference(), BTreeMap::new()).unwrap();

        let names = |is_child| -> Vec<String> {
            runtime
                .advertised_tools(is_child)
                .into_iter()
                .map(|t| t.function.name)
                .collect()
        };
        let root = names(false);
        assert!(root.contains(&"get_logs".to_string()));
        assert!(root.contains(&EXECUTE_REMEDY_PLAN.to_string()));
        assert!(!root.contains(&SUBMIT_RESULT.to_string()));
        assert!(names(true).contains(&SUBMIT_RESULT.to_string()));
    }
}
