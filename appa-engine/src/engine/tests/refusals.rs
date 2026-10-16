//! Which `TransitionRefusal` a forged log earns. The runtime maps every variant to its own wire
//! class, so the variant — and, where one record breaks several rules, the rule that refuses
//! first — is observable behavior.

use super::*;

fn insider() -> Audience {
    Audience::restricted([ReaderId::new("insider")])
}

/// An engine under which `get_ticket` releases with no remedy.
fn releasing_engine() -> Engine {
    engine_at(vec![crm_tool()], Label::new(TRUSTED, insider()))
}

fn ticket() -> ResolvedCall {
    call("get_ticket", json!({}))
}

/// A denial by an authority no deployment registers.
fn ghost_denial(trajectory: &TrajectoryId) -> Fact {
    Fact::Denial {
        trajectory: trajectory.clone(),
        digest: ticket().digest(),
        authority: AuthorityName::new("ghost"),
    }
}

fn position(facts: &[Fact], wanted: impl Fn(&Fact) -> bool) -> usize {
    facts.iter().position(wanted).expect("the batch carries the record")
}

fn last_opening(log: &[Fact]) -> Fact {
    log.iter()
        .rev()
        .find(|fact| matches!(fact, Fact::DispatchOpened { .. }))
        .cloned()
        .expect("the log opened a dispatch")
}

/// `get_ticket` blocked under a neutral start, and the facts of approving its one offer:
/// the log up to the approval, the approval's own facts, and the offer.
fn ticket_approval(e: &Engine) -> (Vec<Fact>, Vec<Fact>, crate::value::OfferId) {
    let log = vec![opened(e)];
    let blocked = appended_facts(blocked_batch(e, &log, "b1", nonce()));
    let offer = opened_offers(&blocked)[0].0;
    let log = [log, blocked].concat();
    let approval =
        appended_facts(execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    (log, approval, offer)
}

/// The approved `get_ticket` proposed again: the log up to the release, and the release's facts.
fn ticket_release(e: &Engine) -> (Vec<Fact>, Vec<Fact>, crate::value::OfferId) {
    let (log, approval, offer) = ticket_approval(e);
    let log = [log, approval].concat();
    let release = appended_facts(proposed(e, &log, "b2", nonce(), ticket()).expect("the approval releases"));
    (log, release, offer)
}

/// `wire` approved by `officer-a` and proposed again: the log up to the release, and the
/// release's facts, which land the officer's Ruling on the dispatch.
fn ruled_release(e: &Engine) -> (Vec<Fact>, Vec<Fact>) {
    let wire = call("wire", json!({}));
    let log = vec![opened(e)];
    let blocked = appended_facts(proposed(e, &log, "b1", nonce(), wire.clone()).expect("the batch decides"));
    let (offer, plan) = opened_offers(&blocked)[0].clone();
    let log = [log, blocked].concat();
    let evidence = evidence_for(offer, &plan, "wire", Label::new(SUSPICIOUS, Audience::public()));
    let approval =
        appended_facts(execute_offer(e, &log, offer, OfferOutcome::Approved(evidence)).expect("the officer approves"));
    let log = [log, approval].concat();
    let release = appended_facts(proposed(e, &log, "b2", nonce(), wire).expect("the approval releases"));
    (log, release)
}

/// A single marked spawn released under the bare declaration, its fork prepared and not yet
/// bound: the log and the fork.
fn released_spawn(e: &Engine) -> (Vec<Fact>, ForkId) {
    let mut log = vec![opened(e)];
    log.extend(declared_spawn(e, &log.clone(), "spawn"));
    let released = e
        .handle(
            &viewing(e, &log),
            spawn_batch("spawn-approved", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("the approved spawn releases");
    let fork = match answered(&released) {
        ([release], []) => release.fork.clone().expect("the marked spawn carries its fork"),
        other => panic!("the approved spawn releases alone, got {other:?}"),
    };
    log.extend(appended_facts(released));
    (log, fork)
}

fn bound(e: &Engine, log: &[Fact], fork: &ForkId, child: &TrajectoryId) -> Vec<Fact> {
    appended_facts(
        e.handle(
            &viewing(e, log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fork.clone(),
                child: child.clone(),
            }),
        )
        .expect("the child binds to its fork"),
    )
}

/// The opening record of `call` released at the end of `log`, re-addressed to `dispatch`.
fn opening_as(log: &[Fact], dispatch: DispatchId) -> Fact {
    let mut opening = last_opening(log);
    if let Fact::DispatchOpened { dispatch: id, .. } = &mut opening {
        *id = dispatch;
    }
    opening
}

fn with_forged_label(mut opening: Fact) -> Fact {
    if let Fact::DispatchOpened { proposed_label, .. } = &mut opening {
        *proposed_label = Label::top();
    }
    opening
}

/// `get_ticket` released and checkpointed: the log and its dispatch.
fn checkpointed_ticket(e: &Engine) -> (Vec<Fact>, DispatchId) {
    let mut log = vec![opened(e)];
    let dispatch = open(e, &mut log, &ticket());
    let checkpoint = admit::observe_success(
        &e.registry,
        &Projection::build(&log, log.len() as u64).view(&traj()),
        &dispatch,
        &ticket(),
        crate::fact::ObservedResult::Available(crate::value::RawResultDigest::of(b"the ticket")),
    )
    .expect("an open dispatch checkpoints");
    log.extend(checkpoint);
    (log, dispatch)
}

// --- one test per variant ---

#[test]
fn a_dispatch_identity_that_opens_twice_is_refused() {
    let e = releasing_engine();
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &ticket());
    assert_eq!(e.validate_replay(&log), Ok(()));

    let reopened = [log.clone(), vec![last_opening(&log)]].concat();
    assert_eq!(e.validate_replay(&reopened), Err(TransitionRefusal::DispatchReopened));
}

#[test]
fn an_opening_at_other_than_the_next_occurrence_is_refused() {
    let e = releasing_engine();
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &ticket());

    let skipped = opening_as(&log, DispatchId::new(traj(), ticket().digest(), 3));
    assert_eq!(
        e.validate_replay(&[log, vec![skipped]].concat()),
        Err(TransitionRefusal::WrongOccurrence)
    );
}

