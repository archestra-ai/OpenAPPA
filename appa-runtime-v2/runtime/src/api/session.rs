//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::elicit::Elicitation;
use crate::engine::{
    AuthorityVerdict, EngineDecision, EngineEvent, EngineView, ExternalEvidence, ExternalRequest, Feedback, ForkStatus,
    Liveness, Next, OfferNonce, OpenDispatch, Presentation, engine_id,
};
use crate::external::{ConsultKind, ConsultOutcome, ReadersResolution};

use super::{
    ChildReturnDecision, EventError, ExactCall, Inner, OfferId, OutcomeBody, ProposedCall, RemedyDecision, SpawnRef,
    SpawnResultDecision, ToolCallDecision, ToolOutcome, ToolResultDecision, TrajectoryId,
};

/// The runtime's own control tool, recognized by its exact
/// wire names: the bare name and each name the runtime's distribution
/// channels produce — the directly registered MCP server and the
/// `appa-runtime` plugin's server. Selecting an offer is not a checked
/// flow. A lookalike on another server — say
/// `mcp__evil__execute_remedy_plan` — is an ordinary checked call.
pub(crate) fn is_control_tool(tool: &str) -> bool {
    tool == "execute_remedy_plan"
        || tool == "mcp__appa__execute_remedy_plan"
        || tool == "mcp__plugin_appa-runtime_appa__execute_remedy_plan"
}

/// Why a reported outcome named no reportable dispatch. The threat model puts
/// operator diagnostics for these reports on the runtime: an
/// uncontrolled host makes integration mistakes the engine cannot
/// diagnose for it. The model-facing refusal is unchanged — the case
/// goes to the operator, not into feedback.
///
/// These are the contexts the runtime can observe, which are not
/// the threat model's three cases. An already-consumed report is
/// indistinguishable from an unknown one here: a closed dispatch leaves
/// the open set, and an outcome is attributable solely through its open
/// dispatch, so a byte match against a closed one would name
/// a call, never the occurrence that report belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnreportableOutcome {
    NoOpenDispatch,
    ByteMismatch,
}

impl UnreportableOutcome {
    fn case(self) -> &'static str {
        match self {
            UnreportableOutcome::NoOpenDispatch => "no_open_dispatch",
            UnreportableOutcome::ByteMismatch => "byte_mismatch",
        }
    }

    fn refusal(self) -> EventError {
        match self {
            UnreportableOutcome::NoOpenDispatch => EventError::UnknownDispatch,
            UnreportableOutcome::ByteMismatch => EventError::OutcomeMismatch,
        }
    }
}

fn is_open_call(call: &ProposedCall, canonical: impl FnOnce() -> Option<Vec<u8>>, open: &OpenDispatch) -> bool {
    call.tool == open.tool && canonical().as_deref() == Some(open.bytes.as_slice())
}

/// Which dispatch a reported outcome belongs to, or why none can take
/// it. Total over everything the log shows: the reported call in the
/// engine's canonical domain and the dispatches this
/// trajectory has open. `canonical` is deferred because a report with
/// no open dispatch — the duplicate every crash recovery produces —
/// settles without canonicalizing anything.
///
/// More than one open dispatch is not a state this deployment reaches:
/// one call in flight is refused at the decision that would open the
/// second. It is still refused here rather than resolved, because a
/// byte match that named one of several occurrences would be a guess.
fn classify_report(
    call: &ProposedCall,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: &[OpenDispatch],
) -> Result<appa_engine::value::DispatchId, UnreportableOutcome> {
    let [open] = open else {
        return Err(UnreportableOutcome::NoOpenDispatch);
    };
    if !is_open_call(call, canonical, open) {
        return Err(UnreportableOutcome::ByteMismatch);
    }
    Ok(open.id.clone())
}

#[derive(Debug, Clone, PartialEq)]
enum Standing {
    Runs(OpenDispatch),
    Abandoned(OpenDispatch),
}

/// What a late child open did: bound the child now, or found the
/// same pair already bound — the engine's own idempotent answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateOpen {
    Opened,
    AlreadyOpen,
}

enum SpawnPlan {
    Outcome,
    Return(TrajectoryId),
    Close(EventError),
}

fn outcome_decision(decision: EngineDecision) -> Result<ToolResultDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::KeepOutput) => Ok(ToolResultDecision::Keep),
        Next::PresentToModel(Presentation::ReplaceOutput { placeholder, .. }) => {
            Ok(ToolResultDecision::Replace { placeholder })
        }
        // An admitted value delivered in place of the raw output.
        Next::PresentToModel(Presentation::Value { value }) => Ok(ToolResultDecision::Replace { placeholder: value }),
        Next::PresentToModel(Presentation::Blocked { feedback, .. }) => {
            Ok(ToolResultDecision::Replace { placeholder: feedback })
        }
        _ => Err(EventError::UnexpectedDecision),
    }
}

fn return_decision(decision: EngineDecision) -> Result<ChildReturnDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::Value { value }) => Ok(ChildReturnDecision::Returned { value }),
        Next::PresentToModel(Presentation::NoValue) => Ok(ChildReturnDecision::NoValue),
        Next::PresentToModel(Presentation::Blocked { feedback, .. }) => Ok(ChildReturnDecision::Blocked { feedback }),
        _ => Err(EventError::UnexpectedDecision),
    }
}

const REPLAY_LIMIT: u32 = 8;

const EVIDENCE_LIMIT: u32 = 8;

fn fresh_entropy() -> OfferNonce {
    OfferNonce(rand::random::<[u8; 32]>())
}

/// One per trajectory (root or child). The adapter drives it; it never
/// renders, and the adapter never stores.
pub struct Session {
    inner: Arc<Inner>,
    trajectory: TrajectoryId,
    root: TrajectoryId,
}

impl Session {
    pub(super) fn attach(inner: Arc<Inner>, trajectory: TrajectoryId, root: TrajectoryId) -> Session {
        Session {
            inner,
            trajectory,
            root,
        }
    }

    #[cfg(test)]
    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    /// The user submitted a prompt. A prompt is not an engine event:
    /// nothing is reported to the engine,
    /// nothing is recorded, and offer freshness stays the engine's
    /// judgment — a stale offer declines at execution by live re-plan.
    /// The runtime keeps no transcript, so there is nothing
    /// left for this event to do.
    pub fn on_prompt(&mut self, _text: String) -> Result<(), EventError> {
        tracing::debug!(trajectory = %self.trajectory.0, "prompt acknowledged");
        Ok(())
    }

