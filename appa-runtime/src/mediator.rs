//! Canonical runtime assembly and trajectory-family ownership.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use appa_engine::contract::ToolContract;
use appa_engine::engine::Engine;
use appa_engine::fact::{Fact, Revision};
use appa_engine::label::Dim;
use appa_engine::names::{AuthorityName, CastName, SanitizerName};
use appa_engine::projection::Projection;
use appa_engine::registry::TrustChain;
use appa_engine::value::{ToolName, TrajectoryId};
use serde_json::json;
use thiserror::Error;
use tokio::sync::OwnedMutexGuard;

use crate::config::{AuthorityImpl, CastImpl, Config, SanitizerImpl, ToolImpl};
use crate::external::{AuthorityBackend, CastBackend, SanitizerBackend};
use crate::store::{SessionStore, StoreError, StoreIdentity, TenantId};
use crate::tool::{BuiltinTool, EXECUTE_REMEDY_PLAN, FORK, HttpClient, HttpTool, SUBMIT_RESULT, ToolBackend};
use crate::wire::{WireMessage, WireTool, WireToolSchema};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InitError {
    #[error("registered tool {0} has no execution backend (no `http` impl and no backend supplied)")]
    UncoveredTool(String),
    #[error("supplied backend for {0} is not an unimplemented registered policy tool")]
    UnexpectedSuppliedBackend(String),
    #[error("registered tool {0} collides with a runtime-owned reserved tool name")]
    ReservedToolConflict(String),
}

#[derive(Debug, Error)]
pub enum SessionForkError {
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
    #[error("fork depth {depth} reaches the configured maximum {maximum}")]
    DepthLimit { depth: u32, maximum: u32 },
}

#[must_use = "a reserved child must be admitted into a Turn"]
pub struct ForkedSession {
    pub(crate) session: TrajectoryId,
    pub(crate) lease: OwnedMutexGuard<()>,
    pub(crate) store_identity: StoreIdentity,
}

impl ForkedSession {
    pub fn session(&self) -> &TrajectoryId {
        &self.session
    }
}

/// The assembled policy mediator. It deliberately owns no inference client or turn state machine.
pub struct Mediator {
    config: Config,
    engine: Engine,
    store: SessionStore,
    tool_backends: BTreeMap<ToolName, ToolBackend>,
    authority_backends: BTreeMap<AuthorityName, AuthorityBackend>,
    sanitizer_backends: BTreeMap<SanitizerName, SanitizerBackend>,
    cast_backends: BTreeMap<CastName, CastBackend>,
    preamble: Vec<WireMessage>,
}

impl Mediator {
    pub fn new(config: Config, builtin_tools: BTreeMap<ToolName, BuiltinTool>) -> Result<Mediator, InitError> {
        let tool_backends = builtin_tools
            .into_iter()
            .map(|(name, backend)| (name, ToolBackend::Builtin(backend)))
            .collect();
        Self::with_tool_backends(config, tool_backends)
    }

