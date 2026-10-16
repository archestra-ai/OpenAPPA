use super::*;

impl Engine {
    /// The guards a proposal batch passes before anything is read for a decision, in the order the
    /// boundary applies them. Returns how many provider results the log already admitted for this
    /// batch identity.
    fn admissible_batch(&self, views: &Views, batch: &ProposalBatch) -> Result<usize, TransitionError> {
        if let Some(mark) = batch.spawn {
            if mark.index() >= batch.proposals.len() {
                return Err(TransitionError::SpawnMarkOutOfRange);
            }
            if !self.profile().context_control() {
                return Err(TransitionError::SpawnUncontrolled);
            }
        }
        if batch.proposals.is_empty() && batch.provider_results.is_empty() {
            return Err(TransitionError::EmptyBatch);
        }
        // An ended branch is closed to new work, releases and admissions included.
        if views.has_ended(&batch.trajectory) {
            return Err(TransitionError::BranchEnded);
        }
        let admitted = views.provider_admissions(&batch.id).len();
        let known = admitted > 0 || views.decided_batch(&batch.id).is_some();
        if known
            && !views.provider_admissions(&batch.id).eq(batch
                .provider_results
                .iter()
                .map(|result| (&batch.trajectory, &result.tool, &result.body)))
        {
            return Err(TransitionError::BatchIdentityConflict);
        }
        for result in &batch.provider_results {
            if self.registry.provider_run_annotation(&result.tool).is_none() {
                return Err(TransitionError::Call(match self.registry.declared(&result.tool) {
                    true => EngineError::NotProviderRun(result.tool.as_str().to_string()),
                    false => EngineError::UnknownTool(result.tool.as_str().to_string()),
                }));
            }
        }
        Ok(admitted)
    }

    /// A batch identity the log already decided answers from the record and appends nothing. The
    /// repeat must carry the same trajectory and the same canonical payload, or it is a different
    /// batch wearing a spent identity.
    fn replayed_batch(
        &self,
        views: &Views,
        batch: &ProposalBatch,
        act: &ActEvidence,
    ) -> Result<Option<EngineDecision>, TransitionError> {
        let Some(decided) = views.decided_batch(&batch.id) else {
            return Ok(None);
        };
        if decided.trajectory != batch.trajectory {
            return Err(TransitionError::BatchIdentityConflict);
        }
        let recorded = decided.clone();
        let Ok(proposals) = self.resolve_proposals(batch) else {
            return Err(TransitionError::BatchIdentityConflict);
        };
        if recorded.payload != CanonicalDigest::of_batch(&proposals, batch.spawn) {
            return Err(TransitionError::BatchIdentityConflict);
        }
        // The pinned audience answers are act payload: a repeat under other answers is a
        // different act wearing a spent identity.
        if recorded.evidence != batch.audience {
            return Err(TransitionError::BatchIdentityConflict);
        }
        act.inherit(&recorded.evidence)?;
        let under = self.act_evidence(
            act.inheriting(&recorded.evidence)?,
            AudienceEvidence::default(),
            act.principal.clone(),
        )?;
        let follow_up = self.decided_follow_up(views, batch, &proposals, &recorded.released, &under)?;
        act.absorb(&under);
        Ok(Some(EngineDecision {
            append: None,
            follow_up,
        }))
    }

    /// Every proposal carries the annotation its declaration requires. The ask comes back
    /// before the batch composes; a foreign or out-of-policy claim is a refusal.
    fn answered_proposals(&self, proposals: &[ResolvedCall]) -> Result<(), TransitionError> {
        let mut unresolved = Vec::new();
        for call in proposals {
            let declaration = self
                .registry
                .declaration(call)
                .expect("a resolved call names a checkable tool");
            if let Some(annotator) = unanswered(&self.registry, declaration, call)?
                && !unresolved.contains(&annotator)
            {
                unresolved.push(annotator);
            }
        }
        if !unresolved.is_empty() {
            return Err(TransitionError::AnnotationNeeded { annotators: unresolved });
        }
        Ok(())
    }

