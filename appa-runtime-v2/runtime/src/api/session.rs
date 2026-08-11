//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::external::{ConsultKind, ConsultOutcome, DynamicResolution};
use crate::mock_engine::{
    EngineDecision, EngineEvent, ExternalEvidence, ExternalRequest, Feedback, Next, OfferNonce, Presentation,
};
use crate::store::{BatchAppend, DispatchState, EventWrite, Revision, RuntimeRecord};

use super::{
    AuthorizedCall, ChildReturnDecision, EventError, Inner, OfferId, OutcomeBody, ProposedCall, RemedyDecision,
    ToolCallDecision, ToolOutcome, ToolResultDecision, TrajectoryId,
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

    /// The user submitted a prompt. Records it as this turn's request
    /// and reports it to the engine as a `PrincipalRequest`, which
    /// expires the previous turn's pending offers.
    pub fn on_prompt(&mut self, text: String) -> Result<(), EventError> {
        self.refuse_if_ended()?;
        let trajectory = self.trajectory.clone();
        let decision = self.drive(
            || EngineEvent::PrincipalRequest,
            |_| {
                vec![RuntimeRecord::Request {
                    trajectory: trajectory.clone(),
                    text: text.clone(),
                }]
            },
        )?;
        tracing::debug!(trajectory = %self.trajectory.0, "prompt recorded; prior offers expire");
        match decision.then {
            Next::Done => Ok(()),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    pub async fn on_tool_call(&mut self, call: ProposedCall) -> Result<ToolCallDecision, EventError> {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %self.trajectory.0, "control tool passes unchecked");
            return Ok(ToolCallDecision::Control);
        }
        self.refuse_if_ended()?;
        if let Some(open) = self
            .inner
            .store
            .open_dispatch(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?
        {
            return match open.state {
                DispatchState::Executing => Err(EventError::CallOutstanding),
                DispatchState::Awaiting => {
                    let proposed = serde_json::to_vec(&call).map_err(|error| EventError::Storage(error.to_string()))?;
                    if call.tool == open.tool && proposed == open.bytes {
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
        let open = self
            .inner
            .store
            .open_dispatch(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        let Some(open) = open else {
            return Err(EventError::UnknownDispatch);
        };
        let reported = serde_json::to_vec(&call).map_err(|error| EventError::Storage(error.to_string()))?;
        if call.tool != open.tool || reported != open.bytes {
            return Err(EventError::OutcomeMismatch);
        }
        if open.state != DispatchState::Executing {
            return Err(EventError::UnknownDispatch);
        }
        let d = open.id;
        let o = self.cap_outcome(o);

        let trajectory = self.trajectory.clone();
        let dispatch = d.clone();
        let decision = self
            .drive_with_evidence(
                |evidence| EngineEvent::ToolOutcome {
                    dispatch: d.clone(),
                    outcome: o.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                },
                move |decision| {
                    match &decision.then {
                        Next::PresentToModel(Presentation::KeepOutput)
                        | Next::PresentToModel(Presentation::Value { .. }) => {
                            vec![RuntimeRecord::CloseDispatch { id: dispatch.clone() }]
                        }
                        Next::PresentToModel(Presentation::ReplaceOutput { offers, .. }) => {
                            let mut records = vec![RuntimeRecord::CloseDispatch { id: dispatch.clone() }];
                            records.extend(offer_records(&trajectory, offers));
                            records
                        }
                        _ => Vec::new(),
                    }
                },
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
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The model called the `execute_remedy_plan` MCP tool. Executes
    /// one offer by its id; the id is unguessable,
    /// so naming it proves the model read the offer. An id
    /// this runtime never surfaced for this trajectory is refused.
    pub async fn on_remedy(&mut self, offer: OfferId) -> Result<RemedyDecision, EventError> {
        self.refuse_if_ended()?;
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
                |evidence| EngineEvent::ExecuteOffer {
                    offer: offer.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                },
                move |decision| match &decision.then {
                    Next::InvokeTool(released) => vec![RuntimeRecord::OpenDispatch {
                        id: released.dispatch.clone(),
                        trajectory: trajectory.clone(),
                        tool: released.tool.clone(),
                        bytes: released.bytes.clone(),
                        state: DispatchState::Awaiting,
                    }],
                    Next::PresentToModel(Presentation::Staged { offers, .. }) => offer_records(&trajectory, offers),
                    _ => Vec::new(),
                },
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
            Next::PresentToModel(Presentation::Staged { feedback, .. }) => Ok(RemedyDecision::Staged { feedback }),
            Next::PresentToModel(Presentation::Declined { feedback }) => Ok(RemedyDecision::Declined { feedback }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// A child agent started. Opens the child's trajectory in the one
    /// family log — a child never starts cleaner than its
    /// parent, which the engine derives from that shared
    /// log. No engine event: the spawn call itself was checked as a
    /// released dispatch, and that dispatch remains the record of what
    /// the child was asked — the runtime stores no separate task text.
    pub fn on_child_start(&mut self, id: TrajectoryId) -> Result<Session, EventError> {
        self.refuse_if_ended()?;
        let existing = self
            .inner
            .store
            .trajectory(&id)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        if existing.is_some() {
            return Err(EventError::TrajectoryExists);
        }
        self.commit_records(vec![RuntimeRecord::OpenChild {
            id: id.clone(),
            parent: self.trajectory.clone(),
        }])?;
        Ok(Session::attach(Arc::clone(&self.inner), id, self.family.clone()))
    }

    /// The child finished. Its final message is its only return
    /// channel and is checked before it may cross to the parent;
    /// `None` returns no value. The child ends
    /// in the same transaction as its return's facts.
    pub async fn on_child_end(&mut self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        self.refuse_if_ended()?;
        let owner = self
            .inner
            .store
            .trajectory(&self.trajectory)
            .map_err(|error| EventError::Storage(error.to_string()))?
            .and_then(|row| row.parent)
            .ok_or(EventError::NotAChild)?;
        let trajectory = self.trajectory.clone();
        let child = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |evidence| EngineEvent::ChildReturn {
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
                        Next::PresentToModel(Presentation::Blocked { offers, .. }) => {
                            let mut records = vec![RuntimeRecord::End { id: trajectory.clone() }];
                            records.extend(offer_records(&owner, offers));
                            records
                        }
                        _ => Vec::new(),
                    }
                },
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
        mut event: impl FnMut(Vec<ExternalEvidence>) -> EngineEvent,
        records: impl Fn(&EngineDecision) -> Vec<RuntimeRecord>,
    ) -> Result<EngineDecision, EventError> {
        let mut evidence: Vec<ExternalEvidence> = Vec::new();
        for _ in 0..EVIDENCE_LIMIT {
            let carried = evidence.clone();
            let decision = self.drive(|| event(carried.clone()), &records)?;
            match decision.then {
                Next::ResolveExternal(requests) => {
                    for request in requests {
                        evidence.push(self.consult(request).await);
                    }
                }
                _ => return Ok(decision),
            }
        }
        Err(EventError::UnexpectedDecision)
    }

    fn drive(
        &self,
        mut event: impl FnMut() -> EngineEvent,
        records: impl Fn(&EngineDecision) -> Vec<RuntimeRecord>,
    ) -> Result<EngineDecision, EventError> {
        for attempt in 1..=REPLAY_LIMIT {
            let (log, _revision) = self
                .inner
                .store
                .load_log(&self.family)
                .map_err(|error| EventError::Storage(error.to_string()))?;
            let view = self.inner.engine.rebuild_view(&log);
            let decision = self.inner.engine.handle(&view, event());

            let event_records = if matches!(decision.then, Next::ResolveExternal(_)) {
                Vec::new()
            } else {
                records(&decision)
            };
            let batch = decision.append.as_ref().map(|batch| BatchAppend {
                bytes: batch.bytes.clone(),
                based_on: Revision(batch.based_on.0),
            });
            if batch.is_none() {
                if event_records.is_empty() {
                    return Ok(decision);
                }
                return match self.inner.store.commit_event(
                    &self.family,
                    EventWrite {
                        batch: None,
                        records: event_records,
                    },
                ) {
                    Ok(_) => Ok(decision),
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
                Ok(_) => return Ok(decision),
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

    async fn consult(&self, request: ExternalRequest) -> ExternalEvidence {
        let outcome = match &request {
            ExternalRequest::Authority { name, question } => {
                self.inner
                    .externals
                    .consult(ConsultKind::Authority, name, question)
                    .await
            }
            ExternalRequest::Sanitizer { name, input } => {
                self.inner.externals.consult(ConsultKind::Sanitizer, name, input).await
            }
            ExternalRequest::Cast { name, input } => self.inner.externals.consult(ConsultKind::Cast, name, input).await,
            ExternalRequest::Membership { name, question } => {
                self.inner
                    .externals
                    .consult(ConsultKind::Membership, name, question)
                    .await
            }
            ExternalRequest::Dynamic { name, question } => {
                return self.resolve_dynamic(request.clone(), name, question).await;
            }
        };
        match outcome {
            ConsultOutcome::Answer(body) => ExternalEvidence::Answer { request, body },
            ConsultOutcome::NoAnswer(_) => ExternalEvidence::NoAnswer { request },
        }
    }

    async fn resolve_dynamic(
        &self,
        request: ExternalRequest,
        name: &str,
        question: &serde_json::Value,
    ) -> ExternalEvidence {
        let field = |key: &str| question.get(key).and_then(|value| value.as_str());
        let (Some(tool), Some(argument), Some(value)) = (field("tool"), field("argument"), field("value")) else {
            return ExternalEvidence::NoAnswer { request };
        };
        match self.inner.externals.resolve_dynamic(name, tool, argument, value).await {
            DynamicResolution::Resolved { readers } => ExternalEvidence::Answer {
                request,
                body: serde_json::json!({ "readers": readers }),
            },
            DynamicResolution::Unresolved(_) => ExternalEvidence::NoAnswer { request },
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

#[cfg(test)]
mod tests {
    use super::super::{DispatchId, OpenError, OutcomeBody, Runtime, SessionError};
    use super::*;
    use crate::config::Config;
    use crate::mock_engine::{
        EngineDecision, Feedback, LogRevision, MockEngine, Next, Presentation, ReleasedCall, ValidatedFactBatch,
    };
    use crate::store::DispatchRow;

    fn config() -> Config {
        let text = r#"
            [policy]
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
        Runtime::open_with_engine(config(), dir.path().join("appa.db"), MockEngine::test_mode())
            .expect("a fresh runtime opens")
    }

    fn root() -> TrajectoryId {
        TrajectoryId("cc:root".to_string())
    }

    fn done_with_batch(bytes: &[u8], based_on: u64) -> EngineDecision {
        EngineDecision {
            append: Some(ValidatedFactBatch {
                bytes: bytes.to_vec(),
                based_on: LogRevision(based_on),
            }),
            then: Next::Done,
        }
    }

    fn call() -> ProposedCall {
        ProposedCall {
            tool: "Bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
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
        EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: vec![released(id, call)],
                feedback: Vec::new(),
            },
        }
    }

    fn present(p: Presentation) -> EngineDecision {
        EngineDecision {
            append: None,
            then: Next::PresentToModel(p),
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
        match Runtime::open(config(), db) {
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
    fn on_prompt_commits_the_batch_and_the_request_record_before_returning() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        runtime.inner.engine.enqueue(done_with_batch(b"prompt-facts", 0));
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session
            .on_prompt("read the report".to_string())
            .expect("the prompt is accepted");

        let (log, revision) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(log, vec![b"prompt-facts".to_vec()]);
        assert_eq!(revision, Revision(1));
    }

    #[tokio::test]
    async fn a_decision_whose_commit_fails_never_acts() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(EngineDecision {
            append: Some(ValidatedFactBatch {
                bytes: b"facts".to_vec(),
                based_on: LogRevision(0),
            }),
            then: Next::ModelResponse {
                invocations: vec![released("d1", &call())],
                feedback: Vec::new(),
            },
        });
        runtime.inner.store.fail_next_commit();
        assert!(matches!(
            session.on_tool_call(call()).await,
            Err(EventError::Storage(_)),
        ));
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert!(log.is_empty());
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
        runtime.inner.engine.enqueue(EngineDecision {
            append: Some(ValidatedFactBatch {
                bytes: b"stale".to_vec(),
                based_on: LogRevision(1),
            }),
            then: Next::ModelResponse {
                invocations: vec![released("d-stale", &call())],
                feedback: Vec::new(),
            },
        });
        runtime.inner.engine.enqueue(EngineDecision {
            append: Some(ValidatedFactBatch {
                bytes: b"fresh".to_vec(),
                based_on: LogRevision(0),
            }),
            then: Next::ModelResponse {
                invocations: vec![released("d-fresh", &call())],
                feedback: Vec::new(),
            },
        });
        let decision = session.on_tool_call(call()).await.expect("the replayed event commits");
        assert_eq!(decision, ToolCallDecision::Allow);
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the log loads");
        assert_eq!(log, vec![b"fresh".to_vec()]);
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
                crate::mock_engine::EngineEvent::ModelResponse { entropy, .. } => Some(entropy.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 2);
        assert_ne!(entropies[0], entropies[1]);
    }

    #[tokio::test]
    async fn a_second_call_while_one_executes_is_refused_without_an_engine_call() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        let decision = session.on_tool_call(call()).await.expect("the call is allowed");
        assert_eq!(decision, ToolCallDecision::Allow);

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
            arguments: serde_json::json!({"offer_id": "x"}),
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
            let decision = session
                .on_tool_call(control_call(name))
                .await
                .expect("the control tool passes");
            assert_eq!(decision, ToolCallDecision::Control, "{name} is the control tool");
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
        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: "blocked: not the runtime's tool".to_string(),
                    offers: Vec::new(),
                }],
            },
        });
        let decision = session
            .on_tool_call(control_call("mcp__evil__execute_remedy_plan"))
            .await
            .expect("the deny is delivered");
        assert!(matches!(decision, ToolCallDecision::Deny { .. }));
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
        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: "blocked: the recipient cannot read this".to_string(),
                    offers: vec![OfferId("offer-1".to_string())],
                }],
            },
        });
        let decision = session.on_tool_call(call()).await.expect("the deny is delivered");
        assert_eq!(
            decision,
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

        runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
        let decision = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("output".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(decision, ToolResultDecision::Keep);
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
    async fn a_replaced_result_delivers_the_placeholder_and_records_its_offers() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        runtime.inner.engine.enqueue(present(Presentation::ReplaceOutput {
            placeholder: "the output is confined; remedies are offered".to_string(),
            offers: vec![OfferId("offer-2".to_string())],
        }));
        let decision = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("secret".to_string()),
                },
            )
            .await
            .expect("the replacement is delivered");
        assert_eq!(
            decision,
            ToolResultDecision::Replace {
                placeholder: "the output is confined; remedies are offered".to_string(),
            },
        );
        assert_eq!(
            runtime
                .inner
                .store
                .offer_trajectory(&OfferId("offer-2".to_string()))
                .expect("the offer query runs"),
            Some(root()),
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
                    arguments: serde_json::json!({"command": "rm"}),
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
    async fn an_over_cap_success_body_is_carried_as_unavailable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(allow_decision("d1", &call()));
        session.on_tool_call(call()).await.expect("the call is allowed");

        runtime.inner.engine.enqueue(present(Presentation::KeepOutput));
        let decision = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("x".repeat(70000)),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(decision, ToolResultDecision::Keep);
        let seen = runtime.inner.engine.seen();
        match seen.last() {
            Some(crate::mock_engine::EngineEvent::ToolOutcome { outcome, .. }) => assert_eq!(
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
            session.on_remedy(OfferId("never-surfaced".to_string())).await,
            Err(EventError::UnknownOffer),
        ));
        assert!(runtime.inner.engine.seen().is_empty());
    }

    #[tokio::test]
    async fn an_authorized_remedy_opens_a_dispatch_the_reproposed_call_resumes() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: "blocked; execute_remedy_plan(offer-3) authorizes it".to_string(),
                    offers: vec![OfferId("offer-3".to_string())],
                }],
            },
        });
        session.on_tool_call(call()).await.expect("the deny is delivered");

        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::InvokeTool(released("d-authorized", &call())),
        });
        let decision = session
            .on_remedy(OfferId("offer-3".to_string()))
            .await
            .expect("the remedy authorizes");
        let expected_bytes = serde_json::to_vec(&call()).expect("the test call serializes");
        assert_eq!(
            decision,
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
                    arguments: serde_json::json!({"command": "rm -rf /"}),
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
        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: "blocked".to_string(),
                    offers: vec![OfferId("offer-4".to_string())],
                }],
            },
        });
        session.on_tool_call(call()).await.expect("the deny is delivered");

        runtime.inner.engine.enqueue(present(Presentation::Staged {
            feedback: "cleaned; the next stage offers remain".to_string(),
            offers: vec![OfferId("offer-5".to_string())],
        }));
        assert_eq!(
            session.on_remedy(OfferId("offer-4".to_string())).await.expect("staged"),
            RemedyDecision::Staged {
                feedback: "cleaned; the next stage offers remain".to_string()
            },
        );
        runtime.inner.engine.enqueue(present(Presentation::Declined {
            feedback: "the authority declined".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-5".to_string()))
                .await
                .expect("declined"),
            RemedyDecision::Declined {
                feedback: "the authority declined".to_string()
            },
        );
        runtime.inner.engine.enqueue(present(Presentation::NoAnswer {
            feedback: "no answer yet; the offer stands".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-5".to_string()))
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
                .on_remedy(OfferId("offer-5".to_string()))
                .await
                .expect("returned"),
            RemedyDecision::Returned {
                value: "the cleaned result".to_string()
            },
        );
    }

    #[tokio::test]
    async fn evidence_round_trips_replay_the_same_event_and_no_answer_grants_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ResolveExternal(vec![ExternalRequest::Authority {
                name: "security".to_string(),
                question: serde_json::json!({"call": "Bash"}),
            }]),
        });
        runtime.inner.engine.enqueue(EngineDecision {
            append: None,
            then: Next::ModelResponse {
                invocations: Vec::new(),
                feedback: vec![Feedback {
                    text: "no answer is no permission".to_string(),
                    offers: vec![],
                }],
            },
        });
        let decision = session.on_tool_call(call()).await.expect("the deny is delivered");
        assert_eq!(
            decision,
            ToolCallDecision::Deny {
                feedback: "no answer is no permission".to_string()
            },
        );

        let seen = runtime.inner.engine.seen();
        assert_eq!(seen.len(), 2);
        match (&seen[0], &seen[1]) {
            (
                crate::mock_engine::EngineEvent::ModelResponse { evidence: first, .. },
                crate::mock_engine::EngineEvent::ModelResponse { evidence: second, .. },
            ) => {
                assert!(first.is_empty());
                assert_eq!(second.len(), 1);
                assert!(matches!(second[0], ExternalEvidence::NoAnswer { .. }));
            }
            other => panic!("expected two ModelResponse events, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_child_opens_in_the_family_returns_its_value_and_ends() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        let mut child = session
            .on_child_start(TrajectoryId("cc:child".to_string()))
            .expect("the child opens");

        assert!(matches!(
            session.on_child_start(TrajectoryId("cc:child".to_string())),
            Err(EventError::TrajectoryExists),
        ));

        runtime.inner.engine.enqueue(done_with_batch(b"child-facts", 0));
        child
            .on_prompt("work".to_string())
            .expect("the child prompt is accepted");
        let (log, _) = runtime.inner.store.load_log(&root()).expect("the family log loads");
        assert_eq!(log, vec![b"child-facts".to_vec()]);

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
    async fn a_blocked_child_return_carries_feedback_and_records_its_offers() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
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

        runtime.inner.engine.enqueue(present(Presentation::NoValue));
        let none = session
            .on_child_start(TrajectoryId("cc:child-2".to_string()))
            .expect("a second child opens")
            .on_child_end(None)
            .await
            .expect("the void return is delivered");
        assert_eq!(none, ChildReturnDecision::NoValue);
    }

    #[tokio::test]
    async fn the_parent_executes_a_blocked_returns_offer() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_test_runtime(&dir);
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
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

        runtime.inner.engine.enqueue(present(Presentation::Value {
            value: "the cleaned return".to_string(),
        }));
        assert_eq!(
            session
                .on_remedy(OfferId("offer-7".to_string()))
                .await
                .expect("the remedy runs"),
            RemedyDecision::Returned {
                value: "the cleaned return".to_string()
            },
        );
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
    async fn the_offer_mode_engine_drives_the_full_remedy_loop() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open_with_engine(config(), dir.path().join("appa.db"), MockEngine::offer_mode())
            .expect("a fresh runtime opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");
        session.on_prompt("run ls".to_string()).expect("the prompt is accepted");

        let denied = session.on_tool_call(call()).await.expect("the deny is delivered");
        let ToolCallDecision::Deny { feedback } = denied else {
            panic!("offer mode must block the first proposal");
        };
        let offer = feedback
            .split('"')
            .find(|part| part.starts_with("offer-"))
            .expect("the feedback names the offer id")
            .to_string();
        assert_eq!(
            runtime
                .inner
                .store
                .offer_trajectory(&OfferId(offer.clone()))
                .expect("the offer query runs"),
            Some(root()),
            "the surfaced offer routes to this trajectory",
        );

        let authorized = session.on_remedy(OfferId(offer)).await.expect("the remedy authorizes");
        let RemedyDecision::Authorized { call: authorized_call } = authorized else {
            panic!("executing the offer must authorize the call");
        };
        assert_eq!(
            authorized_call.bytes,
            serde_json::to_vec(&call()).expect("the test call serializes"),
            "the authorized call is byte-exact",
        );

        let resumed = session.on_tool_call(call()).await.expect("the re-proposal resumes");
        assert_eq!(resumed, ToolCallDecision::Allow);

        let kept = session
            .on_tool_result(
                call(),
                ToolOutcome::Success {
                    body: OutcomeBody::Available("ok".to_string()),
                },
            )
            .await
            .expect("the result is admitted");
        assert_eq!(kept, ToolResultDecision::Keep);
    }

    /// An offer from the prior turn lapses at the next prompt: the
    /// session delivers the mock's decline instead of an authorization.
    #[tokio::test]
    async fn an_offer_mode_offer_lapses_across_a_prompt() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open_with_engine(config(), dir.path().join("appa.db"), MockEngine::offer_mode())
            .expect("a fresh runtime opens");
        let mut session = runtime.create_session(root()).expect("a fresh id opens");

        let denied = session.on_tool_call(call()).await.expect("the deny is delivered");
        let ToolCallDecision::Deny { feedback } = denied else {
            panic!("offer mode must block the first proposal");
        };
        let offer = feedback
            .split('"')
            .find(|part| part.starts_with("offer-"))
            .expect("the feedback names the offer id")
            .to_string();

        session
            .on_prompt("next turn".to_string())
            .expect("the prompt is accepted");
        assert!(matches!(
            session
                .on_remedy(OfferId(offer))
                .await
                .expect("the decline is delivered"),
            RemedyDecision::Declined { .. },
        ));
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
                crate::mock_engine::EngineEvent::ModelResponse { entropy, .. }
                | crate::mock_engine::EngineEvent::ExecuteOffer { entropy, .. }
                | crate::mock_engine::EngineEvent::ToolOutcome { entropy, .. }
                | crate::mock_engine::EngineEvent::ChildReturn { entropy, .. } => Some(entropy.0),
                _ => None,
            })
            .collect();
        assert_eq!(entropies.len(), 10);
        entropies.sort();
        entropies.dedup();
        assert_eq!(entropies.len(), 10, "a random number repeated within the session");
    }
}
