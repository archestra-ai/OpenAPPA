//! The per-call session facade: **a framework owns the loop**.

use thiserror::Error;

use appa_engine::value::ResolvedCall;

use appa_runtime::store::StoreError;
use appa_runtime::tool::{RenderedCall, ToolOutcome};
use appa_runtime::wire::WireTool;

use crate::common::{self, Admission, Checked, Core, Remedied};
use crate::types::{AdmittedResult, DispatchHandle, HandleInner, OpenError, ReportError, SdkOptions, ToolSurfaceError};

#[derive(Debug, Error)]
pub enum CallError {
    #[error("no turn is active; call begin_turn first")]
    NoTurn,
    #[error("a turn is already active for this session")]
    TurnActive,
    #[error("a surfaced call is still outstanding; report or abandon it first")]
    CallOutstanding,
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

#[derive(Debug)]
pub enum CallDecision {
    Allow { handle: DispatchHandle },
    Block { feedback: String },
}

#[derive(Debug)]
pub enum RemedyDecision {
    Authorized { handle: DispatchHandle, call: RenderedCall },
    Declined { feedback: String },
}

pub struct CallSession {
    core: Core,
    turn_active: bool,
    in_flight: Option<u64>,
}

impl CallSession {
    pub fn open(config: crate::Config, options: SdkOptions) -> Result<CallSession, OpenError> {
        Ok(CallSession {
            core: Core::open(config, options)?,
            turn_active: false,
            in_flight: None,
        })
    }

    /// Bind the tool surface, once. The framework must advertise exactly the returned list —
    /// including the reserved `execute_remedy_plan` — for the whole session.
    pub fn bind_tools(&mut self, surface: Vec<WireTool>) -> Result<&[WireTool], ToolSurfaceError> {
        self.core.bind_tools(surface)
    }

    pub fn tools(&self) -> Option<&[WireTool]> {
        self.core.tools.as_deref()
    }

    pub fn begin_turn(&mut self, text: impl Into<String>) -> Result<(), CallError> {
        if self.turn_active {
            return Err(CallError::TurnActive);
        }
        self.core.admit_user_turn(text.into())?;
        self.turn_active = true;
        Ok(())
    }

    /// Check one proposed tool call (the hook's `ToolCall` event, for an ordinary tool). On allow a
    /// dispatch opens and a handle is returned; on block the model-visible feedback is returned. No
    /// transcript fact is authored — the framework delivers the feedback (e.g. as a hook skip).
    pub fn check_call(&mut self, call: RenderedCall) -> Result<CallDecision, CallError> {
        self.guard_ready()?;
        let resolved = ResolvedCall::new(call.tool.clone(), call.arguments.clone(), Vec::new());
        match self.core.check_ordinary(resolved)? {
            Checked::Feedback(feedback) => Ok(CallDecision::Block { feedback }),
            Checked::Allow(dispatch) => {
                let resolved = ResolvedCall::new(call.tool.clone(), call.arguments.clone(), Vec::new());
                let id = self.core.next_handle_id();
                self.in_flight = Some(id);
                Ok(CallDecision::Allow {
                    handle: DispatchHandle::new(HandleInner {
                        id,
                        dispatch,
                        call: resolved,
                    }),
                })
            }
        }
    }

    /// Resolve an `execute_remedy_plan(plan_id)` invocation (the hook intercepts the reserved tool).
    /// On authorization the underlying call is surfaced to execute now; otherwise feedback.
    pub async fn resolve_remedy(&mut self, plan_id: Option<&str>) -> Result<RemedyDecision, CallError> {
        self.guard_ready()?;
        match self.core.resolve_remedy(plan_id).await? {
            Remedied::Feedback(feedback) => Ok(RemedyDecision::Declined { feedback }),
            Remedied::Authorized { dispatch, call } => {
                let rendered = RenderedCall::from_call(&call);
                let id = self.core.next_handle_id();
                self.in_flight = Some(id);
                Ok(RemedyDecision::Authorized {
                    handle: DispatchHandle::new(HandleInner { id, dispatch, call }),
                    call: rendered,
                })
            }
        }
    }

    /// Report the outcome of the outstanding surfaced call: admit or seal it, returning the
    /// model-visible face (the admitted content or a sealed token) for the framework to deliver.
    /// Authors no `BlockFeedback` fact — the framework owns the transcript; only the label-moving
    /// `ValueAdmitted`/`DispatchClosed` enter the log.
    pub fn report_outcome(
        &mut self,
        handle: DispatchHandle,
        outcome: ToolOutcome,
    ) -> Result<AdmittedResult, ReportError> {
        let h = handle.inner();
        match self.in_flight {
            Some(id) if id == h.id => {}
            Some(_) => return Err(ReportError::UnknownHandle),
            None => {
                return Err(ReportError::Busy(crate::types::SessionBusy {
                    actual: "no call outstanding",
                    required: "a surfaced call",
                }));
            }
        }

        let admission = common::outcome_to_admission(&outcome);
        let admitted = match self.core.admit_result(&h.dispatch, &h.call, admission)? {
            Ok(Admission::Admitted(value)) => value,
            Ok(Admission::NotOpen) => return Err(ReportError::DispatchIdentity),
            Ok(Admission::Refused) => {
                match self.core.admit_result(
                    &h.dispatch,
                    &h.call,
                    appa_engine::admit::ResultAdmission::SuccessNoValue,
                )? {
                    Ok(_) => {}
                    Err(_) => return Err(ReportError::DispatchIdentity),
                }
                None
            }
            Err(_) => return Err(ReportError::DispatchIdentity),
        };
        self.in_flight = None;

        Ok(match (admitted, common::sealed_token(&outcome, false)) {
            (Some((content, label)), _) => AdmittedResult::Admitted { content, label },
            (None, Some(token)) => AdmittedResult::Sealed {
                token: token.to_string(),
            },
            (None, None) => AdmittedResult::Sealed {
                token: common::SEALED_FAILED.to_string(),
            },
        })
    }

    /// Abandon the outstanding surfaced call without a result (the framework aborted mid-call):
    /// close the dispatch `Indeterminate` so nothing is orphaned.
    pub fn abandon(&mut self, handle: DispatchHandle) -> Result<(), ReportError> {
        let h = handle.inner();
        match self.in_flight {
            Some(id) if id == h.id => {}
            _ => return Err(ReportError::UnknownHandle),
        }
        match self
            .core
            .admit_result(&h.dispatch, &h.call, appa_engine::admit::ResultAdmission::Indeterminate)?
        {
            Ok(_) => {}
            Err(_) => return Err(ReportError::DispatchIdentity),
        }
        self.in_flight = None;
        Ok(())
    }

    /// End the active run (the framework's `prompt` returned): append the `TurnEnd` boundary,
    /// clear pending remedies, and release the run lease.
    pub fn end_turn(&mut self) -> Result<(), CallError> {
        if !self.turn_active {
            return Err(CallError::NoTurn);
        }
        if self.in_flight.is_some() {
            return Err(CallError::CallOutstanding);
        }
        self.core.end_turn()?;
        self.turn_active = false;
        Ok(())
    }

    fn guard_ready(&self) -> Result<(), CallError> {
        if !self.turn_active {
            return Err(CallError::NoTurn);
        }
        if self.in_flight.is_some() {
            return Err(CallError::CallOutstanding);
        }
        Ok(())
    }
}