#[test]
fn a_second_success_checkpoint_is_refused() {
    let e = releasing_engine();
    let (log, _) = checkpointed_ticket(&e);
    assert_eq!(e.validate_replay(&log), Ok(()));

    let checkpoint = log
        .iter()
        .find(|fact| matches!(fact, Fact::DispatchSucceeded { .. }))
        .cloned()
        .expect("the checkpoint recorded its success");
    assert_eq!(
        e.validate_replay(&[log, vec![checkpoint]].concat()),
        Err(TransitionRefusal::RepeatCheckpoint)
    );
}

#[test]
fn a_failed_close_after_a_success_checkpoint_is_refused() {
    let e = releasing_engine();
    let (log, dispatch) = checkpointed_ticket(&e);

    let failed = Fact::DispatchClosed {
        trajectory: traj(),
        dispatch,
        outcome: crate::fact::CloseOutcome::Failure,
    };
    assert_eq!(
        e.validate_replay(&[log, vec![failed]].concat()),
        Err(TransitionRefusal::ContradictedSuccess)
    );
}

#[test]
fn a_spawn_mark_past_the_batchs_proposals_is_refused() {
    let e = releasing_engine();
    let decided = Fact::ProposalBatchDecided {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new("marked"),
        proposals: vec![ticket()],
        spawn: Some(crate::transition::SpawnMark::at(1)),
        released: Vec::new(),
        evidence: crate::audience::AudienceEvidence::default(),
    };
    assert_eq!(
        e.validate_replay(&[opened(&e), decided]),
        Err(TransitionRefusal::SpawnMarkOutOfRange)
    );
}

#[test]
fn a_member_fork_prepared_twice_is_refused() {
    let e = engine(vec![plain_tool("spawn")]);
    let (mut log, fan_out) = fan_out_family(&e, Label::new(TRUSTED, Audience::public()));
    member_started(&e, &mut log, &fan_out, &TrajectoryId::new("child"));
    assert_eq!(e.validate_replay(&log), Ok(()));

    let member = log
        .iter()
        .find(|fact| {
            matches!(
                fact,
                Fact::ForkPrepared {
                    fork: ForkId::Member { .. },
                    ..
                }
            )
        })
        .cloned()
        .expect("the child's start prepared its member fork");
    assert_eq!(
        e.validate_replay(&[log, vec![member]].concat()),
        Err(TransitionRefusal::ForkReprepared)
    );
}

#[test]
fn a_fork_that_binds_a_second_time_is_refused() {
    let e = engine(vec![]);
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);
    assert_eq!(e.validate_replay(&log), Ok(()));

    let binding = Fact::ForkOpened {
        trajectory: child.clone(),
        fork: fork_in(&log, &child),
    };
    assert_eq!(
        e.validate_replay(&[log, vec![binding]].concat()),
        Err(TransitionRefusal::ForkAlreadyBound)
    );
}

