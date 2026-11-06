use super::*;

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
pub(super) enum UnreportableOutcome {
    NoOpenDispatch,
    ByteMismatch,
}

impl UnreportableOutcome {
    pub(super) fn case(self) -> &'static str {
        match self {
            UnreportableOutcome::NoOpenDispatch => "no_open_dispatch",
            UnreportableOutcome::ByteMismatch => "byte_mismatch",
        }
    }

    pub(super) fn refusal(self) -> EventError {
        match self {
            UnreportableOutcome::NoOpenDispatch => EventError::UnknownDispatch,
            UnreportableOutcome::ByteMismatch => EventError::OutcomeMismatch,
        }
    }
}

pub(super) enum SpawnPlan {
    Outcome,
    Bind {
        fork: appa_engine::value::ForkId,
        child: TrajectoryId,
    },
    Replay,
    Withheld,
    Close(EventError),
}

/// Delivered when a child terminated without completing its return check.
pub(super) const UNCHECKED_RETURN: &str = "[appa] the subagent ended outside the return check and its message is withheld; \
                                nothing from it crossed into this session";

pub(super) fn outcome_decision(
    decision: EngineDecision,
    externals: &crate::external::ExternalServices,
) -> Result<ToolResultDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::KeepOutput) => Ok(ToolResultDecision::Keep),
        Next::PresentToModel(Presentation::ReplaceOutput { placeholder, .. }) => Ok(ToolResultDecision::Replace {
            placeholder,
            presentation: None,
        }),
        // An admitted value delivered in place of the raw output, as it crossed.
        Next::PresentToModel(Presentation::Value { value }) => Ok(ToolResultDecision::Deliver { value }),
        // The runtime's own words: the narrowing this result causes and the control call
        // that accepts it.
        Next::PresentToModel(Presentation::Blocked {
            feedback,
            offers,
            review,
            display,
        }) => {
            let presentation = remedy_presentation(
                feedback.clone(),
                offers,
                review,
                display.into_iter().collect(),
                externals,
            );
            Ok(ToolResultDecision::Replace {
                placeholder: feedback,
                presentation: Some(presentation),
            })
        }
        _ => Err(EventError::UnexpectedDecision),
    }
}

