//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::elicit::Elicitation;

use crate::consult::{
    AnnotationAnswer, AnnotationArtifact, AudienceSourceArtifact, AudienceSourceDeclaration, Consult, ConsultBody,
    LookupAnswer, MembersAnswer, PrincipalAnswer, SanitizerAnswer,
};
use crate::engine::{
    AuthorityVerdict, EngineDecision, EngineEvent, EngineView, ExternalEvidence, ExternalRequest, Feedback, ForkStatus,
    Liveness, Next, OfferNonce, OpenDispatch, Presentation, engine_id,
};
use crate::external::ConsultOutcome;

use super::{
    ChildReturnDecision, Deployment, EventError, ExactCall, Inner, OfferId, OutcomeBody, ProposedCall, RemedyDecision,
    SpawnRef, SpawnResultDecision, ToolCallDecision, ToolOutcome, ToolResultDecision, TrajectoryId,
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

/// The most external-resolution rounds one invocation runs before refusing operationally.
/// Gathering is designed to close at least one ask per round, so this cap never fires on a
/// healthy deployment; it bounds the blast radius of a gathering bug or a hostile external.
const RESOLUTION_ROUNDS: u32 = 8;

fn fresh_entropy() -> OfferNonce {
    OfferNonce(rand::random::<[u8; 32]>())
}

/// One per trajectory (root or child). The adapter drives it; it never
/// renders, and the adapter never stores.
///
/// The dispatcher builds one per hook event and drops it when the event
/// ends; nothing caches a session across events. That is what makes the
/// deployment snapshot below an event-scoped read rather than a
/// trajectory-scoped one, and a dispatcher that started caching sessions
/// would have to take the snapshot per event instead.
pub struct Session {
    deployment: Arc<Deployment>,
    inner: Arc<Inner>,
    trajectory: TrajectoryId,
    root: TrajectoryId,
}

impl Session {
    pub(super) fn attach(
        inner: Arc<Inner>,
        deployment: Arc<Deployment>,
        trajectory: TrajectoryId,
        root: TrajectoryId,
    ) -> Session {
        Session {
            deployment,
            inner,
            trajectory,
            root,
        }
    }

    #[cfg(test)]
    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    /// The actor's turn is over. A call still open here got no outcome
    /// hook and will never get one: Claude Code reports none for a call
    /// refused at its permission prompt, and none for a turn the user
    /// interrupted. Left open it refuses every later proposal as a
    /// second call in flight, for the life of the trajectory. Two
    /// hooks reach here: the turn end, and the first tool call after a
    /// prompt that no turn end preceded.
    ///
    /// The close is `Indeterminate`, not a failure. What the runtime
    /// observed is the absence of a report, which does not say whether
    /// the call ran: an outcome hook that never reached the runtime
    /// looks the same as a call the harness refused. Indeterminate is
    /// the outcome for exactly that — the dispatch closes and the
    /// effect reservation stands, so a real emission is never dropped
    /// from the ledger on the strength of a missing hook.
    ///
    /// Not an engine event when nothing is carried, which is every
    /// ordinary turn: the view is read, no fact is appended.
    pub async fn on_turn_end(&self) -> Result<(), EventError> {
        let Some(open) = self.carried_call()? else {
            tracing::debug!(trajectory = %self.trajectory.0, "no call outstanding");
            return Ok(());
        };
        self.abandon_open(&ToolOutcome::Indeterminate).await?;
        tracing::debug!(
            trajectory = %self.trajectory.0,
            dispatch = ?open.id,
            tool = %open.tool,
            "call closed as unreported",
        );
        Ok(())
    }

    /// The call a turn end closes. A trajectory that has ended or never
    /// opened carries nothing, so a turn end that names one is a no-op
    /// rather than a refusal — a turn ends for reasons the engine does
    /// not model.
    ///
    /// A substituted release is never carried. It stands across turns by
    /// construction: no proposal released it, so no outcome hook is owed
    /// for it, and it ends only when the harness runs it or proposes
    /// past it (`claim_or_abandon`). Closing it here would discard the
    /// remedy that minted it.
    fn carried_call(&self) -> Result<Option<OpenDispatch>, EventError> {
        let log = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&self.deployment, &log)?;
        let view = policy.engine().rebuild_view(&log).map_err(EventError::from)?;
        match policy.engine().liveness(&view, &self.trajectory) {
            Liveness::Ended | Liveness::Unopened => Ok(None),
            Liveness::Live if policy.engine().substituted_release(&view, &self.trajectory).is_some() => Ok(None),
            Liveness::Live => Ok(policy.engine().open_dispatches(&view, &self.trajectory).pop()),
        }
    }

    pub async fn on_tool_call(&self, call: ProposedCall, spawn: bool) -> Result<ToolCallDecision, EventError> {
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
            Next::ModelResponse { invocations, feedback } => match (invocations.as_slice(), feedback.as_slice()) {
                ([released], []) => {
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
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
            },
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    fn substituted_release(&self, call: &ProposedCall) -> Result<Option<Standing>, EventError> {
        let log = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&self.deployment, &log)?;
        let view = policy.engine().rebuild_view(&log).map_err(EventError::from)?;
        match policy.engine().liveness(&view, &self.trajectory) {
            Liveness::Ended => return Err(EventError::TrajectoryEnded),
            Liveness::Unopened => return Err(EventError::SpawnNotTaken),
            Liveness::Live => {}
        }
        let Some(open) = policy.engine().substituted_release(&view, &self.trajectory) else {
            return Ok(None);
        };
        let canonical = || policy.engine().canonical_bytes(call);
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
        self.abandon_open(&outcome).await?;
        tracing::debug!(
            trajectory = %self.trajectory.0,
            dispatch = ?open.id,
            tool = %open.tool,
            proposed = %call.tool,
            "substituted call abandoned"
        );
        Err(EventError::SubstitutionAbandoned { tool: open.tool })
    }

    /// Close the call this trajectory has open as one that did not run.
    /// The dispatch is re-read from the view on every replay, so a
    /// contended append never closes an occurrence the winning writer
    /// already closed. Both callers reach here with one dispatch open:
    /// a substituted release the harness declined to run, and a
    /// released call whose turn ended without an outcome.
    async fn abandon_open(&self, outcome: &ToolOutcome) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                let open = context.open_dispatches();
                let [open] = open.as_slice() else {
                    return Err(EventError::UnknownDispatch);
                };
                Ok(EngineEvent::ToolOutcome {
                    dispatch: open.id.clone(),
                    outcome: outcome.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
        )
        .await
    }

    pub async fn on_tool_result(&self, call: ProposedCall, o: ToolOutcome) -> Result<ToolResultDecision, EventError> {
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
        &self,
        call: ProposedCall,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> Result<SpawnResultDecision, EventError> {
        let outcome = self.cap_outcome(outcome);
        // The attempt that commits is the one whose plan the delivery below
        // follows, so each attempt overwrites what the last one wrote.
        let mut plan: Option<SpawnPlan> = None;
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
                    plan = Some(next);
                    Ok(event)
                },
                None,
            )
            .await;
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

    /// The model called the `execute_remedy_plan` MCP tool. Executes one
    /// offer by its canonical id, which the runtime
    /// resolved from the quoted form before this point. An offer this
    /// trajectory does not pursue is refused.
    pub async fn on_remedy(
        &self,
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
    pub fn on_child_start(&self, id: TrajectoryId, spawn: SpawnRef) -> Result<Session, EventError> {
        self.bind_child(id, spawn).map(|(session, _)| session)
    }

    /// Open a child whose start hook never arrived, or has not arrived yet:
    /// bind it to the family's one spawn in flight, as its start
    /// would. Whether this opened the child or found it already open tells the
    /// dispatcher whether the refused event was the missing start's, and is
    /// worth running once more, or the child's own answer.
    pub(crate) fn open_late(&self, child: TrajectoryId) -> Result<LateOpen, EventError> {
        self.bind_child(child, SpawnRef::InFlight).map(|(_, opened)| opened)
    }

    fn bind_child(&self, id: TrajectoryId, spawn: SpawnRef) -> Result<(Session, LateOpen), EventError> {
        let child = id.clone();
        let opened = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(&self.deployment, &opened)?;
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
            // The child rides the parent event's snapshot, not a fresh one.
            Next::Done => Ok((
                Session::attach(
                    Arc::clone(&self.inner),
                    Arc::clone(&self.deployment),
                    id,
                    self.root.clone(),
                ),
                opened,
            )),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The child finished. Its final message is its only return
    /// channel and is checked before it may cross to the parent;
    /// `None` returns no value. The return names the
    /// fork that opened the child, recovered from the log. A child
    /// with a call still open does not end: the end is refused, and the same
    /// end crosses once the call's outcome is reported (`ChildDispatchOpen`).
    pub async fn on_child_end(&self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
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

        return_decision(decision)
    }

    fn cap_outcome(&self, outcome: ToolOutcome) -> ToolOutcome {
        match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } if body.len() > self.deployment.config.externals.max_body_bytes => ToolOutcome::Success {
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
        let policy = self.inner.resolve_policy(&self.deployment, &opened)?;
        let mut opened = Some(opened);
        // External answers carry the exact call or group they answered for, and the
        // engine matches them only while that is still the one in front of it — a
        // rewritten call is annotated afresh by construction, so this loop carries
        // evidence blindly. Every round either decides or gathers an answer the engine
        // did not hold. Rounds are finite, so a round that changes nothing is the only
        // way this loop fails to converge.
        let mut evidence: Vec<ExternalEvidence> = Vec::new();
        for _ in 0..RESOLUTION_ROUNDS {
            let carried = evidence.clone();
            let entering = carried.is_empty();
            let decision = self.drive(&policy, opened.take(), entering, |context| {
                event(context, carried.clone())
            })?;
            match decision.then {
                // The engine batches every missing answer into one request set, and evidence
                // is matched by name, never by position. Without a reviewer the
                // consults run concurrently — an annotation consult can take a model call's
                // seconds, and the batch should cost its slowest member, not their sum. With a
                // reviewer they stay serial: one staged review on screen at a time.
                Next::ResolveExternal(requests) => match elicitation {
                    None => {
                        // Batch-terminal: join_all settles every sibling first; any
                        // no-answer then aborts the invocation, discarding the
                        // siblings' answers, before another engine round or any append.
                        let consults = requests.into_iter().map(|request| self.consult(request, None));
                        for answered in futures_util::future::join_all(consults).await {
                            evidence.push(answered?);
                        }
                    }
                    Some(_) => {
                        for request in requests {
                            let answered = self.consult(request, elicitation).await?;
                            evidence.push(answered);
                        }
                    }
                },
                _ => return Ok(decision),
            }
            if evidence.len() == carried.len() {
                return Err(EventError::UnexpectedDecision);
            }
        }
        // Every round is supposed to close at least one ask for good; a run this long is a
        // gathering bug or a hostile external, and the invocation refuses operationally
        // rather than hold the turn lease against a source forever.
        Err(EventError::ResolutionDiverged {
            rounds: RESOLUTION_ROUNDS,
        })
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
            let view = policy.engine().rebuild_view(&log).map_err(EventError::from)?;
            let context = Decided {
                session: self,
                policy,
                view: &view,
            };
            if entering {
                match policy.engine().liveness(&view, &self.trajectory) {
                    Liveness::Ended => return Err(EventError::TrajectoryEnded),
                    Liveness::Unopened => return Err(EventError::SpawnNotTaken),
                    Liveness::Live => {}
                }
            }
            let event = event(&context)?;
            if let EngineEvent::ChildReturn { child, .. } = &event
                && !policy.engine().open_dispatches(&view, child).is_empty()
            {
                return Err(EventError::ChildDispatchOpen);
            }
            let decision = policy
                .engine()
                .handle(&view, &self.trajectory, event)
                .map_err(EventError::from)?;

            let Some(facts) = decision.append.as_ref() else {
                return Ok(decision);
            };
            if policy.engine().opens_a_second_dispatch(&view, &self.trajectory, facts) {
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

    /// One external consult. Only an annotation failure is an error: the call cannot be
    /// judged without its annotation, no fact may be appended, and the refusal is operational
    /// — never a policy denial. Every other external keeps its no-answer evidence shape.
    async fn consult(
        &self,
        request: ExternalRequest,
        elicitation: Option<&Elicitation>,
    ) -> Result<ExternalEvidence, EventError> {
        Ok(match &request {
            ExternalRequest::Authority {
                authority,
                declaration,
                artifact,
                review,
            } => {
                let consult = Consult {
                    name: authority.clone(),
                    body: ConsultBody::Authority {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let verdict = match self.deployment.externals.consult(&consult, elicitation).await {
                    ConsultOutcome::Answer(answer) => AuthorityVerdict::from_wire(&answer),
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
                declaration,
                artifact,
            } => {
                let consult = Consult {
                    name: sanitizer.clone(),
                    body: ConsultBody::Sanitizer {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let derived = match self.deployment.externals.consult(&consult, None).await {
                    ConsultOutcome::Answer(answer) => SanitizerAnswer::from_wire(&answer).map(|answer| answer.body),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Sanitizer {
                    sanitizer: sanitizer.clone(),
                    source: *source,
                    derived,
                }
            }
            ExternalRequest::Annotation {
                annotator,
                call,
                declaration,
                args,
            } => {
                let consult = Consult {
                    name: annotator.clone(),
                    body: ConsultBody::Annotation {
                        declaration: declaration.clone(),
                        artifact: AnnotationArtifact { args: args.clone() },
                    },
                };
                let answer = match self.deployment.externals.consult(&consult, None).await {
                    ConsultOutcome::Answer(answer) => {
                        AnnotationAnswer::from_wire(&answer, declaration).ok_or_else(|| {
                            crate::external::NoAnswerReason::MalformedAnswer(
                                "detail=invalid_fields_or_value_types".to_string(),
                            )
                        })
                    }
                    ConsultOutcome::NoAnswer(reason) => Err(reason),
                };
                // Annotation failure is an operational refusal, never model feedback: the
                // call is not judged, nothing is appended, and the harness fails closed.
                let answer = match answer {
                    Ok(answer) => answer,
                    Err(reason) => {
                        match reason {
                            crate::external::NoAnswerReason::Unreachable | crate::external::NoAnswerReason::Timeout => {
                                tracing::warn!(annotator, ?reason, "an annotation consult produced no answer")
                            }
                            _ => tracing::debug!(annotator, ?reason, "an annotation consult produced no answer"),
                        }
                        return Err(EventError::annotation_refused(annotator.clone(), reason.diagnostic()));
                    }
                };
                ExternalEvidence::Annotation {
                    annotator: annotator.clone(),
                    // The evidence names the exact call it answered for: a rewritten call
                    // never consumes a stale annotation.
                    call: *call,
                    answer,
                }
            }
            ExternalRequest::AudienceSource {
                provider,
                selector,
                templates,
            } => {
                let consult = Consult {
                    name: provider.clone(),
                    body: ConsultBody::AudienceSource {
                        declaration: AudienceSourceDeclaration {
                            templates: templates.clone(),
                        },
                        artifact: AudienceSourceArtifact::Selector {
                            selector: selector.clone(),
                        },
                    },
                };
                let members = match self.deployment.externals.consult(&consult, None).await {
                    ConsultOutcome::Answer(answer) => MembersAnswer::from_wire(&answer).map(|answer| answer.members),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::AudienceSource {
                    provider: provider.clone(),
                    selector: selector.clone(),
                    members,
                }
            }
            ExternalRequest::MemberLookup {
                provider,
                member,
                templates,
            } => {
                let consult = Consult {
                    name: provider.clone(),
                    body: ConsultBody::AudienceSource {
                        declaration: AudienceSourceDeclaration {
                            templates: templates.clone(),
                        },
                        artifact: AudienceSourceArtifact::Member { member: member.clone() },
                    },
                };
                let claims = match self.deployment.externals.consult(&consult, None).await {
                    ConsultOutcome::Answer(answer) => LookupAnswer::from_wire(&answer).map(|answer| answer.claims),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::MemberLookup {
                    provider: provider.clone(),
                    member: member.clone(),
                    claims,
                }
            }
            ExternalRequest::Identity { implementation, claims } => {
                let consult = Consult {
                    name: implementation.clone(),
                    body: ConsultBody::Identity {
                        artifact: claims.clone(),
                    },
                };
                let principal = match self.deployment.externals.consult(&consult, None).await {
                    ConsultOutcome::Answer(answer) => PrincipalAnswer::from_wire(&answer)
                        .map(|answer| appa_engine::label::ReaderId::new(answer.principal)),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Identity {
                    implementation: implementation.clone(),
                    id: claims.id.clone(),
                    principal,
                }
            }
        })
    }
}

/// What one attempt of an event may read before it decides: the log as this
/// attempt rebuilt it. A branch's parent, the dispatch it has open, the
/// trajectory an offer belongs to — all are answered from here, so a
/// replay after a lost race reads the state that actually won rather
/// than the state it first saw.
pub(crate) struct Decided<'a> {
    session: &'a Session,
    policy: &'a crate::engine::PolicyEngine<'a>,
    view: &'a EngineView,
}

impl Decided<'_> {
    fn engine(&self) -> &crate::engine::RuntimeEngine {
        self.policy.engine()
    }

    fn open_dispatches(&self) -> Vec<OpenDispatch> {
        self.engine().open_dispatches(self.view, &self.session.trajectory)
    }

    fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        self.engine().canonical_bytes(call)
    }

    fn parent_of(&self, child: &TrajectoryId) -> Option<TrajectoryId> {
        self.engine().parent_of(self.view, child)
    }

    fn offer_pursuer(&self, offer: &OfferId) -> Option<TrajectoryId> {
        self.engine().offer_pursuer(self.view, offer)
    }

    fn fork_status(&self, fork: &appa_engine::value::ForkId) -> ForkStatus {
        self.engine().fork_status(self.view, fork)
    }

    fn in_flight_fork(&self, child: &TrajectoryId) -> Result<appa_engine::value::ForkId, EventError> {
        if let Some(fork) = self.engine().fork_of(self.view, child) {
            return Ok(fork);
        }
        match self.engine().forks_in_flight(self.view).as_slice() {
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
mod real_engine_tests {
    use super::super::{OpenError, OutcomeBody, Runtime};
    use super::*;
    use crate::api::{RemedyDecision, SpawnBinding, ToolCallDecision, ToolOutcome, ToolResultDecision};
    use crate::config::Config;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One fixture configuration, from its whole TOML text. The file has
    /// to exist on disk because `Config::load` reads the policy file's
    /// bytes, which the opening record keys the deployment by.
    fn config_from(text: &str) -> Config {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the fixture validates")
    }

    fn config_with(policy: &str, authority_url: Option<&str>) -> Config {
        let binding = match authority_url {
            Some(url) => format!("[externals.authorities.approver]\nurl = \"{url}\"\n"),
            None => String::new(),
        };
        let text = format!("[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n{binding}");
        config_from(&text)
    }

    const FETCH_AND_SEND: &str = r#"
version = 2

# A neutral fetch: its result folds at the trajectory's own label, so the
# lifecycle tests release it freely. `taint` brings outside content in at the
# low rank; releasing it takes the narrowing acceptance its block offers.
[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }
delta = {}

[[policy.tool]]
name = "taint"
parameters = { type = "object", properties = { a = { type = "integer" } } }
delta = { trust = "suspicious" }

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

    fn taint(spelling: serde_json::Value) -> ProposedCall {
        ProposedCall {
            tool: "taint".to_string(),
            arguments: raw(spelling),
        }
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
        let config = config_with("version = 2\nstray_key = true\n", None);
        assert!(matches!(
            Runtime::open(config, dir.path().join("appa.db"), None),
            Err(OpenError::Policy(_)),
        ));
    }

    #[test]
    fn an_inline_impl_binding_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 2
[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
[policy.authority.implementation]
builtin = "approve"
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::Policy(error))
                if matches!(*error, appa_policy::ConfigError::ForbiddenInlineBinding { .. }),
        ));
    }

    #[test]
    fn a_policy_naming_an_unbound_authority_opens() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 2
[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
"#;
        assert!(Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None).is_ok());
    }

    #[test]
    fn a_non_neutral_starting_label_seeds_the_root_and_survives_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let policy = r#"
version = 2
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
            Err(EventError::UnknownTrajectory),
        ));
    }

    #[test]
    fn a_reserved_tool_name_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 2
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let quoted = runtime
            .minted_offers(&root(), &root())
            .last()
            .expect("the block surfaced the sanitize plan")
            .clone();
        let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
    async fn a_tool_nothing_covers_is_refused_typed_before_it_runs() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        let refused = session
            .on_tool_call(
                ProposedCall {
                    tool: "wrench".to_string(),
                    arguments: raw(serde_json::json!({})),
                },
                false,
            )
            .await;
        assert!(matches!(
            refused,
            Err(EventError::UndeclaredTool { tool }) if tool == "wrench"
        ));
        assert!(only_the_opening(&runtime), "the refusal appends nothing");
    }

    fn latest_offer(runtime: &Runtime) -> OfferId {
        let quoted = runtime
            .minted_offers(&root(), &root())
            .into_iter()
            .next_back()
            .expect("the deny surfaced an offer");
        runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0
    }

    const READ_ONLY: &str = r#"
version = 2

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
            let session = runtime.create_session(root()).expect("a fresh id opens");
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

        let old = runtime.session(&root(), &root()).expect("the old root reopens");
        let decision = old
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the old root decides");
        assert_eq!(
            decision,
            ToolCallDecision::Allow { spawn: None },
            "the old root keeps fetch"
        );

        let new = runtime
            .create_session(TrajectoryId("cc:new".to_string()))
            .expect("a fresh id opens");
        let refused = new.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await;
        assert!(
            matches!(refused, Err(EventError::UndeclaredTool { tool }) if tool == "fetch"),
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
            let session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered");
        }

        {
            let runtime =
                Runtime::open(config_with(READ_ONLY, None), db.clone(), None).expect("the edited deployment opens");
            let session = runtime.session(&root(), &root()).expect("the old root reopens");
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
            assert_eq!(log_before.len(), log_after.len(), "an abstention appends no fact",);
            assert!(matches!(
                session.on_remedy(offer, None).await.expect("the offer is still live"),
                RemedyDecision::NoAnswer { .. },
            ));
        }

        // The binding comes back with the declaration that registers it: a binding no
        // policy declares is refused at open.
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None)
            .expect("the deployment with the restored binding opens");
        let session = runtime.session(&root(), &root()).expect("the old root reopens");
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
            let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
    async fn tampered_audience_evidence_refuses_the_log() {
        let source = {
            use axum::routing::post;
            let app = axum::Router::new().route(
                "/",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "version": 1,
                        "answer": {"members": [{"id": "slack:U1", "verified_email": "alice@corp.example"}]}
                    }))
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
        };
        let policy = r#"
version = 2

[[policy.audience.group]]
name = "team"
from = ["slack:user-group/team"]

[[policy.tool]]
name = "send"
requires = { audience = { contains = ["@team"] } }
delta = {}

[policy.deployment]
starting_label = { audience = ["alice@corp.example"] }
"#;
        let text = format!(
            "[policy]\n{policy}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.audience.slack]\nurl = \"{source}\"\n"
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        let config = Config::load(&path).expect("the fixture validates");
        let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        let send = ProposedCall {
            tool: "send".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        // The source reports Alice, whose principal the starting audience holds: released.
        assert!(matches!(
            session.on_tool_call(send.clone(), false).await,
            Ok(ToolCallDecision::Allow { .. })
        ));
        let released: Vec<_> = runtime
            .log_facts(&root())
            .into_iter()
            .skip_while(|fact| matches!(fact, appa_engine::fact::Fact::TrajectoryOpened { .. }))
            .collect();
        let persisted = serde_json::to_string(&released).expect("the batch serializes");
        assert!(
            persisted.contains("alice@corp.example"),
            "the decision pins the claims it read: {persisted}"
        );
        let tampered = persisted.replace("alice@corp.example", "mallory@evil.example");
        assert_ne!(tampered, persisted);
        runtime
            .store()
            .corrupt_batch(&crate::engine::engine_id(&root()), 1, tampered.as_bytes());
        assert!(matches!(
            session.on_tool_call(send, false).await,
            Err(EventError::UntrustedLog(_)),
        ));
    }

    #[tokio::test]
    async fn a_suspicious_result_blocks_a_trusted_floor_sink() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        admit_success(&runtime, &mut session, taint(serde_json::json!({"a": 1}))).await;

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
        assert!(
            matches!(decision, ToolCallDecision::Deny { .. }),
            "a narrowed trajectory must block the trusted-floor sink, got {decision:?}"
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
        let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child.clone()).await;
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
version = 2

[[policy.tool]]
name = "spawn"
delta = {}

[policy.deployment]
context_control = false
"#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(UNCONTROLLED, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let child = open_child(
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
            Err(EventError::TrajectoryEnded),
        ));
    }

    #[tokio::test]
    async fn a_child_returns_crossing_narrows_the_parent_and_charges_its_sink() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child = open_child(
            &mut session,
            fetch(serde_json::json!({"a": 9})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        let mut child = child;
        admit_success(&runtime, &mut child, taint(serde_json::json!({"a": 1}))).await;

        // The raw return narrows the parent, so the crossing is staged behind the
        // parent's own acceptance instead of merging silently.
        let staged = child
            .on_child_end(Some("summary of untrusted data".to_string()))
            .await
            .expect("the staged return is delivered");
        assert!(
            matches!(staged, crate::api::ChildReturnDecision::Blocked { .. }),
            "a narrowing return is staged, not crossed: {staged:?}"
        );
        let quoted = runtime
            .minted_offers(&root(), &root())
            .first()
            .expect("the stage surfaced the parent's acceptance")
            .clone();
        let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
        assert_eq!(
            session.on_remedy(offer, None).await.expect("the acceptance executes"),
            RemedyDecision::Returned {
                value: "summary of untrusted data".to_string()
            },
            "accepting the narrowing crosses the staged return"
        );
        assert!(matches!(
            runtime.live(&root(), &TrajectoryId("cc:child".to_string())),
            Err(EventError::TrajectoryEnded),
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

        assert_eq!(
            runtime.status(&root()).expect("the root answers").trust,
            "suspicious",
            "the merged crossing narrowed the parent"
        );
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
        assert!(
            matches!(decision, ToolCallDecision::Deny { .. }),
            "the crossed narrowing must charge the parent's send, got {decision:?}"
        );
    }

    const ATTENTION: &str = r#"
version = 2

[[policy.tool]]
name = "wire"
parameters = { type = "object", properties = { amount = { type = "integer" } } }
requires = { attention = ["irreversible"] }
delta = {}

[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
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
        let quoted = runtime
            .minted_offers(root, trajectory)
            .into_iter()
            .next()
            .expect("the deny surfaced an offer");
        runtime.resolve_in(root, &quoted).expect("the quoted id resolves").0
    }

    #[tokio::test]
    async fn an_authority_approval_authorizes_the_exact_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");

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
        let session = runtime.create_session(root()).expect("a fresh id opens");

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
        let first = runtime.create_session(first_id.clone()).expect("the first root opens");
        let second = runtime
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
        let still_quoted = runtime.minted_offers(&second_id, &second_id);
        assert_eq!(
            still_quoted.len(),
            1,
            "the second trajectory keeps exactly its own offer"
        );
        assert_eq!(
            runtime
                .resolve_in(&second_id, &still_quoted[0])
                .expect("the quoted id resolves")
                .0,
            second_offer,
            "one trajectory's denial must not retire another trajectory's same-call offer"
        );
    }

    #[tokio::test]
    async fn an_abstain_keeps_the_offer() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"note": "still thinking"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");

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
        let session = runtime.create_session(root()).expect("a fresh id opens");

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
            let session = runtime.create_session(root()).expect("a fresh id opens");
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered");
            surfaced_offer(&runtime)
        };
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None).expect("the deployment reopens");
        let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
        assert!(matches!(
            session
                .on_remedy(offer, None)
                .await
                .expect("the reopened offer executes"),
            RemedyDecision::Authorized { .. },
        ));
    }

    const SUBSTITUTED_SEND: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }
"#;

    const SUBSTITUTED_ATTENDED_SEND: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] }, attention = ["irreversible"] }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }

