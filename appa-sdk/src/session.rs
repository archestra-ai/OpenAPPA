//! The turn-shaped session facade: **the host owns the loop**.
//!
//! `AppaSession` is the strong deployment: the model's context is rebuilt from the SDK's own log
//! (`transcript()`), so the log *is* the context and cannot diverge — nothing the model sees was not
//! admitted here. Mediation is serial (one surfaced call at a time, each checked against a
//! projection containing the previous call's admitted result) behind a three-state lifecycle gate at
//! one choke point: `Idle` (no active turn), `ReadyForCompletion` (a turn is active, the next move is
//! the model's), `AwaitingOutcome` (exactly one surfaced call is outstanding).
//!
//! Every fact appended mirrors the runtime turn-drive's sequence, so `model_transcript` and the
//! engine's `Projection` see exactly the shapes they were built for.

use std::collections::VecDeque;

use thiserror::Error;

use appa_engine::fact::{Fact, ProposedCall};
use appa_engine::value::ToolCallId;

use appa_runtime::inference::Completion;
use appa_runtime::runtime::{EXECUTE_REMEDY_PLAN, SUBMIT_RESULT};
use appa_runtime::store::StoreError;
use appa_runtime::tool::{RenderedCall, ToolOutcome};
use appa_runtime::transcript::model_transcript;
use appa_runtime::wire::{WireMessage, WireTool};

use crate::common::{self, Admission, Checked, Core, Remedied};
use crate::types::{
    AdmittedResult, DispatchHandle, HandleInner, OpenError, ReportError, SdkOptions, SessionBusy, ToolSurfaceError,
};

/// Why a mediation step failed. Policy outcomes (blocks, denials) are not errors — they become
/// feedback facts the model sees; these are lifecycle or infrastructure faults.
#[derive(Debug, Error)]
pub enum MediateError {
    #[error(transparent)]
    Busy(#[from] SessionBusy),
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

/// The next move the host must make.
#[derive(Debug)]
pub enum Step {
    /// The model yielded its final answer; the turn is over.
    Final { text: String },
    /// Execute exactly this call, then report through the handle before anything else.
    Execute { handle: DispatchHandle, call: RenderedCall },
    /// Nothing outstanding — run the next inference round from a fresh [`AppaSession::transcript`].
    Continue,
}

/// What entered (or was withheld from) the trajectory for a reported outcome, plus the next move.
#[derive(Debug)]
pub struct Outcome {
    pub result: AdmittedResult,
    pub next: Step,
}

/// The lifecycle gate — one private state checked by every public entry point.
#[derive(Debug)]
enum SessionState {
    Idle,
    ReadyForCompletion,
    AwaitingOutcome { handle_id: u64 },
}

impl SessionState {
    fn name(&self) -> &'static str {
        match self {
            SessionState::Idle => "Idle",
            SessionState::ReadyForCompletion => "ReadyForCompletion",
            SessionState::AwaitingOutcome { .. } => "AwaitingOutcome",
        }
    }
}

/// A proposed call held in the round queue, with its malformed-arguments flag preserved.
struct Proposal {
    call: ProposedCall,
    malformed: bool,
}

/// One mediated trajectory driven by a host-owned loop.
pub struct AppaSession {
    core: Core,
    state: SessionState,
    round: VecDeque<Proposal>,
}

impl AppaSession {
    /// Open a session on a loaded policy. Fails closed on any policy feature the SDK v0 defers.
    pub fn open(config: crate::Config, options: SdkOptions) -> Result<AppaSession, OpenError> {
        Ok(AppaSession {
            core: Core::open(config, options)?,
            state: SessionState::Idle,
            round: VecDeque::new(),
        })
    }

    /// Bind the tool surface, once: the host's schemas validated name-for-name against the registry,
    /// plus the reserved `execute_remedy_plan` schema. The host must advertise exactly the returned
    /// list on every inference request.
    pub fn bind_tools(&mut self, surface: Vec<WireTool>) -> Result<&[WireTool], ToolSurfaceError> {
        self.core.bind_tools(surface)
    }

    /// The bound tool surface, if bound.
    pub fn tools(&self) -> Option<&[WireTool]> {
        self.core.tools.as_deref()
    }

    /// Admit one user turn.
    pub fn admit_user_turn(&mut self, text: impl Into<String>) -> Result<(), MediateError> {
        self.require(&SessionState::Idle, "Idle")?;
        self.core.admit_user_turn(text.into())?;
        self.state = SessionState::ReadyForCompletion;
        Ok(())
    }

