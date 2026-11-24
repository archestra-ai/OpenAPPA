use super::*;

/// What a late child open did: bound the child now, or found the
/// same pair already bound — the engine's own idempotent answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateOpen {
    Opened,
    AlreadyOpen,
}

/// Blocks subsequent output from a child that already ended without a return.
pub(super) const ENDED_CHILD: &str = "[appa] this subagent ended without a return; nothing it says now can cross. Stop with an \
                           empty final message (send no text or explanation).";

pub(super) fn return_decision(decision: EngineDecision) -> Result<ChildReturnDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::Value { value, receipt }) => {
            Ok(ChildReturnDecision::Returned { value, receipt })
        }
        Next::PresentToModel(Presentation::Staged { value }) => Ok(ChildReturnDecision::Staged { value }),
        Next::PresentToModel(Presentation::NoValue) => Ok(ChildReturnDecision::NoValue),
        Next::PresentToModel(Presentation::Deferred) => Ok(ChildReturnDecision::Deferred),
        Next::PresentToModel(Presentation::Blocked { feedback, outcomes, .. }) => {
            Ok(ChildReturnDecision::Blocked { feedback, outcomes })
        }
        _ => Err(EventError::UnexpectedDecision),
    }
}

impl Session {
    /// A same-family spawned child agent started. `spawn` names the prepared fork the child binds
    /// to: the [`crate::api::SpawnBinding`] the parent's spawn release handed the harness, or — for a
    /// harness whose start signal carries no reference to the spawn call — the fork already bound
    /// to this child, else the family's one spawn in flight. The engine's `BindFork` opens the
    /// child before its first engine event; the child exists exactly when the log's `ForkOpened`
    /// does and keeps this session's root. A start for a child already bound is the parent
    /// addressing it again: the parent's current label flows into the child, and a return
    /// derivation the child never echoed is dropped. This is not `Runtime::open_root_fork`, which
    /// opens an independent conversation root without a spawn or return contract.
    ///
    /// The child's session comes with the contract the child works under: what its return must
    /// look like, where the fork's policy shapes it. Nothing for a child whose return crosses as
    /// spoken. Read from the view that binds the child, so the start rebuilds nothing twice.
    pub fn start_child(&self, id: TrajectoryId, spawn: SpawnRef) -> Result<(Session, Option<String>), EventError> {
        let child = id.clone();
        self.bind_child(id, |context| match &spawn {
            SpawnRef::Binding(binding) => {
                crate::engine::parse_fork(context.log, binding).ok_or(EventError::SpawnNotTaken)
            }
            SpawnRef::InFlight => context.in_flight_fork(&child),
            SpawnRef::FanOut(prompt) => context.fan_out_fork(&child, prompt),
        })
    }