    pub(super) fn decide_proposals(
        &self,
        view: &EngineView,
        batch: &ProposalBatch,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let views = view.projection().view(&batch.trajectory);
        let admitted = self.admissible_batch(&views, batch)?;
        if let Some(answer) = self.replayed_batch(&views, batch, act)? {
            return Ok(answer);
        }

        let mut facts: Vec<Fact> = Vec::new();
        facts.extend(match admitted {
            0 => batch
                .provider_results
                .iter()
                .enumerate()
                .map(|(position, result)| {
                    let contract = self
                        .registry
                        .provider_run_annotation(&result.tool)
                        .expect("every exposed result was classified above");
                    Fact::ValueAdmitted {
                        trajectory: batch.trajectory.clone(),
                        value: LabeledValue::new(result.body.clone(), contract.output_label()),
                        provenance: Provenance::ProviderRun {
                            tool: result.tool.clone(),
                            batch: batch.id.clone(),
                            position: position as u32,
                            effects: contract.emits.clone(),
                            evidence: act.pinned().clone(),
                        },
                    }
                })
                .collect(),
            _ => Vec::new(),
        });

        let proposals = match self.resolve_proposals(batch) {
            Ok(proposals) => proposals,
            Err((position, error)) => {
                return Ok(EngineDecision {
                    append: self.seal_admissions(view, &batch.id, facts)?,
                    follow_up: FollowUp::Malformed { position, error },
                });
            }
        };
        self.answered_proposals(&proposals)?;

        let mut working = std::borrow::Cow::Borrowed(view.projection());
        for fact in &facts {
            working.to_mut().fold(fact);
        }
        let admissions = Sequence::advance_of(self, view, &facts);
        let composed = compose_batch(
            &self.registry,
            &mut working,
            ComposingBatch {
                trajectory: &batch.trajectory,
                id: &batch.id,
            },
            &proposals,
            batch.spawn,
            &|views, call| {
                views
                    .approvals_for(call)
                    .find(|(offer, approval)| {
                        approval.basis == views.basis_after(&admissions, &crate::basis::SubjectKey::Approval(*offer))
                    })
                    .map(|(offer, _)| offer)
            },
            &|views, call, taken| {
                views
                    .call_candidates_for(call)
                    .filter(|(subject, recorded)| {
                        !taken.contains(*subject)
                            && recorded.generation == views.basis_after(&admissions, subject).subject
                    })
                    .map(|(subject, _)| subject.clone())
                    .collect()
            },
            act,
        )
        .map_err(|refusal| match refusal {
            ComposeRefusal::Malformed(error) => TransitionError::Call(error),
            ComposeRefusal::MembershipNeeded(needed) => TransitionError::from(needed),
            ComposeRefusal::Evidence(refusal) => TransitionError::ForeignEvidence(refusal),
        })?;

        let released: Vec<Released> = composed
            .iter()
            .zip(&proposals)
            .filter_map(|(release, call)| {
                release.as_ref().map(|release| Released {
                    dispatch: release.dispatch.clone(),
                    call: call.clone(),
                    fork: release.prepares_fork.map(|_| ForkId::of(&release.dispatch)),
                })
            })
            .collect();
        facts.push(Fact::ProposalBatchDecided {
            trajectory: batch.trajectory.clone(),
            batch: batch.id.clone(),
            proposals: proposals.clone(),
            spawn: batch.spawn,
            released: released.iter().map(|release| release.dispatch.clone()).collect(),
            // The act's own pinned answers: batch payload, compared on repeat. A release that
            // spends an approval reads under the approval's pins too, but those are the
            // approval record's — replay re-merges them from it.
            evidence: act.pinned().clone(),
        });
        facts.extend(composed.iter().flatten().flat_map(|release| release.facts.clone()));
        // What this decision moves, derived before the offers that have to record where it lands.
        // The declaration prepended below re-derives it over the whole batch; an offer record
        // moves nothing, so the two agree by construction.
        let advance = Sequence::advance_of(self, view, &facts);
        let final_views = working.view(&batch.trajectory);
        let mut refused: Vec<(usize, &ResolvedCall, ToolAnnotation, RawBlock, CallRole)> = Vec::new();
        for (position, call) in composed
            .iter()
            .enumerate()
            .filter(|(_, release)| release.is_none())
            .map(|(position, _)| (position, &proposals[position]))
        {
            let contract = self.validated_contract(call)?.into_owned();
            let role = CallRole::of(batch.spawn, position);
            let raw = match check::evaluate(
                &contract,
                &final_views,
                call,
                &CallStage::default(),
                role,
                &self.context(act),
            )? {
                CheckOutcome::Block(raw) => raw,
                CheckOutcome::Allow => {
                    unreachable!("an in-batch release only ever adds gaps to a refused sibling's block")
                }
            };
            refused.push((position, call, contract, raw, role));
        }
        let block_stage: Vec<SymbolicAtom> = refused
            .iter()
            .flat_map(|(_, _, contract, raw, _)| plan::block_atoms(&self.registry, contract, raw))
            .collect();
        require_atoms(act, block_stage)?;
        let mut blocked = Vec::new();
        for (position, call, contract, raw, role) in refused {
            let subject = crate::basis::SubjectKey::Call {
                trajectory: batch.trajectory.clone(),
                batch: batch.id.clone(),
                position: position as u32,
            };
            let (block, opened_offers) = self.surface_call_block(
                &final_views,
                Opening {
                    act: &crate::basis::DecidedAct::Proposals(batch.id.clone()),
                    advance: &advance,
                    nonce: &batch.offer_nonce,
                    subject: &subject,
                },
                BlockedCall {
                    call,
                    contract: &contract,
                    raw: &raw,
                    stage: &CallStage::default(),
                    role,
                },
                act,
            )?;
            facts.extend(opened_offers);
            blocked.push(block);
        }
        let decided = self.declaring(crate::basis::DecidedAct::Proposals(batch.id.clone()), advance, facts);
        let append = self.seal(view, decided)?;
        Ok(EngineDecision {
            append: Some(append),
            follow_up: FollowUp::Proposals {
                released,
                blocked,
                spent: Vec::new(),
                settled: Vec::new(),
            },
        })
    }