    /// The model-visible conversation, rebuilt from the log. Quiescent-only — while a surfaced call
    /// is outstanding the current round is unpaired and must not be rendered.
    pub fn transcript(&self) -> Result<Vec<WireMessage>, SessionBusy> {
        if let SessionState::AwaitingOutcome { .. } = self.state {
            return Err(SessionBusy {
                actual: self.state.name(),
                required: "Idle or ReadyForCompletion",
            });
        }
        let (log, _) = self
            .core
            .store
            .snapshot(&self.core.tenant, &self.core.session)
            .expect("the session owns its store");
        Ok(model_transcript(self.core.config.preamble(), &log, &self.core.session))
    }

    /// Mediate one model completion: record the assistant round, then advance.
    pub async fn mediate(&mut self, completion: Completion) -> Result<Step, MediateError> {
        self.require(&SessionState::ReadyForCompletion, "ReadyForCompletion")?;
        debug_assert!(self.round.is_empty(), "ReadyForCompletion implies an empty round queue");

        let parsed: Vec<(ProposedCall, bool)> = completion.tool_calls.iter().map(common::proposal_of).collect();
        let calls: Vec<ProposedCall> = parsed.iter().map(|(call, _)| call.clone()).collect();
        self.core.append(vec![Fact::AssistantMessage {
            trajectory: self.core.session.clone(),
            content: completion.content.clone(),
            calls,
        }])?;

        if parsed.is_empty() {
            self.finish_turn()?;
            return Ok(Step::Final {
                text: completion.content.unwrap_or_default(),
            });
        }
        self.round = parsed
            .into_iter()
            .map(|(call, malformed)| Proposal { call, malformed })
            .collect();
        self.advance().await
    }