    pub async fn on_tool_call(&mut self, call: ProposedCall, spawn: bool) -> Result<ToolCallDecision, EventError> {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %self.trajectory.0, "control tool passes unchecked");
            return Ok(ToolCallDecision::Control);
        }
        if let Some(open) = self.substituted_release(&call)? {
            return self.claim_or_abandon(call, open).await;
        }
        let decision = self
            .drive_with_evidence(
                |_, evidence| {
                    Ok(EngineEvent::ModelResponse {
                        call: call.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        spawn,
                    })
                },
                None,
            )
            .await?;

        match decision.then {
            Next::ModelResponse { invocations, feedback } => {
                match (invocations.as_slice(), feedback.as_slice()) {
                    ([released], []) => {
                        tracing::debug!(
                            trajectory = %self.trajectory.0,
                            dispatch = %released.dispatch.0,
                            tool = %released.tool,
                            spawn = released.fork.is_some(),
                            "call released"
                        );
                        Ok(ToolCallDecision::Allow {
                            spawn: released.fork.clone(),
                        })
                    }
                    ([], feedback) if !feedback.is_empty() => {
                        tracing::debug!(trajectory = %self.trajectory.0, "call blocked");
                        Ok(ToolCallDecision::Deny {
                            feedback: join_feedback(feedback),
                        })
                    }
                    _ => Err(EventError::UnexpectedDecision),
                }
            }
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    fn substituted_release(&self, call: &ProposedCall) -> Result<Option<Standing>, EventError> {
        let log = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&log)?;
        let view = self
            .inner
            .engine
            .rebuild_view(&policy, &log)
            .map_err(EventError::from)?;
        match self.inner.engine.liveness(&view, &self.trajectory) {
            Liveness::Ended => return Err(EventError::TrajectoryEnded),
            Liveness::Unopened => return Err(EventError::SpawnNotTaken),
            Liveness::Live => {}
        }
        let Some(open) = self.inner.engine.substituted_release(&view, &self.trajectory) else {
            return Ok(None);
        };
        let canonical = || self.inner.engine.canonical_bytes(&policy, call);
        Ok(Some(if is_open_call(call, canonical, &open) {
            Standing::Runs(open)
        } else {
            Standing::Abandoned(open)
        }))
    }

    async fn claim_or_abandon(&self, call: ProposedCall, standing: Standing) -> Result<ToolCallDecision, EventError> {
        let open = match standing {
            Standing::Runs(open) => {
                tracing::debug!(
                    trajectory = %self.trajectory.0,
                    dispatch = ?open.id,
                    tool = %open.tool,
                    "substituted call handed to the harness"
                );
                return Ok(ToolCallDecision::Allow { spawn: None });
            }
            Standing::Abandoned(open) => open,
        };
        let outcome = ToolOutcome::Failure {
            message: "the harness did not run the substituted call".to_string(),
        };
        self.abandon_release(&outcome).await?;
        tracing::debug!(
            trajectory = %self.trajectory.0,
            dispatch = ?open.id,
            tool = %open.tool,
            proposed = %call.tool,
            "substituted call abandoned"
        );
        Err(EventError::SubstitutionAbandoned { tool: open.tool })
    }

    async fn abandon_release(&self, outcome: &ToolOutcome) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                let Some(open) = context.substituted_release() else {
                    return Err(EventError::CallOutstanding);
                };
                Ok(EngineEvent::ToolOutcome {
                    dispatch: open.id,
                    outcome: outcome.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
        )
        .await
    }

    pub async fn on_tool_result(
        &mut self,
        call: ProposedCall,
        o: ToolOutcome,
    ) -> Result<ToolResultDecision, EventError> {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %self.trajectory.0, "control tool outcome absorbed");
            return Ok(ToolResultDecision::Keep);
        }
        let o = self.cap_outcome(o);
        outcome_decision(self.report_outcome(&call, &o).await?)
    }

    async fn report_outcome(&self, call: &ProposedCall, o: &ToolOutcome) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                let open = context.open_dispatches();
                let dispatch = match classify_report(call, || context.canonical_bytes(call), &open) {
                    Ok(dispatch) => dispatch,
                    Err(case) => return Err(self.refuse_report(case, call, &open)),
                };
                Ok(EngineEvent::ToolOutcome {
                    dispatch,
                    outcome: o.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
        )
        .await
    }

    pub async fn on_spawn_result(
        &mut self,
        call: ProposedCall,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> Result<SpawnResultDecision, EventError> {
        let outcome = self.cap_outcome(outcome);
        let plan: std::sync::Mutex<Option<SpawnPlan>> = std::sync::Mutex::new(None);
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    let open = context.open_dispatches();
                    let dispatch = match classify_report(&call, || context.canonical_bytes(&call), &open) {
                        Ok(dispatch) => dispatch,
                        Err(case) => return Err(self.refuse_report(case, &call, &open)),
                    };
                    let fork = appa_engine::value::ForkId::of(&dispatch);
                    let next = match context.fork_status(&fork) {
                        _ if matches!(outcome, ToolOutcome::Indeterminate) => SpawnPlan::Outcome,
                        ForkStatus::Unprepared => SpawnPlan::Outcome,
                        ForkStatus::Bound(bound) => match &child {
                            Some(child) if engine_id(child) == bound => SpawnPlan::Return(child.clone()),
                            _ => SpawnPlan::Close(EventError::BindingMismatch),
                        },
                        ForkStatus::Prepared | ForkStatus::Failed | ForkStatus::ParentEnded => {
                            SpawnPlan::Close(EventError::SpawnNotTaken)
                        }
                    };
                    let event = match &next {
                        SpawnPlan::Outcome => EngineEvent::ToolOutcome {
                            dispatch,
                            outcome: outcome.clone(),
                            evidence,
                            entropy: fresh_entropy(),
                        },
                        SpawnPlan::Return(child) => EngineEvent::ChildReturn {
                            child: child.clone(),
                            value: value.clone(),
                            evidence,
                            entropy: fresh_entropy(),
                        },
                        SpawnPlan::Close(refusal) => EngineEvent::ToolOutcome {
                            dispatch,
                            outcome: ToolOutcome::Failure {
                                message: refusal.to_string(),
                            },
                            evidence,
                            entropy: fresh_entropy(),
                        },
                    };
                    *plan.lock().expect("the spawn plan mutex is never poisoned") = Some(next);
                    Ok(event)
                },
                None,
            )
            .await;
        let plan = plan.into_inner().expect("the spawn plan mutex is never poisoned");
        let decision = match (&plan, decision) {
            (_, Ok(decision)) => decision,
            (
                Some(SpawnPlan::Return(_)),
                Err(refusal @ (EventError::TrajectoryEnded | EventError::ChildDispatchOpen)),
            ) => {
                return Err(self.close_spawn(&call, refusal).await);
            }
            (_, Err(error)) => return Err(error),
        };
        // The engine decided on an event this handler built from the view.
        match plan.expect("the spawn result is typed before the engine decides") {
            SpawnPlan::Outcome => outcome_decision(decision).map(SpawnResultDecision::Outcome),
            SpawnPlan::Close(refusal) => Err(refusal),
            SpawnPlan::Return(_) => {
                let decision = return_decision(decision)?;
                let close = match &decision {
                    ChildReturnDecision::Returned { .. } | ChildReturnDecision::NoValue => ToolOutcome::Success {
                        body: OutcomeBody::Unavailable,
                    },
                    ChildReturnDecision::Blocked { feedback } => ToolOutcome::Failure {
                        message: feedback.clone(),
                    },
                };
                self.report_outcome(&call, &close).await?;
                Ok(SpawnResultDecision::Return(decision))
            }
        }
    }

    async fn close_spawn(&self, call: &ProposedCall, refusal: EventError) -> EventError {
        let close = ToolOutcome::Failure {
            message: refusal.to_string(),
        };
        match self.report_outcome(call, &close).await {
            Ok(_) => refusal,
            Err(error) => error,
        }
    }

    /// The model called the `execute_remedy_plan` MCP tool. Executes
    /// one offer by its id; the id is unguessable,
    /// so naming it proves the model read the offer. An id
    /// this runtime never surfaced for this trajectory is refused.
    pub async fn on_remedy(
        &mut self,
        offer: OfferId,
        elicitation: Option<&Elicitation>,
    ) -> Result<RemedyDecision, EventError> {
        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.offer_pursuer(&offer).as_ref() != Some(&trajectory) {
                        return Err(EventError::UnknownOffer);
                    }
                    Ok(EngineEvent::ExecuteOffer {
                        trajectory: trajectory.clone(),
                        offer: offer.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                    })
                },
                elicitation,
            )
            .await?;

        match decision.then {
            Next::Approved { tool, bytes } => Ok(RemedyDecision::Authorized {
                call: ExactCall { tool, bytes },
            }),
            Next::InvokeTool(released) => Ok(RemedyDecision::Substituted {
                call: ExactCall {
                    tool: released.tool,
                    bytes: released.bytes,
                },
            }),
            Next::PresentToModel(Presentation::Value { value }) => Ok(RemedyDecision::Returned { value }),
            Next::PresentToModel(Presentation::Declined { feedback }) => Ok(RemedyDecision::Declined { feedback }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            Next::PresentToModel(Presentation::Blocked { feedback, .. }) => Ok(RemedyDecision::Declined { feedback }),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// A child agent started. `spawn` names the prepared fork the
    /// child binds to: the [`super::SpawnBinding`] the parent's spawn release
    /// handed the harness, or — for a harness whose start signal carries no
    /// reference to the spawn call — the family's one spawn in flight. The
    /// engine's `BindFork` opens the child before its first engine event; the
    /// child exists exactly when the log's `ForkOpened` does.
    pub fn on_child_start(&mut self, id: TrajectoryId, spawn: SpawnRef) -> Result<Session, EventError> {
        self.bind_child(id, spawn).map(|(session, _)| session)
    }

    /// Open a child whose start hook never arrived, or has not arrived yet:
    /// bind it to the family's one spawn in flight, as its start
    /// would. Whether this opened the child or found it already open tells the
    /// dispatcher whether the refused event was the missing start's, and is
    /// worth running once more, or the child's own answer.
    pub(crate) fn open_late(&mut self, child: TrajectoryId) -> Result<LateOpen, EventError> {
        self.bind_child(child, SpawnRef::InFlight).map(|(_, opened)| opened)
    }

    fn bind_child(&mut self, id: TrajectoryId, spawn: SpawnRef) -> Result<(Session, LateOpen), EventError> {
        let child = id.clone();
        let opened = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&opened)?;
        let decision = self.drive(&policy, Some(opened), true, |context| {
            let fork = match &spawn {
                SpawnRef::Binding(binding) => {
                    let Some(fork) = crate::engine::parse_fork(binding) else {
                        return Err(EventError::SpawnNotTaken);
                    };
                    fork
                }
                SpawnRef::InFlight => context.in_flight_fork(&child)?,
            };
            match context.fork_status(&fork) {
                ForkStatus::Unprepared | ForkStatus::Failed | ForkStatus::ParentEnded => Err(EventError::SpawnNotTaken),
                ForkStatus::Prepared | ForkStatus::Bound(_) => Ok(EngineEvent::BindFork {
                    fork,
                    child: child.clone(),
                }),
            }
        })?;
        // The same pair again appends nothing: the child was already open.
        let opened = match decision.append {
            Some(_) => LateOpen::Opened,
            None => LateOpen::AlreadyOpen,
        };
        match decision.then {
            Next::Done => Ok((Session::attach(Arc::clone(&self.inner), id, self.root.clone()), opened)),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The child finished. Its final message is its only return
    /// channel and is checked before it may cross to the parent;
    /// `None` returns no value. The return names the
    /// fork that opened the child, recovered from the log. A child
    /// with a call still open does not end: the end is refused, and the same
    /// end crosses once the call's outcome is reported (`ChildDispatchOpen`).
    pub async fn on_child_end(&mut self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        let child = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.parent_of(&child).is_none() {
                        return Err(EventError::NotAChild);
                    }
                    Ok(EngineEvent::ChildReturn {
                        child: child.clone(),
                        value: value.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                    })
                },
                None,
            )
            .await?;

        match decision.then {
            Next::PresentToModel(Presentation::Value { value }) => Ok(ChildReturnDecision::Returned { value }),
            Next::PresentToModel(Presentation::NoValue) => Ok(ChildReturnDecision::NoValue),
            Next::PresentToModel(Presentation::Blocked { feedback, .. }) => {
                Ok(ChildReturnDecision::Blocked { feedback })
            }
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    fn cap_outcome(&self, outcome: ToolOutcome) -> ToolOutcome {
        match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } if body.len() > self.inner.config.externals.max_body_bytes => ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
            other => other,
        }
    }

    fn refuse_report(&self, case: UnreportableOutcome, call: &ProposedCall, open: &[OpenDispatch]) -> EventError {
        tracing::warn!(
            trajectory = %self.trajectory.0,
            tool = %call.tool,
            dispatch = open.first().map(|d| format!("{:?}", d.id)).unwrap_or_else(|| "-".to_string()),
            open = open.len(),
            case = case.case(),
            "an outcome report named no reportable dispatch",
        );
        case.refusal()
    }

    async fn drive_with_evidence(
        &self,
        mut event: impl FnMut(&Decided<'_>, Vec<ExternalEvidence>) -> Result<EngineEvent, EventError>,
        elicitation: Option<&Elicitation>,
    ) -> Result<EngineDecision, EventError> {
        let opened = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&opened)?;
        let mut opened = Some(opened);
        let mut evidence: Vec<ExternalEvidence> = Vec::new();
        for _ in 0..EVIDENCE_LIMIT {
            let carried = evidence.clone();
            let entering = carried.is_empty();
            let decision = self.drive(&policy, opened.take(), entering, |context| {
                event(context, carried.clone())
            })?;
            match decision.then {
                Next::ResolveExternal(requests) => {
                    for request in requests {
                        evidence.push(self.consult(request, elicitation).await);
                    }
                }
                _ => return Ok(decision),
            }
        }
        Err(EventError::UnexpectedDecision)
    }

    fn drive(
        &self,
        policy: &crate::engine::PolicyEngine<'_>,
        mut opened: Option<appa_eventlog::Log>,
        entering: bool,
        mut event: impl FnMut(&Decided<'_>) -> Result<EngineEvent, EventError>,
    ) -> Result<EngineDecision, EventError> {
        for attempt in 1..=REPLAY_LIMIT {
            let log = match opened.take() {
                Some(log) => log,
                None => self.inner.log(&self.root)?,
            };
            let view = self.inner.engine.rebuild_view(policy, &log).map_err(EventError::from)?;
            let context = Decided {
                session: self,
                policy,
                view: &view,
            };
            if entering {
                match self.inner.engine.liveness(&view, &self.trajectory) {
                    Liveness::Ended => return Err(EventError::TrajectoryEnded),
                    Liveness::Unopened => return Err(EventError::SpawnNotTaken),
                    Liveness::Live => {}
                }
            }
            let event = event(&context)?;
            if let EngineEvent::ChildReturn { child, .. } = &event
                && !self.inner.engine.open_dispatches(&view, child).is_empty()
            {
                return Err(EventError::ChildDispatchOpen);
            }
            let decision = self
                .inner
                .engine
                .handle(policy, &view, &self.trajectory, event)
                .map_err(EventError::from)?;

            let Some(facts) = decision.append.as_ref() else {
                return Ok(decision);
            };
            if self
                .inner
                .engine
                .opens_a_second_dispatch(&view, &self.trajectory, facts)
            {
                return Err(EventError::CallOutstanding);
            }
            match self.inner.store.append(&log, facts) {
                Ok(()) => return Ok(decision),
                Err(appa_eventlog::AppendError::Conflict { .. }) => {
                    tracing::debug!(
                        root = %self.root.0,
                        attempt,
                        "another writer won the append: discarding the decision and replaying the event"
                    );
                    continue;
                }
                Err(error) => return Err(EventError::Storage(error.to_string())),
            }
        }
        Err(EventError::Contended { attempts: REPLAY_LIMIT })
    }

    async fn consult(&self, request: ExternalRequest, elicitation: Option<&Elicitation>) -> ExternalEvidence {
        match &request {
            ExternalRequest::Authority {
                authority,
                payload,
                review,
            } => {
                let outcome = self
                    .inner
                    .externals
                    .consult(ConsultKind::Authority, authority, payload, elicitation)
                    .await;
                let verdict = match outcome {
                    ConsultOutcome::Answer(body) => AuthorityVerdict::from_wire(&body),
                    ConsultOutcome::NoAnswer(_) => AuthorityVerdict::Abstain,
                };
                ExternalEvidence::Authority {
                    authority: authority.clone(),
                    verdict,
                    review: review.clone(),
                }
            }
            ExternalRequest::Sanitizer {
                sanitizer,
                source,
                body,
            } => {
                let payload = serde_json::json!({ "body": body.as_str() });
                let outcome = self
                    .inner
                    .externals
                    .consult(ConsultKind::Sanitizer, sanitizer, &payload, None)
                    .await;
                let derived = match outcome {
                    ConsultOutcome::Answer(body) => body.get("body").and_then(|b| b.as_str()).map(String::from),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Sanitizer {
                    sanitizer: sanitizer.clone(),
                    source: *source,
                    derived,
                }
            }
            // The dynamic resolver wire is the declared external contract verbatim.
            ExternalRequest::Dynamic {
                resolver,
                tool,
                argument,
                value,
            } => {
                let readers = match self
                    .inner
                    .externals
                    .resolve_dynamic(resolver, tool, argument, value)
                    .await
                {
                    ReadersResolution::Resolved { readers } => Some(readers),
                    ReadersResolution::Unresolved(_) => None,
                };
                ExternalEvidence::Dynamic {
                    resolver: resolver.clone(),
                    argument: argument.clone(),
                    readers,
                }
            }
            // The membership resolver wire is the declared external contract verbatim.
            ExternalRequest::Membership { resolver, group } => {
                let readers = match self.inner.externals.resolve_membership(resolver, group).await {
                    ReadersResolution::Resolved { readers } => Some(readers),
                    ReadersResolution::Unresolved(_) => None,
                };
                ExternalEvidence::Membership {
                    resolver: resolver.clone(),
                    group: group.clone(),
                    readers,
                }
            }
        }
    }
}

/// What one attempt of an event may read before it decides: the log as this
/// attempt rebuilt it. Everything the runtime used to keep beside the log —
/// a branch's parent, the dispatch it has open, the trajectory an offer
/// belongs to — is answered from here, so a replay after a lost race reads
/// the state that actually won rather than the state it first saw.
pub(crate) struct Decided<'a> {
    session: &'a Session,
    policy: &'a crate::engine::PolicyEngine<'a>,
    view: &'a EngineView,
}

impl Decided<'_> {
    fn open_dispatches(&self) -> Vec<OpenDispatch> {
        self.session
            .inner
            .engine
            .open_dispatches(self.view, &self.session.trajectory)
    }

    fn substituted_release(&self) -> Option<OpenDispatch> {
        self.session
            .inner
            .engine
            .substituted_release(self.view, &self.session.trajectory)
    }

    fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        self.session.inner.engine.canonical_bytes(self.policy, call)
    }

    fn parent_of(&self, child: &TrajectoryId) -> Option<TrajectoryId> {
        self.session.inner.engine.parent_of(self.view, child)
    }

    fn offer_pursuer(&self, offer: &OfferId) -> Option<TrajectoryId> {
        self.session.inner.engine.offer_pursuer(self.view, offer)
    }

    fn fork_status(&self, fork: &appa_engine::value::ForkId) -> ForkStatus {
        self.session.inner.engine.fork_status(self.policy, self.view, fork)
    }

    fn in_flight_fork(&self, child: &TrajectoryId) -> Result<appa_engine::value::ForkId, EventError> {
        let engine = &self.session.inner.engine;
        if let Some(fork) = engine.fork_of(self.policy, self.view, child) {
            return Ok(fork);
        }
        match engine.forks_in_flight(self.policy, self.view).as_slice() {
            [fork] => Ok(fork.clone()),
            [] => Err(EventError::SpawnNotTaken),
            _ => Err(EventError::SpawnAmbiguous),
        }
    }
}