    fn resolve_proposals(&self, batch: &ProposalBatch) -> Result<Vec<ResolvedCall>, (usize, EngineError)> {
        let mut file_bases = vec![None; batch.proposals.len()];
        for evidence in &batch.evidence {
            if let Evidence::File { position, basis } = evidence {
                let Some(slot) = file_bases.get_mut(*position) else {
                    return Err((
                        *position,
                        EngineError::InvalidFileEvidence("position is outside the proposal batch".into()),
                    ));
                };
                if slot.replace(basis.clone()).is_some() {
                    return Err((*position, EngineError::InvalidFileEvidence("duplicate position".into())));
                }
            }
        }
        let proposals: Vec<ResolvedCall> = batch
            .proposals
            .iter()
            .enumerate()
            .map(|(position, proposed)| {
                self.resolve_call(proposed.tool.clone(), &proposed.arguments)
                    .map(|call| {
                        call.with_annotation(proposed.annotation.clone())
                            .with_file_basis(file_bases[position].clone())
                    })
                    .map_err(|error| (position, error))
            })
            .collect::<Result<_, _>>()?;
        Ok(proposals)
    }

    fn seal_admissions(
        &self,
        view: &EngineView,
        id: &crate::transition::ProposalBatchId,
        facts: Vec<Fact>,
    ) -> Result<Option<ValidatedFactBatch>, TransitionError> {
        if facts.is_empty() {
            return Ok(None);
        }
        let advance = Sequence::advance_of(self, view, &facts);
        let batch = self.declaring(crate::basis::DecidedAct::Proposals(id.clone()), advance, facts);
        Ok(Some(self.seal(view, batch)?))
    }

