use super::*;

pub(super) fn is_open_call(
    call: &ProposedCall,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: &OpenDispatch,
) -> bool {
    call.tool == open.tool && canonical().as_deref() == Some(open.bytes.as_slice())
}

/// Which dispatch a reported outcome belongs to, or why none can take
/// it. Total over everything the log shows: the reported call in the
/// engine's canonical domain and the dispatches this
/// trajectory has open. `canonical` is deferred because a report with
/// no open dispatch — the duplicate every crash recovery produces —
/// settles without canonicalizing anything.
///
/// A host call id selects one occurrence among parallel dispatches. A
/// legacy event without an id can report only when exactly one dispatch
/// is open; a byte match among several occurrences would be a guess.
pub(super) fn classify_report_identified<'a>(
    call: &ProposedCall,
    call_id: Option<&str>,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: &[OpenDispatch],
    bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
) -> Result<appa_engine::value::DispatchId, UnreportableOutcome> {
    let open = match call_id {
        Some(call_id) => {
            let Some(binding) = bindings
                .into_iter()
                .find(|binding| binding.trajectory == trajectory && binding.call_id == call_id)
            else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            let Some(open) = open.iter().find(|open| open.id == *binding.dispatch) else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            open
        }
        None => {
            let [open] = open else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            open
        }
    };
    if !is_open_call(call, canonical, open) {
        return Err(UnreportableOutcome::ByteMismatch);
    }
    Ok(open.id.clone())
}

/// Whether an open dispatch is an identified spawn: a spawn the harness named with a call id.
/// The harness reports its result under that id, so a report or a proposal that carries none
/// never concerns it, for as long as it is open.
pub(super) fn identified_spawn<'a>(
    engine: &crate::engine::RuntimeEngine,
    view: &EngineView,
    mut bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
    open: &OpenDispatch,
) -> bool {
    bindings.any(|binding| binding.trajectory == trajectory && *binding.dispatch == open.id)
        && !matches!(
            engine.fork_status(view, &appa_engine::value::ForkId::of(&open.id)),
            ForkStatus::Unprepared
        )
}

/// Whether an open dispatch is an identified spawn whose child runs on past its parent's
/// turn: its fork is bound to a live child.
pub(super) fn child_runs_on<'a>(
    engine: &crate::engine::RuntimeEngine,
    view: &EngineView,
    bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
    open: &OpenDispatch,
) -> bool {
    identified_spawn(engine, view, bindings, trajectory, open)
        && matches!(
            engine.fork_status(view, &appa_engine::value::ForkId::of(&open.id)),
            ForkStatus::Bound(child) if engine.liveness(view, &child) == Liveness::Live
        )
}

pub(super) const REPLAY_LIMIT: u32 = 8;

/// What a proposal's append binds beside its facts: the host's call id, and the prompt a
/// fan-out spawn it releases is keyed by.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Opening<'a> {
    call_id: Option<&'a str>,
    prompt: Option<&'a PromptKey>,
}

/// The most external-resolution rounds one invocation runs before refusing operationally.
/// Gathering is designed to close at least one ask per round, so this cap never fires on a
/// healthy deployment; it bounds the blast radius of a gathering bug or a hostile external.
pub(in crate::api) const RESOLUTION_ROUNDS: u32 = 8;

/// How many publishes one event follows before it refuses. Each one means the head moved
/// while the event was decided; a run this long is a publisher looping, not a policy change.
/// Exceeding it surfaces [`EventError::PolicyMoved`], an operational refusal the harness
/// retries: the next attempt decides at the head.
pub(super) const PUBLISHES_FOLLOWED: u32 = 4;

/// What one attempt of an event may read before it decides: the log as this
/// attempt rebuilt it. A branch's parent, the dispatch it has open, the
/// trajectory an offer belongs to — all are answered from here, so a
/// replay after a lost race reads the state that actually won rather
/// than the state it first saw.
pub(crate) struct Decided<'a> {
    session: &'a Session,
    pub(super) policy: &'a crate::engine::PolicyEngine<'a>,
    pub(super) view: &'a EngineView,
    pub(super) log: &'a appa_eventlog::Log,
}