#[test]
fn a_fork_whose_spawn_failed_binds_no_child() {
    let e = engine(vec![]);
    let child = TrajectoryId::new("child");
    let (log, fork) = released_spawn(&e);
    let binding = bound(&e, &log, &fork, &child);
    assert_eq!(e.validate_replay(&[log.clone(), binding.clone()].concat()), Ok(()));

    let failed = Fact::DispatchClosed {
        trajectory: traj(),
        dispatch: fork.dispatch().clone(),
        outcome: crate::fact::CloseOutcome::Failure,
    };
    assert_eq!(
        e.validate_replay(&[log, vec![failed], binding].concat()),
        Err(TransitionRefusal::SpawnFailed)
    );
}

#[test]
fn an_attested_crossing_in_non_canonical_form_is_refused() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: Some(ReturnSanitizer::Attest(attest_shape())),
        ..floor_crossing(&e, &parent)
    });
    let log = [parent.clone(), spawn_family_at(&e, &parent, &child, policy)].concat();
    let crossing = appended_facts(
        e.handle(
            &viewing(&e, &log),
            child_report(
                &log,
                &child,
                ChildSubmission::Value {
                    body: ValueBody::new(r#"{"verdict":"allow","confidence":97}"#),
                },
            ),
        )
        .expect("a conforming submission crosses"),
    );
    let whole = [log.clone(), crossing].concat();
    assert_eq!(e.validate_replay(&whole), Ok(()));

    let mut forged = whole;
    let Fact::ChildReturn { value, .. } = &mut forged[log.len()] else {
        panic!("the crossing leads with its return record")
    };
    // The shape admits these bytes; only their canonical form crosses.
    *value = LabeledValue::new(
        ValueBody::new(r#"{ "verdict": "allow", "confidence": 97 }"#),
        value.label.clone(),
    );
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ReturnShapeViolation));
}

