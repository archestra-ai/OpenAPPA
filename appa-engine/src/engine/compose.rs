use super::*;

fn approved_release(
    registry: &Registry,
    contract: &ToolAnnotation,
    output: &Label,
    trajectory: &TrajectoryId,
    dispatch: &DispatchId,
    approval: &crate::projection::PreparedApproval,
    act: &ActEvidence,
) -> Vec<Fact> {
    let context = membership_context(registry, act);
    let mut facts = Vec::new();
    if let Some(narrowing) = &approval.acceptance {
        facts.push(Fact::Acceptance {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
            narrowing: narrowing.clone(),
        });
    }
    facts.extend(approval.rulings.iter().map(|given| Fact::Ruling {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        plan: approval.plan,
        authority: given.authority.clone(),
        covers: given.covers.clone(),
        reviewed: given.reviewed.clone(),
        evidence: act.pinned().clone(),
    }));
    match &approval.output {
        Some(crate::plan::OutputRemedy::Withhold) => facts.push(Fact::OutputWithheld {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            plan: approval.plan,
        }),
        Some(crate::plan::OutputRemedy::Sanitize(sanitizer)) => {
            facts.push(Fact::OutputSanitizerBound {
                trajectory: trajectory.clone(),
                dispatch: dispatch.clone(),
                plan: approval.plan,
                sanitizer: sanitizer.clone(),
                contribution: crate::plan::bound_contribution(registry, contract, output, sanitizer, &context)
                    .expect("the compose gate answers a spent approval's sanitizer atoms")
                    .expect("a prepared approval binds an output sanitizer enumeration found applicable"),
                evidence: act.pinned().clone(),
            });
        }
        None => {}
    }
    facts
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, its occurrence (a repeat identical call is a new dispatch), and the subject
/// whose decision released it.
pub(crate) fn opened_dispatch(
    contract: &ToolAnnotation,
    views: &Views,
    call: &ResolvedCall,
    subject: crate::basis::SubjectKey,
    act: &ActEvidence,
) -> (DispatchId, Fact) {
    let digest = call.digest();
    let occurrence = views.dispatch_count(&digest);
    let dispatch = DispatchId::new(views.trajectory().clone(), digest, occurrence);
    let current = views.current_label();
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        tool: call.tool().clone(),
        declaration: call.declaration_id(),
        arguments: call.canonical_arguments().clone(),
        proposed_label: current.combine(&call.output_label(contract, &current)),
        receiving: current.clone(),
        proposed_effects: contract.emits.clone(),
        annotation: call.annotation().cloned(),
        file_basis: call.file_basis().cloned(),
        evidence: act.pinned().clone(),
        subject,
        terms: Some(crate::fact::ReleaseTerms::of(contract)),
    };
    (dispatch, fact)
}

pub(crate) struct SiblingRelease {
    pub(crate) dispatch: DispatchId,
    pub(crate) consumes: Option<crate::value::OfferId>,
    /// The staged derivation this release took, by the subject that staged it.
    pub(crate) takes: Option<crate::basis::SubjectKey>,
    /// The kind of fork a marked spawn's release prepares, at `ForkId::of(&dispatch)`.
    pub(crate) prepares_fork: Option<crate::transition::SpawnKind>,
    pub(crate) facts: Vec<Fact>,
    /// The pinned audience evidence this release's check read under: the act's, behind the
    /// spent approval's where one is consumed.
    pub(crate) evidence: AudienceEvidence,
}

/// Which batch a composition is running: the trajectory it belongs to, and the batch's own id.
/// Together they name each position's subject — what the openings record, so a repeat
/// answers with the dispatch its own position opened.
#[derive(Clone, Copy)]
pub(crate) struct ComposingBatch<'a> {
    pub(crate) trajectory: &'a TrajectoryId,
    pub(crate) id: &'a crate::transition::ProposalBatchId,
}

impl ComposingBatch<'_> {
    pub(crate) fn subject(&self, position: usize) -> crate::basis::SubjectKey {
        crate::basis::SubjectKey::Call {
            trajectory: self.trajectory.clone(),
            batch: self.id.clone(),
            position: position as u32,
        }
    }
}

pub(crate) enum ComposeRefusal {
    Malformed(EngineError),
    MembershipNeeded(crate::label::MembershipNeeded),
    Evidence(crate::audience::EvidenceRefusal),
}

pub(crate) fn membership_context<'e>(registry: &'e Registry, act: &'e ActEvidence) -> MembershipContext<'e> {
    let audience = registry.audience();
    MembershipContext::new(audience.within_assertions(), audience.providers(), &act.expansions)
}