[[policy.authority]]
name = "approver"
[policy.authority.permits]
attention = ["irreversible"]
"#;

    const SUBSTITUTED_SEND_FORKING: &str = r#"
version = 2

[[policy.tool]]
name = "read_hr"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "send"
parameters = { type = "object", properties = { body = { type = "string" } }, required = ["body"] }
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { a = { type = "integer" } } }

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]
[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }

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
        config_from(&text)
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
        let quoted = runtime
            .minted_offers(&root(), trajectory)
            .pop()
            .expect("the block surfaced an offer");
        runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0
    }

    fn standing_release(runtime: &Runtime) -> Option<crate::engine::OpenDispatch> {
        runtime.substituted_release(&root(), &root())
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let hop = latest_offer(&runtime);

        for _ in 0..2 {
            assert!(matches!(
                session.on_remedy(hop.clone(), None).await,
                Ok(RemedyDecision::NoAnswer { .. }),
            ));
            assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        }
    }

    /// No proposal released the substituted call, so no outcome hook is
    /// owed for it and a turn that ends before the harness runs it
    /// leaves it standing. Closing it would discard the remedy.
    #[tokio::test]
    async fn a_turn_end_leaves_a_standing_substituted_call_alone() {
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
        assert!(standing_release(&runtime).is_some());

        session.on_turn_end().await.expect("the turn end acks");

        assert!(
            standing_release(&runtime).is_some(),
            "the substituted call still stands after the turn ended"
        );
        assert_eq!(
            session
                .on_tool_call(send(REDACTED_BODY), false)
                .await
                .expect("the next turn is still handed the substituted call"),
            ToolCallDecision::Allow { spawn: None },
        );
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
        let session = runtime.session(&root(), &root()).expect("the trajectory reopens");
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
        let approval = latest_offer(&runtime);
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
version = 2

[[policy.tool]]
name = "fetch"

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

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
        config_from(&text)
    }

    #[tokio::test]
    async fn a_sanitized_child_return_crosses_as_the_derivation() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child = open_child(
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
        let child = open_child(
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
        let child = open_child(
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
            Err(EventError::TrajectoryEnded),
        ));
    }

    const ATTESTED_CHILD: &str = r#"
