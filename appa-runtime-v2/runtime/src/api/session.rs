//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::elicit::Elicitation;
use crate::engine::{
    AuthorityVerdict, EngineDecision, EngineEvent, ExternalEvidence, ExternalRequest, Feedback, Next, OfferNonce,
    PolicyEngine, Presentation,
};
use crate::external::{ConsultKind, ConsultOutcome, DynamicResolution};
use crate::store::{BatchAppend, DispatchRow, DispatchState, EventWrite, Revision, RuntimeRecord};
use appa_engine::fact::ObservedResult;
use appa_engine::value::RawResultDigest;

use super::{
    AuthorizedCall, ChildReturnDecision, DispatchId, EventError, Inner, OfferId, OutcomeBody, ProposedCall,
    RemedyDecision, ToolCallDecision, ToolOutcome, ToolResultDecision, TrajectoryId,
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
/// These are the three contexts the runtime can observe, which are not
/// the threat model's three cases. An already-consumed report is
/// indistinguishable from an unknown one here: closed dispatches leave
/// the open view, and an outcome is attributable solely through its
/// open dispatch, so a byte match against a closed row
/// would name a call, never the occurrence that report belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnreportableOutcome {
    NoOpenDispatch,
    ByteMismatch,
    UnreleasedDispatch,
}

impl UnreportableOutcome {
    fn case(self) -> &'static str {
        match self {
            UnreportableOutcome::NoOpenDispatch => "no_open_dispatch",
            UnreportableOutcome::ByteMismatch => "byte_mismatch",
            UnreportableOutcome::UnreleasedDispatch => "unreleased_dispatch",
        }
    }

    fn refusal(self) -> EventError {
        match self {
            UnreportableOutcome::NoOpenDispatch | UnreportableOutcome::UnreleasedDispatch => {
                EventError::UnknownDispatch
            }
            UnreportableOutcome::ByteMismatch => EventError::OutcomeMismatch,
        }
    }
}