impl Decided<'_> {
    pub(super) fn engine(&self) -> &crate::engine::RuntimeEngine {
        self.policy.engine()
    }

    pub(super) fn open_dispatches(&self) -> Vec<OpenDispatch> {
        self.engine().open_dispatches(self.view, &self.session.trajectory)
    }

    pub(super) fn identified_spawn(&self, open: &OpenDispatch) -> bool {
        identified_spawn(
            self.engine(),
            self.view,
            self.log.call_bindings(),
            &self.session.trajectory,
            open,
        )
    }

    pub(super) fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        self.engine().canonical_bytes(call)
    }

    pub(super) fn classify_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        // A report without a call id never names an identified spawn: its result comes under
        // its own call id.
        let named: Vec<OpenDispatch> = match call_id {
            Some(_) => open.clone(),
            None => open
                .iter()
                .filter(|open| !self.identified_spawn(open))
                .cloned()
                .collect(),
        };
        self.classify_among(call, call_id, &named, &open)
    }

    /// The spawn a resume names. A resume carries no call id, so the child it names picks
    /// the spawn whose fork is bound to it, when the call matches that spawn byte for byte;
    /// with no such spawn open, the resume is classified as any spawn's report.
    pub(super) fn classify_resume(
        &self,
        call: &ProposedCall,
        child: &TrajectoryId,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        let bound: Vec<OpenDispatch> = open
            .iter()
            .filter(|open| {
                matches!(
                    self.fork_status(&appa_engine::value::ForkId::of(&open.id)),
                    ForkStatus::Bound(bound) if bound == *child
                )
            })
            .cloned()
            .collect();
        match bound.as_slice() {
            [spawn] if is_open_call(call, || self.canonical_bytes(call), spawn) => Ok(spawn.id.clone()),
            [_] => Err(self
                .session
                .refuse_report(UnreportableOutcome::ByteMismatch, call, &open)),
            _ => self.classify_spawn_report(call, None),
        }
    }

    /// A spawn's own report: its result, or a resume that names it by the call alone. Every
    /// open dispatch can be the one it names.
    pub(super) fn classify_spawn_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        self.classify_among(call, call_id, &open, &open)
    }

    pub(super) fn classify_among(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        named: &[OpenDispatch],
        open: &[OpenDispatch],
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        match classify_report_identified(
            call,
            call_id,
            || self.canonical_bytes(call),
            named,
            self.log.call_bindings(),
            &self.session.trajectory,
        ) {
            Ok(dispatch) => Ok(dispatch),
            // A bound id whose dispatch left the open set is the duplicate every crash
            // recovery produces: the report belongs to the closed occurrence, and the
            // engine re-answers it from the record. Only a byte-exact repeat is one.
            Err(UnreportableOutcome::NoOpenDispatch) => match self.classify_closed_report(call, call_id) {
                Some(dispatch) => Ok(dispatch),
                None => Err(self
                    .session
                    .refuse_report(UnreportableOutcome::NoOpenDispatch, call, open)),
            },
            Err(case) => Err(self.session.refuse_report(case, call, open)),
        }
    }

    /// The closed dispatch a repeated report belongs to: the one the host's call id is bound
    /// to, when the report carries that call byte for byte. Anything else — an unknown id, an
    /// id bound to different bytes — is no replay and classifies nothing.
    pub(super) fn classify_closed_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Option<appa_engine::value::DispatchId> {
        let call_id = call_id?;
        let binding = self
            .log
            .call_bindings()
            .find(|binding| *binding.trajectory == self.session.trajectory && binding.call_id == call_id)?;
        if self.open_dispatches().iter().any(|open| open.id == *binding.dispatch) {
            return None;
        }
        let recorded = self.engine().recorded_dispatch_call(self.view, binding.dispatch)?;
        is_open_call(call, || self.canonical_bytes(call), &recorded).then(|| binding.dispatch.clone())
    }

    /// The batch identity this proposal was already decided under, when the host's call id
    /// names a recorded decision: the batch that released the bound dispatch, or the batch a
    /// denial was recorded under while its block still stands. A bound id naming different
    /// bytes is a reused id, refused; a denied id whose block no longer stands is a fresh
    /// proposal, so the deny → remedy → re-propose round decides anew.
    pub(super) fn replay_batch(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Result<Option<ProposalBatchId>, EventError> {
        let Some(call_id) = call_id else {
            return Ok(None);
        };
        if let Some(binding) = self
            .log
            .call_bindings()
            .find(|binding| *binding.trajectory == self.session.trajectory && binding.call_id == call_id)
        {
            let recorded = self
                .engine()
                .recorded_dispatch_call(self.view, binding.dispatch)
                .ok_or_else(|| EventError::EngineInvariant("a bound call names no recorded dispatch".to_string()))?;
            if !is_open_call(call, || self.canonical_bytes(call), &recorded) {
                return Err(EventError::CallIdReused);
            }
            let batch = self
                .engine()
                .decided_batch_of(self.view, binding.dispatch)
                .ok_or_else(|| EventError::EngineInvariant("a bound call names no decided batch".to_string()))?;
            return Ok(Some(batch));
        }
        if let Some(batch) = self.log.denied_call_batch(&self.session.trajectory, call_id) {
            let batch = ProposalBatchId::new(batch);
            if self.engine().denial_stands(self.view, &batch) {
                return Ok(Some(batch));
            }
        }
        Ok(None)
    }

    /// Is a call this trajectory has open one the harness gave no identity for? Its outcome can
    /// only be matched by tool and bytes, which cannot tell two calls apart, so nothing else may
    /// open beside it.
    pub(super) fn has_unbound_open_dispatch(&self) -> bool {
        self.open_dispatches().iter().any(|open| {
            !self
                .log
                .call_bindings()
                .any(|binding| *binding.trajectory == self.session.trajectory && *binding.dispatch == open.id)
        })
    }

    pub(super) fn parent_of(&self, child: &TrajectoryId) -> Option<TrajectoryId> {
        self.engine().parent_of(self.view, child)
    }

    pub(super) fn offer_pursuer(&self, offer: &OfferId) -> Option<TrajectoryId> {
        self.engine().offer_pursuer(self.view, offer)
    }

    /// What a child bound to `fork` is told at its start, where the fork's policy shapes its return.
    pub(super) fn fork_return_contract(&self, fork: &appa_engine::value::ForkId) -> Option<String> {
        self.engine()
            .fork_return_contract(self.view, &self.session.trajectory, fork)
    }

    pub(super) fn fork_status(&self, fork: &appa_engine::value::ForkId) -> ForkStatus {
        self.engine().fork_status(self.view, fork)
    }

    pub(super) fn latest_return(&self, child: &TrajectoryId) -> Option<String> {
        self.engine().latest_return(self.view, child)
    }

    /// The fan-out spawn released under this prompt that still binds children: its fork
    /// prepared, its call not reported finished, its parent live.
    pub(super) fn open_fan_out(&self, prompt: &PromptKey) -> Option<appa_engine::value::ForkId> {
        let settled = self.log.settled_calls();
        self.log
            .call_bindings()
            .filter(|binding| binding.prompt == Some(prompt.0.as_str()))
            .filter(|binding| !settled.contains(&(binding.trajectory, binding.call_id)))
            .map(|binding| appa_engine::value::ForkId::of(binding.dispatch))
            .find(|fork| matches!(self.fork_status(fork), ForkStatus::FanOut))
    }

    /// The fork a fan-out spawn's child binds to: the one it already has, else a new member
    /// of the fan-out released under its prompt.
    pub(super) fn fan_out_fork(
        &self,
        child: &TrajectoryId,
        prompt: &PromptKey,
    ) -> Result<appa_engine::value::ForkId, EventError> {
        match self.engine().fork_of(self.view, child) {
            Some(fork) => Ok(fork),
            None => self.open_fan_out(prompt).ok_or(EventError::SpawnNotTaken),
        }
    }

    pub(super) fn in_flight_fork(&self, child: &TrajectoryId) -> Result<appa_engine::value::ForkId, EventError> {
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

impl Session {
    /// Decide one event at the head. A decision made at a revision a publish moved past does
    /// not commit ([`EventError::PolicyMoved`]); when this view now serves the head, the event
    /// is decided afresh there, consults included: an answer gathered for the earlier
    /// revision's question is not carried to the next.
    pub(super) async fn drive_with_evidence(
        &self,
        mut event: impl FnMut(&Decided<'_>, Vec<ExternalEvidence>) -> Result<EngineEvent, EventError>,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<EngineDecision, EventError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let deployment = self.deployment();
            match self
                .drive_at(&deployment, &mut event, elicitation, ruling, occasion)
                .await
            {
                Err(EventError::PolicyMoved { .. })
                    if attempt < PUBLISHES_FOLLOWED && self.follow_head(&deployment) =>
                {
                    tracing::debug!(
                        root = %self.root.0,
                        attempt,
                        "a publish moved the head under the decision: deciding again at the head"
                    );
                }
                decided => return decided,
            }
        }
    }

    pub(super) async fn drive_at(
        &self,
        deployment: &Deployment,
        mut event: impl FnMut(&Decided<'_>, Vec<ExternalEvidence>) -> Result<EngineEvent, EventError>,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<EngineDecision, EventError> {
        let opening = match occasion {
            Occasion::Proposal { call_id, prompt } => Opening { call_id, prompt },
            Occasion::Report { .. } | Occasion::Remedy { .. } | Occasion::Other => Opening::default(),
        };
        let opened = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(deployment, &opened)?;
        tracing::Span::current().record(
            "appa.policy.id",
            match policy.engine().revision() {
                Some(revision) => format!("{}@{revision}", opened.deployment()),
                None => crate::engine::policy_file_key(opened.policy_file()),
            },
        );
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
            let decision = self.drive(&policy, entering, opening, |context| event(context, carried.clone()))?;
            match decision.then {
                // The engine batches every missing answer into one request set, and evidence
                // is matched by name, never by position. Without a reviewer the
                // consults run concurrently — an annotation consult can take a model call's
                // seconds, and the batch should cost its slowest member, not their sum. With a
                // reviewer they stay serial: one staged review on screen at a time.
                Next::ResolveExternal(requests) => match elicitation {
                    None => {
                        // Batch-terminal: every sibling settles first; any no-answer
                        // then aborts the invocation, discarding the siblings' answers,
                        // before another engine round or any append.
                        // Embedded hosts supply the ruling outside an MCP elicitation context.
                        let consults = requests
                            .into_iter()
                            .map(|request| self.consult(request, None, ruling, occasion));
                        for answered in crate::external::settle_batch(consults).await {
                            evidence.push(answered?);
                        }
                    }
                    Some(_) => {
                        for request in requests {
                            let answered = self.consult(request, elicitation, ruling, occasion).await?;
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

    pub(super) fn drive(
        &self,
        policy: &crate::engine::PolicyEngine<'_>,
        entering: bool,
        opening: Opening<'_>,
        mut event: impl FnMut(&Decided<'_>) -> Result<EngineEvent, EventError>,
    ) -> Result<EngineDecision, EventError> {
        for attempt in 1..=REPLAY_LIMIT {
            // The read, the decision and the append are one step: the family's other writers
            // wait for it rather than lose their compare-and-swap to it.
            let step = self.inner.step(&self.root)?;
            let log = self.inner.step_log(&step, &self.root)?;
            let view = policy.rebuild_view(&log).map_err(EventError::from)?;
            let context = Decided {
                session: self,
                policy,
                view: &view,
                log: &log,
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
                .handle(&view, &self.trajectory, event, &self.presentation)
                .map_err(EventError::from)?;

            let Some(facts) = decision.append.as_ref() else {
                return Ok(decision);
            };
            let opens_dispatch = facts.iter().find_map(|fact| match fact {
                appa_engine::fact::Fact::DispatchOpened { dispatch, .. }
                    if dispatch.trajectory() == &self.trajectory =>
                {
                    Some(dispatch.clone())
                }
                _ => None,
            });
            // Only a batch that opens a dispatch can be the second one. An outcome closes a
            // dispatch and drives the count down, so refusing it would leave a parallel batch
            // with no way to drain: every report would be refused for the calls it is settling.
            //
            // Parallel calls need every one of them to carry an identity, not just the newest.
            // An unidentified call already open cannot be told apart from this one when its
            // outcome arrives, so the second dispatch is refused whichever end is unidentified.
            // An identified spawn is not counted: its result comes under its own call id.
            if opens_dispatch.is_some() {
                let spawns: Vec<_> = context
                    .open_dispatches()
                    .into_iter()
                    .filter(|open| context.identified_spawn(open))
                    .map(|open| open.id)
                    .collect();
                if policy
                    .engine()
                    .opens_a_second_dispatch(&view, &self.trajectory, facts, &spawns)
                    && (opening.call_id.is_none() || context.has_unbound_open_dispatch())
                {
                    return Err(EventError::CallOutstanding);
                }
            }
            let released = facts.iter().find_map(|fact| match fact {
                appa_engine::fact::Fact::ForkPrepared { kind, .. } => Some(*kind),
                _ => None,
            });
            // A single spawn binds whichever child starts next, so one waits at a time. A
            // fan-out spawn's children bind by the prompt it was released under, so it waits
            // only on another fan-out of the same prompt.
            let fan_out_prompt = match (opens_dispatch.is_some(), released) {
                (true, Some(appa_engine::transition::SpawnKind::Single))
                    if !policy.engine().forks_in_flight(&view).is_empty() =>
                {
                    return Err(EventError::SpawnOutstanding);
                }
                (true, Some(appa_engine::transition::SpawnKind::FanOut)) => match (opening.call_id, opening.prompt) {
                    (Some(_), Some(prompt)) if context.open_fan_out(prompt).is_some() => {
                        return Err(EventError::FanOutOutstanding);
                    }
                    (Some(_), Some(prompt)) => Some(prompt.0.clone()),
                    _ => return Err(EventError::FanOutUnkeyed),
                },
                _ => None,
            };
            let host_record = match (opening.call_id, opens_dispatch) {
                (Some(call_id), Some(dispatch)) => {
                    if call_id.is_empty()
                        || log
                            .call_bindings()
                            .any(|binding| *binding.trajectory == self.trajectory && binding.call_id == call_id)
                    {
                        return Err(EventError::CallIdReused);
                    }
                    Some(appa_eventlog::HostObservation::CallBound {
                        trajectory: self.trajectory.clone(),
                        call_id: call_id.to_string(),
                        dispatch,
                        prompt: fan_out_prompt,
                    })
                }
                // A decided proposal batch that opens nothing denied the call: bind the host
                // id to the decision's batch identity, so a repeat of this delivery is
                // answered from the record rather than decided again with fresh offers.
                (Some(call_id), None) if !call_id.is_empty() => facts.iter().find_map(|fact| match fact {
                    appa_engine::fact::Fact::ProposalBatchDecided { batch, released, .. } if released.is_empty() => {
                        Some(appa_eventlog::HostObservation::CallDenied {
                            trajectory: self.trajectory.clone(),
                            call_id: call_id.to_string(),
                            batch: batch.as_str().to_string(),
                        })
                    }
                    _ => None,
                }),
                _ => None,
            };
            let binds_read = facts.iter().any(|fact| {
                matches!(
                    fact,
                    appa_engine::fact::Fact::DispatchOpened { .. } | appa_engine::fact::Fact::ValueAdmitted { .. }
                )
            });
            let hold = crate::api::embedded::EMBEDDED_READ_HOLD
                .try_with(|hold| hold.clone())
                .ok()
                .filter(|hold| hold.root == self.root.as_str());
            // A batch an engine at a revision decided carries that revision on its first record,
            // and commits only while the revision is still the deployment's head.
            let appended = if let Some(hold) = hold {
                step.append_holding_embedded_read(
                    &log,
                    facts,
                    host_record.as_ref(),
                    &appa_eventlog::embedded::EmbeddedAppendHold {
                        id: &hold.id,
                        call_id: &hold.call_id,
                        generation: hold.generation,
                        bind: binds_read,
                    },
                )
            } else {
                step.append(&log, facts, host_record.as_ref())
            };
            match appended {
                Ok(()) => {
                    self.inner.shared.spawn_holds.appended();
                    return Ok(decision);
                }
                Err(appa_eventlog::AppendError::ReadClaimLost) => {
                    return Err(EventError::OutcomeMismatch);
                }
                // A publish moved the head after this event read it: the decision was made at a
                // revision that no longer decides. Nothing was written; the event decides again
                // once this view serves the head.
                Err(appa_eventlog::AppendError::PolicyMoved { head }) => {
                    return Err(EventError::PolicyMoved {
                        deployment: self.inner.line().to_string(),
                        head: head.map_or_else(|| "none".to_string(), |head| head.to_string()),
                    });
                }
                Err(appa_eventlog::AppendError::Conflict { .. }) => {
                    tracing::debug!(
                        root = %self.root.0,
                        attempt,
                        "another writer won the append: discarding the decision and replaying the event"
                    );
                    continue;
                }
                Err(error) => {
                    self.inner
                        .note_store_error(Some(&self.root), crate::events::StoreOperation::Append, &error);
                    return Err(EventError::Storage(error.to_string()));
                }
            }
        }
        Err(EventError::Contended { attempts: REPLAY_LIMIT })
    }
}