#[test]
fn a_staged_confined_result_closes_only_with_the_admission_that_takes_it() {
    let e = staged_engine();
    let (log, dispatch, _, staged) = staged_candidate(&e);
    let log = [log, appended_facts(staged)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let closed = Fact::DispatchClosed {
        trajectory: traj(),
        dispatch,
        outcome: crate::fact::CloseOutcome::Success {
            effects: EffectSet::default(),
        },
    };
    assert_eq!(
        e.validate_replay(&[log, vec![closed]].concat()),
        Err(TransitionRefusal::StagedClose)
    );
}

#[test]
fn a_second_approval_of_one_offer_is_refused() {
    let e = engine(vec![crm_tool()]);
    let (log, approval, _) = ticket_approval(&e);
    let log = [log, approval.clone()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let approved = approval
        .into_iter()
        .find(|fact| matches!(fact, Fact::CallApproved { .. }))
        .expect("the acceptance prepared its approval");
    assert_eq!(
        e.validate_replay(&[log, vec![approved]].concat()),
        Err(TransitionRefusal::ApprovalRepeated)
    );
}

#[test]
fn a_take_of_a_candidate_never_staged_is_refused() {
    let e = releasing_engine();
    let take = Fact::CandidateConsumed {
        trajectory: traj(),
        subject: quiet_subject(),
        dispatch: DispatchId::new(traj(), ticket().digest(), 0),
    };
    assert_eq!(
        e.validate_replay(&[opened(&e), take]),
        Err(TransitionRefusal::UnknownCandidate)
    );
}

#[test]
fn a_ruling_by_an_unregistered_authority_is_refused() {
    let e = two_officer_engine();
    let (log, release) = ruled_release(&e);
    assert_eq!(e.validate_replay(&[log.clone(), release.clone()].concat()), Ok(()));

    let mut forged = release;
    let at = position(&forged, |fact| matches!(fact, Fact::Ruling { .. }));
    let Fact::Ruling { authority, .. } = &mut forged[at] else {
        unreachable!("the position names a Ruling")
    };
    *authority = AuthorityName::new("ghost");
    assert_eq!(
        e.validate_replay(&[log, forged].concat()),
        Err(TransitionRefusal::UnknownAuthority("ghost".to_string()))
    );
}

#[test]
fn a_denial_by_an_unregistered_authority_is_refused() {
    let e = releasing_engine();
    assert_eq!(
        e.validate_replay(&[opened(&e), ghost_denial(&traj())]),
        Err(TransitionRefusal::UnknownAuthority("ghost".to_string()))
    );
}

#[test]
fn a_second_spend_of_one_approval_is_stale() {
    let e = engine(vec![crm_tool()]);
    let (log, release, offer) = ticket_release(&e);
    let log = [log, release].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let Fact::DispatchOpened { dispatch, .. } = last_opening(&log) else {
        unreachable!("last_opening returns an opening")
    };
    let subject = crate::basis::SubjectKey::Approval(offer);
    let respent = vec![
        Fact::BasisAdvanced {
            trajectory: traj(),
            act: crate::basis::DecidedAct::Proposals(crate::transition::ProposalBatchId::new("again")),
            advance: crate::basis::BasisAdvance {
                subjects: vec![subject],
                ..crate::basis::BasisAdvance::default()
            },
            revision: None,
        },
        Fact::CallApprovalConsumed {
            trajectory: traj(),
            offer,
            dispatch,
        },
    ];
    assert_eq!(
        e.validate_replay(&[log, respent].concat()),
        Err(TransitionRefusal::StaleSpend)
    );
}

#[test]
fn a_second_take_of_one_candidate_is_stale() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let offers = opened_offers(&blocked);
    let log = [log, blocked].concat();
    let hopped = execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs");
    let approved = approved_by(&hopped);
    let log = [log, appended_facts(hopped)].concat();
    let log = [log.clone(), taken(&e, &log, "b2", &approved)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let Some(Fact::CandidateConsumed { subject, dispatch, .. }) = log
        .iter()
        .find(|fact| matches!(fact, Fact::CandidateConsumed { .. }))
        .cloned()
    else {
        panic!("the proposal took the derivation")
    };
    let retaken = vec![
        Fact::BasisAdvanced {
            trajectory: traj(),
            act: crate::basis::DecidedAct::Proposals(crate::transition::ProposalBatchId::new("again")),
            advance: crate::basis::BasisAdvance {
                subjects: vec![subject.clone()],
                ..crate::basis::BasisAdvance::default()
            },
            revision: None,
        },
        Fact::CandidateConsumed {
            trajectory: traj(),
            subject,
            dispatch,
        },
    ];
    assert_eq!(
        e.validate_replay(&[log, retaken].concat()),
        Err(TransitionRefusal::StaleSpend)
    );
}

#[test]
fn an_offer_on_a_child_return_is_refused() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let mut blocked = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    assert_eq!(e.validate_replay(&[log.clone(), blocked.clone()].concat()), Ok(()));

    let at = position(&blocked, |fact| matches!(fact, Fact::OfferOpened { .. }));
    let Fact::OfferOpened { subject, .. } = &mut blocked[at] else {
        unreachable!("the position names an OfferOpened")
    };
    *subject = crate::basis::SubjectKey::Return(crate::value::ChildReturnId::new(TrajectoryId::new("child"), 0));
    assert_eq!(
        e.validate_replay(&[log, blocked].concat()),
        Err(TransitionRefusal::ForeignOffer)
    );
}

// --- precedence: which rule refuses a record that breaks two ---

#[test]
fn an_unopened_family_refuses_before_any_record_level_fault() {
    let e = releasing_engine();
    assert_eq!(
        e.validate_replay(&[ghost_denial(&traj())]),
        Err(TransitionRefusal::Unopened)
    );
}

#[test]
fn a_foreign_trajectory_refuses_before_an_undischarged_acceptance() {
    let e = engine(vec![crm_tool()]);
    let (log, mut approval, _) = ticket_approval(&e);
    let at = position(&approval, |fact| matches!(fact, Fact::CallApproved { .. }));
    approval.insert(at, ghost_denial(&TrajectoryId::new("elsewhere")));
    assert_eq!(
        e.validate_replay(&[log, approval].concat()),
        Err(TransitionRefusal::ForeignTrajectory)
    );
}

#[test]
fn an_undischarged_acceptance_refuses_before_the_records_own_checks() {
    let e = engine(vec![crm_tool()]);
    let (log, mut approval, _) = ticket_approval(&e);
    let at = position(&approval, |fact| matches!(fact, Fact::CallApproved { .. }));
    approval.insert(at, ghost_denial(&traj()));
    assert_eq!(
        e.validate_replay(&[log, approval].concat()),
        Err(TransitionRefusal::UndischargedAcceptance)
    );
}

#[test]
fn an_unbacked_obligation_refuses_before_an_unknown_authority() {
    let e = engine(vec![crm_tool()]);
    let (log, mut release, _) = ticket_release(&e);
    let at = position(&release, |fact| matches!(fact, Fact::ProposalBatchDecided { .. }));
    release.insert(at + 1, ghost_denial(&traj()));
    assert_eq!(
        e.validate_replay(&[log, release].concat()),
        Err(TransitionRefusal::UnbackedDecision)
    );
}

#[test]
fn a_released_opening_that_departs_from_its_decision_refuses_before_its_digest_check() {
    let e = releasing_engine();
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &ticket());
    let at = log.len() - 1;
    let Fact::DispatchOpened { arguments, .. } = &mut log[at] else {
        panic!("the release ends with its opening")
    };
    // The dispatch identity still carries the decided call's digest.
    *arguments = call("get_ticket", json!({ "id": "other" }))
        .canonical_arguments()
        .clone();
    assert_eq!(e.validate_replay(&log), Err(TransitionRefusal::UnbackedDecision));
}