    /// Open a child whose start hook never arrived, or has not arrived yet:
    /// bind it to the family's one spawn in flight, as its start
    /// would. Whether this opened the child or found it already open tells the
    /// dispatcher whether the refused event was the missing start's, and is
    /// worth running once more, or the child's own answer.
    /// A fan-out spawn's child names the prompt its spawn was released under, and binds there.
    ///
    /// A publish between attempts can leave this view behind a start that opened the child at
    /// the head; the whole attempt, first read included, is then made again there.
    pub(crate) fn open_late(&self, child: TrajectoryId, prompt: Option<&PromptKey>) -> Result<LateOpen, EventError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let deployment = self.deployment();
            match self.open_late_at_snapshot(&child, prompt) {
                Err(EventError::PolicyMoved { .. })
                    if attempt < PUBLISHES_FOLLOWED && self.follow_head(&deployment) =>
                {
                    tracing::debug!(root = %self.root.0, attempt, "a publish moved the head under a late open: opening again at the head");
                }
                opened => return opened,
            }
        }
    }

    fn open_late_at_snapshot(&self, child: &TrajectoryId, prompt: Option<&PromptKey>) -> Result<LateOpen, EventError> {
        if self.liveness_of(child)? != Liveness::Unopened {
            return Ok(LateOpen::AlreadyOpen);
        }
        // Read again in the guarded bind: a start that lands first is not addressed again.
        let bound = self.bind_child(child.clone(), |context| {
            if context.engine().liveness(context.view, child) != Liveness::Unopened {
                return Err(EventError::TrajectoryExists);
            }
            match prompt {
                Some(prompt) => context.fan_out_fork(child, prompt),
                None => context.in_flight_fork(child),
            }
        });
        match bound {
            Ok(_) => Ok(LateOpen::Opened),
            Err(EventError::TrajectoryExists) => Ok(LateOpen::AlreadyOpen),
            Err(error) => Err(error),
        }
    }

    pub(super) fn bind_child(
        &self,
        id: TrajectoryId,
        fork: impl Fn(&Decided<'_>) -> Result<appa_engine::value::ForkId, EventError>,
    ) -> Result<(Session, Option<String>), EventError> {
        let child = id.clone();
        let opened = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &opened)?;
        let mut contract = None;
        let decision = self.drive(&policy, true, Opening::default(), |context| {
            let fork = fork(context)?;
            match context.fork_status(&fork) {
                ForkStatus::Unprepared | ForkStatus::Failed | ForkStatus::ParentEnded => Err(EventError::SpawnNotTaken),
                ForkStatus::Prepared | ForkStatus::Bound(_) | ForkStatus::FanOut => {
                    contract = context.fork_return_contract(&fork);
                    Ok(EngineEvent::BindFork {
                        fork,
                        child: child.clone(),
                    })
                }
            }
        })?;
        match decision.then {
            // The child rides the parent event's snapshot, not a fresh one.
            Next::Done => Ok((
                Session::attach(
                    Arc::clone(&self.inner),
                    self.deployment(),
                    id,
                    self.root.clone(),
                    EmbeddedPresentationOptions::default(),
                ),
                contract,
            )),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// Finish an empty child turn under its immutable recorded return contract.
    /// A void contract ends here; a crossing contract remains available for follow-ups.
    pub async fn on_child_turn_end(&self) -> Result<ChildReturnDecision, EventError> {
        if self.returns_void()? {
            self.on_child_end(None).await
        } else {
            self.on_turn_end().await?;
            Ok(ChildReturnDecision::NoValue)
        }
    }

    /// [`Session::on_child_turn_end`] for a harness that reports outcomes by call id after the
    /// stop: held while a call the stop would close is not in `answered`.
    pub(crate) async fn on_child_turn_end_holding(
        &self,
        answered: &BTreeSet<String>,
    ) -> Result<Stop<ChildReturnDecision>, EventError> {
        if self.returns_void()? {
            self.on_child_end_holding(None, answered).await
        } else {
            Ok(match self.on_turn_end_holding(answered).await? {
                Stop::Taken(()) => Stop::Taken(ChildReturnDecision::NoValue),
                Stop::Held(running) => Stop::Held(running),
            })
        }
    }

    fn returns_void(&self) -> Result<bool, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        let returns = policy
            .engine()
            .return_policy_of(&view, &self.trajectory)
            .ok_or(EventError::NotAChild)?;
        Ok(matches!(returns, appa_engine::fact::ReturnPolicy::Void))
    }

    /// The child finished its turn and returns `value`; `None` returns no
    /// value. The return is the child's only channel to the parent and is
    /// checked before it may cross; it names the fork that opened the
    /// child, recovered from the log.
    ///
    /// Any call the child still has open got no outcome hook and never
    /// will, so each closes first as unreported, exactly as the child's
    /// turn end would close it.
    ///
    /// A child may stop more than once: each stop is judged under the
    /// fork's return policy as its own crossing, and a stop repeating the
    /// latest crossed value replays it, appending nothing. A child that
    /// ended by returning nothing has nothing left to cross: an empty stop
    /// replays, a message is held until the child gives it up.
    pub async fn on_child_end(&self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        match self.child_calls()? {
            ChildCalls::Ended(end) => ended_child(end, value),
            ChildCalls::Open(open) => self.close_and_cross(open, value).await,
        }
    }

    /// [`Session::on_child_end`] for a harness that reports outcomes by call id after the
    /// stop: held while a call the stop would close is not in `answered`.
    pub(crate) async fn on_child_end_holding(
        &self,
        value: Option<String>,
        answered: &BTreeSet<String>,
    ) -> Result<Stop<ChildReturnDecision>, EventError> {
        let open = match self.child_calls()? {
            ChildCalls::Ended(end) => return ended_child(end, value).map(Stop::Taken),
            ChildCalls::Open(open) => open,
        };
        match self.unanswered(&open, answered)?.as_slice() {
            [] => self.close_and_cross(open, value).await.map(Stop::Taken),
            running => Ok(Stop::Held(running.to_vec())),
        }
    }

    /// The calls a child's end closes, or how a child that already ended did so. A trajectory
    /// with no parent is refused here, before its end closes any call it has open.
    fn child_calls(&self) -> Result<ChildCalls, EventError> {
        self.with_view(|context| {
            Ok(match context.engine().liveness(context.view, &self.trajectory) {
                Liveness::Ended => ChildCalls::Ended(context.child_end(&self.trajectory)),
                Liveness::Live | Liveness::Unopened => {
                    match (context.open_dispatches(), context.parent_of(&self.trajectory)) {
                        (open, None) if !open.is_empty() => return Err(EventError::NotAChild),
                        (open, _) => ChildCalls::Open(open),
                    }
                }
            })
        })
    }

    async fn close_and_cross(
        &self,
        open: Vec<OpenDispatch>,
        value: Option<String>,
    ) -> Result<ChildReturnDecision, EventError> {
        // Each call it still has open got no outcome hook and closes as unreported, exactly as
        // a turn end closes it.
        self.close_unreported(open, "call closed as unreported at the child's end")
            .await?;
        self.cross_return(value, ReturnKind::Stop, Occasion::Other).await
    }

    /// A return the child makes without ending, judged exactly as a stop's return: the calls
    /// it has open stay open, and a return made beside one is refused until they report. A
    /// deferral here holds the value for the parent, and the child runs on. `operation` names
    /// this return across the host's retries: a retry of a held return's operation with the same
    /// words answers as that return did, one with other words is refused, and an operation no
    /// held return bound is decided afresh. A return named by its words alone repeats the latest
    /// held return in the same words, until the parent addresses the child again.
    pub async fn on_child_return(
        &self,
        value: String,
        operation: Option<ReturnOperation>,
    ) -> Result<ChildReturnDecision, EventError> {
        match &operation {
            Some(operation) => {
                let keyed = KeyedReturn {
                    operation,
                    value: &value,
                };
                self.cross_return(Some(value.clone()), ReturnKind::KeyedTurn, Occasion::Return(keyed))
                    .await
            }
            // Only a retry of the stop repeats the deferral that ended the child: a turn on
            // the ended child is refused as ended.
            None => self.cross_return(Some(value), ReturnKind::Turn, Occasion::Other).await,
        }
    }

    pub(super) async fn cross_return(
        &self,
        value: Option<String>,
        kind: ReturnKind,
        occasion: Occasion<'_>,
    ) -> Result<ChildReturnDecision, EventError> {
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
                        kind,
                        evidence,
                    })
                },
                None,
                None,
                occasion,
            )
            .await?;

        return_decision(decision)
    }

    pub(super) fn liveness_of(&self, trajectory: &TrajectoryId) -> Result<Liveness, EventError> {
        self.with_view(|context| Ok(context.engine().liveness(context.view, trajectory)))
    }
}

enum ChildCalls {
    Open(Vec<OpenDispatch>),
    Ended(ChildEnd),
}

/// How an ended child ended: every later stop repeats what that end answered. A deferral at
/// the stop keeps the words it froze, which only a retry of that stop repeats: other words come
/// from a child that has ended.
pub(super) enum ChildEnd {
    Void,
    Deferred { said: String },
}

fn ended_child(end: ChildEnd, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
    match (value, end) {
        (None, _) => Ok(ChildReturnDecision::NoValue),
        (Some(value), ChildEnd::Deferred { said }) if value == said => Ok(ChildReturnDecision::Deferred),
        (Some(_), ChildEnd::Deferred { .. }) => Err(EventError::TrajectoryEnded),
        (Some(_), ChildEnd::Void) => Ok(ChildReturnDecision::Blocked {
            feedback: ENDED_CHILD.to_string(),
            outcomes: appa_runtime_api::Outcomes::HardBlock,
        }),
    }
}