fn join_feedback(feedback: &[Feedback]) -> String {
    feedback
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fixture arguments, from a `json!` value to the bytes a harness would
/// have sent. Production never takes this direction — the adapter holds
/// the harness's bytes already — so this is a test helper and not a
/// constructor on `ProposedCall`, which would invite the parse this
/// change exists to remove.
#[cfg(test)]
pub(crate) fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}
#[cfg(test)]
mod tests {
    use super::super::{DispatchId, OpenError, OutcomeBody, Runtime, SessionError};
    use super::*;
    use crate::config::Config;
    use crate::engine::{ReleasedCall, TestSeam};
    use appa_engine::fact::{BoundaryKind, Fact};

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 1
            [externals]
            timeout_ms = 1000
            max_body_bytes = 65536
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    fn open_test_runtime(dir: &tempfile::TempDir) -> Runtime {
        Runtime::open_with_engine(config(), dir.path().join("appa.db"), TestSeam::new()).expect("a fresh runtime opens")
    }

    fn root() -> TrajectoryId {
        TrajectoryId("cc:root".to_string())
    }

    fn decision(append: Option<Vec<Fact>>, then: Next) -> EngineDecision {
        EngineDecision { append, then }
    }

    #[derive(Clone, Copy)]
    enum Marker {
        One,
        Two,
    }

    fn batch(marker: Marker) -> Vec<Fact> {
        let punctuation = match marker {
            Marker::One => 1,
            Marker::Two => 2,
        };
        (0..punctuation)
            .map(|_| Fact::Boundary {
                trajectory: appa_engine::value::TrajectoryId::new("cc:root"),
                kind: BoundaryKind::TurnEnd,
            })
            .collect()
    }

    fn boundaries(runtime: &Runtime) -> usize {
        runtime
            .log_facts(&root())
            .iter()
            .filter(|fact| matches!(fact, Fact::Boundary { .. }))
            .count()
    }

    fn call() -> ProposedCall {
        ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "ls"})),
        }
    }

    fn bash_dispatch(label: &str) -> appa_engine::value::DispatchId {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 1
                [[tool]]
                name = "Bash"
            "#,
        )
        .expect("the fixture policy compiles");
        let call = policy
            .engine()
            .resolve_call(appa_engine::value::ToolName::new("Bash"), br#"{"command":"ls"}"#)
            .expect("the fixture call resolves through the engine");
        appa_engine::value::DispatchId::new(appa_engine::value::TrajectoryId::new(label), call.digest(), 0)
    }

    fn released(id: &str, call: &ProposedCall) -> ReleasedCall {
        ReleasedCall {
            dispatch: DispatchId(serde_json::to_string(&bash_dispatch(id)).expect("a dispatch id serializes")),
            tool: call.tool.clone(),
            bytes: serde_json::to_vec(call).expect("the test call serializes"),
            fork: None,
        }
    }

    fn deny_decision(text: &str, offers: &[&str]) -> EngineDecision {
        decision(
            None,
            Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: text.to_string(),
                    offers: offers.iter().map(|id| OfferId(id.to_string())).collect(),
                }],
            },
        )
    }

    fn review() -> appa_engine::execute::AuthorityReview {
        appa_engine::execute::AuthorityReview {
            tool: appa_engine::value::ToolName::new("Bash"),
            trajectory_label: appa_engine::label::PartialLabel::established(appa_engine::label::EstablishedLabel::top()),
        }
    }

    #[test]
    fn a_used_root_id_is_refused_and_a_persisted_one_reopens() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            runtime.create_session(root()),
            Err(SessionError::AlreadyExists),
        ));
        assert!(runtime.session(&root(), &root()).is_ok());
        assert!(matches!(
            runtime.session(
                &TrajectoryId("cc:ghost".to_string()),
                &TrajectoryId("cc:ghost".to_string())
            ),
            Err(SessionError::Unknown),
        ));
    }

    #[test]
    fn a_damaged_database_is_refused_at_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        std::fs::write(&path, b"not a sqlite database at all").expect("the file writes");
        assert!(matches!(
            Runtime::open_with_engine(config(), path, TestSeam::new()),
            Err(OpenError::Damaged(_)),
        ));
    }

    #[tokio::test]
    async fn a_prompt_consults_no_engine_and_records_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_prompt("read the report".to_string())
            .expect("the prompt acks");
        assert!(matches!(
            runtime.log_facts(&root()).as_slice(),
            [Fact::TrajectoryOpened { .. }]
        ));
        assert!(runtime.engine_seen().is_empty(), "the engine is never consulted");
    }

    #[tokio::test]
    async fn a_decision_whose_append_fails_never_acts() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.enqueue(decision(
            Some(batch(Marker::One)),
            Next::ModelResponse {
                invocations: vec![released("cc:root", &call())],
                feedback: Vec::new(),
            },
        ));
        runtime.store().fail_commit_after(0);
        assert!(matches!(
            session.on_tool_call(call(), false).await,
            Err(EventError::Storage(_)),
        ));
        assert_eq!(boundaries(&runtime), 0, "the killed append left nothing");
    }

    #[tokio::test]
    async fn a_lost_race_discards_the_decision_and_replays_with_a_fresh_random_number() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().contend_next_appends(1);
        runtime.enqueue(decision(
            Some(batch(Marker::One)),
            Next::ModelResponse {
                invocations: vec![released("cc:root", &call())],
                feedback: Vec::new(),
            },
        ));
        runtime.enqueue(decision(
            Some(batch(Marker::Two)),
            Next::ModelResponse {
                invocations: vec![released("cc:root", &call())],
                feedback: Vec::new(),
            },
        ));
        assert_eq!(
            session.on_tool_call(call(), false).await.expect("the replay commits"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert_eq!(boundaries(&runtime), 5);

        let entropies: Vec<_> = runtime
            .engine_seen()
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ModelResponse { entropy, .. } => Some(entropy.0),
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 2);
        assert_ne!(entropies[0], entropies[1], "each attempt carried a fresh number");
    }

    #[tokio::test]
    async fn a_permanently_contended_log_refuses_the_event() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().contend_next_appends(REPLAY_LIMIT as u64);
        for _ in 0..REPLAY_LIMIT {
            runtime.enqueue(decision(
                Some(batch(Marker::One)),
                Next::ModelResponse {
                    invocations: vec![released("cc:root", &call())],
                    feedback: Vec::new(),
                },
            ));
        }
        assert!(matches!(
            session.on_tool_call(call(), false).await,
            Err(EventError::Contended { attempts: REPLAY_LIMIT }),
        ));
        assert_eq!(
            runtime.engine_seen().len(),
            REPLAY_LIMIT as usize,
            "every attempt decided"
        );
        assert_eq!(boundaries(&runtime), 3 * REPLAY_LIMIT as usize);
    }

    fn control_call(name: &str) -> ProposedCall {
        ProposedCall {
            tool: name.to_string(),
            arguments: raw(serde_json::json!({"offer_id": "o1:cc:root:ff"})),
        }
    }

    #[tokio::test]
    async fn the_control_tool_passes_unchecked_under_every_shipped_name() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        for name in [
            "execute_remedy_plan",
            "mcp__appa__execute_remedy_plan",
            "mcp__plugin_appa-runtime_appa__execute_remedy_plan",
        ] {
            assert_eq!(
                session
                    .on_tool_call(control_call(name), false)
                    .await
                    .expect("it passes"),
                ToolCallDecision::Control,
                "{name} is a control call",
            );
            assert_eq!(
                session
                    .on_tool_result(control_call(name), ToolOutcome::Indeterminate)
                    .await
                    .expect("its outcome is absorbed"),
                ToolResultDecision::Keep,
            );
        }
        assert!(runtime.engine_seen().is_empty(), "no control call reached the engine");
    }

    #[tokio::test]
    async fn a_lookalike_control_tool_reaches_the_engine() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.enqueue(deny_decision("blocked", &[]));
        assert!(matches!(
            session
                .on_tool_call(control_call("mcp__evil__execute_remedy_plan"), false)
                .await
                .expect("the lookalike is decided"),
            ToolCallDecision::Deny { .. },
        ));
        assert_eq!(runtime.engine_seen().len(), 1);
    }

    #[tokio::test]
    async fn a_denied_call_returns_its_feedback() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.enqueue(deny_decision(
            "blocked; execute_remedy_plan(o1:cc:root:ab)",
            &["o1:cc:root:ab"],
        ));
        assert_eq!(
            session
                .on_tool_call(call(), false)
                .await
                .expect("the deny is delivered"),
            ToolCallDecision::Deny {
                feedback: "blocked; execute_remedy_plan(o1:cc:root:ab)".to_string(),
            },
        );
    }

    #[test]
    fn an_over_cap_success_body_is_carried_as_unavailable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        let success = |len: usize| ToolOutcome::Success {
            body: OutcomeBody::Available("x".repeat(len)),
        };
        assert!(matches!(
            session.cap_outcome(success(70_000)),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable
            },
        ));
        assert_eq!(session.cap_outcome(success(8)), success(8));
    }

    #[tokio::test]
    async fn an_unknown_offer_is_refused_without_an_engine_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session.on_remedy(OfferId("o1:cc:root:never".to_string()), None).await,
            Err(EventError::UnknownOffer),
        ));
        assert!(runtime.engine_seen().is_empty());
    }

    #[tokio::test]
    async fn evidence_round_trips_replay_the_same_event_and_no_answer_grants_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.enqueue(decision(
            None,
            Next::ResolveExternal(vec![ExternalRequest::Authority {
                authority: "approver".to_string(),
                payload: serde_json::json!({}),
                review: review(),
            }]),
        ));
        runtime.enqueue(deny_decision("no answer grants nothing", &[]));
        assert!(matches!(
            session.on_tool_call(call(), false).await.expect("the event settles"),
            ToolCallDecision::Deny { .. },
        ));
        let carried: Vec<_> = runtime
            .engine_seen()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::ModelResponse { evidence, .. } => Some(evidence),
                _ => None,
            })
            .collect();
        assert_eq!(carried.len(), 2, "the same event replayed once with the answer");
        assert!(carried[0].is_empty());
        assert!(matches!(
            carried[1].as_slice(),
            [ExternalEvidence::Authority {
                verdict: AuthorityVerdict::Abstain,
                ..
            }],
        ));
    }

    #[tokio::test]
    async fn the_random_number_never_repeats_in_a_session() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        for _ in 0..5 {
            runtime.enqueue(deny_decision("blocked", &[]));
            session
                .on_tool_call(call(), false)
                .await
                .expect("the deny is delivered");
        }
        let mut entropies: Vec<_> = runtime
            .engine_seen()
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ModelResponse { entropy, .. } => Some(entropy.0),
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 5);
        entropies.sort();
        entropies.dedup();
        assert_eq!(entropies.len(), 5, "a random number repeated within the session");
    }

    #[test]
    fn an_outcome_report_is_classified_against_the_open_dispatches() {
        let id = bash_dispatch("cc:root");
        let open = |tool: &str, bytes: &[u8]| OpenDispatch {
            id: id.clone(),
            tool: tool.to_string(),
            bytes: bytes.to_vec(),
        };
        let canonical = || Some(b"{}".to_vec());

        assert_eq!(
            classify_report(&call(), canonical, &[]),
            Err(UnreportableOutcome::NoOpenDispatch),
        );
        assert_eq!(
            classify_report(&call(), canonical, &[open("Bash", b"{}")]),
            Ok(id.clone()),
        );
        assert_eq!(
            classify_report(&call(), canonical, &[open("Write", b"{}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "another tool is another call",
        );
        assert_eq!(
            classify_report(&call(), canonical, &[open("Bash", b"{\"other\":1}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "other bytes are another occurrence",
        );
        assert_eq!(
            classify_report(&call(), || None, &[open("Bash", b"{}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "a call that cannot canonicalize matches nothing",
        );
        assert_eq!(
            classify_report(&call(), canonical, &[open("Bash", b"{}"), open("Bash", b"{}")]),
            Err(UnreportableOutcome::NoOpenDispatch),
            "several open dispatches name no one occurrence",
        );
    }
}

#[cfg(test)]
mod real_engine_tests {
    use super::super::{OpenError, OutcomeBody, Runtime, SessionError};
    use super::*;
    use crate::api::{RemedyDecision, SpawnBinding, ToolCallDecision, ToolOutcome, ToolResultDecision};
    use crate::config::Config;

    fn config_with(policy: &str, authority_url: Option<&str>) -> Config {
        let binding = match authority_url {
            Some(url) => format!("[externals.authorities.approver]\nurl = \"{url}\"\n"),
            None => String::new(),
        };
        let text = format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    const FETCH_AND_SEND: &str = r#"
version = 1

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }

[[policy.tool]]
name = "send"
requires = { trust = "trusted" }
delta = {}

# The child tests fork under this fixture; branching takes declared context control.
[policy.deployment]
context_control = true
"#;

    fn root() -> TrajectoryId {
        TrajectoryId("cc:root".to_string())
    }

    fn fetch(spelling: serde_json::Value) -> ProposedCall {
        ProposedCall {
            tool: "fetch".to_string(),
            arguments: raw(spelling),
        }
    }

    async fn open_child(session: &mut Session, spawn: ProposedCall, child: TrajectoryId) -> Session {
        let ToolCallDecision::Allow { spawn: Some(binding) } =
            session.on_tool_call(spawn, true).await.expect("the spawn releases")
        else {
            panic!("a context-controlled spawn releases a fork binding");
        };
        session
            .on_child_start(child, SpawnRef::Binding(binding))
            .expect("the fork binds and the child opens")
    }

    async fn stub(answer: serde_json::Value) -> String {
        use axum::routing::post;
        let app = axum::Router::new().route(
            "/",
            post(move || {
                let answer = answer.clone();
                async move { axum::Json(serde_json::json!({"version": 1, "answer": answer})) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback stub binds");
        let addr = listener.local_addr().expect("the stub has an address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("the stub serves");
        });
        format!("http://{addr}/")
    }

    #[test]
    fn a_policy_in_the_documented_dialect_builds_the_engine() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        assert!(Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None).is_ok());
    }

    #[test]
    fn an_undialectal_policy_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let config = config_with("version = 1\nstray_key = true\n", None);
        assert!(matches!(
            Runtime::open(config, dir.path().join("appa.db"), None),
            Err(OpenError::Policy(_)),
        ));
    }

    #[test]
    fn an_inline_impl_binding_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.authority]]
name = "approver"
[policy.authority.mandate]
attends = ["irreversible"]
[policy.authority.implementation]
builtin = "approve"
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::Policy(
                appa_policy::ConfigError::ForbiddenInlineBinding { .. }
            )),
        ));
    }

    #[test]
    fn a_policy_naming_an_unbound_external_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.authority]]
name = "approver"
[policy.authority.mandate]
attends = ["irreversible"]
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::UnboundExternal { kind: "authority", .. }),
        ));
    }

    #[test]
    fn a_pending_cast_contract_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.tool]]