fn classify_report(
    call: &ProposedCall,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: Option<&DispatchRow>,
) -> Result<DispatchId, UnreportableOutcome> {
    let Some(open) = open else {
        return Err(UnreportableOutcome::NoOpenDispatch);
    };
    if call.tool != open.tool || canonical().as_deref() != Some(open.bytes.as_slice()) {
        return Err(UnreportableOutcome::ByteMismatch);
    }
    match open.state {
        DispatchState::Executing => Ok(open.id.clone()),
        DispatchState::Awaiting => Err(UnreportableOutcome::UnreleasedDispatch),
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
    family: TrajectoryId,
}

impl Session {
    pub(super) fn attach(inner: Arc<Inner>, trajectory: TrajectoryId, family: TrajectoryId) -> Session {
        Session {
            inner,
            trajectory,
            family,
        }
    }

    #[cfg(test)]
    pub(crate) fn trajectory(&self) -> &TrajectoryId {
        &self.trajectory
    }

    /// The user submitted a prompt. A request is an outer record, not
    /// an engine fact: nothing is reported to the
    /// engine, and offer freshness stays the engine's judgment — a
    /// stale offer declines at execution by live re-plan.
    pub fn on_prompt(&mut self, text: String) -> Result<(), EventError> {
        self.refuse_if_ended()?;
        self.commit_records(vec![RuntimeRecord::Request {
            trajectory: self.trajectory.clone(),
            text,
        }])?;
        tracing::debug!(trajectory = %self.trajectory.0, "prompt recorded");
        Ok(())
    }

    pub async fn on_tool_call(&mut self, call: ProposedCall) -> Result<ToolCallDecision, EventError> {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %self.trajectory.0, "control tool passes unchecked");
            return Ok(ToolCallDecision::Control);
        }
        self.refuse_if_ended()?;
        let policy = self.inner.resolve_policy(&self.family)?;
        if let Some(open) = self
            .inner
            .store
            .open_dispatch(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?
        {
            return match open.state {
                DispatchState::Executing => Err(EventError::CallOutstanding),
                DispatchState::Awaiting => {
                    let proposed = self.inner.engine.canonical_bytes(&policy, &call);
                    if call.tool == open.tool && proposed.as_deref() == Some(open.bytes.as_slice()) {
                        self.commit_records(vec![RuntimeRecord::PromoteDispatch { id: open.id }])?;
                        Ok(ToolCallDecision::Allow)
                    } else {
                        Err(EventError::CallOutstanding)
                    }
                }
            };
        }

        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                &policy,
                |evidence| EngineEvent::ModelResponse {
                    call: call.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                },
                move |decision| {
                    match &decision.then {
                        Next::ModelResponse { invocations, feedback } if feedback.is_empty() => {
                            match invocations.as_slice() {
                                [released] => vec![RuntimeRecord::OpenDispatch {
                                    id: released.dispatch.clone(),
                                    trajectory: trajectory.clone(),
                                    tool: released.tool.clone(),
                                    bytes: released.bytes.clone(),
                                    state: DispatchState::Executing,
                                }],
                                _ => Vec::new(),
                            }
                        }
                        Next::ModelResponse { invocations, feedback } if invocations.is_empty() => {
                            surfaced(&trajectory, feedback)
                        }
                        _ => Vec::new(),
                    }
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
                            "call released"
                        );
                        Ok(ToolCallDecision::Allow)
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

    pub async fn on_tool_result(
        &mut self,
        call: ProposedCall,
        o: ToolOutcome,
    ) -> Result<ToolResultDecision, EventError> {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %self.trajectory.0, "control tool outcome absorbed");
            return Ok(ToolResultDecision::Keep);
        }
        self.refuse_if_ended()?;
        let policy = self.inner.resolve_policy(&self.family)?;
        let open = self
            .inner
            .store
            .open_dispatch(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        let dispatch = match classify_report(
            &call,
            || self.inner.engine.canonical_bytes(&policy, &call),
            open.as_ref(),
        ) {
            Ok(dispatch) => dispatch,
            Err(case) => return Err(self.refuse_report(case, &call, open.as_ref())),
        };
        let o = self.cap_outcome(o);

        if let ToolOutcome::Success { body } = &o {
            let observed = match body {
                OutcomeBody::Available(raw) => ObservedResult::Available(RawResultDigest::of(raw.as_bytes())),
                OutcomeBody::Unavailable => ObservedResult::Unavailable,
            };
            let checkpoint = self.drive(
                &policy,
                || EngineEvent::SuccessObserved {
                    call: call.clone(),
                    observed: observed.clone(),
                },
                |_| Vec::new(),
            )?;
            match checkpoint.then {
                Next::Done => {}
                _ => return Err(EventError::UnexpectedDecision),
            }
        }

        let decision = self
            .drive_with_evidence(
                &policy,
                |evidence| EngineEvent::ToolOutcome {
                    call: call.clone(),
                    outcome: o.clone(),
                    evidence,
                },
                move |decision| {
                    match &decision.then {
                        Next::PresentToModel(Presentation::KeepOutput)
                        | Next::PresentToModel(Presentation::Value { .. }) => {
                            vec![RuntimeRecord::CloseDispatch { id: dispatch.clone() }]
                        }
                        Next::PresentToModel(Presentation::ReplaceOutput { .. }) => {
                            vec![RuntimeRecord::CloseDispatch { id: dispatch.clone() }]
                        }
                        _ => Vec::new(),
                    }
                },
                None,
            )
            .await?;

        match decision.then {
            Next::PresentToModel(Presentation::KeepOutput) => Ok(ToolResultDecision::Keep),
            Next::PresentToModel(Presentation::ReplaceOutput { placeholder, .. }) => {
                Ok(ToolResultDecision::Replace { placeholder })
            }
            // An admitted value delivered in place of the raw output.
            Next::PresentToModel(Presentation::Value { value }) => {
                Ok(ToolResultDecision::Replace { placeholder: value })
            }
            Next::PresentToModel(Presentation::Blocked { feedback, .. }) => {
                Ok(ToolResultDecision::Replace { placeholder: feedback })
            }
            _ => Err(EventError::UnexpectedDecision),
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
        self.refuse_if_ended()?;
        let policy = self.inner.resolve_policy(&self.family)?;
        let owner = self
            .inner
            .store
            .offer_trajectory(&offer)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        if owner.as_ref() != Some(&self.trajectory) {
            return Err(EventError::UnknownOffer);
        }

        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                &policy,
                |evidence| EngineEvent::ExecuteOffer {
                    offer: offer.clone(),
                    evidence,
                },
                move |decision| {
                    let mut records = match &decision.then {
                        Next::InvokeTool(released) => vec![RuntimeRecord::OpenDispatch {
                            id: released.dispatch.clone(),
                            trajectory: trajectory.clone(),
                            tool: released.tool.clone(),
                            bytes: released.bytes.clone(),
                            state: DispatchState::Awaiting,
                        }],
                        _ => Vec::new(),
                    };
                    if let Some(child) = &decision.ends_child {
                        records.push(RuntimeRecord::End { id: child.clone() });
                    }
                    records
                },
                elicitation,
            )
            .await?;

        match decision.then {
            Next::InvokeTool(released) => Ok(RemedyDecision::Authorized {
                call: AuthorizedCall {
                    tool: released.tool,
                    bytes: released.bytes,
                },
            }),
            Next::PresentToModel(Presentation::Value { value }) => Ok(RemedyDecision::Returned { value }),
            Next::PresentToModel(Presentation::Declined { feedback }) => Ok(RemedyDecision::Declined { feedback }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    pub fn on_child_start(&mut self, id: TrajectoryId) -> Result<Session, EventError> {
        self.refuse_if_ended()?;
        let policy = self.inner.resolve_policy(&self.family)?;
        let existing = self
            .inner
            .store
            .trajectory(&id)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        if existing.is_some() {
            return Err(EventError::TrajectoryExists);
        }
        let child = id.clone();
        let parent = self.trajectory.clone();
        let decision = self.drive(
            &policy,
            || EngineEvent::ChildStart { child: child.clone() },
            |_| {
                vec![RuntimeRecord::OpenChild {
                    id: child.clone(),
                    parent: parent.clone(),
                }]
            },
        )?;
        match decision.then {
            Next::Done => Ok(Session::attach(Arc::clone(&self.inner), id, self.family.clone())),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The child finished. Its final message is its only return
    /// channel and is checked before it may cross to the parent;
    /// `None` returns no value. The child ends
    /// in the same transaction as its return's facts.
    pub async fn on_child_end(&mut self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        self.refuse_if_ended()?;
        let policy = self.inner.resolve_policy(&self.family)?;
        let owner = self
            .inner
            .store
            .trajectory(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?
            .and_then(|row| row.parent)
            .ok_or(EventError::NotAChild)?;
        let trajectory = self.trajectory.clone();
        let child = self.trajectory.clone();
        let parent = owner.clone();
        let decision = self
            .drive_with_evidence(
                &policy,
                |evidence| EngineEvent::ChildReturn {
                    parent: parent.clone(),
                    child: child.clone(),
                    value: value.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                },
                move |decision| {
                    match &decision.then {
                        Next::PresentToModel(Presentation::Value { .. })
                        | Next::PresentToModel(Presentation::NoValue) => {
                            vec![RuntimeRecord::End { id: trajectory.clone() }]
                        }
                        Next::PresentToModel(Presentation::Blocked { offers, .. }) => offer_records(&owner, offers),
                        _ => Vec::new(),
                    }
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

    fn refuse_report(&self, case: UnreportableOutcome, call: &ProposedCall, open: Option<&DispatchRow>) -> EventError {
        tracing::warn!(
            trajectory = %self.trajectory.0,
            tool = %call.tool,
            dispatch = open.map(|row| row.id.0.as_str()).unwrap_or("-"),
            case = case.case(),
            "an outcome report named no reportable dispatch",
        );
        case.refusal()
    }

    fn refuse_if_ended(&self) -> Result<(), EventError> {
        let row = self
            .inner
            .store
            .trajectory(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?
            .ok_or(EventError::UnknownTrajectory)?;
        if row.ended {
            return Err(EventError::TrajectoryEnded);
        }
        Ok(())
    }

    fn commit_records(&self, records: Vec<RuntimeRecord>) -> Result<(), EventError> {
        match self
            .inner
            .store
            .commit_event(&self.family, EventWrite { batch: None, records })
        {
            Ok(_) => Ok(()),
            // Another handle of this trajectory won the race.
            Err(crate::store::CommitError::DispatchAlreadyOpen) => Err(EventError::CallOutstanding),
            // A concurrent event won a child-opening race.
            Err(crate::store::CommitError::TrajectoryExists) => Err(EventError::TrajectoryExists),
            Err(error) => Err(EventError::Storage(error.to_string())),
        }
    }

    async fn drive_with_evidence(
        &self,
        policy: &PolicyEngine<'_>,
        mut event: impl FnMut(Vec<ExternalEvidence>) -> EngineEvent,
        records: impl Fn(&EngineDecision) -> Vec<RuntimeRecord>,
        elicitation: Option<&Elicitation>,
    ) -> Result<EngineDecision, EventError> {
        let mut evidence: Vec<ExternalEvidence> = Vec::new();
        for _ in 0..EVIDENCE_LIMIT {
            let carried = evidence.clone();
            let decision = self.drive(policy, || event(carried.clone()), &records)?;
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
        policy: &PolicyEngine<'_>,
        mut event: impl FnMut() -> EngineEvent,
        records: impl Fn(&EngineDecision) -> Vec<RuntimeRecord>,
    ) -> Result<EngineDecision, EventError> {
        for attempt in 1..=REPLAY_LIMIT {
            let (log, _revision) = self
                .inner
                .store
                .load_log(&self.family)
                .map_err(|error| EventError::Storage(error.to_string()))?;
            let view = self
                .inner
                .engine
                .rebuild_view(policy, &log, &self.family, &self.trajectory)
                .map_err(EventError::from)?;
            let decision = self
                .inner
                .engine
                .handle(policy, &view, event())
                .map_err(EventError::from)?;

            let event_records = if matches!(decision.then, Next::ResolveExternal(_)) {
                Vec::new()
            } else {
                let mut event_records = records(&decision);
                event_records.extend(
                    decision
                        .offers
                        .retire
                        .iter()
                        .map(|id| RuntimeRecord::RetireOffer { id: id.clone() }),
                );
                event_records
            };
            let batch = decision
                .append
                .as_ref()
                .map(|batch| {
                    Ok::<BatchAppend, EventError>(BatchAppend {
                        bytes: serde_json::to_vec(&batch.facts)
                            .map_err(|error| EventError::Storage(format!("batch does not serialize: {error}")))?,
                        based_on: Revision(batch.basis.value()),
                    })
                })
                .transpose()?;
            if batch.is_none() {
                if event_records.is_empty() {
                    self.inner.engine.apply_offers(decision.offers.clone());
                    return Ok(decision);
                }
                return match self.inner.store.commit_event(
                    &self.family,
                    EventWrite {
                        batch: None,
                        records: event_records,
                    },
                ) {
                    Ok(_) => {
                        self.inner.engine.apply_offers(decision.offers.clone());
                        Ok(decision)
                    }
                    Err(crate::store::CommitError::DispatchAlreadyOpen) => Err(EventError::CallOutstanding),
                    Err(error) => Err(EventError::Storage(error.to_string())),
                };
            }
            match self.inner.store.commit_event(
                &self.family,
                EventWrite {
                    batch,
                    records: event_records,
                },
            ) {
                Ok(_) => {
                    self.inner.engine.apply_offers(decision.offers.clone());
                    return Ok(decision);
                }
                Err(crate::store::CommitError::Conflict { .. }) => {
                    tracing::debug!(
                        family = %self.family.0,
                        attempt,
                        "stale revision: discarding the decision and replaying the event"
                    );
                    continue;
                }
                Err(crate::store::CommitError::DispatchAlreadyOpen) => {
                    return Err(EventError::CallOutstanding);
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
                dispatch,
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
                    dispatch: dispatch.clone(),
                }
            }
            ExternalRequest::Sanitizer { sanitizer, payload } => {
                let outcome = self
                    .inner
                    .externals
                    .consult(ConsultKind::Sanitizer, sanitizer, payload, None)
                    .await;
                let derived = match outcome {
                    ConsultOutcome::Answer(body) => body.get("body").and_then(|b| b.as_str()).map(String::from),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Sanitizer {
                    sanitizer: sanitizer.clone(),
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
                    DynamicResolution::Resolved { readers } => Some(readers),
                    DynamicResolution::Unresolved(_) => None,
                };
                ExternalEvidence::Dynamic {
                    resolver: resolver.clone(),
                    argument: argument.clone(),
                    readers,
                }
            }
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

fn surfaced(trajectory: &TrajectoryId, feedback: &[Feedback]) -> Vec<RuntimeRecord> {
    feedback
        .iter()
        .flat_map(|entry| offer_records(trajectory, &entry.offers))
        .collect()
}

fn offer_records(trajectory: &TrajectoryId, offers: &[OfferId]) -> Vec<RuntimeRecord> {
    offers
        .iter()
        .map(|offer| RuntimeRecord::SurfaceOffer {
            id: offer.clone(),
            trajectory: trajectory.clone(),
        })
        .collect()
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
    use crate::store::DispatchRow;
    use appa_engine::fact::{BoundaryKind, Fact, FactBatch, Revision as EngineRevision};

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

    fn decision(append: Option<FactBatch>, then: Next) -> EngineDecision {
        EngineDecision {
            append,
            then,
            offers: crate::engine::OfferMutations::default(),
            ends_child: None,
        }
    }

    #[derive(Clone, Copy)]
    enum Marker {
        One,
        Two,
    }

    fn batch(marker: Marker, based_on: u64) -> FactBatch {
        let punctuation = match marker {
            Marker::One => 1,
            Marker::Two => 2,
        };
        FactBatch::new(
            EngineRevision::new(based_on),
            (0..punctuation)
                .map(|_| Fact::Boundary {
                    trajectory: appa_engine::value::TrajectoryId::new("cc:root"),
                    kind: BoundaryKind::TurnEnd,
                })
                .collect(),
        )
    }

    fn batch_bytes(marker: Marker) -> Vec<u8> {
        serde_json::to_vec(&batch(marker, 0).facts).expect("the test facts serialize")
    }

    fn assert_only_the_opening(log: &[Vec<u8>]) {
        assert_eq!(log.len(), 1);
        let facts: Vec<Fact> = serde_json::from_slice(&log[0]).expect("the row decodes as engine facts");
        assert!(matches!(facts.as_slice(), [Fact::TrajectoryOpened { .. }]));
    }

    fn call() -> ProposedCall {
        ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "ls"})),
        }
    }

    fn released(id: &str, call: &ProposedCall) -> ReleasedCall {
        ReleasedCall {
            dispatch: DispatchId(id.to_string()),
            tool: call.tool.clone(),
            bytes: serde_json::to_vec(call).expect("the test call serializes"),
        }
    }

    fn allow_decision(id: &str, call: &ProposedCall) -> EngineDecision {
        decision(
            None,
            Next::ModelResponse {
                invocations: vec![released(id, call)],
                feedback: Vec::new(),
            },
        )
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

    fn present(p: Presentation) -> EngineDecision {
        decision(None, Next::PresentToModel(p))
    }

    fn done() -> EngineDecision {
        decision(None, Next::Done)
    }

    fn reviewed_dispatch() -> appa_engine::value::DispatchId {
        let policy = appa_policy::Config::from_toml_str(
            r#"
                version = 1
                [[tool]]
                name = "Bash"
            "#,
        )
        .expect("the fixture policy compiles");
        let engine = policy.engine().clone();
        let call = engine
            .resolve_call(appa_engine::value::ToolName::new("Bash"), br#"{"command":"ls"}"#)
            .expect("the fixture call resolves through the engine");
        appa_engine::value::DispatchId::new(appa_engine::value::TrajectoryId::new("cc:root"), call.digest(), 0)
    }

    fn review() -> appa_engine::execute::AuthorityReview {
        appa_engine::execute::AuthorityReview {
            tool: appa_engine::value::ToolName::new("Bash"),
            trajectory_label: appa_engine::label::PartialLabel::established(appa_engine::label::EstablishedLabel::top()),
        }
    }

    #[test]
    fn a_used_trajectory_id_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            runtime.create_session(root()),
            Err(SessionError::AlreadyExists),
        ));
    }

    #[test]
    fn an_unknown_trajectory_is_refused_and_a_persisted_one_reopens() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        assert!(matches!(
            runtime.session(&TrajectoryId("cc:ghost".to_string())),
            Err(SessionError::Unknown),
        ));
        runtime.create_session(root()).expect("a fresh id opens");
        let session = runtime.session(&root()).expect("the persisted trajectory reopens");
        assert_eq!(session.trajectory(), &root());
    }

    #[test]
    fn a_damaged_database_is_refused_at_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        std::fs::write(&db, b"not a sqlite database at all").expect("the file writes");
        match Runtime::open(config(), db, None) {
            Err(OpenError::Damaged(_) | OpenError::Storage(_)) => {}
            other => panic!("a damaged database must refuse to open, opened={}", other.is_ok()),
        }
    }

    #[test]
    fn an_event_on_an_ended_trajectory_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .store
            .commit_event(
                &root(),
                EventWrite {
                    batch: None,
                    records: vec![RuntimeRecord::End { id: root() }],
                },
            )
            .expect("the end record commits");
        assert!(matches!(
            session.on_prompt("late".to_string()),
            Err(EventError::TrajectoryEnded),
        ));
        assert!(matches!(runtime.session(&root()), Err(SessionError::Ended)));
    }

    #[test]
    fn on_prompt_commits_the_request_record_without_an_engine_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_prompt("read the report".to_string())
            .expect("the prompt is accepted");

        assert_eq!(runtime.inner.store.request_texts(&root()), vec!["read the report"]);
        let (log, revision) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_only_the_opening(&log);
        assert_eq!(revision, Revision(1));
        assert!(runtime.inner.engine.seen().is_empty(), "the engine is never consulted");
    }

    #[tokio::test]
    async fn a_decision_whose_commit_fails_never_acts() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(decision(
            Some(batch(Marker::One, 1)),
            Next::ModelResponse {
                invocations: vec![released("d1", &call())],
                feedback: Vec::new(),
            },
        ));
        runtime.inner.store.fail_next_commit();
        assert!(matches!(
            session.on_tool_call(call()).await,
            Err(EventError::Storage(_)),
        ));
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_only_the_opening(&log);
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_stale_decision_is_discarded_and_the_replay_uses_a_fresh_random_number() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(decision(
            Some(batch(Marker::One, 2)),
            Next::ModelResponse {
                invocations: vec![released("d-stale", &call())],
                feedback: Vec::new(),
            },
        ));
        runtime.inner.engine.enqueue(decision(
            Some(batch(Marker::Two, 1)),
            Next::ModelResponse {
                invocations: vec![released("d-fresh", &call())],
                feedback: Vec::new(),
            },
        ));
        let outcome = session.on_tool_call(call()).await.expect("the replayed event commits");
        assert_eq!(outcome, ToolCallDecision::Allow);
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(log.len(), 2);
        assert_eq!(log[1], batch_bytes(Marker::Two));
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the released call opened a dispatch");
        assert_eq!(open.id, DispatchId("d-fresh".to_string()));
        assert_eq!(open.state, DispatchState::Executing);

        let seen = runtime.inner.engine.seen();
        let entropies: Vec<_> = seen
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ModelResponse { entropy, .. } => Some(entropy.0),
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 2);
        assert_ne!(entropies[0], entropies[1]);
    }

    #[tokio::test]
    async fn a_permanently_contended_log_refuses_the_event() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        for _ in 0..REPLAY_LIMIT {
            runtime.inner.engine.enqueue(decision(
                Some(batch(Marker::One, 2)),
                Next::ModelResponse {
                    invocations: vec![released("d-contended", &call())],
                    feedback: Vec::new(),
                },
            ));
        }
        assert!(matches!(
            session.on_tool_call(call()).await,
            Err(EventError::Contended { attempts: REPLAY_LIMIT }),
        ));
        assert_eq!(runtime.inner.engine.seen().len(), REPLAY_LIMIT as usize);
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_only_the_opening(&log);
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
            "the refused event released nothing",
        );
    }

    #[tokio::test]
    async fn a_second_call_while_one_executes_is_refused_without_an_engine_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        let outcome = session.on_tool_call(call()).await.expect("the call is allowed");
        assert_eq!(outcome, ToolCallDecision::Allow);

        let before = runtime.inner.engine.seen().len();
        assert!(matches!(
            session.on_tool_call(call()).await,
            Err(EventError::CallOutstanding),
        ));
        assert_eq!(runtime.inner.engine.seen().len(), before);
    }

    fn control_call(name: &str) -> ProposedCall {
        ProposedCall {
            tool: name.to_string(),
            arguments: raw(serde_json::json!({"offer_id": "x"})),
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
            let outcome = session
                .on_tool_call(control_call(name))
                .await
                .expect("the control tool passes");
            assert_eq!(outcome, ToolCallDecision::Control, "{name} is the control tool");
        }
        assert!(runtime.inner.engine.seen().is_empty(), "the engine is never consulted");
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
            "the control tool opens no dispatch",
        );
    }

    #[tokio::test]
    async fn a_lookalike_control_tool_reaches_the_engine() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .engine
            .enqueue(deny_decision("blocked: not the runtime's tool", &[]));
        let outcome = session
            .on_tool_call(control_call("mcp__evil__execute_remedy_plan"))
            .await
            .expect("the deny is delivered");
        assert!(matches!(outcome, ToolCallDecision::Deny { .. }));
        assert_eq!(
            runtime.inner.engine.seen().len(),
            1,
            "the lookalike is checked like any call",
        );
    }

    #[tokio::test]
    async fn a_control_call_and_its_outcome_pass_while_a_dispatch_is_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        assert_eq!(
            session.on_tool_call(call()).await.expect("the call is allowed"),
            ToolCallDecision::Allow,
        );
        let before = runtime.inner.engine.seen().len();

        assert_eq!(
            session
                .on_tool_call(control_call("execute_remedy_plan"))
                .await
                .expect("the control call passes"),
            ToolCallDecision::Control,
        );
        assert_eq!(
            session
                .on_tool_result(
                    control_call("execute_remedy_plan"),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("Authorized.".to_string()),
                    },
                )
                .await
                .expect("the control outcome is absorbed"),
            ToolResultDecision::Keep,
        );
        assert_eq!(
            runtime.inner.engine.seen().len(),
            before,
            "the engine was not consulted since the release",
        );
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the original dispatch stays open");
        assert_eq!(open.id, DispatchId("d1".to_string()));
        assert_eq!(open.state, DispatchState::Executing);

        runtime.inner.engine.enqueue(done());
        runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
        assert_eq!(
            session
                .on_tool_result(
                    call(),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("ok".to_string()),
                    },
                )
                .await
                .expect("the result is admitted"),
            ToolResultDecision::Keep,
        );
    }

    #[tokio::test]
    async fn a_denied_call_returns_feedback_and_records_its_offers() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .engine
            .enqueue(deny_decision("blocked: the recipient cannot read this", &["offer-1"]));
        let outcome = session.on_tool_call(call()).await.expect("the deny is delivered");
        assert_eq!(
            outcome,
            ToolCallDecision::Deny {
                feedback: "blocked: the recipient cannot read this".to_string(),
            },
        );
        assert_eq!(
            runtime
                .inner
                .store
                .offer_trajectory(&OfferId("offer-1".to_string()))
                .expect("the offer query runs"),
            Some(root()),
        );
    }

    #[tokio::test]
    async fn a_kept_result_closes_the_dispatch_and_a_second_report_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        runtime.inner.engine.enqueue(done());
        runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
        let outcome = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("output".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(outcome, ToolResultDecision::Keep);
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );
        assert!(matches!(
            session.on_tool_result(call(), ToolOutcome::Indeterminate).await,
            Err(EventError::UnknownDispatch),
        ));
    }

    #[tokio::test]
    async fn a_replaced_result_delivers_the_placeholder_and_closes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        runtime.inner.engine.enqueue(done());
        runtime.inner.engine.enqueue(present(Presentation::ReplaceOutput {
            placeholder: "the output is confined".to_string(),
        }));
        let outcome = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("secret".to_string()),
                },
            )
            .await
            .expect("the replacement is delivered");
        assert_eq!(
            outcome,
            ToolResultDecision::Replace {
                placeholder: "the output is confined".to_string(),
            },
        );
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
            "the replaced result closed the dispatch",
        );
    }

    #[tokio::test]
    async fn a_mismatched_outcome_is_refused_and_the_dispatch_stays_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        let error = session
            .on_tool_result(
                ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: raw(serde_json::json!({"command": "rm"})),
                },
                ToolOutcome::Success {
                    body: OutcomeBody::Available("output".to_string()),
                },
            )
            .await
            .expect_err("a mismatched outcome is refused");
        assert!(matches!(error, EventError::OutcomeMismatch));
        assert_eq!(
            error.to_string(),
            "this outcome does not match the open dispatch; it is not reported",
        );
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the dispatch stays open");
        assert_eq!(open.id, DispatchId("d1".to_string()));
        assert_eq!(open.state, DispatchState::Executing);
        assert_eq!(runtime.inner.engine.seen().len(), 1);
    }

    #[tokio::test]
    async fn an_outcome_for_an_unreleased_dispatch_is_refused_and_it_stays_awaiting() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(deny_decision(
            "blocked; execute_remedy_plan(offer-1) authorizes it",
            &["offer-1"],
        ));
        session.on_tool_call(call()).await.expect("the deny is delivered");
        runtime
            .inner
            .engine
            .enqueue(decision(None, Next::InvokeTool(released("d-authorized", &call()))));
        session
            .on_remedy(OfferId("offer-1".to_string()), None)
            .await
            .expect("the remedy authorizes");

        let before = runtime.inner.engine.seen().len();
        assert!(matches!(
            session
                .on_tool_result(
                    call(),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("ran anyway".to_string()),
                    },
                )
                .await,
            Err(EventError::UnknownDispatch),
        ));
        assert_eq!(runtime.inner.engine.seen().len(), before);
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the dispatch stays open");
        assert_eq!(open.state, DispatchState::Awaiting);
    }

    #[test]
    fn an_outcome_report_is_classified_against_the_open_dispatch() {
        let row = |state| DispatchRow {
            id: DispatchId("d1".to_string()),
            tool: "Bash".to_string(),
            bytes: serde_json::to_vec(&call()).expect("the test call serializes"),
            state,
        };
        let canonical = |call: &ProposedCall| {
            let bytes = serde_json::to_vec(call).expect("the reported call serializes");
            move || Some(bytes)
        };

        assert_eq!(
            classify_report(&call(), canonical(&call()), Some(&row(DispatchState::Executing))),
            Ok(DispatchId("d1".to_string())),
            "the executing dispatch takes its own call's outcome",
        );
        assert_eq!(
            classify_report(&call(), canonical(&call()), None),
            Err(UnreportableOutcome::NoOpenDispatch),
            "an already-consumed outcome arrives here too: closed dispatches leave the open view",
        );
        assert_eq!(
            classify_report(&call(), canonical(&call()), Some(&row(DispatchState::Awaiting))),
            Err(UnreportableOutcome::UnreleasedDispatch),
            "an approved dispatch is not a released one",
        );
        let other = ProposedCall {
            tool: "Bash".to_string(),
            arguments: raw(serde_json::json!({"command": "rm"})),
        };
        assert_eq!(
            classify_report(&other, canonical(&other), Some(&row(DispatchState::Executing))),
            Err(UnreportableOutcome::ByteMismatch),
        );
        assert_eq!(
            classify_report(
                &ProposedCall {
                    tool: "Write".to_string(),
                    arguments: call().arguments,
                },
                canonical(&call()),
                Some(&row(DispatchState::Executing)),
            ),
            Err(UnreportableOutcome::ByteMismatch),
            "a different tool over identical bytes is still not that dispatch",
        );
        assert_eq!(
            classify_report(&call(), || None, Some(&row(DispatchState::Executing))),
            Err(UnreportableOutcome::ByteMismatch),
            "a call the engine cannot canonicalize matches nothing",
        );

        assert_eq!(
            classify_report(&call(), || panic!("no open dispatch canonicalizes nothing"), None),
            Err(UnreportableOutcome::NoOpenDispatch),
        );
    }

    #[tokio::test]
    async fn an_over_cap_success_body_is_carried_as_unavailable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        runtime.inner.engine.enqueue(done());
        runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
        let outcome = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("x".repeat(70000)),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(outcome, ToolResultDecision::Keep);
        let seen = runtime.inner.engine.seen();
        match seen.last() {
            Some(EngineEvent::ToolOutcome { outcome, .. }) => assert_eq!(
                outcome,
                &ToolOutcome::Success {
                    body: OutcomeBody::Unavailable
                },
                "the over-cap body is carried as unavailable",
            ),
            other => panic!("expected a ToolOutcome event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_offer_is_refused_without_an_engine_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session.on_remedy(OfferId("never-surfaced".to_string()), None).await,
            Err(EventError::UnknownOffer),
        ));
        assert!(runtime.inner.engine.seen().is_empty());
    }

    #[tokio::test]
    async fn an_authorized_remedy_opens_a_dispatch_the_reproposed_call_resumes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        runtime.inner.engine.enqueue(deny_decision(
            "blocked; execute_remedy_plan(offer-3) authorizes it",
            &["offer-3"],
        ));
        session.on_tool_call(call()).await.expect("the deny is delivered");

        runtime
            .inner
            .engine
            .enqueue(decision(None, Next::InvokeTool(released("d-authorized", &call()))));
        let outcome = session
            .on_remedy(OfferId("offer-3".to_string()), None)
            .await
            .expect("the remedy authorizes");
        let expected_bytes = serde_json::to_vec(&call()).expect("the test call serializes");
        assert_eq!(
            outcome,
            RemedyDecision::Authorized {
                call: AuthorizedCall {
                    tool: "Bash".to_string(),
                    bytes: expected_bytes.clone()
                },
            },
        );

        assert!(matches!(
            runtime.inner.store.open_dispatch(&root()),
            Ok(Some(DispatchRow {
                state: DispatchState::Awaiting,
                ..
            })),
        ));

        let before = runtime.inner.engine.seen().len();
        let resumed = session.on_tool_call(call()).await.expect("the re-proposal resumes");
        assert_eq!(resumed, ToolCallDecision::Allow);
        assert_eq!(runtime.inner.engine.seen().len(), before);
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the resumed dispatch stays open");
        assert_eq!(open.id, DispatchId("d-authorized".to_string()));
        assert_eq!(open.state, DispatchState::Executing);

        assert!(matches!(
            session
                .on_tool_call(ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: raw(serde_json::json!({"command": "rm -rf /"})),
                })
                .await,
            Err(EventError::CallOutstanding),
        ));
    }

    #[tokio::test]
    async fn remedy_outcomes_map_onto_their_decisions() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(deny_decision("blocked", &["offer-4"]));
        session.on_tool_call(call()).await.expect("the deny is delivered");

        runtime.inner.engine.enqueue(present(Presentation::NoAnswer {
            feedback: "no answer yet; the offer stands".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-4".to_string()), None)
                .await
                .expect("no answer"),
            RemedyDecision::NoAnswer {
                feedback: "no answer yet; the offer stands".to_string()
            },
        );
        runtime.inner.engine.enqueue(present(Presentation::Value {
            value: "the cleaned result".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-4".to_string()), None)
                .await
                .expect("returned"),
            RemedyDecision::Returned {
                value: "the cleaned result".to_string()
            },
        );
        runtime.inner.engine.enqueue(present(Presentation::Declined {
            feedback: "the authority declined".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-4".to_string()), None)
                .await
                .expect("declined"),
            RemedyDecision::Declined {
                feedback: "the authority declined".to_string()
            },
        );
    }

    #[tokio::test]
    async fn evidence_round_trips_replay_the_same_event_and_no_answer_grants_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(decision(
            None,
            Next::ResolveExternal(vec![ExternalRequest::Authority {
                authority: "security".to_string(),
                payload: serde_json::json!({"call": "Bash"}),
                review: review(),
                dispatch: reviewed_dispatch(),
            }]),
        ));
        runtime
            .inner
            .engine
            .enqueue(deny_decision("no answer is no permission", &[]));
        let outcome = session.on_tool_call(call()).await.expect("the deny is delivered");
        assert_eq!(
            outcome,
            ToolCallDecision::Deny {
                feedback: "no answer is no permission".to_string()
            },
        );

        let seen = runtime.inner.engine.seen();
        assert_eq!(seen.len(), 2);
        match (&seen[0], &seen[1]) {
            (
                EngineEvent::ModelResponse { evidence: first, .. },
                EngineEvent::ModelResponse { evidence: second, .. },
            ) => {
                assert!(first.is_empty());
                assert_eq!(
                    second.as_slice(),
                    [ExternalEvidence::Authority {
                        authority: "security".to_string(),
                        verdict: AuthorityVerdict::Abstain,
                        review: review(),
                        dispatch: reviewed_dispatch(),
                    }],
                    "an unconfigured authority abstains; no answer grants nothing",
                );
            }
            other => panic!("expected two ModelResponse events, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_child_opens_in_the_family_returns_its_value_and_ends() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .engine
            .enqueue(decision(Some(batch(Marker::Two, 1)), Next::Done));
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child opens");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the family log loads");
        assert_eq!(log.len(), 2, "the seed follows the opening in the one family log");
        assert_eq!(log[1], batch_bytes(Marker::Two));

        assert!(matches!(
            session.on_child_start(TrajectoryId("cc:child".to_string())),
            Err(EventError::TrajectoryExists),
        ));

        child
            .on_prompt("work".to_string())
            .expect("the child prompt is accepted");

        runtime.inner.engine.enqueue(present(Presentation::Value {
            value: "the summary".to_string(),
        }));
        let returned = child
            .on_child_end(Some("the summary".to_string()))
            .await
            .expect("the return crosses");
        assert_eq!(
            returned,
            ChildReturnDecision::Returned {
                value: "the summary".to_string()
            },
        );

        assert!(matches!(
            child.on_prompt("late".to_string()),
            Err(EventError::TrajectoryEnded),
        ));
        assert!(matches!(
            runtime.session(&TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));
    }

    #[tokio::test]
    async fn a_blocked_child_return_carries_feedback_and_leaves_the_child_unended() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(done());
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child opens");

        runtime.inner.engine.enqueue(present(Presentation::Blocked {
            feedback: "the return may not cross; options are offered".to_string(),
            offers: vec![OfferId("offer-6".to_string())],
        }));
        let blocked = child
            .on_child_end(Some("the secret".to_string()))
            .await
            .expect("the block is delivered");
        assert_eq!(
            blocked,
            ChildReturnDecision::Blocked {
                feedback: "the return may not cross; options are offered".to_string(),
            },
        );
        assert_eq!(
            runtime
                .inner
                .store
                .offer_trajectory(&OfferId("offer-6".to_string()))
                .expect("the offer query runs"),
            Some(root()),
        );
        assert!(
            runtime.session(&TrajectoryId("cc:child".to_string())).is_ok(),
            "the child is not ended while its crossing is pending",
        );

        runtime.inner.engine.enqueue(done());
        runtime.inner.engine.enqueue(present(Presentation::NoValue));
        let none = session
            .on_child_start(TrajectoryId("cc:child-2".to_string()))
            .expect("a second child opens")
            .on_child_end(None)
            .await
            .expect("the void return is delivered");
        assert_eq!(none, ChildReturnDecision::NoValue);
        assert!(matches!(
            runtime.session(&TrajectoryId("cc:child-2".to_string())),
            Err(SessionError::Ended),
        ));
    }

    #[tokio::test]
    async fn the_parent_executes_a_blocked_returns_offer_and_the_merge_ends_the_child() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(done());
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child opens");
        runtime.inner.engine.enqueue(present(Presentation::Blocked {
            feedback: "blocked; execute_remedy_plan(offer-7)".to_string(),
            offers: vec![OfferId("offer-7".to_string())],
        }));
        child
            .on_child_end(Some("the secret".to_string()))
            .await
            .expect("the block is delivered");

        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::PresentToModel(Presentation::Value {
                value: "the cleaned return".to_string(),
            }),
            offers: crate::engine::OfferMutations::default(),
            ends_child: Some(TrajectoryId("cc:child".to_string())),
        });
        assert_eq!(
            session
                .on_remedy(OfferId("offer-7".to_string()), None)
                .await
                .expect("the remedy runs"),
            RemedyDecision::Returned {
                value: "the cleaned return".to_string()
            },
        );
        assert!(matches!(
            runtime.session(&TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));
    }

    #[tokio::test]
    async fn a_root_cannot_submit_a_child_return() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session.on_child_end(Some("value".to_string())).await,
            Err(EventError::NotAChild),
        ));
        assert!(runtime.session(&root()).is_ok());
    }

    #[tokio::test]
    async fn the_random_number_never_repeats_in_a_session() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        for turn in 0..5 {
            runtime
                .inner
                .engine
                .enqueue(allow_decision(&format!("d{turn}"), &call()));
            session.on_tool_call(call()).await.expect("the call is allowed");
            runtime.inner.engine.enqueue(done());
            runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
            session
                .on_tool_result(
                    call(),
                    ToolOutcome::Success {
                        body: OutcomeBody::Available("ok".to_string()),
                    },
                )
                .await
                .expect("the result is admitted");
        }
        let mut entropies: Vec<[u8; 32]> = runtime
            .inner
            .engine
            .seen()
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ModelResponse { entropy, .. } | EngineEvent::ChildReturn { entropy, .. } => {
                    Some(entropy.0)
                }
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 5);
        entropies.sort();
        entropies.dedup();
        assert_eq!(entropies.len(), 5, "a random number repeated within the session");
    }
}

