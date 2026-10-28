use super::*;

impl Engine {
    pub(super) fn decide_offer(
        &self,
        view: &EngineView,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let views = view.projection().view(&execution.trajectory);
        let recorded = views
            .offer(&execution.offer)
            .ok_or(TransitionError::UnknownOffer)?
            .clone();
        if recorded.trajectory != execution.trajectory {
            return Err(TransitionError::OfferElsewhere);
        }
        if let Some(end) = recorded.end.clone() {
            return self.ended_offer(&views, &recorded, &end, execution, act);
        }
        if recorded.basis != views.basis_for(&recorded.subject) {
            return Err(TransitionError::StaleOffer);
        }
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            let dispatch = dispatch.clone();
            return self.decide_confined(view, &views, execution, &recorded, &dispatch, act);
        }
        if let crate::basis::SubjectKey::Return(_) = &recorded.subject {
            return self.invalidated(view, execution, &recorded);
        }
        let call = self.offer_call(&views, &recorded);
        let contract = self.validated_contract(&call)?;
        let stage = views.call_stage(&recorded.subject);
        let role = views.call_role(&recorded.subject);
        let live = match check::evaluate(&contract, &views, &call, &stage, role, &self.context(act))? {
            CheckOutcome::Block(raw) => {
                require_atoms(act, plan::block_atoms(&self.registry, &contract, &raw))?;
                require_atoms(act, plan::plan_atoms(&self.registry, &recorded.plan))?;
                plan::plan(
                    &self.registry,
                    &views,
                    BlockedCall {
                        call: &call,
                        contract: &contract,
                        raw: &raw,
                        stage: &stage,
                        role,
                    },
                    &self.context(act),
                )?
                .plans
                .iter()
                .filter_map(plan::RemedyPlan::executable)
                .any(|offered| offered == &recorded.plan)
                .then_some(raw)
            }
            // The block is gone: whatever the agent would have remedied, nothing needs it now.
            CheckOutcome::Allow => None,
        };
        let Some(raw) = live else {
            return self.invalidated(view, execution, &recorded);
        };
        match (&execution.outcome, recorded.plan.hop()) {
            (OfferOutcome::Derived(evidence), Some(sanitizer)) => self.hop_call(
                view, &views, execution, &recorded, &contract, &raw, &call, &stage, sanitizer, evidence, act,
            ),
            (OfferOutcome::Approved(evidence), None) => self.approve_offer(
                view, &views, execution, &recorded, &contract, &raw, &call, evidence, act,
            ),
            (OfferOutcome::Denied { authority }, None) => self.deny_offer(
                view, &views, execution, &recorded, &contract, &call, &raw, &stage, authority, act,
            ),
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    fn invalidated(
        &self,
        view: &EngineView,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
    ) -> Result<EngineDecision, TransitionError> {
        let batch = vec![Fact::OfferInvalidated {
            trajectory: recorded.trajectory.clone(),
            offer: execution.offer,
        }];
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
        })
    }

    fn decide_confined(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        dispatch: &DispatchId,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        let call = views
            .dispatch_call(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let receiving = views
            .receiving_bound(dispatch)
            .ok_or(TransitionError::UnknownDispatch)?
            .clone();
        let Some(DerivedCandidate::Result {
            value,
            residual: Some(residual),
            ..
        }) = views.candidate(&subject).cloned()
        else {
            return self.invalidated(view, execution, recorded);
        };
        let lineage = views.lineage(&subject);
        let stage = self.confined_menu(views, dispatch, &receiving, &value.label, &residual, &lineage, act)?;
        if !stage.contains(&recorded.plan) {
            return self.invalidated(view, execution, recorded);
        }
        let facts = accepted(views, &recorded.trajectory, &subject, execution.offer);
        match (&execution.outcome, recorded.plan.hop()) {
            (OfferOutcome::Derived(evidence), Some(sanitizer)) => self.hop_candidate(
                view, views, execution, dispatch, &call, &receiving, &value, &lineage, sanitizer, evidence, facts, act,
            ),
            (OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                self.accept_candidate(view, views, execution, dispatch, &call, facts, act)
            }
            _ => Err(TransitionError::PlanOutcomeMismatch),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let admitted = admit::admit_result(
            &self.registry,
            views,
            dispatch,
            call,
            ResultAdmission::CandidateAccepted { offer: execution.offer },
            &self.context(act),
            &act.pinned(),
        )
        .unwrap_or_else(|error| unreachable!("the confined stage admits what the log already proved: {error}"));
        facts.extend(admitted);
        let value = crossed(&facts);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn hop_candidate(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        receiving: &Label,
        predecessor: &crate::value::LabeledValue,
        lineage: &SanitizerLineage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        mut facts: Vec<Fact>,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let Evidence::Sanitizer {
            sanitizer: named,
            source,
            derived: body,
        } = evidence
        else {
            return Err(TransitionError::EvidenceMismatch);
        };
        let source_digest = RawResultDigest::of(predecessor.body.as_str().as_bytes());
        if named != sanitizer || source != &source_digest {
            return Err(TransitionError::EvidenceMismatch);
        }
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_output(
                &predecessor.label,
                &self.validated_contract(call)?.tags,
                &self.context(act),
            )?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        if !plan::confined_hop_helps(receiving, &predecessor.label, &label) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let lineage = lineage
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let residual = crate::admit::confined_residual(receiving, &label);
        let staged = residual.is_some();
        facts.push(Fact::CandidateDerived {
            trajectory: views.trajectory().clone(),
            subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
            sanitizer: sanitizer.clone(),
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: source_digest,
                from: ConfinedFrom::Offer(execution.offer),
                value: crate::value::LabeledValue::new(body.clone(), label),
                residual,
            },
            lineage,
            evidence: act.pinned().clone(),
        });
        if staged {
            let (batch, confined) = self.staged(
                view,
                views,
                crate::basis::DecidedAct::Offer(execution.offer),
                execution.offer_nonce,
                dispatch,
                facts,
                act,
            )?;
            return Ok(EngineDecision {
                append: Some(batch),
                follow_up: FollowUp::Offer(OfferFollowUp::Staged(Box::new(confined))),
            });
        }
        // The successor owes nothing, so it crosses here. The admission re-derives from the record
        // just written, which is why the candidate is folded first: the one admission choke point
        // reads state, never a caller's claim.
        let admitted = admit::admit_result(
            &self.registry,
            &folded(view, &facts).view(views.trajectory()),
            dispatch,
            call,
            ResultAdmission::CandidateAdmissible,
            &self.context(act),
            &act.pinned(),
        )
        .unwrap_or_else(|error| unreachable!("the confined stage admits what this act just derived: {error}"));
        facts.extend(admitted);
        let value = crossed(&facts);
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Offer(execution.offer), facts)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Admitted { value }),
        })
    }

    /// One input-substitution progress hop.
    ///
    /// The sanitizer read the engine's own canonical argument bytes and returned one complete
    /// replacement object. Its bytes are untrusted: the engine strictly parses them, selects the
    /// ordered contract their arguments name, schema-checks them against that contract's
    /// parameters, constructs the canonical arguments itself, and only then has a call to measure.
    /// Nothing about the replacement is taken on the runtime's word, and the tool is never
    /// replaced. The sanitizer's scope and the block it improves are judged on the contract the
    /// offer was planned on; the rewritten call is judged on the contract it selects.
    ///
    /// A valid strictly helpful replacement commits as the next candidate on this subject and ends
    /// every sibling offer standing on its predecessor. The engine then re-checks it: where nothing
    /// is left to remedy it is released in this same batch — the immediate-admissibility exception
    /// — and otherwise the stage it leaves is the agent's next choice. An invalid
    /// derivation produces no record and no dispatch, and leaves the offer pending for a deliberate
    /// retry.
    #[allow(clippy::too_many_arguments)]
    fn hop_call(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        stage: &CallStage,
        sanitizer: &SanitizerName,
        evidence: &Evidence,
        under: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let Evidence::Rewrite {
            sanitizer: named,
            source,
            derived: body,
            annotation,
            interceptions,
        } = evidence
        else {
            return Err(TransitionError::EvidenceMismatch);
        };
        if named != sanitizer || source != &RawResultDigest::of(call.canonical_arguments().canonical_bytes()) {
            return Err(TransitionError::EvidenceMismatch);
        }
        let registered = self
            .registry
            .sanitizer(sanitizer)
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let label = registered
            .derive_input(
                &stage.released(&views.current_label()),
                &contract.tags,
                &self.context(under),
            )?
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let lineage = stage
            .lineage()
            .extend(sanitizer.clone())
            .ok_or(TransitionError::SanitizerUnapplicable)?;
        let substituted = substituted_call(&self.registry, call, body, annotation.as_ref(), interceptions.as_ref())?;
        let contract = self
            .registry
            .annotation_of(&substituted)
            .expect("a validated call resolves its annotation");
        // The sanitizer's jurisdiction reaches the contract the rewrite selects as well as the
        // one the offer was planned on: a rewrite is no way past a tag that keeps sanitizers off
        // a contract.
        if !registered.applies_to(&contract.tags) {
            return Err(TransitionError::SanitizerUnapplicable);
        }

        let next = CallStage::substituting(label.clone(), lineage.clone());
        let role = views.call_role(&recorded.subject);
        let after = check::evaluate(&contract, views, &substituted, &next, role, &self.context(under))?;
        if !plan::substitution_helps(raw, &after) {
            return Err(TransitionError::SanitizerUnapplicable);
        }
        let derived = DerivedCandidate::Call {
            source: *source,
            from: execution.offer,
            call: substituted.clone(),
            label,
        };
        let trajectory = &recorded.trajectory;
        let mut facts = accepted(views, trajectory, &recorded.subject, execution.offer);
        facts.push(Fact::CandidateDerived {
            trajectory: trajectory.clone(),
            subject: recorded.subject.clone(),
            sanitizer: sanitizer.clone(),
            derived,
            lineage,
            evidence: under.pinned().clone(),
        });
        let staged = Sequence::advance_of(self, view, &facts);
        let act = crate::basis::DecidedAct::Offer(execution.offer);
        let follow_up = match after {
            // The rewrite clears the block, and the derivation it staged is what says so. No
            // dispatch opens here: the model proposes the substituted call, and that proposal
            // takes the derivation and is judged against the state it arrives at. Releasing the
            // call here instead would decide it once, in advance, against state that can have
            // moved by the time the harness delivers it.
            CheckOutcome::Allow => OfferFollowUp::Approved {
                call: Box::new(substituted),
            },
            CheckOutcome::Block(raw) => {
                let role = views.call_role(&recorded.subject);
                require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw))?;
                let (block, opened) = self.surface_call_block(
                    views,
                    Opening {
                        act: &act,
                        advance: &staged,
                        nonce: &execution.offer_nonce,
                        subject: &recorded.subject,
                    },
                    BlockedCall {
                        call: &substituted,
                        contract: &contract,
                        raw: &raw,
                        stage: &next,
                        role,
                    },
                    under,
                )?;
                facts.extend(opened);
                OfferFollowUp::Substituted { block: Box::new(block) }
            }
        };
        Ok(EngineDecision {
            append: Some(self.decided(view, act, facts)?),
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn substituted_repeat(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<OfferFollowUp, TransitionError> {
        let Some(candidate) = views.call_candidate(&recorded.subject).cloned() else {
            return Ok(OfferFollowUp::Invalidated);
        };
        if views.pending_block(&recorded.subject).is_some() {
            // The candidate may stand under another contract than the offer was planned on; the
            // atoms that contract reads were pinned by the hop that derived it.
            let pinned = views.candidate_evidence(&recorded.subject);
            let under = act.overlay(pinned, self.registry.audience())?;
            let reblocked = self.reblocked(views, recorded, execution, &under)?;
            act.absorb(&under);
            return Ok(match reblocked {
                Some(block) => OfferFollowUp::Substituted { block },
                None => OfferFollowUp::Invalidated,
            });
        }
        // A hop opens no dispatch, so its subject never has one: the derivation it staged is
        // the whole of what it did. While that derivation is current the repeat answers with the
        // same approved call — executing an offer twice must not change what the model is told
        // to do — and once something has taken or replaced it the offer no longer stands.
        Ok(match views.recorded_candidate(&recorded.subject) {
            Some(held) if held.generation == views.basis_for(&recorded.subject).subject => OfferFollowUp::Approved {
                call: Box::new(candidate),
            },
            Some(_) | None => OfferFollowUp::Invalidated,
        })
    }

    pub(super) fn offer_call(&self, views: &Views, recorded: &crate::projection::RecordedOffer) -> ResolvedCall {
        views
            .standing_call(&recorded.subject)
            .expect("an opened offer names a proposal of a decided batch, or the candidate that replaced it")
            .clone()
    }

    fn ended_offer(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        end: &crate::projection::OfferEnd,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        use crate::projection::OfferEnd;
        if let crate::basis::SubjectKey::ConfinedResult(dispatch) = &recorded.subject {
            return match (end, &execution.outcome, recorded.plan.hop()) {
                (OfferEnd::Accepted, OfferOutcome::Derived(Evidence::Sanitizer { .. }), Some(_)) => {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.confined_repeat(views, dispatch)),
                    })
                }
                (OfferEnd::Accepted, OfferOutcome::Approved(evidence), None) if evidence.is_empty() => {
                    Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Offer(self.confined_repeat(views, dispatch)),
                    })
                }
                (OfferEnd::Invalidated, _, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_), Some(_))
                | (_, OfferOutcome::Derived(_), None)
                | (_, OfferOutcome::Approved(_), Some(_))
                | (OfferEnd::Accepted, OfferOutcome::Approved(_), None) => Err(TransitionError::PlanOutcomeMismatch),
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        if recorded.plan.hop().is_some() {
            return match (end, &execution.outcome) {
                (OfferEnd::Accepted, OfferOutcome::Derived(Evidence::Rewrite { .. })) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(self.substituted_repeat(views, recorded, execution, act)?),
                }),
                (OfferEnd::Accepted, OfferOutcome::Derived(_)) => Err(TransitionError::PlanOutcomeMismatch),
                (OfferEnd::Invalidated, _) => Ok(EngineDecision {
                    append: None,
                    follow_up: FollowUp::Offer(OfferFollowUp::Invalidated),
                }),
                (_, OfferOutcome::Approved(_)) => Err(TransitionError::PlanOutcomeMismatch),
                // A hop assigns no authority, so none of its offers can have been denied.
                _ => Err(TransitionError::TerminalOffer),
            };
        }
        let follow_up = match (end, &execution.outcome) {
            (OfferEnd::Accepted, OfferOutcome::Approved(_)) => OfferFollowUp::Approved {
                call: Box::new(
                    views
                        .approval(&execution.offer)
                        .ok_or(TransitionRefusal::UndischargedAcceptance)?
                        .call
                        .clone(),
                ),
            },
            (OfferEnd::Denied(recorded_authority), OfferOutcome::Denied { authority })
                if recorded_authority == authority =>
            {
                match self.reblocked(views, recorded, execution, act)? {
                    Some(block) => OfferFollowUp::Denied { block },
                    None => OfferFollowUp::Invalidated,
                }
            }
            (OfferEnd::Invalidated, _) => OfferFollowUp::Invalidated,
            // A terminal plan never took a sanitizer's derivation, spent or not.
            (_, OfferOutcome::Derived(_)) => return Err(TransitionError::PlanOutcomeMismatch),
            _ => return Err(TransitionError::TerminalOffer),
        };
        Ok(EngineDecision {
            append: None,
            follow_up: FollowUp::Offer(follow_up),
        })
    }

    fn confined_repeat(&self, views: &Views, dispatch: &DispatchId) -> OfferFollowUp {
        if let Some(value) = views.admitted_body(dispatch) {
            return OfferFollowUp::Admitted { value: value.clone() };
        }
        let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
        match (views.candidate(&subject), views.pending_block(&subject)) {
            (
                Some(DerivedCandidate::Result {
                    value,
                    residual: Some(residual),
                    ..
                }),
                Some((_, offers)),
            ) => OfferFollowUp::Staged(Box::new(Confined {
                dispatch: dispatch.clone(),
                candidate: value.clone(),
                residual: residual.clone(),
                offers,
            })),
            _ => OfferFollowUp::Invalidated,
        }
    }

    fn reblocked(
        &self,
        views: &Views,
        recorded: &crate::projection::RecordedOffer,
        execution: &OfferExecution,
        act: &ActEvidence,
    ) -> Result<Option<Box<Blocked>>, TransitionError> {
        let call = self.offer_call(views, recorded);
        Ok(
            match self.recheck(views, &recorded.subject, &execution.offer_nonce, call, act)? {
                Recheck::Blocked(block) => Some(block),
                Recheck::Clear(_) => None,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_offer(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        raw: &crate::check::RawBlock,
        call: &ResolvedCall,
        evidence: &[execute::AuthorityEvidence],
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        if evidence.len() != recorded.plan.required.len() {
            return Err(PlanError::RulingAssignmentMismatch.into());
        }
        for required in &recorded.plan.required {
            let matched = evidence
                .iter()
                .filter(|given| given.authority == required.authority && given.covers == required.covers)
                .count();
            if matched != 1 {
                return Err(PlanError::RulingAssignmentMismatch.into());
            }
        }
        execute::rulings_cover(
            &self.registry,
            contract,
            raw,
            evidence.iter().map(|given| (&given.authority, given.covers.as_slice())),
            &self.context(act),
        )
        .map_err(|error| match error {
            // The undecided atoms surface as the act's ask, exactly as the gates raise them.
            PlanError::MembershipNeeded(needed) => TransitionError::from(needed),
            other => TransitionError::Plan(other),
        })?;
        if evidence.iter().any(|given| given.offer != execution.offer) {
            return Err(PlanError::EvidenceOfferMismatch.into());
        }
        // And each reviewed exactly this call at the fold the release will run against.
        let live = views.current_label();
        if evidence
            .iter()
            .any(|given| given.reviewed.tool != *call.tool() || given.reviewed.trajectory_label != live)
        {
            return Err(PlanError::ReviewMismatch.into());
        }
        // One current approval per call. A second would leave the release picking between two
        // plans the agent selected separately, and only one of them is the choice it made;
        // the approval that stands is released by proposing its call.
        if views.current_approval(call).is_some() {
            return Err(TransitionError::ApprovalPending);
        }
        // A marked spawn's plan declares the child's return policy the parent now supplies.
        let return_policy = match (recorded.plan.return_step(), &execution.return_policy) {
            (None, None) => None,
            (Some(step), Some(policy)) => {
                plan::declared_return_policy(&self.registry, views, step, policy)?;
                Some(policy.clone())
            }
            _ => return Err(TransitionError::PlanOutcomeMismatch),
        };
        let trajectory = &recorded.trajectory;
        let mut facts = accepted(views, trajectory, &recorded.subject, execution.offer);
        let advance = Sequence::advance_of(self, view, &facts);
        let subject = crate::basis::SubjectKey::Approval(execution.offer);
        facts.push(Fact::CallApproved {
            trajectory: trajectory.clone(),
            offer: execution.offer,
            call: call.clone(),
            plan: recorded.plan.id,
            acceptance: recorded.plan.narrowing().cloned(),
            rulings: recorded
                .plan
                .required
                .iter()
                .map(|required| {
                    evidence
                        .iter()
                        .find(|given| given.authority == required.authority && given.covers == required.covers)
                        .expect("the assignment check matched each required entry to exactly one response")
                        .clone()
                })
                .collect(),
            output: recorded.plan.output_remedy(),
            return_policy,
            basis: views.basis_after(&advance, &subject),
            evidence: act.pinned().clone(),
        });
        let batch = self.declaring(
            view.family(),
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            facts,
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Approved {
                call: Box::new(call.clone()),
            }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn deny_offer(
        &self,
        view: &EngineView,
        views: &Views,
        execution: &OfferExecution,
        recorded: &crate::projection::RecordedOffer,
        contract: &ToolAnnotation,
        call: &ResolvedCall,
        raw: &crate::check::RawBlock,
        stage: &CallStage,
        authority: &AuthorityName,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        if !recorded.plan.names_authority(authority) {
            return Err(TransitionError::UnassignedAuthority);
        }
        if self.registry.authority(authority).is_none() {
            return Err(PlanError::UnknownAuthority(authority.as_str().to_string()).into());
        }
        let trajectory = &recorded.trajectory;
        let mut facts = vec![Fact::Denial {
            trajectory: trajectory.clone(),
            digest: recorded.call,
            authority: authority.clone(),
        }];
        facts.extend(
            views
                .offers_naming(&recorded.call, authority)
                .into_iter()
                .map(|offer| Fact::OfferDenied {
                    trajectory: trajectory.clone(),
                    offer,
                    authority: authority.clone(),
                }),
        );
        let after = folded(view, &facts);
        let after = after.view(trajectory);
        let advance = Sequence::advance_of(self, view, &facts);
        let role = after.call_role(&recorded.subject);
        let (block, opened) = self.surface_call_block(
            &after,
            Opening {
                act: &crate::basis::DecidedAct::Offer(execution.offer),
                advance: &advance,
                nonce: &execution.offer_nonce,
                subject: &recorded.subject,
            },
            BlockedCall {
                call,
                contract,
                raw,
                stage,
                role,
            },
            act,
        )?;
        facts.extend(opened);
        let batch = self.declaring(
            view.family(),
            crate::basis::DecidedAct::Offer(execution.offer),
            advance,
            facts,
        );
        Ok(EngineDecision {
            append: Some(self.seal(view, batch)?),
            follow_up: FollowUp::Offer(OfferFollowUp::Denied { block: Box::new(block) }),
        })
    }
}

/// An offer's acceptance, ending every sibling offer pending on the same subject.
fn accepted(
    views: &Views,
    trajectory: &TrajectoryId,
    subject: &crate::basis::SubjectKey,
    offer: crate::value::OfferId,
) -> Vec<Fact> {
    let mut facts = vec![Fact::OfferAccepted {
        trajectory: trajectory.clone(),
        offer,
    }];
    facts.extend(
        views
            .pending_block(subject)
            .map(|(_, offers)| offers)
            .unwrap_or_default()
            .into_iter()
            .map(|(sibling, _)| sibling)
            .filter(|sibling| sibling != &offer)
            .map(|sibling| Fact::OfferInvalidated {
                trajectory: trajectory.clone(),
                offer: sibling,
            }),
    );
    facts
}
