//! The policy engine: evaluate one requested flow against exactly the values
//! it depends on.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use tracing::debug;

use crate::ToolName;
use crate::approval::Authority;
use crate::transition::{DuplicateRegistration, RegisteredTransformer};

mod application;
mod capability;
mod evaluation;
mod planning;
mod pursue;

#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod tests;

pub(crate) use capability::ReceiptParts;
pub use capability::{
    BlockReason, CanonicalRequest, DispatchReceipt, DuplicateContract, Emitted, ExecutionToken, FlowOutcome,
    FlowPermit, FlowRefusal, RejectedToken, ResponsePolicy, StepCapability, StepOutcome, StepRefused, ToolContract,
};
pub use pursue::{EmissionPursuit, Pursuit, StallCause};

/// Identity of one engine configuration, unique within the process. Plans,
/// step capabilities, and pending approvals bind to it: registries are the
/// semantic trust decision, so a capability minted under one engine's
/// registries must never resolve against another's — even if both registered
/// a transformer under the same public name and version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EngineId(u64);

impl EngineId {
    fn next() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine#{}", self.0)
    }
}

/// Holds the tool contracts, the transition registries, the authorities, and
/// the response policy. Registries are populated at construction time and
/// mechanically frozen at the first evaluation: routing is resolved live, so
/// a mid-run registration would change which authority rules an
/// already-minted plan.
pub struct PolicyEngine {
    id: EngineId,
    contracts: BTreeMap<ToolName, ToolContract>,
    transformers: Vec<RegisteredTransformer>,
    authorities: Vec<Authority>,
    response_policy: Option<ResponsePolicy>,
    evaluated: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{engine} already evaluated a flow; its registries are frozen")]
pub struct RegistryFrozen {
    pub engine: EngineId,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistrationRefused {
    #[error(transparent)]
    Duplicate(#[from] DuplicateRegistration),
    #[error(transparent)]
    Frozen(#[from] RegistryFrozen),
}

#[derive(Debug, thiserror::Error)]
pub enum ContractRefused {
    #[error(transparent)]
    Duplicate(#[from] DuplicateContract),
    #[error(transparent)]
    Frozen(#[from] RegistryFrozen),
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            id: EngineId::next(),
            contracts: BTreeMap::new(),
            transformers: Vec::new(),
            authorities: Vec::new(),
            response_policy: None,
            evaluated: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn freeze(&self) {
        self.evaluated.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn frozen(&self) -> Result<(), RegistryFrozen> {
        match self.evaluated.load(std::sync::atomic::Ordering::Relaxed) {
            true => Err(RegistryFrozen { engine: self.id }),
            false => Ok(()),
        }
    }

    /// Register a decision-making authority. All authorities share one name
    /// space; a duplicate name is refused. Routing consults inline authorities
    /// before external ones, each in registration order, so registration order
    /// is load-bearing.
    pub fn register_authority(&mut self, authority: Authority) -> Result<(), RegistrationRefused> {
        self.frozen()?;
        if self.authorities.iter().any(|a| a.name == authority.name) {
            debug!(authority = %authority.name, "register_authority: duplicate refused");
            return Err(DuplicateRegistration {
                id: authority.name.to_string(),
            }
            .into());
        }
        debug!(authority = %authority.name, "register_authority: registered");
        self.authorities.push(authority);
        Ok(())
    }

    /// Register a value transformer. Fails on a duplicate identity+version;
    /// registration order is the deterministic candidate order for planning.
    pub fn register_transformer(&mut self, transformer: RegisteredTransformer) -> Result<(), RegistrationRefused> {
        self.frozen()?;
        let id = &transformer.descriptor.transformer;
        if self.transformers.iter().any(|t| t.descriptor.transformer == *id) {
            debug!(transformer = %id, "register_transformer: duplicate refused");
            return Err(DuplicateRegistration { id: id.to_string() }.into());
        }
        debug!(transformer = %id, "register_transformer: registered");
        self.transformers.push(transformer);
        Ok(())
    }

    /// Register the reserved assistant-response sink's policy. An emission
    /// is checked through the same pipeline as any tool flow; without a
    /// policy it is unprovable (like calling a tool with no contract) and
    /// fails closed through the same remedy chain.
    pub fn with_response_policy(mut self, policy: ResponsePolicy) -> Result<Self, RegistryFrozen> {
        self.frozen()?;
        self.response_policy = Some(policy);
        Ok(self)
    }

    /// Register a tool's contract. Fails if one is already registered for that
    /// tool: contracts are the policy boundary, so an accidental replace is an
    /// error, not a silent overwrite.
    pub fn register(&mut self, contract: ToolContract) -> Result<(), ContractRefused> {
        self.frozen()?;
        if self.contracts.contains_key(&contract.name) {
            debug!(tool = %contract.name, "register: duplicate contract refused");
            return Err(DuplicateContract { tool: contract.name }.into());
        }
        debug!(tool = %contract.name, "register: contract registered");
        self.contracts.insert(contract.name.clone(), contract);
        Ok(())
    }
}