/// The atoms a spent approval's consumption reads: each ruling's mandate over its covered
/// gaps. The bound output sanitizer reads its transition against the result when it applies.
fn approval_atoms(registry: &Registry, approval: &crate::projection::PreparedApproval) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    approval
        .rulings
        .iter()
        .filter_map(|given| registry.authority(&given.authority).map(|authority| (authority, given)))
        .flat_map(|(authority, given)| authority.mandate.reads(&given.covers, providers))
        .collect()
}

/// The ordered in-batch composition, position by position: what each proposed sibling
/// does, and the records that say so. `None` at a position is a refusal.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_batch<'a>(
    registry: &Registry,
    working: &mut std::borrow::Cow<'a, Projection>,
    batch: ComposingBatch<'_>,
    proposals: &[ResolvedCall],
    spawn: Option<SpawnMark>,
    approval: &impl Fn(&Views, &ResolvedCall) -> Option<crate::value::OfferId>,
    staged: &impl Fn(
        &Views,
        &ResolvedCall,
        &std::collections::BTreeSet<crate::basis::SubjectKey>,
    ) -> Vec<crate::basis::SubjectKey>,
    act: &ActEvidence,
) -> Result<Vec<Option<SiblingRelease>>, ComposeRefusal> {
    let trajectory = batch.trajectory;
    let singleton = proposals.len() == 1;
    let mut needed: Vec<SymbolicAtom> = Vec::new();
    // Owned only where a spent approval overlays its pins; every other sibling reads the act's.
    let mut per_call: Vec<(std::borrow::Cow<'_, ActEvidence>, Option<crate::value::OfferId>)> =
        Vec::with_capacity(proposals.len());
    {
        let views = working.view(trajectory);
        for (position, call) in proposals.iter().enumerate() {
            if contract_for_call(registry, call).is_err() {
                // Reported as malformed by the composition below, at its position.
                per_call.push((std::borrow::Cow::Borrowed(act), None));
                continue;
            }
            // An approval declaring a child's return policy spends only at the marked spawn's
            // position, and one declaring none only at an ordinary one.
            let marked = matches!(CallRole::of(spawn, position), CallRole::MarkedSpawn(_));
            let spends = if singleton { approval(&views, call) } else { None }.filter(|offer| {
                views
                    .approval(offer)
                    .is_some_and(|prepared| prepared.return_policy.is_some() == marked)
            });
            let under = match spends.and_then(|offer| views.approval(&offer)) {
                Some(prepared) => std::borrow::Cow::Owned(
                    act.overlay(&prepared.evidence, registry.audience())
                        .map_err(ComposeRefusal::Evidence)?,
                ),
                None => std::borrow::Cow::Borrowed(act),
            };
            // The check's own reads surface from its three-valued evaluation below — asking
            // by the label's actual state, not the contract's whole vocabulary. Only a spent
            // approval's consumption reads atoms the evaluation never touches.
            if let Some(prepared) = spends.and_then(|offer| views.approval(&offer)) {
                needed.extend(
                    approval_atoms(registry, prepared)
                        .into_iter()
                        .filter(|atom| !under.expansions.answered(atom)),
                );
            }
            per_call.push((under, spends));
        }
    }
    if !needed.is_empty() {
        needed.sort();
        needed.dedup();
        return Err(ComposeRefusal::MembershipNeeded(crate::label::MembershipNeeded {
            needed,
        }));
    }
    let mut composed = Vec::with_capacity(proposals.len());
    // Whether any earlier sibling was refused, and so will be re-planned against the final state.
    let mut refused = false;
    // What earlier siblings of this batch have already taken. One derivation releases one call,
    // and a sibling's take does not reach `working` — only the batch's own `BasisAdvanced` does —
    // so two identical proposals would otherwise both stand on it.
    let mut taken: std::collections::BTreeSet<crate::basis::SubjectKey> = std::collections::BTreeSet::new();
    for (position, call) in proposals.iter().enumerate() {
        let (under, spends) = &per_call[position];
        let release = {
            let views = working.view(trajectory);
            let malformed = ComposeRefusal::Malformed;
            let contract = contract_for_call(registry, call).map_err(malformed)?;
            contract
                .parameters
                .validate(call.arguments())
                .map_err(|error| malformed(EngineError::InvalidCall(error)))?;
            let role = CallRole::of(spawn, position);
            // The derivations earlier blocks' remedies staged for exactly these bytes. Taking one
            // puts the call at that sanitizer's stage — its derived label and spent lineage —
            // while every other term is judged against current state, so a trajectory that
            // narrowed since the derivation landed re-gates the call rather than releasing it on
            // the terms it was staged under. A marked spawn takes none: its fork's return policy
            // rides on an approval, which a derivation does not carry.
            //
            // A derivation's label is a claim about the bytes, so where several stand for the same
            // bytes the call clears if any of them covers it: it is judged under each in subject
            // order and takes the first that clears, else it blocks under the first.
            let stages: Vec<(crate::basis::SubjectKey, CallStage)> = match role {
                CallRole::MarkedSpawn(_) => Vec::new(),
                CallRole::Ordinary => staged(&views, call, &taken)
                    .into_iter()
                    .filter_map(|subject| {
                        let recorded = views.recorded_candidate(&subject)?;
                        Some((
                            subject,
                            CallStage::of(Some(&recorded.derived), recorded.lineage.clone()),
                        ))
                    })
                    .collect(),
            };
            let context = membership_context(registry, under);
            let mut judged: Option<(crate::basis::SubjectKey, CheckOutcome)> = None;
            for (subject, stage) in stages {
                let outcome = check::evaluate(&contract, &views, call, &stage, role, &context)
                    .map_err(ComposeRefusal::MembershipNeeded)?;
                let clears = matches!(outcome, CheckOutcome::Allow);
                if clears || judged.is_none() {
                    judged = Some((subject, outcome));
                }
                if clears {
                    break;
                }
            }
            let (takes, outcome) = match judged {
                Some((subject, outcome)) => (Some(subject), outcome),
                None => (
                    None,
                    check::evaluate(&contract, &views, call, &CallStage::default(), role, &context)
                        .map_err(ComposeRefusal::MembershipNeeded)?,
                ),
            };
            let consumes = match outcome {
                CheckOutcome::Allow => None,
                CheckOutcome::Block(_) => match spends {
                    Some(offer) => Some(*offer),
                    None => {
                        refused = true;
                        composed.push(None);
                        continue;
                    }
                },
            };
            let subject = batch.subject(position);
            let (dispatch, opening) = opened_dispatch(&contract, &views, call, subject, under);
            let mut facts = Vec::new();
            if let Some(staged) = &takes {
                facts.push(Fact::CandidateConsumed {
                    trajectory: trajectory.clone(),
                    subject: staged.clone(),
                    dispatch: dispatch.clone(),
                });
            }
            if let Some(offer) = consumes {
                let prepared = views
                    .approval(&offer)
                    .expect("the currency test answered with an approval this state records")
                    .clone();
                facts.push(Fact::CallApprovalConsumed {
                    trajectory: trajectory.clone(),
                    offer,
                    dispatch: dispatch.clone(),
                });
                facts.extend(approved_release(
                    registry,
                    &contract,
                    &call.output_label(&contract, &views.current_label()),
                    trajectory,
                    &dispatch,
                    &prepared,
                    under,
                ));
            }
            facts.push(opening);
            let prepares_fork = if matches!(role, CallRole::MarkedSpawn(_)) {
                let return_policy = consumes
                    .and_then(|offer| views.approval(&offer))
                    .and_then(|prepared| prepared.return_policy.clone())
                    .expect(
                        "a marked spawn releases only by spending an approval that declares its child's return policy",
                    );
                let kind = spawn
                    .map(SpawnMark::kind)
                    .expect("a marked spawn's role comes from the batch's spawn mark");
                facts.push(Fact::ForkPrepared {
                    trajectory: trajectory.clone(),
                    fork: ForkId::of(&dispatch),
                    snapshot: views.freeze_basis(),
                    return_policy,
                    kind,
                });
                Some(kind)
            } else {
                None
            };
            if let Some(subject) = &takes {
                taken.insert(subject.clone());
            }
            SiblingRelease {
                dispatch,
                consumes,
                takes: takes.clone(),
                prepares_fork,
                facts,
                evidence: under.pinned().clone(),
            }
        };
        if position + 1 < proposals.len() || refused {
            working.to_mut().fold_all(&release.facts);
        }
        composed.push(Some(release));
    }
    // A spent approval's overlay context read on behalf of this act; the act's
    // operation-scope justification must count those asks.
    for (under, _) in &per_call {
        if let std::borrow::Cow::Owned(under) = under {
            act.absorb(under);
        }
    }
    Ok(composed)
}

fn contract_for_call<'a>(
    registry: &'a Registry,
    call: &'a ResolvedCall,
) -> Result<std::borrow::Cow<'a, ToolAnnotation>, EngineError> {
    registry.annotation_of(call).ok_or_else(|| {
        if registry.provider_run_annotation(call.tool()).is_some() {
            EngineError::ProviderRunTool(call.tool().as_str().to_string())
        } else {
            EngineError::UnknownTool(call.tool().as_str().to_string())
        }
    })
}