name = "fetch"
delta = { trust = "unknown" }
[policy.deployment]
confined_results = ["fetch"]
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    #[test]
    fn a_non_neutral_starting_label_seeds_the_root_and_survives_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let policy = r#"
version = 1
[[policy.tool]]
name = "fetch"
[policy.deployment]
starting_label = { trust = "suspicious" }
"#;
        let runtime = Runtime::open(config_with(policy, None), db.clone(), None).expect("the deployment opens");
        runtime.create_session(root()).expect("a fresh id opens");
        let live = runtime.status(&root()).expect("a fresh root answers");
        assert_eq!((live.trust.as_str(), live.audience.as_str()), ("suspicious", "public"));
        drop(runtime);
        let reopened = Runtime::open(config_with(policy, None), db, None).expect("the deployment reopens");
        let restarted = reopened.status(&root()).expect("the persisted root answers");
        assert_eq!((restarted.trust, restarted.audience), (live.trust, live.audience));
    }

    #[test]
    fn liveness_of_an_unopened_child_is_unknown() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        runtime.create_session(root()).expect("a fresh id opens");
        assert!(runtime.live(&root(), &root()).is_ok());
        assert!(matches!(
            runtime.live(&root(), &TrajectoryId("cc:never-bound".to_string())),
            Err(SessionError::Unknown),
        ));
    }

    #[test]
    fn a_cast_declaration_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.cast]]
name = "channel-class"
constant = { trust = "trusted", audience = { exactly = ["public"] } }
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    #[test]
    fn a_reserved_tool_name_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.tool]]
name = "execute_remedy_plan"
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::ReservedTool(_)),
        ));
    }

    #[tokio::test]
    async fn an_allowed_call_is_released_with_canonical_bytes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let decision = session
            .on_tool_call(fetch(serde_json::json!({"b": 1, "a": 2})), false)
            .await
            .expect("the call is decided");
        assert_eq!(decision, ToolCallDecision::Allow { spawn: None });
        let open = runtime
            .open_dispatches(&root(), &root())
            .pop()
            .expect("the released call opened a dispatch");
        assert_eq!(open.bytes, br#"{"a":2,"b":1}"#.to_vec());
        let log = runtime.log_facts(&root());

        let facts: Vec<appa_engine::fact::Fact> = log.clone();
        assert!(matches!(
            facts.last(),
            Some(appa_engine::fact::Fact::DispatchOpened { .. })
        ));
    }

    #[tokio::test]
    async fn a_duplicate_argument_key_is_refused_and_opens_no_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let duplicated = ProposedCall {
            tool: "fetch".to_string(),
            arguments: serde_json::value::RawValue::from_string(r#"{"a":1,"a":2}"#.to_string())
                .expect("the fixture is well-formed JSON"),
        };
        let decision = session
            .on_tool_call(duplicated, false)
            .await
            .expect("the call is decided");
        assert!(
            matches!(decision, ToolCallDecision::Deny { .. }),
            "a duplicate key must be refused, not resolved by last-wins: {decision:?}"
        );
        assert!(
            runtime.open_dispatches(&root(), &root()).pop().is_none(),
            "a refused call opens no dispatch"
        );
    }

    #[tokio::test]
    async fn a_success_admits_the_raw_result_and_closes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is decided");
        let kept = session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(kept, ToolResultDecision::Keep);
        assert!(
            runtime.open_dispatches(&root(), &root()).pop().is_none(),
            "the admitted result closed the dispatch",
        );
    }

    #[tokio::test]
    async fn the_success_checkpoint_commits_once_across_a_lost_admission() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime =
            Runtime::open(emitting_leak_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let outcome = || ToolOutcome::Success {
            body: OutcomeBody::Available("raw with pii".to_string()),
        };

        assert!(matches!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let offer = runtime
            .minted_offers(&root(), &root())
            .last()
            .expect("the block surfaced the sanitize plan")
            .clone();
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the sanitize offer binds"),
            RemedyDecision::Authorized { .. },
        ));
        assert_eq!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the re-proposal resumes"),
            ToolCallDecision::Allow { spawn: None },
        );
        let base = runtime.log_basis(&root());

        runtime.store().fail_commit_after(1);
        assert!(matches!(
            session.on_tool_result(leak(), outcome()).await,
            Err(EventError::Storage(_)),
        ));
        let log = runtime.log_facts(&root());
        assert_eq!(
            runtime.log_basis(&root()),
            base + 1,
            "the checkpoint committed, the admission did not",
        );
        let effects = log
            .iter()
            .cloned()
            .find_map(|fact| match fact {
                appa_engine::fact::Fact::DispatchSucceeded { effects, .. } => Some(effects),
                _ => None,
            })
            .expect("the checkpoint recorded the observed success");
        assert_eq!(effects.len(), 1, "the checkpoint committed the declared effect");
        assert_eq!(
            runtime.open_dispatches(&root(), &root()).len(),
            1,
            "the checkpointed dispatch stays open for its admission",
        );

        let replaced = session
            .on_tool_result(leak(), outcome())
            .await
            .expect("the re-reported outcome admits");
        assert_eq!(
            replaced,
            ToolResultDecision::Replace {
                placeholder: "scrubbed".to_string()
            },
        );
        let log = runtime.log_facts(&root());
        assert_eq!(
            runtime.log_basis(&root()),
            base + 2,
            "the retry appended the admission alone — the checkpoint did not repeat",
        );
        let closed = log
            .iter()
            .cloned()
            .find_map(|fact| match fact {
                appa_engine::fact::Fact::DispatchClosed {
                    outcome: appa_engine::fact::CloseOutcome::Success { effects },
                    ..
                } => Some(effects),
                _ => None,
            })
            .expect("the admission closed the dispatch successfully");
        assert!(
            closed.is_empty(),
            "the close carries no effects: the checkpoint committed them once",
        );
    }

    #[tokio::test]
    async fn a_failure_closes_with_no_effects_and_an_indeterminate_leaves_the_reservation() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is decided");
        let kept = session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Failure {
                    message: "exit 1".to_string(),
                },
            )
            .await
            .expect("the failure closes");
        assert_eq!(kept, ToolResultDecision::Keep);
        assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());

        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the second occurrence is decided");
        let kept = session
            .on_tool_result(fetch(serde_json::json!({"a": 1})), ToolOutcome::Indeterminate)
            .await
            .expect("the indeterminate closes");
        assert_eq!(kept, ToolResultDecision::Keep);
    }

    #[tokio::test]
    async fn an_invalid_argument_call_returns_deny_feedback() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let decision = session
            .on_tool_call(fetch(serde_json::json!({"a": "not a number"})), false)
            .await
            .expect("the refusal is delivered as feedback");
        assert!(matches!(decision, ToolCallDecision::Deny { .. }));
        assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
        let log = runtime.log_facts(&root());
        assert!(
            matches!(log.as_slice(), [appa_engine::fact::Fact::TrajectoryOpened { .. }]),
            "an invalid call appends no record after the opening",
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_call_returns_deny_feedback() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let decision = session
            .on_tool_call(
                ProposedCall {
                    tool: "wrench".to_string(),
                    arguments: raw(serde_json::json!({})),
                },
                false,
            )
            .await
            .expect("the refusal is delivered as feedback");
        assert!(matches!(decision, ToolCallDecision::Deny { .. }));
    }

    fn latest_offer(runtime: &Runtime) -> OfferId {
        runtime
            .minted_offers(&root(), &root())
            .into_iter()
            .next_back()
            .expect("the deny surfaced an offer")
    }

    const READ_ONLY: &str = r#"
version = 1