    /// Report the outcome of the outstanding surfaced call, then advance the round.
    pub async fn report_outcome(
        &mut self,
        handle: DispatchHandle,
        outcome: ToolOutcome,
    ) -> Result<Outcome, ReportError> {
        self.take_outstanding(&handle)?;
        let h = handle.inner();

        let admission = common::outcome_to_admission(&outcome);
        let admitted = match self.core.admit_result(&h.dispatch, &h.call, admission)? {
            Ok(Admission::Admitted(value)) => value,
            Ok(Admission::NotOpen) => return Err(ReportError::DispatchIdentity),
            // The engine refused the value: close success-with-no-value so effects stand, seal.
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

        let result = match (&admitted, common::sealed_token(&outcome, admitted.is_some())) {
            (Some((content, label)), None) => AdmittedResult::Admitted {
                content: content.clone(),
                label: label.clone(),
            },
            (_, Some(token)) => {
                self.core.feedback(&h.response_call_id, token)?;
                AdmittedResult::Sealed {
                    token: token.to_string(),
                }
            }
            // Admitted but sealed_token said seal-less and no value — unreachable, but stay total.
            (None, None) => AdmittedResult::Sealed {
                token: common::SEALED_FAILED.to_string(),
            },
        };

        self.state = SessionState::ReadyForCompletion;
        let next = self.advance().await.map_err(mediate_to_report)?;
        Ok(Outcome { result, next })
    }

    /// Abandon the outstanding surfaced call: one batch closes its dispatch `Indeterminate`, seals
    /// it and every remaining proposal, lands the cancelled terminal and the `TurnEnd`.
    pub fn abandon(&mut self, handle: DispatchHandle) -> Result<(), ReportError> {
        self.take_outstanding(&handle)?;
        let h = handle.inner();
        let unanswered: Vec<ToolCallId> = std::iter::once(h.response_call_id.clone())
            .chain(self.round.iter().map(|p| p.call.id.clone()))
            .collect();

        // Close the dispatch Indeterminate, then seal every unanswered call, in one terminal batch
        // ending with the boundary — mirroring the runtime's shielded cancellation close.
        let mut terminal = Vec::new();
        if let Ok(Ok(batch)) = self
            .core
            .store
            .snapshot(&self.core.tenant, &self.core.session)
            .map(|(log, rev)| {
                let projection = appa_engine::projection::Projection::build(&log, rev);
                let views = projection.view(&self.core.session);
                self.core.engine.admit_result(
                    &views,
                    &h.dispatch,
                    &h.call,
                    appa_engine::admit::ResultAdmission::Indeterminate,
                )
            })
        {
            terminal = batch.facts;
        }
        for call_id in &unanswered {
            terminal.push(Fact::BlockFeedback {
                trajectory: self.core.session.clone(),
                call_id: call_id.clone(),
                content: common::TURN_CANCELLED.to_string(),
            });
        }
        terminal.push(Fact::AssistantMessage {
            trajectory: self.core.session.clone(),
            content: Some(common::TURN_CANCELLED.to_string()),
            calls: Vec::new(),
        });
        self.core.end_turn(terminal)?;
        self.round.clear();
        self.state = SessionState::Idle;
        Ok(())
    }

    /// End the active turn without an outstanding call (an inference fault, a host abort).
    pub fn stop_turn(&mut self, reason: &str) -> Result<(), MediateError> {
        self.require(&SessionState::ReadyForCompletion, "ReadyForCompletion")?;
        let mut terminal: Vec<Fact> = self
            .round
            .iter()
            .map(|p| Fact::BlockFeedback {
                trajectory: self.core.session.clone(),
                call_id: p.call.id.clone(),
                content: reason.to_string(),
            })
            .collect();
        terminal.push(Fact::AssistantMessage {
            trajectory: self.core.session.clone(),
            content: Some(reason.to_string()),
            calls: Vec::new(),
        });
        self.core.end_turn(terminal)?;
        self.round.clear();
        self.state = SessionState::Idle;
        Ok(())
    }

    // --- the advance loop ----------------------------------------------------

    async fn advance(&mut self) -> Result<Step, MediateError> {
        while let Some(proposal) = self.round.pop_front() {
            if proposal.malformed {
                self.core.feedback(&proposal.call.id, common::MALFORMED_ARGUMENTS)?;
                continue;
            }
            match proposal.call.tool.as_str() {
                EXECUTE_REMEDY_PLAN => {
                    if let Some(step) = self.mediate_remedy(&proposal.call).await? {
                        return Ok(step);
                    }
                }
                SUBMIT_RESULT => {
                    self.core
                        .feedback(&proposal.call.id, "submit_result is available only to a child session")?;
                }
                _ => {
                    if let Some(step) = self.mediate_ordinary(&proposal.call)? {
                        return Ok(step);
                    }
                }
            }
        }
        Ok(Step::Continue)
    }

    fn mediate_ordinary(&mut self, proposed: &ProposedCall) -> Result<Option<Step>, MediateError> {
        let call = appa_engine::value::ResolvedCall::new(proposed.tool.clone(), proposed.arguments.clone(), Vec::new());
        match self.core.check_ordinary(call)? {
            Checked::Feedback(text) => {
                self.core.feedback(&proposed.id, &text)?;
                Ok(None)
            }
            Checked::Allow(dispatch) => {
                let call = appa_engine::value::ResolvedCall::new(
                    proposed.tool.clone(),
                    proposed.arguments.clone(),
                    Vec::new(),
                );
                Ok(Some(self.surface(dispatch, call, proposed.id.clone())))
            }
        }
    }

    async fn mediate_remedy(&mut self, proposed: &ProposedCall) -> Result<Option<Step>, MediateError> {
        let plan_id = proposed.arguments.get("plan_id").and_then(|v| v.as_str());
        match self.core.resolve_remedy(plan_id).await? {
            Remedied::Feedback(text) => {
                self.core.feedback(&proposed.id, &text)?;
                Ok(None)
            }
            Remedied::Authorized { dispatch, call } => Ok(Some(self.surface(dispatch, call, proposed.id.clone()))),
        }
    }

    fn surface(
        &mut self,
        dispatch: appa_engine::value::DispatchId,
        call: appa_engine::value::ResolvedCall,
        response_call_id: ToolCallId,
    ) -> Step {
        let id = self.core.next_handle_id();
        let rendered = RenderedCall::from_call(&call);
        self.state = SessionState::AwaitingOutcome { handle_id: id };
        Step::Execute {
            handle: DispatchHandle::new(HandleInner {
                id,
                dispatch,
                call,
                response_call_id,
            }),
            call: rendered,
        }
    }

    fn finish_turn(&mut self) -> Result<(), MediateError> {
        self.core.end_turn(Vec::new())?;
        self.round.clear();
        self.state = SessionState::Idle;
        Ok(())
    }

    fn require(&self, expected: &SessionState, required: &'static str) -> Result<(), SessionBusy> {
        if std::mem::discriminant(&self.state) == std::mem::discriminant(expected) {
            Ok(())
        } else {
            Err(SessionBusy {
                actual: self.state.name(),
                required,
            })
        }
    }

    fn take_outstanding(&self, handle: &DispatchHandle) -> Result<(), ReportError> {
        match &self.state {
            SessionState::AwaitingOutcome { handle_id } if *handle_id == handle.inner().id => Ok(()),
            SessionState::AwaitingOutcome { .. } => Err(ReportError::UnknownHandle),
            other => Err(ReportError::Busy(SessionBusy {
                actual: other.name(),
                required: "AwaitingOutcome",
            })),
        }
    }
}

fn mediate_to_report(e: MediateError) -> ReportError {
    match e {
        MediateError::Busy(busy) => ReportError::Busy(busy),
        MediateError::Store(store) => ReportError::Store(store),
    }
}