version = 2

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }

[policy.deployment]
context_control = true
confined_child_return = true
"#;

    const ATTESTED_CHILD_COMPOSED: &str = r#"
version = 2

[[sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }

[deployment]
context_control = true
confined_child_return = true
"#;

    const ATTEST_BOUND_CHILD: &str = r#"
version = 2

[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
[policy.sanitizer.permits]
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
        config_from(&text)
    }

    fn bare_externals() -> crate::config::ExternalBindings {
        crate::config::ExternalBindings::new(std::time::Duration::from_millis(2000), 65536)
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
            crate::config::Binding::Url {
                url: "http://127.0.0.1:1/".to_string(),
                token_env: None,
            },
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
            crate::config::Binding::Url {
                url: "http://127.0.0.1:1/".to_string(),
                token_env: None,
            },
        );
        let config = Config::embedded(
            "version = 2\n\n[deployment]\ncontext_control = true\n".to_string(),
            externals,
        )
        .expect("the policy embeds");
        assert!(matches!(
            Runtime::open(config, dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
    }

    const NARROWING: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
delta = { audience = ["insider"] }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

    fn narrowing_config(url: &str) -> Config {
        let text = format!(
            "[policy]\n{NARROWING}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        );
        config_from(&text)
    }

    const EMITTING_LEAK: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
effects = ["leak"]
delta = { audience = ["insider"] }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

    fn emitting_leak_config(url: &str) -> Config {
        let text = format!(
            "[policy]\n{EMITTING_LEAK}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        );
        config_from(&text)
    }

    fn leak() -> ProposedCall {
        ProposedCall {
            tool: "leak".to_string(),
            arguments: raw(serde_json::json!({"q": "all"})),
        }
    }

    async fn run_sanitize_offer(runtime: &Runtime, session: &mut crate::api::Session) -> ToolResultDecision {
        let offers = runtime.minted_offers(&root(), &root());
        let quoted = offers.last().expect("the block surfaced offers").clone();
        let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
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

    /// A tool whose result narrows on two dimensions with a sanitizer
    /// that clears only one: the derivation is admitted and staged, and
    /// the residual narrowing is what the model is told about.
    const PARTLY_CLEARED: &str = r#"
version = 2

[[policy.tool]]
name = "leak"
parameters = { type = "object", properties = { q = { type = "string" } } }
delta = { audience = ["insider"], trust = "suspicious" }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.deployment]
confined_results = ["leak"]
"#;

    fn partly_cleared_config(url: &str) -> Config {
        let text = format!(
            "[policy]\n{PARTLY_CLEARED}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        );
        config_from(&text)
    }

    #[tokio::test]
    async fn a_partly_cleared_derivation_is_staged_with_its_own_remedies() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime =
            Runtime::open(partly_cleared_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(leak(), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let before = runtime.minted_offers(&root(), &root()).len();
        let ToolResultDecision::Replace { placeholder } = run_sanitize_offer(&runtime, &mut session).await else {
            panic!("a staged derivation is delivered as a replacement, not kept");
        };
        assert!(
            !placeholder.contains("raw with pii"),
            "the raw body never reaches the model: {placeholder}",
        );
        assert!(
            runtime.minted_offers(&root(), &root()).len() > before,
            "the stage surfaced its own remedy for the narrowing the sanitizer left",
        );
    }

    const PARTLY_CLEARED_CHILD: &str = r#"
version = 2

[[policy.tool]]
name = "fetch"

[[policy.tool]]
name = "browse"
delta = { trust = "suspicious" }

[[policy.sanitizer]]
name = "scrub"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[policy.child]
return_sanitizer = "scrub"

[policy.deployment]
context_control = true
confined_child_return = true
"#;

    fn partly_cleared_child_config(url: &str) -> Config {
        config_from(&format!(
            "[policy]\n{PARTLY_CLEARED_CHILD}\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n"
        ))
    }

    #[tokio::test]
    async fn a_partly_cleared_child_return_is_staged_with_its_own_remedies() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"body": "scrubbed"})).await;
        let runtime = Runtime::open(partly_cleared_child_config(&url), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child = open_child(
            &mut session,
            fetch(serde_json::json!({})),
            TrajectoryId("cc:child".to_string()),
        )
        .await;
        let browse = ProposedCall {
            tool: "browse".to_string(),
            arguments: raw(serde_json::json!({})),
        };
        let child_id = TrajectoryId("cc:child".to_string());
        assert!(matches!(
            child.on_tool_call(browse.clone(), false).await,
            Ok(ToolCallDecision::Deny { .. })
        ));
        let offer = surfaced_offer_for(&runtime, &root(), &child_id);
        assert!(matches!(
            child.on_remedy(offer, None).await,
            Ok(RemedyDecision::Authorized { .. })
        ));
        assert_eq!(
            child
                .on_tool_call(browse.clone(), false)
                .await
                .expect("the accepted narrowing releases the call"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert_eq!(
            child
                .on_tool_result(
                    browse,
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("web page".to_string()),
                    },
                )
                .await
                .expect("the result admits into the child"),
            ToolResultDecision::Keep,
        );

        let before = runtime.minted_offers(&root(), &root()).len();
        let crossing = child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the staged return is delivered");
        let crate::api::ChildReturnDecision::Blocked { feedback } = crossing else {
            panic!("a return the sanitizer only partly cleared is staged, not crossed: {crossing:?}");
        };
        assert!(
            !feedback.contains("raw with pii"),
            "the raw return never reaches the parent: {feedback}",
        );
        assert!(
            runtime.minted_offers(&root(), &root()).len() > before,
            "the stage surfaced the parent's own remedy for the residual narrowing",
        );
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
version = 2

# A neutral second tool: its result folds at the trajectory's own label.
[[policy.tool]]
name = "fetch"
parameters = { type = "object", properties = { b = { type = "integer" }, a = { type = "integer" } } }
delta = {}

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
            let quoted = offers
                .first()
                .expect("the narrowing block surfaced its acceptance")
                .clone();
            let offer = runtime.resolve_in(&root(), &quoted).expect("the quoted id resolves").0;
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
        let child = open_child(
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
                    let handle = runtime.session(&root(), &root()).expect("the root reopens");
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
            matches!(runtime.live(&root(), &child("c1")), Err(EventError::TrajectoryEnded)),
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
        let first = session
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
        let second = session
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
        let session = runtime.create_session(root()).expect("a fresh id opens");
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
        let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
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
        assert!(matches!(
            runtime.live(&root(), &child("c1")),
            Err(EventError::TrajectoryEnded)
        ));
    }

    /// The harness refused the released call at its permission prompt,
    /// so no outcome hook fires for it. Without the turn end that
    /// dispatch would refuse every later proposal for the life of the
    /// trajectory.
    #[tokio::test]
    async fn a_turn_end_closes_the_call_the_harness_never_ran() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        assert_eq!(
            session
                .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
                .await
                .expect("the call releases"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert!(matches!(
            session.on_tool_call(fetch(serde_json::json!({"a": 2})), false).await,
            Err(EventError::CallOutstanding),
        ));

        session.on_turn_end().await.expect("the turn end closes it");

        assert!(runtime.open_dispatches(&root(), &root()).is_empty());
        assert!(
            runtime
                .audit(&root())
                .expect("the audit reads")
                .iter()
                .any(|entry| matches!(
                    &entry.event,
                    crate::engine::AuditEvent::Closed {
                        outcome: crate::engine::DispatchOutcome::Unknown
                    }
                )),
            "the unreported dispatch closed as unknown, not as a run that failed"
        );
        assert_eq!(
            session
                .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
                .await
                .expect("the next turn proposes freely"),
            ToolCallDecision::Allow { spawn: None },
        );
    }

    /// The outcome the close ruled out cannot be reported afterwards:
    /// a call the log says did not run admits no value.
    #[tokio::test]
    async fn an_outcome_reported_after_its_turn_end_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call releases");
        session.on_turn_end().await.expect("the turn end closes it");

        let error = session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("the run did happen".to_string()),
                },
            )
            .await
            .expect_err("the closed dispatch takes no outcome");
        assert!(matches!(error, EventError::UnknownDispatch), "got {error:?}");
    }

    /// A turn end is not an engine event when nothing is outstanding,
    /// which is every ordinary turn.
    #[tokio::test]
    async fn a_turn_end_over_a_settled_trajectory_appends_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call releases");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("body".to_string()),
                },
            )
            .await
            .expect("the outcome closes the dispatch");

        let settled = runtime.log_facts(&root()).len();
        session.on_turn_end().await.expect("the turn end acks");
        session.on_turn_end().await.expect("a repeat acks too");
        assert_eq!(runtime.log_facts(&root()).len(), settled, "no fact is appended");
    }

    /// A child carries its own dispatches, and one left open holds the
    /// child's return at the boundary.
    #[tokio::test]
    async fn a_child_turn_end_closes_its_abandoned_call_so_the_child_can_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child_session = open_child(&mut session, fetch(serde_json::json!({"a": 1})), child("c1")).await;
        child_session
            .on_tool_call(fetch(serde_json::json!({"a": 2})), false)
            .await
            .expect("the child's call releases");

        let error = child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect_err("a child with a call still open does not end");
        assert!(matches!(error, EventError::ChildDispatchOpen), "got {error:?}");

        child_session
            .on_turn_end()
            .await
            .expect("the child's turn end closes it");
        assert!(runtime.open_dispatches(&root(), &child("c1")).is_empty());
        child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect("with nothing in flight the child ends");
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
        let first = session
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

    /// A loopback authority that answers the same ruling every time and
    /// counts the requests it saw, so a test can pin how many
    /// round-trips one event takes.
    async fn counting_stub(answer: serde_json::Value) -> (String, Arc<AtomicUsize>) {
        use axum::routing::post;

        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let app = axum::Router::new().route(
            "/",
            post(move || {
                let answer = answer.clone();
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({"version": 1, "answer": answer}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback stub binds");
        let addr = listener.local_addr().expect("the stub has an address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("the stub serves");
        });
        (format!("http://{addr}/"), seen)
    }

    fn open_runtime(dir: &tempfile::TempDir) -> Runtime {
        Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens")
    }

    fn only_the_opening(runtime: &Runtime) -> bool {
        matches!(
            runtime.log_facts(&root()).as_slice(),
            [appa_engine::fact::Fact::TrajectoryOpened { .. }]
        )
    }

    #[test]
    fn a_used_root_id_is_refused_and_a_persisted_one_reopens() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            runtime.create_session(root()),
            Err(EventError::TrajectoryExists),
        ));
        assert!(runtime.session(&root(), &root()).is_ok());
        assert!(matches!(
            runtime.session(
                &TrajectoryId("cc:ghost".to_string()),
                &TrajectoryId("cc:ghost".to_string())
            ),
            Err(EventError::UnknownTrajectory),
        ));
    }

    #[test]
    fn a_damaged_database_is_refused_at_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        std::fs::write(&path, b"not a sqlite database at all").expect("the file writes");
        assert!(matches!(
            Runtime::open(config_with(FETCH_AND_SEND, None), path, None),
            Err(OpenError::Damaged(_)),
        ));
    }

    #[tokio::test]
    async fn a_decision_whose_append_fails_never_acts() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().fail_commit_after(0);
        assert!(matches!(
            session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
            Err(EventError::Storage(_)),
        ));
        assert!(only_the_opening(&runtime), "the killed append left nothing");
        assert!(
            runtime.open_dispatches(&root(), &root()).is_empty(),
            "a call whose release never committed is not open",
        );
    }

    #[tokio::test]
    async fn a_lost_race_discards_the_decision_and_replays() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().contend_next_appends(1);
        assert_eq!(
            session
                .on_tool_call(fetch(serde_json::json!({"a": 1})), false)
                .await
                .expect("the replay commits"),
            ToolCallDecision::Allow { spawn: None },
        );
        assert_eq!(
            runtime.log_basis(&root()),
            3,
            "the opening, the foreign append, and one committed attempt",
        );
        assert_eq!(
            runtime.open_dispatches(&root(), &root()).len(),
            1,
            "the discarded attempt released nothing",
        );
    }

    #[tokio::test]
    async fn a_permanently_contended_log_refuses_the_event() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.store().contend_next_appends(REPLAY_LIMIT as u64);
        assert!(matches!(
            session.on_tool_call(fetch(serde_json::json!({"a": 1})), false).await,
            Err(EventError::Contended { attempts: REPLAY_LIMIT }),
        ));
        assert_eq!(runtime.log_basis(&root()), 1 + REPLAY_LIMIT as u64);
        assert!(
            runtime.open_dispatches(&root(), &root()).is_empty(),
            "no attempt of a refused event acted",
        );
    }

    fn control_call(name: &str) -> ProposedCall {
        ProposedCall {
            tool: name.to_string(),
            arguments: raw(serde_json::json!({"offer_id": "o1:cc:root:ff"})),
        }
    }

    #[tokio::test]
    async fn a_lookalike_control_tool_is_an_undeclared_tool() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(control_call("mcp__evil__execute_remedy_plan"), false)
                .await,
            Err(EventError::UndeclaredTool { tool }) if tool == "mcp__evil__execute_remedy_plan",
        ));
    }

    #[test]
    fn an_over_cap_success_body_is_carried_as_unavailable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
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
    async fn an_unknown_offer_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session.on_remedy(OfferId("o1:cc:root:never".to_string()), None).await,
            Err(EventError::UnknownOffer),
        ));
        assert!(only_the_opening(&runtime), "a refused offer appends nothing");
    }

    #[tokio::test]
    async fn an_external_answer_settles_the_event_in_one_round_trip() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (url, seen) = counting_stub(serde_json::json!({"ruling": "approve"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session
                .on_tool_call(wire(500), false)
                .await
                .expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        assert_eq!(seen.load(Ordering::SeqCst), 0, "a proposal consults no authority");

        assert!(matches!(
            session
                .on_remedy(latest_offer(&runtime), None)
                .await
                .expect("the approval is delivered"),
            RemedyDecision::Authorized { .. },
        ));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the event re-drove once, carrying the answer it asked for",
        );
    }

    #[tokio::test]
    async fn no_offer_id_repeats_within_a_trajectory() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(
            config_with(ATTENTION, Some("http://127.0.0.1:1/")),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens");
        let session = runtime.create_session(root()).expect("a fresh id opens");
        for _ in 0..5 {
            assert!(matches!(
                session
                    .on_tool_call(wire(500), false)
                    .await
                    .expect("the block is delivered"),
                ToolCallDecision::Deny { .. },
            ));
        }
        let minted = runtime.minted_offers(&root(), &root());
        let distinct: std::collections::HashSet<&str> = minted.iter().map(|offer| offer.0.as_str()).collect();
        assert!(minted.len() >= 5, "each block surfaced an offer: {minted:?}");
        assert_eq!(
            distinct.len(),
            minted.len(),
            "five identical proposals minted five distinct ids: {minted:?}",
        );
    }

    fn bash_call() -> ProposedCall {
        ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "ls"})),
        }
    }

    fn bash_dispatch(label: &str) -> appa_engine::value::DispatchId {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 2
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
            classify_report(&bash_call(), canonical, &[]),
            Err(UnreportableOutcome::NoOpenDispatch),
        );
        assert_eq!(
            classify_report(&bash_call(), canonical, &[open("Bash", b"{}")]),
            Ok(id.clone()),
        );
        assert_eq!(
            classify_report(&bash_call(), canonical, &[open("Write", b"{}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "another tool is another call",
        );
        assert_eq!(
            classify_report(&bash_call(), canonical, &[open("Bash", b"{\"other\":1}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "other bytes are another occurrence",
        );
        assert_eq!(
            classify_report(&bash_call(), || None, &[open("Bash", b"{}")]),
            Err(UnreportableOutcome::ByteMismatch),
            "a call that cannot canonicalize matches nothing",
        );
        assert_eq!(
            classify_report(&bash_call(), canonical, &[open("Bash", b"{}"), open("Bash", b"{}")]),
            Err(UnreportableOutcome::NoOpenDispatch),
            "several open dispatches name no one occurrence",
        );
    }
}
