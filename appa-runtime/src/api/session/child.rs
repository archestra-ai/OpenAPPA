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
        Next::PresentToModel(Presentation::Value { value }) => Ok(ChildReturnDecision::Returned { value }),
        Next::PresentToModel(Presentation::Staged { value }) => Ok(ChildReturnDecision::Staged { value }),
        Next::PresentToModel(Presentation::NoValue) => Ok(ChildReturnDecision::NoValue),
        Next::PresentToModel(Presentation::Blocked { feedback, .. }) => Ok(ChildReturnDecision::Blocked { feedback }),
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
    pub(crate) fn open_late(&self, child: TrajectoryId, prompt: Option<&PromptKey>) -> Result<LateOpen, EventError> {
        match self.liveness_of(&child)? {
            Liveness::Unopened => {
                let id = child.clone();
                self.bind_child(id, |context| match prompt {
                    Some(prompt) => context.fan_out_fork(&child, prompt),
                    None => context.in_flight_fork(&child),
                })
                .map(|_| LateOpen::Opened)
            }
            Liveness::Live | Liveness::Ended => Ok(LateOpen::AlreadyOpen),
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
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        if policy.engine().liveness(&view, &self.trajectory) == Liveness::Ended {
            return Ok(match value {
                None => ChildReturnDecision::NoValue,
                Some(_) => ChildReturnDecision::Blocked {
                    feedback: ENDED_CHILD.to_string(),
                },
            });
        }
        self.settle_open_call(&policy, &view).await?;
        self.cross_return(value).await
    }

    /// A return the child makes without ending, judged exactly as a stop's return: the calls
    /// it has open stay open, and a return made beside one is refused until they report.
    pub async fn on_child_return(&self, value: String) -> Result<ChildReturnDecision, EventError> {
        self.cross_return(Some(value)).await
    }

    pub(super) async fn cross_return(&self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
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
                    })
                },
                None,
                None,
                Occasion::Other,
            )
            .await?;

        return_decision(decision)
    }

    /// Close whatever calls this child still has open before it returns. Each got no outcome
    /// hook and closes as unreported, exactly as a turn end closes it.
    pub(super) async fn settle_open_call(
        &self,
        policy: &crate::engine::PolicyEngine<'_>,
        view: &EngineView,
    ) -> Result<(), EventError> {
        for open in policy.engine().open_dispatches(view, &self.trajectory) {
            match self
                .abandon_dispatch(open.id.clone(), &ToolOutcome::Indeterminate)
                .await
            {
                Ok(_) => {
                    self.repair_file_reservation(&open.id).await;
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        dispatch = ?open.id,
                        tool = %open.tool,
                        "call closed as unreported at the child's end"
                    );
                }
                Err(EventError::UnknownDispatch) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn liveness_of(&self, trajectory: &TrajectoryId) -> Result<Liveness, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        Ok(policy.engine().liveness(&view, trajectory))
    }
}