[[policy.tool]]
name = "read"
parameters = { type = "object", properties = { path = { type = "string" } } }
"#;

    #[tokio::test]
    async fn an_old_root_decides_under_its_opening_policy() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        {
            let runtime =
                Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
                .await
                .expect("the call is decided");
            session
                .on_tool_result(
                    fetch(serde_json::json!({"a": 1})),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("data".to_string()),
                    },
                )
                .await
                .expect("the result is admitted");
        }
        let runtime = Runtime::open(config_with(READ_ONLY, None), db, None).expect("the edited deployment opens");

        let mut old = runtime.session(&root(), &root()).expect("the old root reopens");
        let decision = old
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the old root decides");
        assert_eq!(
            decision,
            ToolCallDecision::Allow { spawn: None },
            "the old root keeps fetch"
        );

        let mut new = runtime
            .create_session(TrajectoryId("cc:new".to_string()))
            .expect("a fresh id opens");
        let denied = new
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the new root decides");
        assert!(
            matches!(denied, ToolCallDecision::Deny { .. }),
            "fetch is gone for new roots"
        );
        let allowed = new
            .on_tool_call(
                ProposedCall {
                    tool: "read".to_string(),
                    arguments: raw(serde_json::json!({"path": "a.txt"})),
                },
                false,
            )
            .await
            .expect("the new root decides");
        assert_eq!(
            allowed,
            ToolCallDecision::Allow { spawn: None },
            "the edited policy's tool releases"
        );
    }

    #[tokio::test]
    async fn a_missing_stored_policy_file_refuses_the_root() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().forget_policy_files();
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn a_stored_file_with_the_same_identity_but_different_bytes_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut tampered = runtime.config_bytes();
        tampered.extend_from_slice(b"\n# tampered\n");
        runtime.store().corrupt_policy_files(&tampered);
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn a_corrupted_opening_batch_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let tampered = serde_json::to_string(&runtime.log_facts(&root()))
            .expect("the opening serializes")
            .replace("cc:root", "cc:evil");
        runtime
            .store()
            .corrupt_batch(&crate::engine::engine_id(&root()), 0, tampered.as_bytes());
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn a_missing_binding_abstains_and_a_restored_binding_answers_for_an_old_root() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        {
            let runtime = Runtime::open(
                config_with(ATTENTION, Some("https://approver.internal/")),
                db.clone(),
                None,
            )
            .expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered");
        }

        {
            let runtime =
                Runtime::open(config_with(READ_ONLY, None), db.clone(), None).expect("the edited deployment opens");
            let mut session = runtime.session(&root(), &root()).expect("the old root reopens");
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered");
            let offer = latest_offer(&runtime);
            let log_before = runtime.log_facts(&root());
            let got = session
                .on_remedy(offer.clone(), None)
                .await
                .expect("the no-answer is delivered");
            assert!(matches!(got, RemedyDecision::NoAnswer { .. }), "got {got:?}");
            let log_after = runtime.log_facts(&root());
            assert_eq!(
                log_before.len(),
                log_after.len(),
                "an abstention appends no fact",
            );
            assert!(matches!(
                session.on_remedy(offer, None).await.expect("the offer is still live"),
                RemedyDecision::NoAnswer { .. },
            ));
        }

        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(READ_ONLY, Some(&url)), db, None)
            .expect("the deployment with the restored binding opens");
        let mut session = runtime.session(&root(), &root()).expect("the old root reopens");
        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        let offer = latest_offer(&runtime);
        runtime.store().fail_commit_after(0);
        assert!(matches!(
            session.on_remedy(offer.clone(), None).await,
            Err(EventError::Storage(_)),
        ));
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the retry executes"),
            RemedyDecision::Authorized { .. },
        ));
    }

    #[tokio::test]
    async fn a_reopened_store_continues_the_trajectory() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        {
            let runtime =
                Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
                .await
                .expect("the call is decided");
            session
                .on_tool_result(
                    fetch(serde_json::json!({"a": 1})),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("data".to_string()),
                    },
                )
                .await
                .expect("the result is admitted");
        }
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
        let mut session = runtime.session(&root(), &root()).expect("the trajectory reopens");
        let decision = session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the reopened trajectory decides");
        assert_eq!(decision, ToolCallDecision::Allow { spawn: None });
    }

    #[tokio::test]
    async fn an_undecodable_batch_row_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .store()
            .corrupt_batch(&crate::engine::engine_id(&root()), 0, b"not engine records");
        assert!(matches!(
            session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
            Err(EventError::UntrustedLog(_)),
        ));
    }

    #[tokio::test]
    async fn a_corrupt_batch_row_is_refused_before_any_decision() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is decided");
        let released: Vec<_> = runtime
            .log_facts(&root())
            .into_iter()
            .skip_while(|fact| matches!(fact, appa_engine::fact::Fact::TrajectoryOpened { .. }))
            .collect();
        let tampered = serde_json::to_string(&released)
            .expect("the batch serializes")
            .replace("\"fetch\"", "\"wrench\"");
        runtime
            .store()
            .corrupt_batch(&crate::engine::engine_id(&root()), 1, tampered.as_bytes());
        assert!(matches!(
            session
                .on_tool_result(
                    fetch(serde_json::json!({"a": 1})),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("data".to_string()),
                    },
                )
                .await,
            Err(EventError::UntrustedLog(_)),
        ));
    }

    #[tokio::test]
    async fn an_unestablished_block_is_terminal_feedback() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the fetch is decided");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("untrusted data".to_string()),
                },
            )
            .await
            .expect("the result is admitted at Unknown");

        let decision = session
            .on_tool_call(
                ProposedCall {
                    tool: "send".to_string(),
                    arguments: raw(serde_json::json!({})),
                },
                false,
            )
            .await
            .expect("the block is delivered");
        let ToolCallDecision::Deny { feedback } = decision else {
            panic!("a consumed Unknown dimension must block the sink");
        };
        assert!(
            feedback.contains("the result of fetch (ValueId(0))"),
            "the block must name the producing tool: {feedback}"
        );
        assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
    }

    #[tokio::test]
    async fn a_subject_a_concurrent_event_moved_reads_as_lifecycle_not_a_fault() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let refuse =
            |trajectory: &TrajectoryId, event: crate::engine::EngineEvent| runtime.refuse(&root(), trajectory, event);

        let child = TrajectoryId("cc:root:child".to_string());
        let mut child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child.clone()).await;
        child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect("the child returns");

        let error = refuse(
            &child,
            crate::engine::EngineEvent::ModelResponse {
                call: fetch(serde_json::json!({"a": 2})),
                evidence: Vec::new(),
                entropy: fresh_entropy(),
                spawn: false,
            },
        );
        assert!(
            matches!(error, EventError::TrajectoryEnded),
            "a call on an ended branch is a lifecycle condition; got {error:?}",
        );
        assert!(!error.is_operational());

        for value in [Some("again".to_string()), None] {
            let error = refuse(
                &root(),
                crate::engine::EngineEvent::ChildReturn {
                    child: child.clone(),
                    value: value.clone(),
                    evidence: Vec::new(),
                    entropy: fresh_entropy(),
                },
            );
            assert!(
                matches!(error, EventError::TrajectoryEnded),
                "a duplicate return answers as a later one would; got {error:?} for {value:?}",
            );
            assert!(!error.is_operational());
        }
    }

    #[tokio::test]
    async fn a_marked_spawn_without_context_control_releases_unmarked() {
        const UNCONTROLLED: &str = r#"
version = 1

[[policy.tool]]
name = "spawn"
delta = {}

[policy.deployment]
context_control = false
"#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(UNCONTROLLED, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let decision = session
            .on_tool_call(
                ProposedCall {
                    tool: "spawn".to_string(),
                    arguments: raw(serde_json::json!({})),
                },
                true,
            )
            .await
            .expect("the marked call is decided");
        assert_eq!(
            decision,
            ToolCallDecision::Allow { spawn: None },
            "the mark is refused and the call releases as an ordinary flow, not a fork",
        );
    }

    #[tokio::test]
    async fn a_child_is_forked_and_a_clean_return_crosses() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({"a": 1})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        let log = runtime.log_facts(&root());
        let fork_records: Vec<_> = log
            .iter()
            .filter(|fact| {
                matches!(
                    fact,
                    appa_engine::fact::Fact::ForkPrepared { .. } | appa_engine::fact::Fact::ForkOpened { .. }
                )
            })
            .collect();
        assert!(matches!(
            fork_records.as_slice(),
            [
                appa_engine::fact::Fact::ForkPrepared { .. },
                appa_engine::fact::Fact::ForkOpened { .. },
            ],
        ));

        let returned = child
            .on_child_end(Some("all done".to_string()))
            .await
            .expect("the clean return crosses");
        assert_eq!(
            returned,
            crate::api::ChildReturnDecision::Returned {
                value: "all done".to_string()
            },
        );
        assert!(matches!(
            runtime.live(&root(), &TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));
    }

    #[tokio::test]
    async fn a_child_return_with_unknown_fold_crosses_and_charges_the_parent() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({"a": 9})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        child
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the child's fetch is decided");
        child
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("untrusted".to_string()),
                },
            )
            .await
            .expect("the child's result is admitted at Unknown");

        let returned = child
            .on_child_end(Some("summary of untrusted data".to_string()))
            .await
            .expect("the crossing merges");
        assert_eq!(
            returned,
            crate::api::ChildReturnDecision::Returned {
                value: "summary of untrusted data".to_string()
            },
        );
        assert!(matches!(
            runtime.live(&root(), &TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));

        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 9})),
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
            )
            .await
            .expect("the spawn dispatch closes");

        let decision = session
            .on_tool_call(
                ProposedCall {
                    tool: "send".to_string(),
                    arguments: raw(serde_json::json!({})),
                },
                false,
            )
            .await
            .expect("the block is delivered");
        let ToolCallDecision::Deny { feedback } = decision else {
            panic!("the crossed unresolved identity must charge the parent's send, got {decision:?}");
        };
        assert!(
            !feedback.contains("fetch"),
            "a parent-facing block must not name the child's tool: {feedback}"
        );
        assert!(
            feedback.contains("value ValueId(0)"),
            "the charged value is named by its id: {feedback}"
        );
    }

    const ATTENTION: &str = r#"
version = 1

[[policy.tool]]
name = "wire"
parameters = { type = "object", properties = { amount = { type = "integer" } } }
requires = { attention = ["irreversible"] }
delta = {}

[[policy.authority]]
name = "approver"
[policy.authority.mandate]
attends = ["irreversible"]
"#;

    fn wire(amount: u64) -> ProposedCall {
        ProposedCall {
            tool: "wire".to_string(),
            arguments: raw(serde_json::json!({"amount": amount})),
        }
    }

    fn surfaced_offer(runtime: &Runtime) -> OfferId {
        surfaced_offer_for(runtime, &root(), &root())
    }

    fn surfaced_offer_for(runtime: &Runtime, root: &TrajectoryId, trajectory: &TrajectoryId) -> OfferId {
        runtime
            .minted_offers(root, trajectory)
            .into_iter()
            .next()
            .expect("the deny surfaced an offer")
    }

    #[tokio::test]
    async fn an_authority_approval_authorizes_the_exact_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        let denied = session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }));
        let offer = surfaced_offer(&runtime);

        let authorized = session
            .on_remedy(offer.clone(), None)
            .await
            .expect("the remedy executes");
        let RemedyDecision::Authorized { call } = authorized else {
            panic!("an approval must authorize the call");
        };
        assert_eq!(call.tool, "wire");
        assert_eq!(call.bytes, br#"{"amount":500}"#.to_vec());

        let resumed = session
            .on_tool_call(wire(500), false)
            .await
            .expect("the re-proposal resumes");
        assert_eq!(resumed, ToolCallDecision::Allow { spawn: None });
        let kept = session
            .on_tool_result(
                wire(500),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("sent".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(kept, ToolResultDecision::Keep);

        assert!(matches!(
            session.on_remedy(offer, None).await,
            Ok(RemedyDecision::Authorized { .. }),
        ));

        let consumed = runtime
            .log_facts(&root())
            .iter()
            .filter(|fact| matches!(fact, appa_engine::fact::Fact::CallApprovalConsumed { .. }))
            .count();
        assert_eq!(consumed, 1, "the approval is consumed exactly once");
    }

    #[tokio::test]
    async fn a_denial_retires_plans_naming_the_denier_and_sticks() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        let offer = surfaced_offer(&runtime);
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the denial is delivered"),
            RemedyDecision::Declined { .. },
        ));
        let before = runtime.minted_offers(&root(), &root()).len();
        assert!(matches!(
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the re-block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let after = runtime.minted_offers(&root(), &root()).len();
        assert_eq!(after, before, "no new offer names the denying authority");
    }

    #[tokio::test]
    async fn a_denial_retires_only_the_owning_trajectorys_offers() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let first_id = root();
        let second_id = TrajectoryId("cc:second-root".to_string());
        let mut first = runtime.create_session(first_id.clone()).expect("the first root opens");
        let mut second = runtime
            .create_session(second_id.clone())
            .expect("the second root opens");

        first
            .on_tool_call(wire(500), false)
            .await
            .expect("the first block is delivered");
        second
            .on_tool_call(wire(500), false)
            .await
            .expect("the second block is delivered");
        let first_offer = surfaced_offer_for(&runtime, &first_id, &first_id);
        let second_offer = surfaced_offer_for(&runtime, &second_id, &second_id);

        assert!(matches!(
            first
                .on_remedy(first_offer, None)
                .await
                .expect("the first denial is delivered"),
            RemedyDecision::Declined { .. },
        ));
        assert_eq!(
            runtime.minted_offers(&second_id, &second_id),
            vec![second_offer],
            "one trajectory's denial must not retire another trajectory's same-call offer"
        );
    }

    #[tokio::test]
    async fn an_abstain_keeps_the_offer() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"note": "still thinking"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        let offer = surfaced_offer(&runtime);
        assert!(matches!(
            session
                .on_remedy(offer.clone(), None)
                .await
                .expect("the no-answer is delivered"),
            RemedyDecision::NoAnswer { .. },
        ));
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the offer is still live"),
            RemedyDecision::NoAnswer { .. },
        ));
    }

    #[tokio::test]
    async fn a_failed_commit_leaves_the_offer_standing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        session
            .on_tool_call(wire(500), false)
            .await
            .expect("the block is delivered");
        let offer = surfaced_offer(&runtime);
        runtime.store().fail_commit_after(0);
        assert!(matches!(
            session.on_remedy(offer.clone(), None).await,
            Err(EventError::Storage(_)),
        ));
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the retry executes"),
            RemedyDecision::Authorized { .. },
        ));
    }

    #[tokio::test]
    async fn an_offer_survives_a_restart_and_executes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let db = dir.path().join("appa.db");
        let offer = {
            let runtime =
                Runtime::open(config_with(ATTENTION, Some(&url)), db.clone(), None).expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered");
            surfaced_offer(&runtime)
        };
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None).expect("the deployment reopens");
        let mut session = runtime.session(&root(), &root()).expect("the trajectory reopens");
        assert!(matches!(
            session
                .on_remedy(offer, None)
                .await
                .expect("the reopened offer executes"),
            RemedyDecision::Authorized { .. },
        ));
    }

    const SUBSTITUTED_SEND: &str = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { includes = ["public"] } }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["hr"] }, to = { exactly = ["public"] } }