    pub(super) fn surface_call_block(
        &self,
        views: &Views,
        opening: Opening<'_>,
        blocked: BlockedCall<'_>,
        act: &ActEvidence,
    ) -> Result<(Blocked, Vec<Fact>), TransitionError> {
        let call = blocked.call;
        let planned = plan::plan(&self.registry, views, blocked, &self.context(act))?;
        let (block_id, offers, opened) =
            self.open_offers(views, opening, &call.digest(), &Engine::executable(&planned), act);
        Ok((
            Blocked {
                call: call.clone(),
                block: planned,
                block_id,
                offers,
            },
            opened,
        ))
    }

    pub(super) fn open_offers(
        &self,
        views: &Views,
        opening: Opening<'_>,
        call: &crate::value::CanonicalDigest,
        plans: &[plan::ExecutableRemedyPlan],
        under: &ActEvidence,
    ) -> (
        crate::value::BlockId,
        Vec<(crate::value::OfferId, plan::PlanId)>,
        Vec<Fact>,
    ) {
        let Opening {
            act,
            advance,
            nonce,
            subject,
        } = opening;
        let basis = views.basis_after(advance, subject);
        let (trajectory, block_id) = match subject {
            crate::basis::SubjectKey::Call {
                trajectory,
                batch,
                position,
            } => (
                trajectory,
                crate::value::BlockId::of_proposal(nonce, trajectory, batch, *position, call),
            ),
            crate::basis::SubjectKey::ConfinedResult(dispatch) => (
                dispatch.trajectory(),
                crate::value::BlockId::of_candidate(nonce, dispatch, basis.subject),
            ),
            // A return crosses at the child's stop or is refused there; a prepared approval is
            // spent by releasing its call. Neither carries a stage of its own.
            crate::basis::SubjectKey::Return(_) => unreachable!("no stage stands on a return"),
            crate::basis::SubjectKey::Approval(_) => unreachable!("no stage stands on an approval"),
        };
        let mut ids = Vec::new();
        let mut facts = Vec::new();
        for (index, executable) in plans.iter().enumerate() {
            let offer = crate::value::OfferId::of_plan(
                &block_id,
                index as u32,
                &serde_json_canonicalizer::to_vec(executable).expect("a derived plan canonicalizes"),
            );
            ids.push((offer, executable.id));
            facts.push(Fact::OfferOpened {
                trajectory: trajectory.clone(),
                offer,
                block: block_id,
                act: act.clone(),
                call: *call,
                subject: subject.clone(),
                plan: executable.clone(),
                basis,
                evidence: under.pinned().clone(),
            });
        }
        (block_id, ids, facts)
    }

    fn executable(planned: &PlannedBlock) -> Vec<plan::ExecutableRemedyPlan> {
        planned
            .plans
            .iter()
            .filter_map(plan::RemedyPlan::executable)
            .cloned()
            .collect()
    }