#[cfg(test)]
mod real_engine_tests {
    use super::super::{OpenError, OutcomeBody, Runtime, SessionError};
    use super::*;
    use crate::api::{RemedyDecision, ToolCallDecision, ToolOutcome, ToolResultDecision};
    use crate::config::Config;
    use crate::store::{BatchAppend, EventWrite, Revision};

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

    const EMITTING_FETCH: &str = r#"
version = 1

[[policy.tool]]
name = "fetch"
effects = ["fetch"]
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

    fn facts(row: &[u8]) -> Vec<appa_engine::fact::Fact> {
        serde_json::from_slice(row).expect("the persisted batch decodes as engine facts")
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
    fn a_non_neutral_starting_label_refuses_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let policy = r#"
version = 1
[[policy.tool]]
name = "fetch"
[policy.deployment]
starting_label = { trust = "suspicious" }
"#;
        assert!(matches!(
            Runtime::open(config_with(policy, None), dir.path().join("appa.db"), None),
            Err(OpenError::UnsupportedPolicy(_)),
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
            .on_tool_call(fetch(serde_json::json!({"b": 1, "a": 2})))
            .await
            .expect("the call is decided");
        assert_eq!(decision, ToolCallDecision::Allow);
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the released call opened a dispatch");
        assert_eq!(open.bytes, br#"{"a":2,"b":1}"#.to_vec());
        let (log, revision) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(revision, Revision(2));
        let facts: Vec<appa_engine::fact::Fact> =
            serde_json::from_slice(&log[1]).expect("the persisted batch decodes as engine facts");
        assert!(matches!(
            facts.as_slice(),
            [appa_engine::fact::Fact::DispatchOpened { .. }]
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
        let decision = session.on_tool_call(duplicated).await.expect("the call is decided");
        assert!(
            matches!(decision, ToolCallDecision::Deny { .. }),
            "a duplicate key must be refused, not resolved by last-wins: {decision:?}"
        );
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
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
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
            "the admitted result closed the dispatch",
        );
    }

    #[tokio::test]
    async fn the_success_checkpoint_commits_once_across_a_lost_admission() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(EMITTING_FETCH, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let outcome = || ToolOutcome::Success {
            body: OutcomeBody::Available("data".to_string()),
        };
        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect("the call is released");

        runtime.inner.store.fail_commit_after(1);
        assert!(matches!(
            session
                .on_tool_result(fetch(serde_json::json!({"a": 1})), outcome())
                .await,
            Err(EventError::Storage(_)),
        ));
        let (log, revision) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(revision, Revision(3), "the checkpoint committed, the admission did not");
        let effects = match facts(&log[2]).as_slice() {
            [appa_engine::fact::Fact::DispatchSucceeded { effects, .. }] => effects.clone(),
            other => panic!("expected the success checkpoint alone, got {other:?}"),
        };
        assert_eq!(effects.len(), 1, "the checkpoint committed the declared effect");
        let open = runtime
            .inner
            .store
            .open_dispatch(&root())
            .expect("the dispatch query runs")
            .expect("the lost admission left the dispatch open");
        assert_eq!(open.state, DispatchState::Executing);

        let kept = session
            .on_tool_result(fetch(serde_json::json!({"a": 1})), outcome())
            .await
            .expect("the re-reported outcome admits");
        assert_eq!(kept, ToolResultDecision::Keep);
        let (log, revision) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(
            revision,
            Revision(4),
            "the retry appended the admission alone — the checkpoint did not repeat",
        );
        let closed = facts(&log[3])
            .into_iter()
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
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );

        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
            .on_tool_call(fetch(serde_json::json!({"a": "not a number"})))
            .await
            .expect("the refusal is delivered as feedback");
        assert!(matches!(decision, ToolCallDecision::Deny { .. }));
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(log.len(), 1, "an invalid call appends no fact after the opening");
        assert!(matches!(
            facts(&log[0]).as_slice(),
            [appa_engine::fact::Fact::TrajectoryOpened { .. }]
        ));
    }

    #[tokio::test]
    async fn an_unknown_tool_call_returns_deny_feedback() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let decision = session
            .on_tool_call(ProposedCall {
                tool: "wrench".to_string(),
                arguments: raw(serde_json::json!({})),
            })
            .await
            .expect("the refusal is delivered as feedback");
        assert!(matches!(decision, ToolCallDecision::Deny { .. }));
    }

    fn raw_sql(db: &std::path::Path) -> rusqlite::Connection {
        rusqlite::Connection::open(db).expect("a second connection opens")
    }

    fn latest_offer(runtime: &Runtime) -> OfferId {
        runtime
            .inner
            .store
            .surfaced_offers(&root())
            .expect("the offer query runs")
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
                .on_tool_call(fetch(serde_json::json!({"a": 1})))
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

        let mut old = runtime.session(&root()).expect("the old root reopens");
        let decision = old
            .on_tool_call(fetch(serde_json::json!({"a": 2})))
            .await
            .expect("the old root decides");
        assert_eq!(decision, ToolCallDecision::Allow, "the old root keeps fetch");

        let mut new = runtime
            .create_session(TrajectoryId("cc:new".to_string()))
            .expect("a fresh id opens");
        let denied = new
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect("the new root decides");
        assert!(
            matches!(denied, ToolCallDecision::Deny { .. }),
            "fetch is gone for new roots"
        );
        let allowed = new
            .on_tool_call(ProposedCall {
                tool: "read".to_string(),
                arguments: raw(serde_json::json!({"path": "a.txt"})),
            })
            .await
            .expect("the new root decides");
        assert_eq!(allowed, ToolCallDecision::Allow, "the edited policy's tool releases");
    }

    #[tokio::test]
    async fn a_root_without_an_opening_record_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        raw_sql(&db)
            .execute(
                "INSERT INTO trajectories (id, family, parent) VALUES ('cc:old', 'cc:old', NULL)",
                [],
            )
            .expect("the bare pre-binding row inserts");
        let mut session = runtime
            .session(&TrajectoryId("cc:old".to_string()))
            .expect("the row reopens");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
        assert!(error.is_operational());
        assert!(runtime.status(&TrajectoryId("cc:old".to_string())).is_none());
    }

    #[tokio::test]
    async fn a_missing_stored_policy_file_refuses_the_root() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        raw_sql(&db)
            .execute("DELETE FROM policies", [])
            .expect("the stored file deletes");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
        let sql = raw_sql(&db);
        let stored: Vec<u8> = sql
            .query_row("SELECT bytes FROM policies", [], |row| row.get(0))
            .expect("the stored file reads");
        let mut tampered = stored;
        tampered.extend_from_slice(b"\n# tampered\n");
        sql.execute("UPDATE policies SET bytes = ?1", rusqlite::params![tampered])
            .expect("the stored file rewrites");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        let tampered = String::from_utf8(log[0].clone())
            .expect("the opening is JSON text")
            .replace("cc:root", "cc:evil");
        raw_sql(&db)
            .execute(
                "UPDATE batches SET bytes = ?1 WHERE seq = 0",
                rusqlite::params![tampered.into_bytes()],
            )
            .expect("the tamper lands");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn a_stored_file_compiling_to_a_different_identity_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let other = b"[policy]\nversion = 1\n\n[[policy.tool]]\nname = \"other\"\n".to_vec();
        let key = crate::config::PolicyFileKey::of(&other);
        let sql = raw_sql(&db);
        sql.execute(
            "INSERT INTO policies (key, bytes) VALUES (?1, ?2)",
            rusqlite::params![key.as_str(), other],
        )
        .expect("the other file inserts");
        sql.execute("UPDATE openings SET policy_key = ?1", rusqlite::params![key.as_str()])
            .expect("the opening rebinds");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect_err("the event refuses");
        assert!(matches!(error, EventError::PolicyUnavailable(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn an_unloadable_stored_dialect_refuses_the_root() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let db = dir.path().join("appa.db");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let future = b"[policy]\nversion = 99\n".to_vec();
        let key = crate::config::PolicyFileKey::of(&future);
        let sql = raw_sql(&db);
        sql.execute(
            "INSERT INTO policies (key, bytes) VALUES (?1, ?2)",
            rusqlite::params![key.as_str(), future],
        )
        .expect("the future-dialect file inserts");
        sql.execute("UPDATE openings SET policy_key = ?1", rusqlite::params![key.as_str()])
            .expect("the opening rebinds");
        let error = session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
            session.on_tool_call(wire(500)).await.expect("the block is delivered");
        }

        {
            let runtime =
                Runtime::open(config_with(READ_ONLY, None), db.clone(), None).expect("the edited deployment opens");
            let mut session = runtime.session(&root()).expect("the old root reopens");
            session.on_tool_call(wire(500)).await.expect("the block is delivered");
            let offer = latest_offer(&runtime);
            let (log_before, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
            let got = session
                .on_remedy(offer.clone(), None)
                .await
                .expect("the no-answer is delivered");
            assert!(matches!(got, RemedyDecision::NoAnswer { .. }), "got {got:?}");
            let (log_after, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
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
        let mut session = runtime.session(&root()).expect("the old root reopens");
        session.on_tool_call(wire(500)).await.expect("the block is delivered");
        let offer = latest_offer(&runtime);
        runtime.inner.store.fail_next_commit();
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
                .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
        let mut session = runtime.session(&root()).expect("the trajectory reopens");
        let decision = session
            .on_tool_call(fetch(serde_json::json!({"a": 2})))
            .await
            .expect("the reopened trajectory decides");
        assert_eq!(decision, ToolCallDecision::Allow);
    }

    #[tokio::test]
    async fn an_undecodable_batch_row_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .store
            .commit_event(
                &root(),
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"not engine facts".to_vec(),
                        based_on: Revision(1),
                    }),
                    records: Vec::new(),
                },
            )
            .expect("the store appends opaque bytes");
        assert!(matches!(
            session.on_tool_call(fetch(serde_json::json!({"a": 1}))).await,
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
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect("the call is decided");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        let tampered = String::from_utf8(log[1].clone())
            .expect("the batch is JSON text")
            .replace("\"fetch\"", "\"wrench\"");
        let db = dir.path().join("appa.db");
        let conn = rusqlite::Connection::open(&db).expect("the test reopens the database");
        conn.execute(
            "UPDATE batches SET bytes = ?1 WHERE seq = 1",
            rusqlite::params![tampered.into_bytes()],
        )
        .expect("the tamper lands");
        drop(conn);
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
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
            .on_tool_call(ProposedCall {
                tool: "send".to_string(),
                arguments: raw(serde_json::json!({})),
            })
            .await
            .expect("the block is delivered");
        let ToolCallDecision::Deny { feedback } = decision else {
            panic!("a consumed Unknown dimension must block the sink");
        };
        assert!(
            feedback.contains("the result of fetch (ValueId(0))"),
            "the block must name the producing tool: {feedback}"
        );
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_subject_a_concurrent_event_moved_reads_as_lifecycle_not_a_fault() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let refuse = |trajectory: &TrajectoryId, event: crate::engine::EngineEvent| {
            let policy = runtime.inner.resolve_policy(&root()).expect("the policy resolves");
            let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
            let view = runtime
                .inner
                .engine
                .rebuild_view(&policy, &log, &root(), trajectory)
                .expect("the log rebuilds");
            let refusal = runtime
                .inner
                .engine
                .handle(&policy, &view, event)
                .expect_err("the moved subject refuses the event");
            EventError::from(refusal)
        };

        let child = TrajectoryId("cc:root:child".to_string());
        session.on_child_start(child.clone()).expect("the child opens");
        let error = refuse(&root(), crate::engine::EngineEvent::ChildStart { child: child.clone() });
        assert!(
            matches!(error, EventError::TrajectoryExists),
            "a lost opening race answers as the store-level race does; got {error:?}",
        );
        assert!(!error.is_operational());

        session
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
            .await
            .expect("the call is released");
        session
            .on_tool_result(
                fetch(serde_json::json!({"a": 1})),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("data".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        let error = refuse(
            &root(),
            crate::engine::EngineEvent::SuccessObserved {
                call: fetch(serde_json::json!({"a": 1})),
                observed: ObservedResult::Unavailable,
            },
        );
        assert!(
            matches!(error, EventError::UnknownDispatch),
            "a duplicate report answers as a later one would; got {error:?}",
        );
        assert!(!error.is_operational());

        let mut child_session = runtime.session(&child).expect("the child reopens");
        child_session
            .on_child_end(Some("done".to_string()))
            .await
            .expect("the child returns");
        let error = refuse(
            &child,
            crate::engine::EngineEvent::ChildStart {
                child: TrajectoryId("cc:root:child:grandchild".to_string()),
            },
        );
        assert!(
            matches!(error, EventError::TrajectoryEnded),
            "an ended parent is a lifecycle condition; got {error:?}",
        );
        assert!(!error.is_operational());

        let error = refuse(
            &child,
            crate::engine::EngineEvent::ModelResponse {
                call: fetch(serde_json::json!({"a": 2})),
                evidence: Vec::new(),
                entropy: fresh_entropy(),
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
                    parent: root(),
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
    async fn a_child_is_seeded_and_a_clean_return_crosses() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child seeds and opens");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the family log loads");
        let facts: Vec<appa_engine::fact::Fact> = serde_json::from_slice(&log[1]).expect("the seed batch decodes");
        assert!(matches!(facts.as_slice(), [appa_engine::fact::Fact::Boundary { .. }]));

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
            runtime.session(&TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));
    }

    #[tokio::test]
    async fn a_child_return_with_unknown_fold_crosses_and_charges_the_parent() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child seeds and opens");
        child
            .on_tool_call(fetch(serde_json::json!({"a": 1})))
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
            runtime.session(&TrajectoryId("cc:child".to_string())),
            Err(SessionError::Ended),
        ));

        let decision = session
            .on_tool_call(ProposedCall {
                tool: "send".to_string(),
                arguments: raw(serde_json::json!({})),
            })
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
        surfaced_offer_for(runtime, &root())
    }

    fn surfaced_offer_for(runtime: &Runtime, trajectory: &TrajectoryId) -> OfferId {
        runtime
            .inner
            .store
            .surfaced_offers(trajectory)
            .expect("the offer query runs")
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

        let denied = session.on_tool_call(wire(500)).await.expect("the block is delivered");
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

        let resumed = session.on_tool_call(wire(500)).await.expect("the re-proposal resumes");
        assert_eq!(resumed, ToolCallDecision::Allow);
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
            Err(EventError::UnknownOffer),
        ));
    }