"#;

    const SUBSTITUTED_ATTENDED_SEND: &str = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { includes = ["public"] }, attention = ["irreversible"] }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["hr"] }, to = { exactly = ["public"] } }

[[policy.authority]]
name = "approver"
[policy.authority.mandate]
attends = ["irreversible"]
"#;

    const SUBSTITUTED_SEND_FORKING: &str = r#"
version = 1

[[policy.tool]]
name = "read_hr"
delta = { audience = { exactly = ["hr"] } }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { includes = ["public"] } }
delta = {}

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["hr"] }, to = { exactly = ["public"] } }

[policy.deployment]
context_control = true
"#;

    fn substituting_config(policy: &str, authority_url: Option<&str>) -> Config {
        let binding = match authority_url {
            Some(url) => format!("[externals.authorities.approver]\nurl = \"{url}\"\n"),
            None => String::new(),
        };
        let text = format!(
            "[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n\
             [externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n{binding}"
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    fn send(body: &str) -> ProposedCall {
        ProposedCall {
            tool: "send".to_string(),
            arguments: raw(serde_json::json!({"body": body})),
        }
    }

    const RAW_BODY: &str = "mail alice@corp.example today";
    const REDACTED_BODY: &str = "mail [redacted-email] today";

    async fn narrowed_and_blocked(runtime: &Runtime, session: &mut Session) -> OfferId {
        narrowed_and_blocked_on(runtime, session, &root()).await
    }

    async fn narrowed_and_blocked_on(runtime: &Runtime, session: &mut Session, trajectory: &TrajectoryId) -> OfferId {
        let read = ProposedCall {
            tool: "read_hr".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        assert!(matches!(
            session.on_tool_call(read.clone(), false).await,
            Ok(ToolCallDecision::Deny { .. }),
        ));
        let accept = surfaced_offer_for(runtime, &root(), trajectory);
        assert!(matches!(
            session.on_remedy(accept, None).await,
            Ok(RemedyDecision::Authorized { .. }),
        ));
        assert_eq!(
            session
                .on_tool_call(read.clone(), false)
                .await
                .expect("the read releases"),
            ToolCallDecision::Allow { spawn: None },
        );
        session
            .on_tool_result(
                read,
                ToolOutcome::Success {
                    body: OutcomeBody::Available("Alice Chen".to_string()),
                },
            )
            .await
            .expect("the read closes");
        assert!(matches!(
            session.on_tool_call(send(RAW_BODY), false).await,
            Ok(ToolCallDecision::Deny { .. }),
        ));
        runtime
            .minted_offers(&root(), trajectory)
            .pop()
            .expect("the block surfaced an offer")
    }

    fn standing_release(runtime: &Runtime) -> Option<crate::engine::OpenDispatch> {
        runtime.substituted_release(&root(), &root())
    }

    fn last_offer(runtime: &Runtime) -> OfferId {
        runtime
            .minted_offers(&root(), &root())
            .pop()
            .expect("the block surfaced an offer")
    }

    #[tokio::test]
    async fn an_input_substitution_releases_the_replaced_call_and_its_outcome_closes_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_SEND, None),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let hop = narrowed_and_blocked(&runtime, &mut session).await;

        let substituted = session.on_remedy(hop.clone(), None).await.expect("the hop executes");
        let bytes = format!(r#"{{"body":"{REDACTED_BODY}"}}"#).into_bytes();
        assert_eq!(
            substituted,
            RemedyDecision::Substituted {
                call: ExactCall {
                    tool: "send".to_string(),
                    bytes: bytes.clone(),
                },
            },
        );
        let standing = standing_release(&runtime).expect("the replaced call stands open");
        assert_eq!(
            (standing.tool.as_str(), standing.bytes.as_slice()),
            ("send", bytes.as_slice()),
        );

        assert_eq!(
            session.on_remedy(hop.clone(), None).await.expect("the replay answers"),
            substituted,
        );
        assert_eq!(runtime.open_dispatches(&root(), &root()).len(), 1);

        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the substituted call is allowed"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the repeat is handed the same release"),
            ToolCallDecision::Allow { spawn: None },
        );

        assert_eq!(
            session
                .on_tool_result(
                    send(REDACTED_BODY),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("sent".to_string()),
                    },
                )
                .await
                .expect("the outcome is reported"),
            ToolResultDecision::Keep,
        );
        assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        assert!(matches!(
            session.on_remedy(hop, None).await,
            Ok(RemedyDecision::Declined { .. }),
        ));

        let entries = runtime.audit(&root()).expect("the audit reads");
        let sends: Vec<_> = entries
            .iter()
            .filter(|entry| matches!(&entry.event, crate::engine::AuditEvent::Released { tool, .. } if tool == "send"))
            .collect();
        assert_eq!(sends.len(), 1, "the replaced call is released once: {entries:?}");
        assert!(
            entries.iter().any(|entry| matches!(
                &entry.event,
                crate::engine::AuditEvent::Closed {
                    outcome: crate::engine::DispatchOutcome::Ran { .. }
                }
            )),
            "the replaced call's dispatch closed as run: {entries:?}"
        );
    }

    #[tokio::test]
    async fn an_unusable_derivation_leaves_the_offer_standing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_SEND, None),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let read = ProposedCall {
            tool: "read_hr".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        session
            .on_tool_call(read.clone(), false)
            .await
            .expect("the read is decided");
        session
            .on_remedy(surfaced_offer(&runtime), None)
            .await
            .expect("the narrowing is accepted");
        session
            .on_tool_call(read.clone(), false)
            .await
            .expect("the read releases");
        session
            .on_tool_result(
                read,
                ToolOutcome::Success {
                    body: OutcomeBody::Available("Alice Chen".to_string()),
                },
            )
            .await
            .expect("the read closes");
        session
            .on_tool_call(send("mail alice@corp.example"), false)
            .await
            .expect("the send is decided");
        let hop = last_offer(&runtime);

        for _ in 0..2 {
            assert!(matches!(
                session.on_remedy(hop.clone(), None).await,
                Ok(RemedyDecision::NoAnswer { .. }),
            ));
            assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        }
    }

    #[tokio::test]
    async fn another_call_while_a_substituted_call_stands_abandons_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_SEND, None),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let hop = narrowed_and_blocked(&runtime, &mut session).await;
        session.on_remedy(hop.clone(), None).await.expect("the hop executes");
        assert!(standing_release(&runtime).is_some());

        let other = ProposedCall {
            tool: "read_hr".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        assert!(matches!(
            session.on_tool_call(other.clone(), false).await,
            Err(EventError::SubstitutionAbandoned { tool }) if tool == "send",
        ));
        assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        let entries = runtime.audit(&root()).expect("the audit reads");
        assert!(
            entries.iter().any(|entry| matches!(
                &entry.event,
                crate::engine::AuditEvent::Closed {
                    outcome: crate::engine::DispatchOutcome::Failed
                }
            )),
            "the abandoned dispatch closed as not run: {entries:?}"
        );

        assert_eq!(
            session.on_tool_call(other, false).await.expect("the repeat is decided"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert!(matches!(
            session.on_remedy(hop, None).await,
            Ok(RemedyDecision::Declined { .. }),
        ));
    }

    #[tokio::test]
    async fn a_substituted_call_survives_a_restart_and_runs() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        {
            let runtime = Runtime::open(substituting_config(SUBSTITUTED_SEND, None), db.clone(), None)
                .expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            let hop = narrowed_and_blocked(&runtime, &mut session).await;
            session.on_remedy(hop, None).await.expect("the hop executes");
        }
        let runtime =
            Runtime::open(substituting_config(SUBSTITUTED_SEND, None), db, None).expect("the deployment reopens");
        let mut session = runtime.session(&root(), &root()).expect("the trajectory reopens");
        assert!(standing_release(&runtime).is_some());
        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the substituted call is allowed after the restart"),
            ToolCallDecision::Allow { spawn: None },
        );
    }

    #[tokio::test]
    async fn a_failed_abandonment_leaves_the_substituted_call_standing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_SEND, None),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let hop = narrowed_and_blocked(&runtime, &mut session).await;
        session.on_remedy(hop, None).await.expect("the hop executes");

        let other = ProposedCall {
            tool: "read_hr".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        runtime.store().fail_commit_after(0);
        assert!(matches!(
            session.on_tool_call(other, false).await,
            Err(EventError::Storage(_)),
        ));
        assert!(standing_release(&runtime).is_some());

        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the replaced call still runs"),
            ToolCallDecision::Allow { spawn: None },
        );
    }

    #[tokio::test]
    async fn a_hop_that_leaves_a_gap_chains_into_an_approval_of_the_replaced_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_ATTENDED_SEND, Some(&url)),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let hop = narrowed_and_blocked(&runtime, &mut session).await;

        assert!(matches!(
            session.on_remedy(hop, None).await,
            Ok(RemedyDecision::Declined { .. }),
        ));
        assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        let approval = last_offer(&runtime);
        assert_eq!(
            session.on_remedy(approval, None).await.expect("the approval executes"),
            RemedyDecision::Authorized {
                call: ExactCall {
                    tool: "send".to_string(),
                    bytes: format!(r#"{{"body":"{REDACTED_BODY}"}}"#).into_bytes(),
                },
            },
        );
        assert!(matches!(
            session.on_tool_call(send(RAW_BODY), false).await,
            Ok(ToolCallDecision::Deny { .. }),
        ));
        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the approved call releases"),
            ToolCallDecision::Allow { spawn: None },
        );
    }

    const SANITIZED_CHILD: &str = r#"
version = 1

[[policy.tool]]
name = "fetch"

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[policy.child]
return_sanitizer = "scrub"

[policy.deployment]
context_control = true
confined_child_return = true
"#;

    fn sanitized_config(url: Option<&str>) -> Config {
        let binding = match url {
            Some(url) => format!("[externals.sanitizers.scrub]\nurl = \"{url}\"\n"),
            None => String::new(),
        };
        let text =
            format!("[policy]\n{SANITIZED_CHILD}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    #[tokio::test]
    async fn a_sanitized_child_return_crosses_as_the_derivation() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        let returned = child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the sanitized return crosses");
        assert_eq!(
            returned,
            crate::api::ChildReturnDecision::Returned {
                value: "scrubbed".to_string()
            },
        );
    }

    #[tokio::test]
    async fn a_duplicate_sanitized_return_reads_as_an_ended_child() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the sanitized return crosses");

        let error = child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect_err("the duplicate meets an ended child");
        assert!(matches!(error, EventError::TrajectoryEnded), "got {error:?}",);
        assert!(!error.is_operational());
    }

    #[tokio::test]
    async fn a_sanitizer_with_no_answer_withholds_the_crossing_and_ends_the_branch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!(42)).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        let blocked = child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the withheld return is delivered");
        let crate::api::ChildReturnDecision::Blocked { .. } = blocked else {
            panic!("a no-answer sanitizer must withhold the crossing");
        };
        assert!(matches!(
            runtime.live(&root(), &TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));
    }

    const ATTESTED_CHILD: &str = r#"
version = 1

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[policy.deployment]
context_control = true
confined_child_return = true
"#;

    const ATTESTED_CHILD_COMPOSED: &str = r#"
version = 1