    fn decided_follow_up(
        &self,
        views: &Views,
        batch: &ProposalBatch,
        proposals: &[ResolvedCall],
        recorded: &[DispatchId],
        act: &ActEvidence,
    ) -> Result<FollowUp, TransitionError> {
        let subject_at = |position: usize| crate::basis::SubjectKey::Call {
            trajectory: batch.trajectory.clone(),
            batch: batch.id.clone(),
            position: position as u32,
        };
        // The call each position is about now: the candidate an input hop derived, under the
        // contract its own arguments select and with the pinned evidence that hop consumed,
        // or the proposal.
        let standing: Vec<(&ResolvedCall, ActEvidence)> = proposals
            .iter()
            .enumerate()
            .map(|(position, call)| {
                let subject = subject_at(position);
                let pinned = views.candidate_evidence(&subject);
                act.inherit(pinned)?;
                let under = self.act_evidence(
                    act.inheriting(pinned)?,
                    AudienceEvidence::default(),
                    act.principal.clone(),
                )?;
                Ok((views.standing_call(&subject).unwrap_or(call), under))
            })
            .collect::<Result<_, TransitionError>>()?;
        let mut released = Vec::new();
        let mut blocked = Vec::new();
        let mut spent = Vec::new();
        let mut settled = Vec::new();
        let mut next = recorded.iter().peekable();
        for (position, call) in proposals.iter().enumerate() {
            match next.next_if(|dispatch| views.dispatch_call(dispatch) == Some(call)) {
                // Only a dispatch still awaiting its result may be handed back for invocation.
                Some(dispatch) if views.is_open(dispatch) && !views.is_succeeded(dispatch) => released.push(Released {
                    dispatch: dispatch.clone(),
                    call: call.clone(),
                    fork: prepared_fork(views, dispatch),
                }),
                Some(dispatch) => {
                    settled.push(Settled {
                        dispatch: dispatch.clone(),
                        call: call.clone(),
                        outcome: settled_outcome(views, dispatch),
                    });
                }
                None => {
                    let subject = subject_at(position);
                    // An input hop the agent has since run replaced this proposal, so the call
                    // this position is about now is the candidate, and the check reads its
                    // substitution. Its offers are the ones already pending on
                    // the same subject, which is why the two must be reported together.
                    let (candidate, under) = &standing[position];
                    let candidate = (*candidate).clone();
                    let contract = self.validated_contract(&candidate)?;
                    let stage = views.call_stage(&subject);
                    let role = views.call_role(&subject);
                    match check::evaluate(&contract, views, &candidate, &stage, role, &self.context(under))? {
                        CheckOutcome::Block(raw) => {
                            require_atoms(under, plan::block_atoms(&self.registry, &contract, &raw))?;
                            let (block_id, offers) = views.pending_block(&subject).unwrap_or_else(|| {
                                let block_id = crate::value::BlockId::of_proposal(
                                    &batch.offer_nonce,
                                    &batch.trajectory,
                                    &batch.id,
                                    position as u32,
                                    &candidate.digest(),
                                );
                                (block_id, Vec::new())
                            });
                            blocked.push(Blocked {
                                block: plan::plan(
                                    &self.registry,
                                    views,
                                    BlockedCall {
                                        call: &candidate,
                                        contract: &contract,
                                        raw: &raw,
                                        stage: &stage,
                                        role,
                                    },
                                    &self.context(under),
                                )?,
                                call: candidate,
                                block_id,
                                offers,
                            });
                        }
                        CheckOutcome::Allow => match views.subject_dispatch(&subject).cloned() {
                            Some(dispatch) if views.is_open(&dispatch) && !views.is_succeeded(&dispatch) => {
                                released.push(Released {
                                    dispatch,
                                    call: candidate,
                                    fork: None,
                                });
                            }
                            Some(dispatch) => settled.push(Settled {
                                outcome: settled_outcome(views, &dispatch),
                                dispatch,
                                call: candidate,
                            }),
                            None => spent.push(candidate),
                        },
                    }
                }
            }
        }
        for (_, under) in &standing {
            act.absorb(under);
        }
        Ok(FollowUp::Proposals {
            released,
            blocked,
            spent,
            settled,
        })
    }
}

/// The Annotator whose annotation a call still owes, if any. A foreign or
/// out-of-policy answer is a refusal.
fn unanswered(
    registry: &Registry,
    declaration: &crate::contract::ToolDeclaration,
    call: &ResolvedCall,
) -> Result<Option<crate::names::AnnotatorName>, TransitionError> {
    match check::validate_annotation(registry, declaration, call) {
        Ok(()) => Ok(None),
        Err(check::AnnotationRefusal::Needed(annotator)) => Ok(Some(annotator)),
        Err(check::AnnotationRefusal::Foreign(reason)) => Err(TransitionError::ForeignAnnotation { reason }),
        Err(check::AnnotationRefusal::OutsidePolicy(reason)) => Err(TransitionError::InvalidAnnotation { reason }),
    }
}

fn settled_outcome(views: &Views, dispatch: &DispatchId) -> SettledOutcome {
    match views.is_open(dispatch) {
        true => SettledOutcome::Confined,
        false => SettledOutcome::Closed {
            admitted: views.admitted_body(dispatch).cloned(),
        },
    }
}

fn prepared_fork(views: &Views, dispatch: &DispatchId) -> Option<ForkId> {
    let fork = ForkId::of(dispatch);
    (views.is_prepared(&fork) && views.bound_child_of(&fork).is_none() && !views.dispatch_failed(dispatch))
        .then_some(fork)
}
