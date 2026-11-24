use super::*;
use appa_runtime_api::AutoApply;

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
            outcomes,
            offers,
            review,
            display,
        }) => {
            let presentation = remedy_presentation(
                feedback.clone(),
                Some(outcomes),
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

/// A spawn whose child's answer was deferred answers its result with the deferral notice,
/// whatever its dispatch closed as: the result carries none of the child's words.
fn deferred_or(deferral: Option<String>, decided: ToolResultDecision) -> Result<ToolResultDecision, EventError> {
    Ok(match deferral {
        Some(placeholder) => ToolResultDecision::Replace {
            placeholder,
            presentation: None,
        },
        None => decided,
    })
}

impl Session {
    pub(super) fn refuse_substituted_embedded_read(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        outcome: &ToolOutcome,
    ) -> Result<(), EventError> {
        // Classify the open dispatch and find its held message in the same snapshot.
        let held = self.with_view(|context| {
            if context.engine().liveness(context.view, &self.trajectory) != Liveness::Live {
                return Ok(None);
            }
            let mut open = context.open_dispatches();
            if call_id.is_none() {
                open.retain(|open| !context.identified_spawn(open));
            }
            let held = classify_report_identified(
                call,
                call_id,
                || context.canonical_bytes(call),
                &open,
                context.log.call_bindings(),
                &self.trajectory,
            )
            .ok()
            .and_then(|dispatch| {
                context
                    .view
                    .views(&self.trajectory)?
                    .dispatch_call(&dispatch)?
                    .held()
                    .cloned()
            });
            Ok(held.map(|held| (held, context.engine().trust_chain().clone())))
        })?;
        let Some((held, chain)) = held else {
            return Ok(());
        };
        let Ok(id) = crate::api::embedded::EmbeddedPeerId::parse(&held.id) else {
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
            || crate::api::embedded::held_message(&row, &chain) != held
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
        self.on_tool_result_receipted(call, call_id, o)
            .await
            .map(|(decision, _)| decision)
    }

    /// [`Session::on_tool_result_identified`], with the receipt of the value it lets the host
    /// deliver.
    pub(crate) async fn on_tool_result_receipted(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        o: ToolOutcome,
    ) -> Result<(ToolResultDecision, Option<appa_runtime_api::ReleaseReceipt>), EventError> {
        self.refuse_substituted_embedded_read(&call, call_id.as_deref(), &o)?;
        let o = self.cap_outcome(o);
        let (mut deferral, mut auto_apply, mut reported) = (None, AutoApply::default(), None);
        let decision = self
            .report_outcome(
                &call,
                call_id.as_deref(),
                &o,
                &mut deferral,
                &mut auto_apply,
                &mut reported,
            )
            .await?;
        let decided = self
            .accept_staged(outcome_decision(decision, &self.deployment().externals)?, auto_apply)
            .await?;
        let decided = deferred_or(deferral, decided)?;
        let receipt = match (&decided, &reported) {
            (ToolResultDecision::Keep | ToolResultDecision::Deliver { .. }, Some(dispatch)) => {
                self.receipt(appa_engine::engine::Receipted::Result(dispatch))?
            }
            _ => None,
        };
        Ok((decided, receipt))
    }

    /// Under `RulingsAndAccept`, as the snapshot that decided the result read it, a staged result whose block offers a plan the family lets the
    /// runtime apply itself ([`Session::auto_applied_offer`]) is settled through the same
    /// execution an agent's plan takes, and its admitted value delivered: the agent sees the
    /// value, never the block. A plan that was not applied lost a race, and its block stands as
    /// decided. An operational failure to apply it refuses the report.
    async fn accept_staged(
        &self,
        decided: ToolResultDecision,
        auto_apply: AutoApply,
    ) -> Result<ToolResultDecision, EventError> {
        let (
            AutoApply::RulingsAndAccept,
            ToolResultDecision::Replace {
                presentation: Some(presentation),
                ..
            },
        ) = (auto_apply, &decided)
        else {
            return Ok(decided);
        };
        let Some(offer) = self.auto_applied_offer(&presentation.offers, auto_apply)? else {
            return Ok(decided);
        };
        match self.auto_apply(offer).await {
            Ok(RemedyDecision::Returned { value }) => Ok(ToolResultDecision::Deliver { value }),
            Err(error) if error.is_operational() => Err(error),
            applied => {
                tracing::debug!(trajectory = %self.trajectory.0, ?applied, "an auto-applied staged plan was not applied");
                Ok(decided)
            }
        }
    }

    /// `deferral` receives the notice of the reported dispatch, where it is a spawn whose
    /// child's answer was deferred, `auto_apply` the family's pinned setting, and `reported`
    /// the dispatch, all as the snapshot that decided the report read them.
    pub(super) async fn report_outcome(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        o: &ToolOutcome,
        deferral: &mut Option<String>,
        auto_apply: &mut AutoApply,
        reported: &mut Option<appa_engine::value::DispatchId>,
    ) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                *auto_apply = crate::api::pinned_settings(context.log).auto_apply;
                let dispatch = context.classify_report(call, call_id)?;
                *deferral = context.deferral_notice(&dispatch);
                *reported = Some(dispatch.clone());
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
            let (mut deferral, mut auto_apply) = (None, AutoApply::default());
            let decision = self
                .drive_with_evidence(
                    |context, evidence| {
                        auto_apply = crate::api::pinned_settings(context.log).auto_apply;
                        let dispatch = context.classify_spawn_report(&call, call_id.as_deref())?;
                        let fork = appa_engine::value::ForkId::of(&dispatch);
                        let status = context.fork_status(&fork);
                        // A report repeating the bound child's latest crossing is that crossing,
                        // whatever the child froze before it.
                        let crossed = match (&status, &child, &value) {
                            (ForkStatus::Bound(bound), Some(named), Some(said)) => {
                                named == bound && context.latest_return(named).as_deref() == Some(said.as_str())
                            }
                            _ => false,
                        };
                        deferral = match crossed {
                            true => None,
                            false => context.deferral_notice(&dispatch),
                        };
                        let next = match (status, &child) {
                            _ if matches!(outcome, ToolOutcome::Indeterminate) => SpawnPlan::Outcome,
                            (ForkStatus::Unprepared | ForkStatus::FanOut, _) => SpawnPlan::Outcome,
                            // A deferred return answers every report of its spawn with the notice,
                            // so the report closes the dispatch as what the host says it was.
                            (ForkStatus::Bound(bound), named)
                                if deferral.is_some() && named.as_ref().is_none_or(|named| *named == bound) =>
                            {
                                SpawnPlan::Outcome
                            }
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
                                // A failure repeating the crossed return closes as the host reported it.
                                Some(_) if crossed => match outcome {
                                    ToolOutcome::Failure { .. } => SpawnPlan::Outcome,
                                    _ => SpawnPlan::Replay,
                                },
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
                            // A withheld message still closes as the failure the host reported, in
                            // the runtime's words: none of the host's bytes cross with the close.
                            SpawnPlan::Replay | SpawnPlan::Withheld => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: match &outcome {
                                    ToolOutcome::Failure { .. } => ToolOutcome::Failure {
                                        message: UNCHECKED_RETURN.to_string(),
                                    },
                                    _ => ToolOutcome::Success {
                                        body: OutcomeBody::Unavailable,
                                    },
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
                    let decided = self
                        .accept_staged(outcome_decision(decision, &self.deployment().externals)?, auto_apply)
                        .await?;
                    return deferred_or(deferral, decided).map(SpawnResultDecision::Outcome);
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
                        outcomes: appa_runtime_api::Outcomes::Integrity,
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