    #[tokio::test]
    async fn a_denial_retires_plans_naming_the_denier_and_sticks() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "deny", "reason": "no"})).await;
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        session.on_tool_call(wire(500)).await.expect("the block is delivered");
        let offer = surfaced_offer(&runtime);
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the denial is delivered"),
            RemedyDecision::Declined { .. },
        ));
        let before = runtime
            .inner
            .store
            .surfaced_offers(&root())
            .expect("the offer query runs")
            .len();
        assert!(matches!(
            session
                .on_tool_call(wire(500))
                .await
                .expect("the re-block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let after = runtime
            .inner
            .store
            .surfaced_offers(&root())
            .expect("the offer query runs")
            .len();
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
            .on_tool_call(wire(500))
            .await
            .expect("the first block is delivered");
        second
            .on_tool_call(wire(500))
            .await
            .expect("the second block is delivered");
        let first_offer = surfaced_offer_for(&runtime, &first_id);
        let second_offer = surfaced_offer_for(&runtime, &second_id);

        assert!(matches!(
            first
                .on_remedy(first_offer, None)
                .await
                .expect("the first denial is delivered"),
            RemedyDecision::Declined { .. },
        ));
        assert_eq!(
            runtime
                .inner
                .store
                .surfaced_offers(&second_id)
                .expect("the second offer query runs"),
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

        session.on_tool_call(wire(500)).await.expect("the block is delivered");
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

        session.on_tool_call(wire(500)).await.expect("the block is delivered");
        let offer = surfaced_offer(&runtime);
        runtime.inner.store.fail_next_commit();
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
    async fn an_offer_does_not_survive_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!({"ruling": "approve"})).await;
        let db = dir.path().join("appa.db");
        let offer = {
            let runtime =
                Runtime::open(config_with(ATTENTION, Some(&url)), db.clone(), None).expect("the deployment opens");
            let mut session = runtime.create_session(root()).expect("a fresh id opens");
            session.on_tool_call(wire(500)).await.expect("the block is delivered");
            surfaced_offer(&runtime)
        };
        let runtime = Runtime::open(config_with(ATTENTION, Some(&url)), db, None).expect("the deployment reopens");
        let mut session = runtime.session(&root()).expect("the trajectory reopens");
        assert!(matches!(
            session.on_remedy(offer, None).await.expect("the decline is delivered"),
            RemedyDecision::Declined { .. },
        ));
    }

    const SANITIZED_CHILD: &str = r#"
version = 1

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
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child seeds and opens");
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
        let child_id = TrajectoryId("cc:child".to_string());
        let mut child = session.on_child_start(child_id.clone()).expect("the child opens");
        child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the sanitized return crosses");

        let policy = runtime.inner.resolve_policy(&root()).expect("the policy resolves");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        let view = runtime
            .inner
            .engine
            .rebuild_view(&policy, &log, &root(), &root())
            .expect("the log rebuilds");
        let refusal = runtime
            .inner
            .engine
            .handle(
                &policy,
                &view,
                crate::engine::EngineEvent::ChildReturn {
                    parent: root(),
                    child: child_id,
                    value: Some("raw with pii".to_string()),
                    evidence: vec![crate::engine::ExternalEvidence::Sanitizer {
                        sanitizer: "scrub".to_string(),
                        derived: Some("scrubbed".to_string()),
                    }],
                    entropy: fresh_entropy(),
                },
            )
            .expect_err("the duplicate return is refused");
        let error = EventError::from(refusal);
        assert!(matches!(error, EventError::TrajectoryEnded), "got {error:?}",);
        assert!(!error.is_operational());
    }

    #[tokio::test]
    async fn sanitizer_no_answer_fails_closed() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!(42)).await;
        let runtime = Runtime::open(sanitized_config(Some(&url)), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child seeds and opens");
        let blocked = child
            .on_child_end(Some("raw with pii".to_string()))
            .await
            .expect("the withheld return is delivered");
        let crate::api::ChildReturnDecision::Blocked { .. } = blocked else {
            panic!("a no-answer sanitizer must withhold the crossing");
        };
        assert!(
            runtime.session(&TrajectoryId("cc:child".to_string())).is_ok(),
            "the child stays unended and the return may be retried",
        );
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
    fn a_child_bound_attest_schema_refuses_open_in_this_runtime() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        assert!(matches!(
            Runtime::open(
                attested_config(ATTEST_BOUND_CHILD, None),
                dir.path().join("appa.db"),
                None
            ),
            Err(OpenError::UnsupportedPolicy(_)),
        ));
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

    fn leak() -> ProposedCall {
        ProposedCall {
            tool: "leak".to_string(),
            arguments: raw(serde_json::json!({"q": "all"})),
        }
    }

    async fn run_sanitize_offer(runtime: &Runtime, session: &mut crate::api::Session) -> ToolResultDecision {
        let offers = runtime
            .inner
            .store
            .surfaced_offers(&root())
            .expect("the offer query runs");
        let offer = offers.last().expect("the block surfaced offers").clone();
        let authorized = session.on_remedy(offer, None).await.expect("the offer executes");
        assert!(matches!(authorized, RemedyDecision::Authorized { .. }));
        assert_eq!(
            session.on_tool_call(leak()).await.expect("the re-proposal resumes"),
            ToolCallDecision::Allow,
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
            session.on_tool_call(leak()).await.expect("the block is delivered"),
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
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_bound_sanitizer_no_answer_closes_valueless() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let url = stub(serde_json::json!(42)).await;
        let runtime =
            Runtime::open(narrowing_config(&url), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        assert!(matches!(
            session.on_tool_call(leak()).await.expect("the block is delivered"),
            ToolCallDecision::Deny { .. },
        ));
        let decision = run_sanitize_offer(&runtime, &mut session).await;
        let ToolResultDecision::Replace { placeholder } = decision else {
            panic!("the raw must be withheld");
        };
        assert!(!placeholder.contains("pii"), "the raw body never reaches the model");
        assert!(
            runtime
                .inner
                .store
                .open_dispatch(&root())
                .expect("the dispatch query runs")
                .is_none(),
            "the valueless success closed the dispatch",
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
        let decision = session.on_tool_call(call.clone()).await.expect("the call is decided");
        if matches!(decision, ToolCallDecision::Deny { .. }) {
            let offers = runtime
                .inner
                .store
                .surfaced_offers(session.trajectory())
                .expect("the offer query runs");
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
                    .on_tool_call(call.clone())
                    .await
                    .expect("the re-proposal resumes"),
                ToolCallDecision::Allow,
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

        let (_, before) = runtime.inner.store.load_log(&root()).expect("the log loads");
        runtime.status(&root()).expect("the root answers");
        runtime.status(&root()).expect("the root answers again");
        let (_, after) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(before, after, "a status read appends nothing");

        runtime
            .inner
            .store
            .commit_event(
                &root(),
                EventWrite {
                    batch: None,
                    records: vec![crate::store::RuntimeRecord::End { id: root() }],
                },
            )
            .expect("the end record commits");
        assert!(matches!(runtime.session(&root()), Err(SessionError::Ended)));
        assert_eq!(
            runtime
                .status(&root())
                .expect("an ended trajectory still answers")
                .trust,
            "suspicious",
        );
    }

    #[tokio::test]
    async fn an_untrusted_log_answers_no_status() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        runtime.create_session(root()).expect("a fresh id opens");
        runtime
            .inner
            .store
            .commit_event(
                &root(),
                EventWrite {
                    batch: Some(BatchAppend {
                        bytes: b"not engine facts".to_vec(),
                        based_on: Revision(1),
                    }),
                    records: Vec::new(),
                },
            )
            .expect("the store appends opaque bytes");
        assert!(runtime.status(&root()).is_none());
    }

    #[tokio::test]
    async fn a_childs_fold_stays_out_of_the_root_status() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_with(MARKED, None), dir.path().join("appa.db"), None).expect("the deployment opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let child_id = TrajectoryId("cc:child".to_string());
        let mut child = session.on_child_start(child_id.clone()).expect("the child opens");
        admit_success(&runtime, &mut child, mark()).await;

        let policy = runtime.inner.resolve_policy(&root()).expect("the policy resolves");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the family log loads");
        let view = runtime
            .inner
            .engine
            .rebuild_view(&policy, &log, &root(), &child_id)
            .expect("the family log replays");
        let child_status = runtime
            .inner
            .engine
            .trajectory_status(&policy, &view)
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
}