    /// Assemble with exact concrete backends for policy tools lacking a configured HTTP
    /// implementation. Missing entries, extra entries, and attempts to replace config-owned HTTP
    /// implementations are load errors.
    pub fn with_tool_backends(
        config: Config,
        mut supplied_tool_backends: BTreeMap<ToolName, ToolBackend>,
    ) -> Result<Mediator, InitError> {
        let preamble = config.preamble().to_vec();
        let client = HttpClient::new();
        let engine = Engine::new(config.registry().clone());

        let mut tool_backends = BTreeMap::new();
        for contract in engine.registry().tools() {
            let name = &contract.name;
            if is_reserved(name.as_str()) {
                return Err(InitError::ReservedToolConflict(name.as_str().to_string()));
            }
            let backend = match config.tool_impl(name) {
                Some(ToolImpl::Http { url, timeout_ms }) => {
                    if supplied_tool_backends.remove(name).is_some() {
                        return Err(InitError::UnexpectedSuppliedBackend(name.as_str().to_string()));
                    }
                    ToolBackend::Http(HttpTool::new(
                        url.clone(),
                        Duration::from_millis(*timeout_ms),
                        client.clone(),
                    ))
                }
                None => supplied_tool_backends
                    .remove(name)
                    .ok_or_else(|| InitError::UncoveredTool(name.as_str().to_string()))?,
            };
            tool_backends.insert(name.clone(), backend);
        }
        if let Some(name) = supplied_tool_backends.into_keys().next() {
            return Err(InitError::UnexpectedSuppliedBackend(name.as_str().to_string()));
        }

        let mut authority_backends = BTreeMap::new();
        for authority in engine.registry().authorities() {
            let backend = match config.authority_impl(&authority.name) {
                Some(AuthorityImpl::Builtin(builtin)) => AuthorityBackend::Builtin(*builtin),
                Some(AuthorityImpl::HttpResolver { url, timeout_ms }) => AuthorityBackend::Http {
                    url: url.clone(),
                    timeout: Duration::from_millis(*timeout_ms),
                    client: client.clone(),
                },
                Some(AuthorityImpl::Hitl) | None => AuthorityBackend::Hitl,
            };
            authority_backends.insert(authority.name.clone(), backend);
        }

        let mut sanitizer_backends = BTreeMap::new();
        for sanitizer in &config.registry_config().sanitizers {
            if let Some(implementation) = config.sanitizer_impl(&sanitizer.name) {
                let backend = match implementation {
                    SanitizerImpl::Builtin(builtin) => SanitizerBackend::Builtin(*builtin),
                    SanitizerImpl::HttpResolver { url, timeout_ms } => SanitizerBackend::Http {
                        url: url.clone(),
                        timeout: Duration::from_millis(*timeout_ms),
                        client: client.clone(),
                    },
                };
                sanitizer_backends.insert(sanitizer.name.clone(), backend);
            }
        }

        let mut cast_backends = BTreeMap::new();
        for cast in &config.registry_config().casts {
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

        Ok(Mediator {
            config,
            engine,
            store: SessionStore::new(),
            tool_backends,
            authority_backends,
            sanitizer_backends,
            cast_backends,
            preamble,
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

    pub fn advertised_tools(&self, is_child: bool, can_fork: bool) -> Vec<WireTool> {
        let mut tools: Vec<WireTool> = self
            .engine
            .registry()
            .tools()
            .map(|contract| {
                policy_tool_schema(
                    contract,
                    self.engine.registry().trust_chain(),
                    self.config.tool_parameters(&contract.name).cloned(),
                )
            })
            .collect();
        tools.push(remedy_tool_schema(can_fork));
        if can_fork {
            tools.push(reserved_tool_schema(FORK));
        }
        if is_child {
            tools.push(reserved_tool_schema(SUBMIT_RESULT));
        }
        tools
    }

    pub fn create_session(&self, tenant: TenantId) -> TrajectoryId {
        self.store.create_session(tenant)
    }

    pub fn fork_session(&self, tenant: &TenantId, parent: &TrajectoryId) -> Result<TrajectoryId, StoreError> {
        let return_policy = self.config.child_return_policy();
        let (child, _) = self.store.fork(tenant, parent, |child, facts, revision| {
            let projection = Projection::build(facts, revision);
            self.engine.seed_child(&projection.view(parent), child, return_policy)
        })?;
        Ok(child)
    }

    pub fn fork_session_reserved(&self, tenant: &TenantId, parent: &TrajectoryId) -> Result<ForkedSession, StoreError> {
        let return_policy = self.config.child_return_policy();
        let (session, _, lease) = self.store.fork_reserved(tenant, parent, |child, facts, revision| {
            let projection = Projection::build(facts, revision);
            self.engine.seed_child(&projection.view(parent), child, return_policy)
        })?;
        Ok(ForkedSession {
            session,
            lease,
            store_identity: self.store.identity(),
        })
    }

    pub async fn fork_session_serialized(
        self: &Arc<Self>,
        tenant: &TenantId,
        parent: &TrajectoryId,
        maximum_depth: u32,
    ) -> Result<TrajectoryId, SessionForkError> {
        let lease = self.store.turn_lock(tenant, parent)?;
        let _parent_turn = lease.lock_owned().await;
        let depth = self.trajectory_depth(tenant, parent)?;
        if depth >= maximum_depth {
            return Err(SessionForkError::DepthLimit {
                depth,
                maximum: maximum_depth,
            });
        }
        Ok(self.fork_session(tenant, parent)?)
    }

    pub fn is_child(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<bool, StoreError> {
        Ok(self.store.parent_of(tenant, session)?.is_some())
    }

    pub fn snapshot(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<(Vec<Fact>, Revision), StoreError> {
        self.store.snapshot(tenant, session)
    }

    pub fn parent_of(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<Option<TrajectoryId>, StoreError> {
        self.store.parent_of(tenant, session)
    }

    fn trajectory_depth(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<u32, StoreError> {
        let mut depth = 0u32;
        let mut cursor = session.clone();
        while let Some(parent) = self.store.parent_of(tenant, &cursor)? {
            depth = depth.saturating_add(1);
            cursor = parent;
        }
        Ok(depth)
    }
}

fn is_reserved(name: &str) -> bool {
    matches!(name, EXECUTE_REMEDY_PLAN | FORK | SUBMIT_RESULT)
}

fn policy_tool_schema(
    contract: &ToolContract,
    trust_chain: &TrustChain,
    parameters: Option<serde_json::Value>,
) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: contract.name.as_str().to_string(),
            description: Some(policy_description(contract, trust_chain)),
            parameters,
        },
    }
}

fn policy_description(contract: &ToolContract, trust_chain: &TrustChain) -> String {
    let mut clauses = Vec::new();
    match &contract.delta {
        None => clauses.push("output label is unknown".to_string()),
        Some(delta) => {
            match &delta.trust {
                Some(Dim::Known(trust)) => clauses.push(format!(
                    "output trust={}",
                    trust_chain
                        .name_of(*trust)
                        .expect("validated tool trust rank is in the chain")
                )),
                Some(Dim::Unknown) => clauses.push("output trust=unknown".to_string()),
                None => {}
            }
            match &delta.audience {
                Some(Dim::Known(audience)) => clauses.push(format!("output audience={audience:?}")),
                Some(Dim::Unknown) => clauses.push("output audience=unknown".to_string()),
                None => {}
            }
            if delta.is_none() {
                clauses.push("output label is neutral".to_string());
            }
        }
    }
    if let Some(trust) = contract.requires.label.trust_floor {
        clauses.push(format!(
            "requires trust>={}",
            trust_chain
                .name_of(trust)
                .expect("validated requirement trust rank is in the chain")
        ));
    }
    if !contract.requires.label.audience.is_empty() {
        clauses.push(format!("audience requirements={:?}", contract.requires.label.audience));
    }
    if !contract.requires.history.is_empty() {
        clauses.push(format!("history requirements={:?}", contract.requires.history));
    }
    if !contract.emits.is_empty() {
        clauses.push(format!(
            "effects=[{}]",
            contract
                .emits
                .iter()
                .map(|effect| effect.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    format!("APPA contract: {}.", clauses.join("; "))
}

fn remedy_tool_schema(can_fork: bool) -> WireTool {
    let escape = if can_fork {
        "use fork instead when later work needs its current label"
    } else {
        "run any later work that needs its current label before you accept"
    };
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: EXECUTE_REMEDY_PLAN.to_string(),
            description: Some(format!(
                "Execute an offered remedy in this trajectory. Accepting a narrowing permanently restricts this trajectory; {escape}."
            )),
            parameters: Some(json!({
                "type": "object",
                "properties": { "plan_id": { "type": "string" } },
                "required": ["plan_id"],
                "additionalProperties": false
            })),
        },
    }
}

fn reserved_tool_schema(name: &str) -> WireTool {
    let (description, parameters) = match name {
        FORK => (
            "Run one self-contained task in an isolated child trajectory. Scope the child to the restrictive read and the work that must sit beside it: every later call in that child runs under the label the read narrowed to, so a write the parent's current label still permits belongs in the parent, issued once the child returns. A child inherits the parent's label and can never widen it — forking again after the parent has narrowed changes nothing. Must be the only call in its assistant round. Child prose does not return; finish a child that did the work itself with submit_result null.",
            json!({
                "type": "object",
                "properties": { "task": { "type": "string", "minLength": 1 } },
                "required": ["task"],
                "additionalProperties": false
            }),
        ),
        SUBMIT_RESULT => (
            "Finish this child. `value` is data a later parent step consumes, not an account of what happened here — the parent is told this child finished either way. Use null when this child already performed the work, so the parent's label stays unchanged.",
            json!({
                "type": "object",
                "properties": { "value": { "type": ["string", "null"] } },
                "required": ["value"],
                "additionalProperties": false
            }),
        ),
        _ => unreachable!("reserved schema requested only for reserved tools"),
    };
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: name.to_string(),
            description: Some(description.to_string()),
            parameters: Some(parameters),
        },
    }
}
