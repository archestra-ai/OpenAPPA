use super::*;

impl Engine {
    pub(super) fn decide_outcome(
        &self,
        view: &EngineView,
        report: &ToolReport,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let dispatch = &report.dispatch;
        let views = view.projection().view(dispatch.trajectory());
        let call = views
            .dispatch_call(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        if matches!(report.outcome, ToolOutcome::FailureWithBody { .. }) && call.file_basis().is_none() {
            return Err(TransitionError::FailureBodyOutsideFile);
        }
        let withheld = views.withholds_output(dispatch)
            || matches!(report.outcome, ToolOutcome::FailureWithBody { .. })
                && views.bound_sanitizer(dispatch).is_some();
        let observed = match (&report.outcome, withheld) {
            (ToolOutcome::Success { .. }, true) => Some(ObservedResult::Unavailable),
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Available(raw),
                },
                false,
            ) => Some(ObservedResult::Available(RawResultDigest::of(raw.as_str().as_bytes()))),
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
                false,
            ) => Some(ObservedResult::Unavailable),
            (ToolOutcome::Failure | ToolOutcome::Indeterminate, _) => None,
            (ToolOutcome::FailureWithBody { body }, _) => {
                Some(ObservedResult::Available(RawResultDigest::of(body.as_str().as_bytes())))
            }
        };
        let closed = || EngineDecision {
            append: None,
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed {
                admitted: views.admitted_body(dispatch).cloned(),
                withheld,
            }),
        };
        if let ToolOutcome::FailureWithBody { body } = &report.outcome
            && views.failed_with_body(dispatch)
        {
            let digest = RawResultDigest::of(body.as_str().as_bytes());
            if views.failure_body_digest(dispatch) != Some(digest) {
                return Err(TransitionError::ObservationMismatch);
            }
            return Ok(closed());
        }
        if !views.is_open(dispatch) {
            match (views.closed_successfully(dispatch), &observed) {
                (true, None) => return Err(TransitionError::ContradictedSuccess),
                (false, Some(_)) if views.closed_unobserved(dispatch) => {
                    return Err(TransitionError::ClosedUnobserved);
                }
                (false, Some(_)) => return Err(TransitionError::ObservationMismatch),
                _ => {}
            }
            if let (Some(recorded), Some(reported)) = (fixed_observation(&views, dispatch), &observed)
                && &recorded != reported
            {
                return Err(TransitionError::ObservationMismatch);
            }
            return Ok(closed());
        }
        let checkpointed = views.observed_result(dispatch).cloned();
        match (&checkpointed, &observed) {
            // A recorded success cannot be withdrawn: its effects are committed.
            (Some(_), None) => return Err(TransitionError::ContradictedSuccess),
            (Some(recorded), Some(reported)) if recorded != reported => {
                return Err(TransitionError::ObservationMismatch);
            }
            _ => {}
        }
        if views
            .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
            .is_some()
        {
            return self.restage(view, &views, dispatch, report.offer_nonce, act);
        }

        let admission = match (&report.outcome, withheld) {
            (ToolOutcome::Success { .. }, true) => ResultAdmission::SuccessNoValue,
            (ToolOutcome::Failure, _) => ResultAdmission::Failure,
            (ToolOutcome::FailureWithBody { .. }, true) => {
                let Some(ObservedResult::Available(observed)) = observed else {
                    unreachable!("a failure with a body observed its digest above")
                };
                ResultAdmission::FailureNoValue { observed }
            }
            (ToolOutcome::FailureWithBody { body }, false) => ResultAdmission::FailureWithBody { body: body.clone() },
            (ToolOutcome::Indeterminate, _) => ResultAdmission::Indeterminate,
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable,
                },
                false,
            ) => ResultAdmission::SuccessNoValue,
            (
                ToolOutcome::Success {
                    body: OutcomeBody::Available(raw),
                },
                false,
            ) => {
                let Some(ObservedResult::Available(raw_digest)) = observed else {
                    unreachable!("an available success observed its body digest above")
                };
                match views.bound_sanitizer(dispatch) {
                    None => {
                        self.validated_contract(&call)?;
                        ResultAdmission::SuccessRaw { body: raw.clone() }
                    }
                    Some(sanitizer) => {
                        let sanitizer = sanitizer.clone();
                        let Some(derived) = sanitizer_evidence(&report.evidence, &sanitizer, raw_digest) else {
                            let append = self.checkpoint_batch(view, &views, dispatch, &call, raw_digest)?;
                            return Ok(EngineDecision {
                                append,
                                follow_up: FollowUp::Outcome(OutcomeFollowUp::Resolve(EvidenceRequest::Sanitizer {
                                    sanitizer,
                                    source: raw_digest,
                                    body: raw.clone(),
                                })),
                            });
                        };
                        let contract = self.validated_contract(&call)?;
                        let (candidate, lineage) = crate::admit::bound_candidate(
                            &self.registry,
                            &views,
                            dispatch,
                            &call,
                            &contract,
                            &sanitizer,
                            raw_digest,
                            derived.clone(),
                            &self.context(act),
                        )
                        .map_err(admission_refusal)?;
                        let DerivedCandidate::Result { residual: Some(_), .. } = &candidate else {
                            return self.admitting_outcome(
                                view,
                                &views,
                                dispatch,
                                &call,
                                ResultAdmission::SuccessSanitized {
                                    body: derived,
                                    sanitizer,
                                    raw_digest,
                                },
                                act,
                            );
                        };
                        return self.stage_candidate(
                            view,
                            &views,
                            dispatch,
                            &call,
                            report.offer_nonce,
                            Fact::CandidateDerived {
                                trajectory: views.trajectory().clone(),
                                subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
                                sanitizer,
                                derived: candidate,
                                lineage,
                                evidence: act.pinned().clone(),
                            },
                            act,
                        );
                    }
                }
            }
        };
        self.admitting_outcome(view, &views, dispatch, &call, admission, act)
    }

    fn admitting_outcome(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let withheld = views.withholds_output(dispatch) || matches!(admission, ResultAdmission::FailureNoValue { .. });
        let batch = admit::admit_result(
            &self.registry,
            views,
            dispatch,
            call,
            admission,
            &self.context(act),
            &act.pinned(),
        )
        .map_err(admission_refusal)?;
        let admitted = batch.iter().find_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.clone()),
            _ => None,
        });
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Outcome(dispatch.clone()), batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Closed { admitted, withheld }),
        })
    }

    /// The remedy menu a confined result's stage offers under its dispatch's contract, with the
    /// group requirement raised before the menu reads anything.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn confined_menu(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        receiving: &Label,
        label: &Label,
        residual: &Narrowing,
        lineage: &SanitizerLineage,
        act: &ActEvidence,
    ) -> Result<Vec<plan::ExecutableRemedyPlan>, TransitionError> {
        let contract = self.dispatch_contract(views, dispatch)?;
        Ok(plan::confined_stage(
            &self.registry,
            &contract,
            receiving,
            label,
            residual,
            lineage,
            views,
            &self.context(act),
        )?)
    }

    /// The success checkpoint a still-open dispatch owes before any external step runs: its
    /// declared effects commit now, while value finalization — an output sanitizer derivation —
    /// is still in flight. Empty where the log already records the observation.
    fn observed_checkpoint(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        source: RawResultDigest,
    ) -> Vec<Fact> {
        if views.observed_result(dispatch).is_some() {
            return Vec::new();
        }
        admit::observe_success(&self.registry, views, dispatch, call, ObservedResult::Available(source))
            .expect("an open, unreported dispatch checkpoints its observed success")
    }

    /// [`Self::observed_checkpoint`] sealed as its own batch, for the paths that append it and
    /// then hand the external step back to the runtime.
    fn checkpoint_batch(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        source: RawResultDigest,
    ) -> Result<Option<ValidatedFactBatch>, TransitionError> {
        let checkpoint = self.observed_checkpoint(views, dispatch, call, source);
        if checkpoint.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.decided(
            view,
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            checkpoint,
        )?))
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        nonce: crate::value::OfferNonce,
        derived: Fact,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let source = match &derived {
            Fact::CandidateDerived {
                derived: DerivedCandidate::Result { source, .. },
                ..
            } => *source,
            _ => unreachable!("a staged record is a derived candidate"),
        };
        let mut facts = self.observed_checkpoint(views, dispatch, call, source);
        facts.push(derived);
        let (batch, confined) = self.staged(
            view,
            views,
            crate::basis::DecidedAct::Outcome(dispatch.clone()),
            nonce,
            dispatch,
            facts,
            act,
        )?;
        Ok(EngineDecision {
            append: Some(batch),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(confined))),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn staged(
        &self,
        view: &EngineView,
        views: &Views,
        act: crate::basis::DecidedAct,
        nonce: crate::value::OfferNonce,
        dispatch: &DispatchId,
        mut facts: Vec<Fact>,
        evidence: &ActEvidence,
    ) -> Result<(crate::transition::ValidatedFactBatch, Confined), TransitionError> {
        let Some(Fact::CandidateDerived {
            subject,
            derived:
                DerivedCandidate::Result {
                    value,
                    residual: Some(residual),
                    ..
                },
            lineage,
            ..
        }) = facts.last()
        else {
            unreachable!("a staged act ends on the candidate it derived")
        };
        let (subject, value, residual, lineage) = (subject.clone(), value.clone(), residual.clone(), lineage.clone());
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let stage = self.confined_menu(views, dispatch, &receiving, &value.label, &residual, &lineage, evidence)?;
        let advance = Sequence::advance_of(self, view, &facts);
        let (_, offers, opened) = self.open_offers(
            views,
            Opening {
                act: &act,
                advance: &advance,
                nonce: &nonce,
                subject: &subject,
            },
            dispatch.digest(),
            &stage,
            evidence,
        );
        facts.extend(opened);
        let batch = self.declaring(act, advance, facts);
        Ok((
            self.seal(view, batch)?,
            Confined {
                dispatch: dispatch.clone(),
                candidate: value,
                residual,
                offers,
            },
        ))
    }

    fn restage(
        &self,
        view: &EngineView,
        views: &Views,
        dispatch: &DispatchId,
        nonce: crate::value::OfferNonce,
        evidence: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        let Some(DerivedCandidate::Result {
            value,
            residual: Some(residual),
            ..
        }) = views.candidate(&subject).cloned()
        else {
            unreachable!("a standing candidate owes a residual")
        };
        if let Some((_, offers)) = views.pending_block(&subject) {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(Confined {
                    dispatch: dispatch.clone(),
                    candidate: value,
                    residual,
                    offers,
                }))),
            });
        }
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let lineage = views.lineage(&subject);
        let stage = self.confined_menu(views, dispatch, &receiving, &value.label, &residual, &lineage, evidence)?;
        let act = crate::basis::DecidedAct::Outcome(dispatch.clone());
        let advance = crate::basis::BasisAdvance::default();
        let (_, offers, opened) = self.open_offers(
            views,
            Opening {
                act: &act,
                advance: &advance,
                nonce: &nonce,
                subject: &subject,
            },
            dispatch.digest(),
            &stage,
            evidence,
        );
        let batch = self.declaring(act, advance, opened);
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Outcome(OutcomeFollowUp::Staged(Box::new(Confined {
                dispatch: dispatch.clone(),
                candidate: value,
                residual,
                offers,
            }))),
        })
    }
}

fn admission_refusal(error: AdmitError) -> TransitionError {
    match error {
        AdmitError::SanitizerTransitionUnmet | AdmitError::SanitizerBindingMismatch => {
            TransitionError::SanitizerUnapplicable
        }
        AdmitError::MembershipNeeded(needed) => TransitionError::from(needed),
        other => unreachable!("the outcome path admits what the log already proved: {other}"),
    }
}

fn fixed_observation(views: &Views, dispatch: &DispatchId) -> Option<ObservedResult> {
    if let Some(observed) = views.observed_result(dispatch) {
        return Some(observed.clone());
    }
    if !views.closed_successfully(dispatch) || views.bound_sanitizer(dispatch).is_some() {
        return None;
    }
    Some(match views.admitted_body(dispatch) {
        Some(body) => ObservedResult::Available(RawResultDigest::of(body.as_str().as_bytes())),
        None => ObservedResult::Unavailable,
    })
}