#[test]
fn an_incomplete_menu_refuses_before_an_unknown_authority() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let blocked =
        appended_facts(proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
    assert!(opened_offers(&blocked).len() > 1, "the block offers more than one plan");
    let first = position(&blocked, |fact| matches!(fact, Fact::OfferOpened { .. }));
    let truncated = [log, blocked[..=first].to_vec(), vec![ghost_denial(&traj())]].concat();
    assert_eq!(e.validate_replay(&truncated), Err(TransitionRefusal::IncompleteMenu));
}

#[test]
fn a_record_level_fault_refuses_before_an_undeclared_advance() {
    let mut emitting = plain_tool("emit");
    emitting.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
    let e = engine(vec![emitting]);
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &call("emit", json!({})));
    assert_eq!(e.validate_replay(&log), Ok(()));

    let declared = position(&log, |fact| matches!(fact, Fact::BasisAdvanced { .. }));
    let Fact::BasisAdvanced { advance, .. } = &mut log[declared] else {
        unreachable!("the position names a BasisAdvanced")
    };
    assert!(advance.family, "the effectful release declares the family's advance");
    advance.family = false;
    assert_eq!(
        e.validate_replay(&log),
        Err(TransitionRefusal::UndeclaredAdvance),
        "the reservation alone is an advance its decision did not declare"
    );

    let at = log.len() - 1;
    log[at] = with_forged_label(log[at].clone());
    assert_eq!(e.validate_replay(&log), Err(TransitionRefusal::ForgedLabel));
}

#[test]
fn an_opening_fault_refuses_before_the_opening_records_own_label_check() {
    let e = releasing_engine();
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &ticket());

    let reopened = with_forged_label(last_opening(&log));
    assert_eq!(
        e.validate_replay(&[log.clone(), vec![reopened]].concat()),
        Err(TransitionRefusal::DispatchReopened)
    );
    // A fresh identity clears every identity check, and then stands on no release.
    let unreleased = with_forged_label(opening_as(&log, DispatchId::new(traj(), ticket().digest(), 1)));
    assert_eq!(
        e.validate_replay(&[log, vec![unreleased]].concat()),
        Err(TransitionRefusal::UnbackedDecision)
    );
}

#[test]
fn an_invalid_payload_refuses_before_a_digest_mismatch() {
    let e = engine(vec![strict_tool("send")]);
    let mut log = vec![opened(&e)];
    let valid = call("send", json!({ "to": "someone" }));
    open(&e, &mut log, &valid);

    let addressed = |call: &ResolvedCall, digest, occurrence| {
        let mut opening = opening_as(&log, DispatchId::new(traj(), digest, occurrence));
        if let Fact::DispatchOpened { arguments, .. } = &mut opening {
            *arguments = call.canonical_arguments().clone();
        }
        [log.clone(), vec![opening]].concat()
    };
    let invalid = call("send", json!({}));
    let elsewhere = call("send", json!({ "to": "elsewhere" })).digest();
    assert!(matches!(
        e.validate_replay(&addressed(&invalid, elsewhere, 1)),
        Err(TransitionRefusal::InvalidPayload(crate::params::ArgumentError::Schema(
            _
        )))
    ));
    assert_eq!(
        e.validate_replay(&addressed(&valid, elsewhere, 7)),
        Err(TransitionRefusal::DigestMismatch),
        "and a digest mismatch before a wrong occurrence"
    );
}