[[sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[deployment]
context_control = true
confined_child_return = true
"#;

    const ATTEST_BOUND_CHILD: &str = r#"
version = 1

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[policy.child]
return_sanitizer = "attest-schema"

[policy.deployment]
context_control = true
confined_child_return = true
"#;

    fn attested_config(policy: &str, binding: Option<&str>) -> Config {
        let binding = match binding {
            Some(url) => format!("[externals.sanitizers.attest-schema]\nurl = \"{url}\"\n"),
            None => String::new(),
        };
        let text = format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    fn bare_externals() -> crate::config::Externals {
        crate::config::Externals {
            timeout: std::time::Duration::from_millis(2000),
            review_timeout: std::time::Duration::from_millis(2000),
            max_body_bytes: 65536,
            authorities: std::collections::BTreeMap::new(),
            sanitizers: std::collections::BTreeMap::new(),
            dynamic: None,
            membership: None,
        }
    }

    #[test]
    fn the_reserved_attest_schema_needs_no_externals_binding() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        assert!(Runtime::open(attested_config(ATTESTED_CHILD, None), dir.path().join("appa.db"), None).is_ok());
    }

    #[test]
    fn an_externals_binding_on_the_reserved_attest_schema_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        assert!(matches!(
            Runtime::open(
                attested_config(ATTESTED_CHILD, Some("http://127.0.0.1:1/")),
                dir.path().join("appa.db"),
                None,
            ),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    #[test]
    fn a_child_bound_attest_schema_opens() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        assert!(
            Runtime::open(
                attested_config(ATTEST_BOUND_CHILD, None),
                dir.path().join("appa.db"),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn an_embedded_attest_schema_policy_needs_no_externals_binding() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let config =
            Config::embedded(ATTESTED_CHILD_COMPOSED.to_string(), bare_externals()).expect("the policy embeds");
        assert!(Runtime::open(config, dir.path().join("appa.db"), None).is_ok());
    }

    #[test]
    fn an_embedded_binding_on_the_reserved_attest_schema_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let mut externals = bare_externals();
        externals.sanitizers.insert(
            "attest-schema".to_string(),
            crate::config::Implementation::Resolver(crate::config::Endpoint {
                url: "http://127.0.0.1:1/".to_string(),
                token: None,
            }),
        );
        let config = Config::embedded(ATTESTED_CHILD_COMPOSED.to_string(), externals).expect("the policy embeds");
        assert!(matches!(
            Runtime::open(config, dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    #[test]
    fn an_unregistered_attest_schema_binding_still_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let mut externals = bare_externals();
        externals.sanitizers.insert(
            "attest-schema".to_string(),
            crate::config::Implementation::Resolver(crate::config::Endpoint {
                url: "http://127.0.0.1:1/".to_string(),
                token: None,
            }),
        );
        let config = Config::embedded(
            "version = 1\n\n[deployment]\ncontext_control = true\n".to_string(),
            externals,
        )
        .expect("the policy embeds");
        assert!(matches!(
            Runtime::open(config, dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    const NARROWING: &str = r#"
version = 1

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
delta = { audience = { exactly = ["internal"] } }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[policy.deployment]
confined_results = ["leak"]
"#;

    fn narrowing_config(url: &str) -> Config {
        let text = format!(
            "[policy]\n{NARROWING}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    const EMITTING_LEAK: &str = r#"
version = 1

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
effects = ["leak"]
delta = { audience = { exactly = ["internal"] } }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[policy.deployment]
confined_results = ["leak"]
"#;

    fn emitting_leak_config(url: &str) -> Config {
        let text = format!(
            "[policy]\n{EMITTING_LEAK}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    fn leak() -> ProposedCall {
        ProposedCall {
            tool: "leak".to_string(),
            arguments: raw(serde_json::json!({"q": "all"})),
        }
    }

    async fn run_sanitize_offer(runtime: &Runtime, session: &mut crate::api::Session) -> ToolResultDecision {
        let offers = runtime.minted_offers(&root(), &root());
        let offer = offers.last().expect("the block surfaced offers").clone();
        let authorized = session.on_remedy(offer, None).await.expect("the offer executes");
        assert!(matches!(authorized, RemedyDecision::Authorized { .. }));
        assert_eq!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the re-proposal resumes"),
            ToolCallDecision::Allow { spawn: None },
        );
        session
            .on_tool_result(
                leak(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("raw with pii".to_string()),
                },
            )
            .await
            .expect("the outcome is delivered")
    }

    #[tokio::test]
    async fn a_bound_sanitizer_derivation_replaces_the_raw() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime =
            Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let decision = run_sanitize_offer(&runtime, &mut session).await;
        assert_eq!(
            decision,
            ToolResultDecision::Replace {
                placeholder: "scrubbed".to_string()
            },
            "the derivation is admitted and the raw is withheld",
        );
        assert!(runtime.open_dispatches(&root(), &root()).pop().is_none());
    }

    #[tokio::test]
    async fn a_bound_sanitizer_no_answer_withholds_and_stays_retryable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!(42)).await;
        let runtime =
            Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let decision = run_sanitize_offer(&runtime, &mut session).await;
        let ToolResultDecision::Replace { placeholder } = decision else {
            panic!("the raw must be withheld");
        };
        assert!(!placeholder.contains("pii"), "the raw body never reaches the model");
        assert!(
            runtime.open_dispatches(&root(), &root()).pop().is_some(),
            "a no-answer sanitizer leaves the dispatch open so the return may be retried",
        );
    }

    const MARKED: &str = r#"
version = 1

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }

[[policy.tool]]
name = "mark"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }

[[policy.tool]]
name = "bare"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = {}

[policy.deployment]
context_control = true
"#;

    fn mark() -> ProposedCall {
        ProposedCall {
            tool: "mark".to_string(),
            arguments: raw(serde_json::json!({"a": 1})),
        }
    }

    async fn admit_success(runtime: &Runtime, session: &mut Session, call: ProposedCall) {
        let decision = session
            .on_tool_call(call.clone(), false)
            .await
            .expect("the call is decided");
        if matches!(decision, ToolCallDecision::Deny { .. }) {
            let offers = runtime.minted_offers(&root(), session.trajectory());
            let offer = offers
                .first()
                .expect("the narrowing block surfaced its acceptance")
                .clone();
            assert!(matches!(
                session.on_remedy(offer, None).await.expect("the acceptance executes"),
                RemedyDecision::Authorized { .. },
            ));
            assert_eq!(
                session
                    .on_tool_call(call.clone(), false)
                    .await
                    .expect("the re-proposal resumes"),
                ToolCallDecision::Allow { spawn: None },
            );
        }
        let kept = session
            .on_tool_result(
                call,
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(kept, ToolResultDecision::Keep, "the fixture result must actually admit");
    }

    #[test]
    fn a_fresh_root_status_renders_the_neutral_label() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        runtime.create_session(root()).expect("a fresh id opens");
        let status = runtime.status(&root()).expect("a fresh root answers");
        assert_eq!(status.trajectory, "cc:root");
        assert_eq!(status.trust, "trusted");
        assert_eq!(status.audience, "public");
    }

    #[tokio::test]
    async fn a_suspicious_admission_narrows_the_status_irreversibly() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        admit_success(&runtime, &mut session, mark()).await;
        assert_eq!(runtime.status(&root()).expect("the root answers").trust, "suspicious");
        admit_success(
            &runtime,
            &mut session,
            ProposedCall {
                tool: "bare".to_string(),
                arguments: raw(serde_json::json!({"a": 2})),
            },
        )
        .await;
        let status = runtime.status(&root()).expect("the root answers");
        assert_eq!(status.trust, "suspicious", "the fold never widens");
        assert_eq!(status.audience, "public", "a neutral admission resolves cleanly");
    }

    #[tokio::test]
    async fn an_unresolved_dimension_renders_unknown() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        admit_success(&runtime, &mut session, fetch(serde_json::json!({"a": 1}))).await;
        let status = runtime.status(&root()).expect("the root answers");
        assert_eq!(status.trust, "unknown");
        assert_eq!(status.audience, "unknown");
    }

    #[tokio::test]
    async fn the_status_read_appends_nothing_and_outlives_the_session() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        admit_success(&runtime, &mut session, mark()).await;

        assert!(runtime.status(&TrajectoryId("cc:ghost".to_string())).is_none());

        let before = runtime.log_facts(&root()).len();
        runtime.status(&root()).expect("the root answers");
        runtime.status(&root()).expect("the root answers again");
        let after = runtime.log_facts(&root()).len();
        assert_eq!(before, after, "a status read appends nothing");

        assert_eq!(runtime.status(&root()).expect("the root answers").trust, "suspicious",);
    }

    #[tokio::test]
    async fn an_untrusted_log_answers_no_status() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .store()
            .corrupt_batch(&crate::engine::engine_id(&root()), 0, b"not engine records");
        assert!(runtime.status(&root()).is_none());
    }

    #[tokio::test]
    async fn an_ended_child_is_refused_on_the_proposal_path() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = open_child(
            &mut session,
            fetch(serde_json::json!({"a": 1})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        child
            .on_child_end(Some("done".to_string()))
            .await
            .expect("the child returns");

        let error = child
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect_err("the ended child proposes nothing further");
        assert!(matches!(error, EventError::TrajectoryEnded), "got {error:?}");
        assert!(!error.is_operational());
    }

    #[tokio::test]
    async fn a_childs_fold_stays_out_of_the_root_status() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child_id = TrajectoryId("cc:child".to_string());
        let mut child = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child_id.clone()).await;
        admit_success(&runtime, &mut child, mark()).await;

        let child_status = runtime
            .branch_status(&root(), &child_id)
            .expect("the child's branch renders");
        assert_eq!(
            child_status.trust, "suspicious",
            "the child's admission narrowed its branch"
        );

        assert_eq!(
            runtime.status(&root()).expect("the root answers").trust,
            "trusted",
            "a dirty child never moves the root fold",
        );
        assert!(runtime.status(&child_id).is_none(), "the status read is root-only");
    }

    fn child(name: &str) -> TrajectoryId {
        TrajectoryId(format!("cc:{name}"))
    }

    fn fork_opened_count(runtime: &Runtime) -> usize {
        runtime
            .log_facts(&root())
            .iter()
            .filter(|fact| matches!(fact, appa_engine::fact::Fact::ForkOpened { .. }))
            .count()
    }

    fn dispatch_open(runtime: &Runtime, trajectory: &TrajectoryId) -> bool {
        !runtime.open_dispatches(&root(), trajectory).is_empty()
    }

    fn opened(runtime: &Runtime, trajectory: &TrajectoryId) -> bool {
        runtime.names_trajectory(&root(), trajectory)
    }

    async fn release_spawn(session: &mut Session, spawn: ProposedCall) -> SpawnBinding {
        match session.on_tool_call(spawn, true).await.expect("the spawn is decided") {
            ToolCallDecision::Allow { spawn: Some(binding) } => binding,
            other => panic!("a context-controlled spawn releases a fork binding, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_child_start_repeats_as_the_same_act_and_refuses_any_other_pairing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        let mut first = session
            .on_child_start(child("c1"), SpawnRef::Binding(binding.clone()))
            .expect("the fork binds and the child opens");
        let after_open = runtime.log_basis(&root());
        assert_eq!(fork_opened_count(&runtime), 1);

        session
            .on_child_start(child("c1"), SpawnRef::Binding(binding.clone()))
            .expect("the same binding again is the same act");
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the same child named as the spawn in flight is the same act");
        assert_eq!(
            runtime.log_basis(&root()),
            after_open,
            "a repeated start appends nothing"
        );
        assert_eq!(fork_opened_count(&runtime), 1);

        let error = session
            .on_child_start(child("c2"), SpawnRef::Binding(binding.clone()))
            .err()
            .expect("a bound fork takes no second child");
        assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
        assert!(!error.is_operational());
        assert!(!opened(&runtime, &child("c2")), "the refused start opens no trajectory");

        let inner = release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
        let error = session
            .on_child_start(child("c1"), SpawnRef::Binding(inner))
            .err()
            .expect("a bound child takes no second fork");
        assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");

        for elsewhere in ["cc:root", "cc:elsewhere"] {
            let error = session
                .on_child_start(
                    child("c3"),
                    SpawnRef::Binding(crate::api::testing::spawn_binding(elsewhere)),
                )
                .err()
                .expect("an unprepared fork opens no child");
            assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
            assert!(!error.is_operational());
        }
        assert!(!opened(&runtime, &child("c3")));

        assert_eq!(fork_opened_count(&runtime), 1, "no refusal bound anything");
        assert!(
            dispatch_open(&runtime, &root()),
            "the spawn dispatch stays open through every refusal"
        );
    }

    #[tokio::test]
    async fn two_concurrent_identical_starts_open_one_child() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;

        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let starters: Vec<_> = (0..2)
                .map(|_| {
                    let mut handle = runtime.session(&root(), &root()).expect("the root reopens");
                    let binding = binding.clone();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        handle.on_child_start(child("c1"), SpawnRef::Binding(binding))
                    })
                })
                .collect();
            for starter in starters {
                starter
                    .join()
                    .expect("the starter thread joins")
                    .expect("both identical starts pass");
            }
        });
        assert_eq!(fork_opened_count(&runtime), 1, "one binding landed");
        assert!(opened(&runtime, &child("c1")));
    }

    #[tokio::test]
    async fn a_start_naming_no_spawn_binds_the_one_fork_in_flight() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        let error = session
            .on_child_start(child("c0"), SpawnRef::InFlight)
            .err()
            .expect("no spawn in flight opens no child");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        assert!(!opened(&runtime, &child("c0")), "the refused start opens no trajectory");

        let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the one spawn in flight binds");
        assert!(opened(&runtime, &child("c1")));
        session
            .on_child_start(child("c1"), SpawnRef::Binding(binding))
            .expect("the echoed binding names the fork the in-flight start bound");
        assert_eq!(fork_opened_count(&runtime), 1);

        let error = session
            .on_child_start(child("c2"), SpawnRef::InFlight)
            .err()
            .expect("a bound fork is not in flight");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
    }

    #[tokio::test]
    async fn two_forks_in_flight_bind_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        let mut first = session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
            )
            .await
            .expect("the spawn dispatch closes");

        release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
        release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
        let error = session
            .on_child_start(child("c2"), SpawnRef::InFlight)
            .err()
            .expect("two forks in flight: none is picked");
        assert!(matches!(error, EventError::SpawnAmbiguous), "got {error:?}");
        assert!(!error.is_operational());
        assert!(!opened(&runtime, &child("c2")), "no child opened");
        assert_eq!(fork_opened_count(&runtime), 1);
    }

    #[tokio::test]
    async fn a_fork_whose_parent_ended_is_unbindable_and_not_in_flight() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        let mut first = session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");
        let orphaned = release_spawn(&mut first, fetch(serde_json::json!({"a": 2}))).await;
        first
            .on_spawn_result(
                fetch(serde_json::json!({"a": 2})),
                ToolOutcome::Indeterminate,
                None,
                None,
            )
            .await
            .expect("the nested spawn dispatch closes, its fork still prepared");
        assert!(!dispatch_open(&runtime, &child("c1")));
        first
            .on_child_end(Some("done early".to_string()))
            .await
            .expect("the child's end crosses");

        let error = session
            .on_child_start(child("orphan"), SpawnRef::Binding(orphaned))
            .err()
            .expect("an ended parent's fork binds nothing");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        assert!(!error.is_operational());
        assert!(!opened(&runtime, &child("orphan")));

        session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("c1")),
                Some("done early".to_string()),
            )
            .await
            .expect("the ended child's message replays and the spawn closes");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
        session
            .on_child_start(child("c2"), SpawnRef::InFlight)
            .expect("the one live fork in flight binds");
        assert!(opened(&runtime, &child("c2")));
    }

    fn agent_response() -> ToolOutcome {
        ToolOutcome::Success {
            body: OutcomeBody::Available(
                r#"{"agentId":"c1","content":[{"type":"text","text":"all done"}]}"#.to_string(),
            ),
        }
    }

    #[tokio::test]
    async fn a_spawn_result_naming_the_bound_child_crosses_its_return_and_closes_the_spawn() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");

        let decision = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("c1")),
                Some("all done".to_string()),
            )
            .await
            .expect("the return crosses");
        assert_eq!(
            decision,
            SpawnResultDecision::Return(ChildReturnDecision::Returned {
                value: "all done".to_string()
            }),
        );
        assert!(
            matches!(runtime.live(&root(), &child("c1")), Err(SessionError::Ended)),
            "the child ended at its return",
        );
        assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the parent proposes again");
    }

    #[tokio::test]
    async fn an_indeterminate_spawn_result_closes_the_spawn_and_leaves_the_child_live() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");

        let decision = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Indeterminate,
                None,
                None,
            )
            .await
            .expect("an indeterminate result is an ordinary close");
        assert_eq!(decision, SpawnResultDecision::Outcome(ToolResultDecision::Keep));
        assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
        assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");
    }

    #[tokio::test]
    async fn a_spawn_result_for_an_ended_child_replays_the_same_message_and_refuses_another() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        let mut first = session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");
        first
            .on_child_end(Some("all done".to_string()))
            .await
            .expect("the child's own end crosses");
        assert!(dispatch_open(&runtime, &root()), "the spawn dispatch is still open");
        let before = runtime.log_facts(&root()).len();

        let decision = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("c1")),
                Some("all done".to_string()),
            )
            .await
            .expect("the same message replays");
        assert_eq!(
            decision,
            SpawnResultDecision::Return(ChildReturnDecision::Returned {
                value: "all done".to_string()
            }),
        );
        assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
        let facts = runtime.log_facts(&root());
        assert!(
            facts[before..]
                .iter()
                .all(|fact| !matches!(fact, appa_engine::fact::Fact::ChildReturn { .. })),
            "the replayed return crossed nothing twice",
        );

        release_spawn(&mut session, fetch(serde_json::json!({"a": 3}))).await;
        let mut second = session
            .on_child_start(child("c2"), SpawnRef::InFlight)
            .expect("a second child opens");
        second
            .on_child_end(Some("the first message".to_string()))
            .await
            .expect("the child's own end crosses");
        let error = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 3})),
                agent_response(),
                Some(child("c2")),
                Some("a second message".to_string()),
            )
            .await
            .expect_err("a second return for an ended child crosses nothing");
        assert!(matches!(error, EventError::TrajectoryEnded), "got {error:?}");
        assert!(!error.is_operational());
        assert!(
            !dispatch_open(&runtime, &root()),
            "the spawn dispatch closed as a failure"
        );
    }

    #[tokio::test]
    async fn a_spawn_result_naming_another_child_crosses_nothing_and_closes_the_spawn() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");

        let error = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("intruder")),
                Some("all done".to_string()),
            )
            .await
            .expect_err("another child's message does not cross");
        assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
        assert!(!error.is_operational());
        assert!(
            !dispatch_open(&runtime, &root()),
            "the spawn dispatch closed as a failure"
        );
        assert!(
            runtime.live(&root(), &child("c1")).is_ok(),
            "the bound child stays live"
        );
        assert!(
            !opened(&runtime, &child("intruder")),
            "the named child was never opened"
        );

        release_spawn(&mut session, fetch(serde_json::json!({"a": 2}))).await;
        session
            .on_child_start(child("c2"), SpawnRef::InFlight)
            .expect("a second child opens");
        let error = session
            .on_spawn_result(fetch(serde_json::json!({"a": 2})), agent_response(), None, None)
            .await
            .expect_err("a result naming no child crosses nothing");
        assert!(matches!(error, EventError::BindingMismatch), "got {error:?}");
        assert!(!dispatch_open(&runtime, &root()));
    }

    #[tokio::test]
    async fn a_spawn_result_before_any_child_bound_closes_the_spawn_unbindable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let binding = release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;

        let error = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("c1")),
                None,
            )
            .await
            .expect_err("no child bound: nothing crosses");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        assert!(!error.is_operational());
        assert!(
            !dispatch_open(&runtime, &root()),
            "the spawn dispatch closed as a failure"
        );

        let error = session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .err()
            .expect("nothing is in flight");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        let error = session
            .on_child_start(child("c1"), SpawnRef::Binding(binding))
            .err()
            .expect("the failed spawn's fork is unbindable");
        assert!(matches!(error, EventError::SpawnNotTaken), "got {error:?}");
        assert!(!opened(&runtime, &child("c1")), "no child opened");
    }

    #[tokio::test]
    async fn a_spawn_result_on_an_unforked_call_is_an_ordinary_outcome() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the ordinary call releases");
        let decision = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
                None,
                None,
            )
            .await
            .expect("the ordinary result is admitted");
        assert_eq!(decision, SpawnResultDecision::Outcome(ToolResultDecision::Keep));
        assert!(!dispatch_open(&runtime, &root()));

        let error = session
            .on_spawn_result(fetch(serde_json::json!({"a": 1})), agent_response(), None, None)
            .await
            .expect_err("no open dispatch takes the result");
        assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");
    }

    #[tokio::test]
    async fn a_withheld_return_closes_the_spawn_as_a_failure() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!(42)).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({}))).await;
        session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");

        let decision = session
            .on_spawn_result(
                fetch(serde_json::json!({})),
                agent_response(),
                Some(child("c1")),
                Some("raw with pii".to_string()),
            )
            .await
            .expect("the withheld return is delivered");
        assert!(
            matches!(
                decision,
                SpawnResultDecision::Return(ChildReturnDecision::Blocked { .. })
            ),
            "got {decision:?}",
        );
        assert!(
            !dispatch_open(&runtime, &root()),
            "the spawn dispatch closed as a failure"
        );
        session
            .on_tool_call(fetch(serde_json::json!({})), false)
            .await
            .expect("the parent proposes again");
    }

    #[tokio::test]
    async fn a_child_with_a_running_call_does_not_end_until_its_outcome_is_reported() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
        child_session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the child's call releases");
        let before = runtime.log_basis(&root());

        for value in [Some("done".to_string()), None] {
            let error = child_session
                .on_child_end(value)
                .await
                .expect_err("a child with a call open does not end");
            assert!(matches!(error, EventError::ChildDispatchOpen), "got {error:?}");
            assert!(!error.is_operational());
        }
        assert_eq!(runtime.log_basis(&root()), before, "the refusal appended nothing");
        assert!(dispatch_open(&runtime, &child("c1")), "the child's dispatch stays open");
        assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");

        child_session
            .on_tool_result(
                fetch(serde_json::json!({"a": 2})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("fetched".to_string()),
                },
            )
            .await
            .expect("the outcome closes the dispatch");
        child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect("with nothing in flight the same end crosses");
        assert!(matches!(runtime.live(&root(), &child("c1")), Err(SessionError::Ended)));
    }

    #[tokio::test]
    async fn a_child_with_an_untaken_substituted_release_does_not_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            substituting_config(SUBSTITUTED_SEND_FORKING, None),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
        let hop = narrowed_and_blocked_on(&runtime, &mut child_session, &child("c1")).await;
        assert!(matches!(
            child_session.on_remedy(hop, None).await,
            Ok(RemedyDecision::Substituted { .. }),
        ));
        assert!(
            runtime.substituted_release(&root(), &child("c1")).is_some(),
            "the child has a substituted release standing"
        );

        let error = child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect_err("a child with a substituted release standing does not end");
        assert!(matches!(error, EventError::ChildDispatchOpen), "got {error:?}");
        assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");

        assert_eq!(
            child_session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the substituted call is handed over"),
            ToolCallDecision::Allow { spawn: None },
        );
        child_session
            .on_tool_result(
                send(REDACTED_BODY),
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
            )
            .await
            .expect("the substituted call's outcome closes its dispatch");
        assert!(
            child_session.on_child_end(Some("done".to_string())).await.is_ok(),
            "with the release settled the child ends"
        );
    }

    #[tokio::test]
    async fn a_spawn_result_for_a_child_with_a_call_open_crosses_nothing_and_closes_the_spawn() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        release_spawn(&mut session, fetch(serde_json::json!({"a": 1}))).await;
        let mut first = session
            .on_child_start(child("c1"), SpawnRef::InFlight)
            .expect("the child opens");
        first
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the child's call releases");

        let error = session
            .on_spawn_result(
                fetch(serde_json::json!({"a": 1})),
                agent_response(),
                Some(child("c1")),
                Some("all done".to_string()),
            )
            .await
            .expect_err("the return does not cross");
        assert!(matches!(error, EventError::ChildDispatchOpen), "got {error:?}");
        assert!(!error.is_operational());
        assert!(
            runtime
                .log_facts(&root())
                .iter()
                .all(|fact| !matches!(fact, appa_engine::fact::Fact::ChildReturn { .. })),
            "nothing crossed",
        );
        assert!(
            runtime.log_facts(&root()).iter().any(|fact| matches!(
                fact,
                appa_engine::fact::Fact::DispatchClosed {
                    outcome: appa_engine::fact::CloseOutcome::Failure,
                    ..
                }
            )),
            "the spawn dispatch closed as a failure",
        );
        assert!(!dispatch_open(&runtime, &root()), "the spawn dispatch closed");
        assert!(dispatch_open(&runtime, &child("c1")), "the child's dispatch stays open");
        assert!(runtime.live(&root(), &child("c1")).is_ok(), "the child stays live");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 3})), false)
            .await
            .expect("the parent proposes again");
    }
}