impl Session {
    pub(super) fn refuse_substituted_embedded_read(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        outcome: &ToolOutcome,
    ) -> Result<(), EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let open = self.report_calls(call_id)?;
        let dispatch = match classify_report_identified(
            call,
            call_id,
            || policy.engine().canonical_bytes(call),
            &open,
            log.call_bindings(),
            &self.trajectory,
        ) {
            Ok(dispatch) => dispatch,
            Err(_) => return Ok(()),
        };
        let Some(basis) = crate::api::embedded::basis_of(&log, &self.trajectory, &dispatch) else {
            return Ok(());
        };
        let appa_engine::value::FileBasis::Read(source) = &basis else {
            return Ok(());
        };
        let Ok(id) = crate::api::embedded::EmbeddedPeerId::parse(&source.version) else {
            return Ok(());
        };
        let Some(row) = self
            .inner
            .store
            .load_embedded_peer(&self.root, id.as_str())
            .map_err(|error| EventError::Storage(error.to_string()))?
        else {
            return Ok(());
        };
        if row.recipient != self.trajectory.as_str()
            || row.status != appa_eventlog::embedded::EmbeddedStatus::Read
            || crate::api::embedded::message_basis(&row, policy.engine().trust_chain()) != basis
        {
            return Ok(());
        }
        let reported = match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } => Some(body.as_str()),
            _ => None,
        };
        if reported != row.body.as_deref() {
            return Err(EventError::OutcomeMismatch);
        }
        Ok(())
    }

    pub async fn on_tool_result_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        o: ToolOutcome,
    ) -> Result<ToolResultDecision, EventError> {
        self.refuse_substituted_embedded_read(&call, call_id.as_deref(), &o)?;
        if self.inner.shared.files.is_some() && crate::api::files::owns(&call) {
            crate::api::files::operation(&call)?;
            let log = self.inner.log(&self.root)?;
            let deployment = self.deployment();
            let policy = self.inner.resolve_policy(&deployment, &log)?;
            let open = self.report_calls(call_id.as_deref())?;
            let dispatch = classify_report_identified(
                &call,
                call_id.as_deref(),
                || policy.engine().canonical_bytes(&call),
                &open,
                log.call_bindings(),
                &self.trajectory,
            )
            .map_err(UnreportableOutcome::refusal)?;
            let key = crate::api::files::key(&dispatch)?;
            // Preserve actual failure text: the native failure hook cannot reliably replace it.
            // A missing observation is repaired after its writer has stopped.
            let o = match o {
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable | OutcomeBody::TooLarge { .. },
                } => ToolOutcome::Success {
                    body: OutcomeBody::Available(String::new()),
                },
                other => other,
            };
            if matches!(o, ToolOutcome::Success { .. }) {
                // Verify the physical version before admitting a successful result. A later
                // trajectory append failure cannot erase this already-published file's Label
                // from the authoritative workspace event stream.
                self.ledger({
                    let key = key.clone();
                    move |store, actor| store.finish(actor, &key, true)
                })
                .await?;
            }
            let decision = self.report_outcome(&call, call_id.as_deref(), &o).await?;
            match &o {
                ToolOutcome::Indeterminate => {
                    self.repair_file_reservation(&dispatch).await;
                    return Err(crate::api::files::refused(
                        "missing outcome; no file result can be delivered",
                    ));
                }
                ToolOutcome::Failure { .. } => {
                    self.ledger({
                        let key = key.clone();
                        move |store, actor| store.finish(actor, &key, false)
                    })
                    .await?;
                }
                ToolOutcome::Success { .. } => {}
            }
            return outcome_decision(decision, &self.deployment().externals);
        }
        let o = self.cap_outcome(o);
        outcome_decision(
            self.report_outcome(&call, call_id.as_deref(), &o).await?,
            &self.deployment().externals,
        )
    }

    pub(super) async fn report_outcome(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        o: &ToolOutcome,
    ) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                let dispatch = context.classify_report(call, call_id)?;
                Ok(EngineEvent::ToolOutcome {
                    dispatch,
                    outcome: o.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
            None,
            Occasion::Report { call_id },
        )
        .await
    }

    /// Resume the exact suspended call without releasing a second dispatch.
    /// Rebinding the same pair inherits the parent's current label in the child.
    pub fn on_spawn_resume(&self, call: ProposedCall, child: TrajectoryId) -> Result<(), EventError> {
        let opened = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &opened)?;
        let decision = self.drive(&policy, true, Opening::default(), |context| {
            let dispatch = context.classify_resume(&call, &child)?;
            let fork = appa_engine::value::ForkId::of(&dispatch);
            match context.fork_status(&fork) {
                ForkStatus::Bound(bound) if bound == child => {}
                _ => return Err(EventError::BindingMismatch),
            }
            match context.policy.engine().liveness(context.view, &child) {
                Liveness::Live => {}
                Liveness::Ended => return Err(EventError::TrajectoryEnded),
                Liveness::Unopened => return Err(EventError::SpawnNotTaken),
            }
            Ok(EngineEvent::BindFork {
                fork,
                child: child.clone(),
            })
        })?;
        match decision.then {
            Next::Done => Ok(()),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The spawn call's own result, keyed on where its fork stands and on
    /// the child the harness names. A prepared fork binds to the named
    /// child here: the harness's acknowledgement can land before the
    /// child's start, and a parallel spawn is told apart by name. A bound
    /// fork's result closes the dispatch; a message it carries either
    /// repeats the child's latest crossed return, and replays it, or was
    /// never checked at a stop — the harness delivered content the child
    /// never returned — and is withheld from the parent with nothing
    /// admitted.
    pub async fn on_spawn_result_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> Result<SpawnResultDecision, EventError> {
        let outcome = self.cap_outcome(outcome);
        // At most two engine events: the binding, then the result on the bound fork.
        for _ in 0..2 {
            // The attempt that commits is the one whose plan the delivery below
            // follows, so each attempt overwrites what the last one wrote.
            let mut plan: Option<SpawnPlan> = None;
            let decision = self
                .drive_with_evidence(
                    |context, evidence| {
                        let dispatch = context.classify_spawn_report(&call, call_id.as_deref())?;
                        let fork = appa_engine::value::ForkId::of(&dispatch);
                        let next = match (context.fork_status(&fork), &child) {
                            _ if matches!(outcome, ToolOutcome::Indeterminate) => SpawnPlan::Outcome,
                            (ForkStatus::Unprepared | ForkStatus::FanOut, _) => SpawnPlan::Outcome,
                            (ForkStatus::Prepared, Some(child)) => SpawnPlan::Bind {
                                fork: fork.clone(),
                                child: child.clone(),
                            },
                            // A failure that names no child: the subagent ended before it
                            // returned. No child bound to a prepared fork, so that subagent
                            // never started; a bound one stopped.
                            (ForkStatus::Prepared, None) if matches!(outcome, ToolOutcome::Failure { .. }) => {
                                SpawnPlan::Close(EventError::SpawnNeverStarted)
                            }
                            (ForkStatus::Bound(_), None) if matches!(outcome, ToolOutcome::Failure { .. }) => {
                                SpawnPlan::Close(EventError::SpawnStopped)
                            }
                            (ForkStatus::Prepared, None) | (ForkStatus::Failed | ForkStatus::ParentEnded, _) => {
                                SpawnPlan::Close(EventError::SpawnNotTaken)
                            }
                            (ForkStatus::Bound(bound), Some(child)) if *child == bound => match &value {
                                None => SpawnPlan::Outcome,
                                Some(said) if context.latest_return(child).as_deref() == Some(said.as_str()) => {
                                    SpawnPlan::Replay
                                }
                                Some(_) => SpawnPlan::Withheld,
                            },
                            (ForkStatus::Bound(_), _) => SpawnPlan::Close(EventError::BindingMismatch),
                        };
                        let event = match &next {
                            SpawnPlan::Outcome => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: outcome.clone(),
                                evidence,
                                entropy: fresh_entropy(),
                            },
                            SpawnPlan::Bind { fork, child } => EngineEvent::BindFork {
                                fork: fork.clone(),
                                child: child.clone(),
                            },
                            SpawnPlan::Replay | SpawnPlan::Withheld => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: ToolOutcome::Success {
                                    body: OutcomeBody::Unavailable,
                                },
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
                    None,
                    Occasion::Report {
                        call_id: call_id.as_deref(),
                    },
                )
                .await?;
            // The engine decided on an event this handler built from the view.
            match plan.expect("the spawn result is typed before the engine decides") {
                SpawnPlan::Outcome => {
                    return outcome_decision(decision, &self.deployment().externals).map(SpawnResultDecision::Outcome);
                }
                SpawnPlan::Close(refusal) => return Err(refusal),
                SpawnPlan::Bind { child, .. } => match decision.then {
                    Next::Done => {
                        tracing::debug!(trajectory = %self.trajectory.0, child = %child.0, "the spawn result bound its child");
                    }
                    _ => return Err(EventError::UnexpectedDecision),
                },
                SpawnPlan::Replay => {
                    outcome_decision(decision, &self.deployment().externals)?;
                    return Ok(SpawnResultDecision::Return(ChildReturnDecision::Returned {
                        value: value.expect("a replay repeats a delivered message"),
                    }));
                }
                SpawnPlan::Withheld => {
                    outcome_decision(decision, &self.deployment().externals)?;
                    return Ok(SpawnResultDecision::Return(ChildReturnDecision::Blocked {
                        feedback: UNCHECKED_RETURN.to_string(),
                    }));
                }
            }
        }
        Err(EventError::UnexpectedDecision)
    }

    pub(super) fn cap_outcome(&self, outcome: ToolOutcome) -> ToolOutcome {
        match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } if body.len() > self.deployment().config.externals.max_body_bytes => ToolOutcome::Success {
                body: OutcomeBody::TooLarge {
                    limit_bytes: Some(self.deployment().config.externals.max_body_bytes),
                },
            },
            other => other,
        }
    }

    pub(super) fn refuse_report(
        &self,
        case: UnreportableOutcome,
        call: &ProposedCall,
        open: &[OpenDispatch],
    ) -> EventError {
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
}
