//! Public types shared by both session facades.

use thiserror::Error;

use appa_engine::label::Label;
use appa_engine::value::{DispatchId, ResolvedCall, ToolName};

use crate::store::StoreError;

#[derive(Clone, Copy, Debug)]
pub struct SdkOptions {
    /// The blocked-proposal budget of `RMD-7`: how many blocked-proposal rounds one **rendered
    /// call** (by digest) may open per turn. Each round is one cohort of offered plans, every plan
    /// in it consultable once. It is spam control on the agent, not a count of denials — a denial
    /// bites by excluding the denying authority's plans, never by shrinking this budget.
    pub max_blocked_proposals_per_call: u32,
    /// The most one external authority consultation or dynamic resolution may take; a timeout
    /// fails closed.
    pub per_external_timeout: std::time::Duration,
}

impl Default for SdkOptions {
    fn default() -> Self {
        SdkOptions {
            max_blocked_proposals_per_call: 2,
            per_external_timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Error)]
pub enum OpenError {
    #[error("unsupported policy for the embedded SDK: {0}")]
    UnsupportedPolicy(String),
    #[error("registered tool {0} collides with a reserved tool name")]
    ReservedToolConflict(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("the session is {actual}; this operation requires {required}")]
pub struct SessionBusy {
    pub actual: &'static str,
    pub required: &'static str,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolSurfaceError {
    #[error("tools are already bound for this session")]
    AlreadyBound,
    #[error("the tool surface advertises {0} but the policy does not register it")]
    UnknownTool(String),
    #[error("the policy registers {0} but the tool surface does not provide it")]
    MissingTool(String),
    #[error("duplicate tool name {0} in the provided surface")]
    Duplicate(String),
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error(transparent)]
    Busy(#[from] SessionBusy),
    #[error("the handle does not identify the outstanding surfaced call")]
    UnknownHandle,
    #[error("dispatch identity no longer matches its call — an SDK invariant was breached")]
    DispatchIdentity,
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmittedResult {
    Admitted { content: String, label: Label },
    Sealed { token: String },
}

/// The one outstanding surfaced call. Move-only by construction: no `Clone`, no serde, private
/// fields, no public constructor — the host can only pass it back whole to a facade's report or
/// abandon method. (Boxed internally so it rides inside a small enum.)
#[derive(Debug)]
pub struct DispatchHandle(Box<HandleInner>);

#[derive(Debug)]
pub(crate) struct HandleInner {
    pub(crate) id: u64,
    pub(crate) dispatch: DispatchId,
    pub(crate) call: ResolvedCall,
}

impl DispatchHandle {
    pub(crate) fn new(inner: HandleInner) -> Self {
        DispatchHandle(Box::new(inner))
    }

    pub(crate) fn inner(&self) -> &HandleInner {
        &self.0
    }

    pub fn occurrence(&self) -> u32 {
        self.0.dispatch.occurrence()
    }

    pub fn tool(&self) -> &ToolName {
        self.0.call.tool()
    }
}
