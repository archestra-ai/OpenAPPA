mod held;

use super::returns::{execute_offer_declaring, gapped_spawn_engine, gapped_spawn_registry};
use super::*;
use crate::transition::{ReturnDecision, ReturnSettlement, SettlementFollowUp, SpawnMark};

fn stop(log: &[Fact], child: &TrajectoryId, body: &str) -> EngineEvent {
    child_report(
        log,
        child,
        ChildSubmission::Value {
            body: ValueBody::new(body),
        },
    )
}

fn settle(child: &TrajectoryId, occurrence: u32, decision: ReturnDecision) -> EngineEvent {
    EngineEvent::SettleReturn(ReturnSettlement {
        trajectory: traj(),
        id: ChildReturnId::new(child.clone(), occurrence),
        decision,
    })
}

fn label_of(e: &Engine, log: &[Fact], trajectory: &TrajectoryId) -> Label {
    viewing(e, log)
        .views(trajectory)
        .expect("the trajectory is opened")
        .current_label()
}

/// A parent with `child` spawned under a deferred return.
fn deferring_family(e: &Engine, child: &TrajectoryId) -> Vec<Fact> {
    let parent = vec![opened(e)];
    let forked = spawn_family_marked(
        e,
        &parent,
        &traj(),
        child,
        ReturnPolicy::Deferred,
        SpawnMark::deferred(0),
    );
    [parent, forked].concat()
}

/// [`deferring_family`] after the child read what narrows it and stopped with `body`.
fn frozen_family(e: &Engine, child: &TrajectoryId, body: &str) -> Vec<Fact> {
    let mut log = deferring_family(e, child);
    reads(e, &mut log, child, "read_suspicious_internal");
    let deferred = e
        .handle(&viewing(e, &log), stop(&log, child, body))
        .expect("a narrowing return under a deferred fork is frozen");
    assert_eq!(
        deferred.follow_up,
        FollowUp::Child(ChildFollowUp::Deferred {
            id: ChildReturnId::new(child.clone(), 0)
        })
    );
    [log, appended_facts(deferred)].concat()
}

#[test]
fn a_deferred_child_narrows_freely_and_its_narrowing_return_is_frozen_at_the_stop() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let before = deferring_family(&e, &child);
    assert_eq!(e.validate_replay(&before), Ok(()));
    let standing = label_of(&e, &before, &traj());

    let log = frozen_family(&e, &child, "the answer");
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert!(matches!(
        log.last(),
        Some(Fact::ReturnDeferred { trajectory, value, .. })
            if trajectory == &child && value.body == ValueBody::new("the answer")
    ));
    assert_eq!(label_of(&e, &log, &traj()), standing, "nothing crossed");
    let views = viewing(&e, &log);
    let root = traj();
    let parent = views.views(&root).expect("the parent is opened");
    assert!(parent.has_ended(&child), "the deferral ends the child's errand");
    assert_eq!(parent.returns_by(&child), 1, "the deferral is its own occurrence");
    assert_eq!(
        e.handle(&viewing(&e, &log), stop(&log, &child, "the answer")),
        Err(TransitionError::BranchEnded)
    );
    assert_eq!(
        e.handle(&viewing(&e, &log), child_report(&log, &child, ChildSubmission::Void)),
        Err(TransitionError::BranchEnded)
    );
}

#[test]
fn accepting_a_deferred_return_crosses_the_frozen_value_and_narrows_the_parent() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    let frozen = label_of(&e, &log, &child);
    let from = label_of(&e, &log, &traj());

    let accepted = e
        .handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Accept))
        .expect("the parent accepts the frozen value");
    assert!(matches!(
        &accepted.follow_up,
        FollowUp::Settlement(SettlementFollowUp::Accepted { admitted }) if admitted.body == ValueBody::new("the answer")
    ));
    let facts = appended_facts(accepted);
    let to = from.combine(&frozen);
    assert!(matches!(
        declared_records(&facts),
        [
            Fact::ReturnAccepted { narrowing, .. },
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                ..
            },
            Fact::ValueAdmitted { .. },
            Fact::Boundary {
                kind: crate::fact::BoundaryKind::Merge { .. },
                ..
            }
        ] if narrowing.from == from && narrowing.to == to
    ));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(label_of(&e, &log, &traj()), to);
    assert_ne!(to, from, "the acceptance narrowed the parent");

    for repeat in [ReturnDecision::Accept, ReturnDecision::Discard] {
        let again = e
            .handle(&viewing(&e, &log), settle(&child, 0, repeat))
            .expect("a settled return answers from the log");
        assert_eq!(again.append, None);
        assert!(matches!(
            &again.follow_up,
            FollowUp::Settlement(SettlementFollowUp::Accepted { admitted }) if admitted.body == ValueBody::new("the answer")
        ));
    }
}

#[test]
fn discarding_a_deferred_return_crosses_nothing() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    let standing = label_of(&e, &log, &traj());

    let discarded = e
        .handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Discard))
        .expect("the parent discards the frozen value");
    assert_eq!(discarded.follow_up, FollowUp::Settlement(SettlementFollowUp::Discarded));
    let facts = appended_facts(discarded);
    assert!(matches!(declared_records(&facts), [Fact::ReturnDiscarded { .. }]));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(label_of(&e, &log, &traj()), standing);
    assert!(!log.iter().any(|fact| matches!(
        fact,
        Fact::ChildReturn { .. }
            | Fact::ValueAdmitted {
                provenance: Provenance::ChildReturn { .. },
                ..
            }
    )));

    let again = e
        .handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Accept))
        .expect("a settled return answers from the log");
    assert_eq!(again.append, None);
    assert_eq!(again.follow_up, FollowUp::Settlement(SettlementFollowUp::Discarded));
}

/// The value freezes at `suspicious`, the middle rank; the next revision drops the rank below
/// it, so `suspicious` moves to the lowest ordinal and the frozen ordinal names `trusted` there.
#[test]
fn a_frozen_return_crosses_into_a_later_revision_s_trust_chain() {
    let read = suspicious_internal_read();
    let mut cfg = returning_registry(vec![]);
    cfg.trust_chain = TrustChain::new(vec!["untrusted".into(), "suspicious".into(), "trusted".into()]);
    cfg.tools = declared(vec![ToolAnnotation {
        delta: Delta {
            trust: Some(Trust::new(1)),
            ..read.delta.clone()
        },
        ..read
    }]);
    let first = open_engine(cfg);
    let second = open_engine(returning_registry(vec![])).at(DeploymentName::new("default"), PolicyRevision::new(2));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&first, &child, "the answer");
    let batches = [StampedBatch {
        revision: Some(PolicyRevision::FIRST),
        facts: &log,
    }];
    let view = second.replay(&traj(), &batches, &[&first]).expect("the log replays");
    let crossed = view.views(&child).expect("the child is opened").current_label();
    let root = traj();
    let parent = view.views(&root).expect("the parent is opened");
    assert_eq!(
        parent
            .deferral(&ChildReturnId::new(child.clone(), 0))
            .map(|deferral| &deferral.value.label),
        Some(&crossed)
    );
    let from = parent.current_label();

    let accepted = appended_facts(
        second
            .handle(&view, settle(&child, 0, ReturnDecision::Accept))
            .expect("the parent accepts the frozen value"),
    );
    assert!(matches!(
        declared_records(&accepted),
        [Fact::ReturnAccepted { narrowing, .. }, ..]
            if narrowing.from == from && narrowing.to == from.combine(&crossed)
    ));
}

#[test]
fn a_deferred_return_that_leaves_the_parent_where_it_is_crosses_at_the_stop() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = deferring_family(&e, &child);

    let crossed = e
        .handle(&viewing(&e, &log), stop(&log, &child, "done"))
        .expect("a return that narrows nothing crosses");
    assert!(matches!(
        &crossed.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { admitted }) if admitted.body == ValueBody::new("done")
    ));
    let log = [log, appended_facts(crossed)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    let views = viewing(&e, &log);
    let root = traj();
    let parent = views.views(&root).expect("the parent is opened");
    assert!(!parent.has_ended(&child), "a crossing ends nothing");
    assert_eq!(
        e.handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Accept)),
        Err(TransitionError::NoDeferredReturn)
    );
}

/// The grandchild's deferral stands under its parent's floor, whose sanitizer leaves trust
/// unbound at revision 1; revision 2 drops the sanitizer, so the floor binds trust there.
#[test]
fn a_deferred_return_that_crosses_at_the_stop_is_held_to_the_inherited_floor() {
    let first = open_engine(returning_registry(vec![lifting_sanitizer("scrubber")]));
    let second = open_engine(returning_registry(vec![])).at(DeploymentName::new("default"), PolicyRevision::new(2));
    let (child, grandchild) = (TrajectoryId::new("child"), TrajectoryId::new("grandchild"));
    let root = vec![opened(&first)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: Some(ReturnSanitizer::Named(SanitizerName::new("scrubber"))),
        ..floor_crossing(&first, &root)
    });
    let mut log = [root.clone(), spawn_family_at(&first, &root, &child, policy)].concat();
    reads(&first, &mut log, &child, "read_suspicious");
    log.extend(spawn_family_marked(
        &first,
        &log,
        &child,
        &grandchild,
        ReturnPolicy::Deferred,
        SpawnMark::deferred(0),
    ));
    let recorded = StampedBatch {
        revision: Some(PolicyRevision::FIRST),
        facts: &log,
    };
    let view = second.replay(&traj(), &[recorded], &[&first]).expect("the log replays");
    assert_eq!(
        second.handle(&view, stop(&log, &grandchild, "done")),
        Err(TransitionError::ReturnBelowFloor {
            floor: Label::new(TRUSTED, Audience::public())
        })
    );

    let crossed = first
        .handle(&viewing(&first, &log), stop(&log, &grandchild, "done"))
        .expect("the sanitizer leaves the floor's trust unbound");
    assert!(matches!(
        &crossed.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { admitted }) if admitted.body == ValueBody::new("done")
    ));
    let mut restamped = appended_facts(crossed);
    let Some(Fact::BasisAdvanced { revision, .. }) = restamped.first_mut() else {
        panic!("a decided batch leads with its declaration");
    };
    *revision = Some(PolicyRevision::new(2));
    let batches = [
        recorded,
        StampedBatch {
            revision: Some(PolicyRevision::new(2)),
            facts: &restamped,
        },
    ];
    assert_eq!(
        second.replay(&traj(), &batches, &[&first]).err(),
        Some(ReplayRefusal::Transition(TransitionRefusal::ReturnBelowFloor))
    );
}

/// A grandchild's deferral froze under its parent's floor at revision 1, whose sanitizer leaves
/// trust unbound; revision 2 drops the sanitizer, so the parent's acceptance would land below
/// the floor there. The live acceptance is refused and appends nothing.
#[test]
fn an_acceptance_is_held_to_the_parents_floor_where_it_settles() {
    let first = open_engine(returning_registry(vec![lifting_sanitizer("scrubber")]));
    let second = open_engine(returning_registry(vec![])).at(DeploymentName::new("default"), PolicyRevision::new(2));
    let (child, grandchild) = (TrajectoryId::new("child"), TrajectoryId::new("grandchild"));
    let root = vec![opened(&first)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: Some(ReturnSanitizer::Named(SanitizerName::new("scrubber"))),
        ..floor_crossing(&first, &root)
    });
    let mut log = [root.clone(), spawn_family_at(&first, &root, &child, policy)].concat();
    log.extend(spawn_family_marked(
        &first,
        &log,
        &child,
        &grandchild,
        ReturnPolicy::Deferred,
        SpawnMark::deferred(0),
    ));
    reads(&first, &mut log, &grandchild, "read_suspicious");
    let frozen = first
        .handle(&viewing(&first, &log), stop(&log, &grandchild, "done"))
        .expect("the sanitizer leaves the floor's trust unbound");
    log.extend(appended_facts(frozen));
    let accept = EngineEvent::SettleReturn(ReturnSettlement {
        trajectory: child.clone(),
        id: ChildReturnId::new(grandchild.clone(), 0),
        decision: ReturnDecision::Accept,
    });
    let recorded = StampedBatch {
        revision: Some(PolicyRevision::FIRST),
        facts: &log,
    };
    let view = second.replay(&traj(), &[recorded], &[&first]).expect("the log replays");
    assert_eq!(
        second.handle(&view, accept.clone()).err(),
        Some(TransitionError::ReturnBelowFloor {
            floor: Label::new(TRUSTED, Audience::public())
        })
    );
    let mut accepted = appended_facts(
        first
            .handle(&viewing(&first, &log), accept)
            .expect("the floor the first revision reads holds the acceptance"),
    );
    // The same acceptance stamped as the second revision's does not replay either.
    let Some(Fact::BasisAdvanced { revision, .. }) = accepted.first_mut() else {
        panic!("a decided batch leads with its declaration");
    };
    *revision = Some(PolicyRevision::new(2));
    let forged = StampedBatch {
        revision: Some(PolicyRevision::new(2)),
        facts: &accepted,
    };
    assert_eq!(
        second.replay(&traj(), &[recorded, forged], &[&first]).err(),
        Some(ReplayRefusal::Transition(TransitionRefusal::ReturnBelowFloor))
    );
}

#[test]
fn only_the_parent_that_owns_a_deferred_return_settles_it() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    let by_child = EngineEvent::SettleReturn(ReturnSettlement {
        trajectory: child.clone(),
        id: ChildReturnId::new(child.clone(), 0),
        decision: ReturnDecision::Accept,
    });
    assert_eq!(
        e.handle(&viewing(&e, &log), by_child),
        Err(TransitionError::NoDeferredReturn)
    );
    assert_eq!(
        e.handle(&viewing(&e, &log), settle(&child, 1, ReturnDecision::Accept)),
        Err(TransitionError::NoDeferredReturn)
    );
}

/// A recorded settlement replays only for the parent that owns the return: one the child wrote
/// for itself is refused, whether it accepts or discards.
#[test]
fn a_settlement_recorded_by_a_trajectory_that_does_not_own_the_return_does_not_replay() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    for decision in [ReturnDecision::Accept, ReturnDecision::Discard] {
        let forged: Vec<Fact> = appended_facts(
            e.handle(&viewing(&e, &log), settle(&child, 0, decision))
                .expect("the parent settles"),
        )
        .into_iter()
        .map(|fact| match fact {
            Fact::ReturnAccepted { id, narrowing, .. } => Fact::ReturnAccepted {
                trajectory: child.clone(),
                id,
                narrowing,
            },
            Fact::ReturnDiscarded { id, .. } => Fact::ReturnDiscarded {
                trajectory: child.clone(),
                id,
            },
            fact => fact,
        })
        .collect();
        assert_eq!(
            e.validate_replay(&[log.clone(), forged].concat()),
            Err(TransitionRefusal::ForeignTrajectory),
            "{decision:?}"
        );
    }
}

#[test]
fn a_frozen_value_crossed_without_its_acceptance_does_not_replay() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    let accepted = appended_facts(
        e.handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Accept))
            .expect("the parent accepts the frozen value"),
    );
    let unaccepted: Vec<Fact> = accepted
        .into_iter()
        .filter(|fact| !matches!(fact, Fact::ReturnAccepted { .. }))
        .collect();
    assert_eq!(
        e.validate_replay(&[log, unaccepted].concat()),
        Err(TransitionRefusal::BranchEnded)
    );
}

#[test]
fn an_acceptance_recorded_in_a_batch_before_its_crossing_does_not_replay() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = frozen_family(&e, &child, "the answer");
    let accepted = appended_facts(
        e.handle(&viewing(&e, &log), settle(&child, 0, ReturnDecision::Accept))
            .expect("the parent accepts the frozen value"),
    );
    let [
        declared @ Fact::BasisAdvanced {
            trajectory,
            act,
            revision,
            ..
        },
        acceptance @ Fact::ReturnAccepted { .. },
        crossing @ ..,
    ] = accepted.as_slice()
    else {
        panic!("an acceptance leads its batch: {accepted:?}");
    };
    let alone = Fact::BasisAdvanced {
        trajectory: trajectory.clone(),
        act: act.clone(),
        advance: Default::default(),
        revision: *revision,
    };
    let split = [vec![alone, acceptance.clone(), declared.clone()], crossing.to_vec()].concat();
    assert_eq!(
        e.validate_replay(&[log, split].concat()),
        Err(TransitionRefusal::UnmergedCrossing)
    );
}

#[test]
fn a_deferred_return_is_declared_only_for_the_proposal_that_carries_the_deferral() {
    let e = open_engine(returning_registry(vec![]));
    for (mark, declares) in [
        (SpawnMark::at(0), false),
        (SpawnMark::fan_out(0), false),
        (SpawnMark::deferred(0), true),
    ] {
        let log = vec![opened(&e)];
        let blocked = e
            .handle(&viewing(&e, &log), spawn_batch("marked", Some(mark)))
            .expect("a marked spawn blocks with its return declarations");
        let log = [log, appended_facts(blocked)].concat();
        let (offer, _) = opened_offers(&log)
            .into_iter()
            .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
            .expect("the bare declaration is offered");
        let declared = e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                initiator: appa_runtime_api::Initiator::Agent,
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(ReturnPolicy::Deferred),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        );
        assert_eq!(declared.is_ok(), declares, "{mark:?}");
    }
}

#[test]
fn a_return_declared_for_one_class_of_proposal_covers_no_proposal_of_the_other() {
    let e = open_engine(returning_registry(vec![]));
    let parent = vec![opened(&e)];

    // A floor declared for an unmarked proposal stands; the same call proposed with the host's
    // deferral is blocked with its own declarations instead of releasing under that floor.
    let floored = [parent.clone(), declared_spawn(&e, &parent, "plain")].concat();
    let deferring = e
        .handle(
            &viewing(&e, &floored),
            spawn_batch("deferring", Some(SpawnMark::deferred(0))),
        )
        .expect("the proposal decides");
    assert!(
        matches!(answered(&deferring), ([], [_])),
        "a floor declared for another class releases nothing"
    );
    assert!(
        opened_offers(&appended_facts(deferring))
            .iter()
            .any(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw)),
        "the proposal is offered its own declaration"
    );

    // And the reverse: the deferred declaration is spent only by the deferring proposal.
    let child = TrajectoryId::new("child");
    let blocked = e
        .handle(
            &viewing(&e, &parent),
            spawn_batch("deferring", Some(SpawnMark::deferred(0))),
        )
        .expect("a marked spawn blocks with its return declarations");
    let mut log = [parent.clone(), appended_facts(blocked)].concat();
    let (offer, _) = opened_offers(&log)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
        .expect("the bare declaration is offered");
    let declared = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                initiator: appa_runtime_api::Initiator::Agent,
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(ReturnPolicy::Deferred),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the deferral is declared for its own proposal");
    log.extend(appended_facts(declared));
    for other in [SpawnMark::at(0), SpawnMark::fan_out(0)] {
        let plain = e
            .handle(&viewing(&e, &log), spawn_batch("plain", Some(other)))
            .expect("the proposal decides");
        assert!(
            matches!(answered(&plain), ([], [_])),
            "{other:?} does not release under another proposal's deferral"
        );
    }
    let released = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("deferring-again", Some(SpawnMark::deferred(0))),
        )
        .expect("the deferring proposal releases");
    let fork = match answered(&released) {
        ([release], []) => release.fork.clone().expect("the marked spawn carries its fork"),
        other => panic!("the deferring proposal releases alone, got {other:?}"),
    };
    log.extend(appended_facts(released));
    let bound = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork,
                child: child.clone(),
            }),
        )
        .expect("the child binds to its fork");
    log.extend(appended_facts(bound));
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        viewing(&e, &log)
            .views(&traj())
            .expect("the parent is opened")
            .return_policy_of(&child),
        Some(&ReturnPolicy::Deferred)
    );
}

fn gapped(id: &str, mark: SpawnMark) -> EngineEvent {
    EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new(id),
        trajectory: traj(),
        provider_results: Vec::new(),
        proposals: vec![raw(&call("spawn", json!({})))],
        spawn: Some(mark),
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    })
}

/// Propose the gapped spawn under `mark` and take the declaration its block offers for `policy`.
fn declare_gapped(e: &Engine, log: &[Fact], id: &str, mark: SpawnMark, policy: ReturnPolicy) -> Vec<Fact> {
    let blocked = appended_facts(e.handle(&viewing(e, log), gapped(id, mark)).expect("the spawn blocks"));
    let (offer, _) = opened_offers(&blocked)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&policy.route()))
        .expect("the block offers the policy's declaration");
    let log = [log, &blocked].concat();
    let declared = appended_facts(
        execute_offer_declaring(e, &log, offer, OfferOutcome::Approved(Vec::new()), Some(policy))
            .expect("the declaration is approved without a ruling"),
    );
    [blocked, declared].concat()
}

/// Propose the declared, gapped spawn under `mark` again and have the officer cover its gap:
/// the covering approval carries the declaration that stands for that class.
fn cover_gap(e: &Engine, log: &[Fact], id: &str, mark: SpawnMark) -> Vec<Fact> {
    let retried = appended_facts(e.handle(&viewing(e, log), gapped(id, mark)).expect("the spawn blocks"));
    let (offer, plan) = match opened_offers(&retried).as_slice() {
        [(offer, plan)] => (*offer, plan.clone()),
        other => panic!("the gap alone is planned, got {other:?}"),
    };
    let log = [log, &retried].concat();
    let label = label_of(e, &log, &traj());
    let covered = appended_facts(
        execute_offer_declaring(
            e,
            &log,
            offer,
            OfferOutcome::Approved(evidence_for(offer, &plan, "spawn", label)),
            None,
        )
        .expect("the officer's ruling covers the gap"),
    );
    [retried, covered].concat()
}

#[test]
fn a_covering_approval_made_for_one_class_is_not_spent_by_a_proposal_of_the_other() {
    let e = gapped_spawn_engine();
    let log = vec![opened(&e)];
    let floor = floor_policy(&e, &log);
    let log = [
        log.clone(),
        declare_gapped(&e, &log, "plain-1", SpawnMark::at(0), floor),
    ]
    .concat();
    let log = [log.clone(), cover_gap(&e, &log, "plain-2", SpawnMark::at(0))].concat();

    let deferring = e
        .handle(&viewing(&e, &log), gapped("deferring", SpawnMark::deferred(0)))
        .expect("the proposal decides");
    assert!(
        matches!(answered(&deferring), ([], [_])),
        "a covering approval made for the other class releases nothing"
    );
    let offered = opened_offers(&appended_facts(deferring));
    assert!(
        !offered.is_empty() && offered.iter().all(|(_, plan)| plan.declares_only()),
        "the deferring proposal declares for itself: {offered:?}"
    );

    let released = e
        .handle(&viewing(&e, &log), gapped("plain-3", SpawnMark::at(0)))
        .expect("the proposal decides");
    assert!(matches!(answered(&released), ([release], []) if release.fork.is_some()));
    assert_eq!(e.validate_replay(&[log, appended_facts(released)].concat()), Ok(()));
}

#[test]
fn spending_one_class_s_covering_approval_leaves_the_other_class_s_declaration_standing() {
    let e = gapped_spawn_engine();
    let log = vec![opened(&e)];
    let log = [
        log.clone(),
        declare_gapped(&e, &log, "plain-1", SpawnMark::at(0), ReturnPolicy::Void),
    ]
    .concat();
    let log = [
        log.clone(),
        declare_gapped(&e, &log, "deferring-1", SpawnMark::deferred(0), ReturnPolicy::Void),
    ]
    .concat();
    let log = [log.clone(), cover_gap(&e, &log, "plain-2", SpawnMark::at(0))].concat();
    let released = e
        .handle(&viewing(&e, &log), gapped("plain-3", SpawnMark::at(0)))
        .expect("the proposal decides");
    assert!(matches!(answered(&released), ([release], []) if release.fork.is_some()));
    let log = [log, appended_facts(released)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    // The deferring proposal's own declaration still stands: its retry plans the gap alone.
    let retried = appended_facts(
        e.handle(&viewing(&e, &log), gapped("deferring-2", SpawnMark::deferred(0)))
            .expect("the proposal decides"),
    );
    let offered = opened_offers(&retried);
    assert!(
        matches!(offered.as_slice(), [(_, plan)] if !plan.declares_only()),
        "the standing declaration is not declared again: {offered:?}"
    );
    assert_eq!(e.validate_replay(&[log, retried].concat()), Ok(()));
}

/// A deferred spawn's own output reaches the parent only by its child's return: a forged log
/// that derives a sanitized result from it and admits that admits nothing.
#[test]
fn a_sanitized_result_of_a_deferred_spawn_s_own_dispatch_does_not_replay() {
    let mut cfg = returning_registry(vec![lifting_sanitizer("redactor")]);
    cfg.tools = declared(vec![read_tool(
        "spawn",
        Delta {
            trust: Some(SUSPICIOUS),
            audience: None,
        },
    )]);
    let e = open_engine(cfg);
    let log = vec![opened(&e)];
    let log = [
        log.clone(),
        declare_gapped(&e, &log, "deferring-1", SpawnMark::deferred(0), ReturnPolicy::Deferred),
    ]
    .concat();
    let retried = appended_facts(
        e.handle(&viewing(&e, &log), gapped("deferring-2", SpawnMark::deferred(0)))
            .expect("the narrowing spawn blocks"),
    );
    let (offer, _) = opened_offers(&retried)
        .into_iter()
        .find(|(_, plan)| plan.sanitizer().is_some())
        .expect("the narrowing is offered the output sanitizer");
    let log = [log, retried].concat();
    let sanitized = appended_facts(
        execute_offer_declaring(&e, &log, offer, OfferOutcome::Approved(Vec::new()), None)
            .expect("the sanitizer is bound without a ruling"),
    );
    let log = [log, sanitized].concat();
    let released = e
        .handle(&viewing(&e, &log), gapped("deferring-3", SpawnMark::deferred(0)))
        .expect("the proposal decides");
    let dispatch = match answered(&released) {
        ([release], []) => release.dispatch.clone(),
        other => panic!("the declared, sanitized spawn releases, got {other:?}"),
    };
    let log = [log, appended_facts(released)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let derived = LabeledValue::new(ValueBody::new("clean"), Label::new(TRUSTED, Audience::public()));
    let sanitizer = SanitizerName::new("redactor");
    let records = vec![
        Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            outcome: crate::fact::CloseOutcome::Success {
                effects: EffectSet::default(),
            },
        },
        Fact::CandidateDerived {
            trajectory: traj(),
            subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
            sanitizer: sanitizer.clone(),
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: RawResultDigest::of(b"raw"),
                from: crate::candidate::ConfinedFrom::Bound,
                value: derived.clone(),
                residual: None,
            },
            lineage: crate::candidate::SanitizerLineage::default()
                .extend(sanitizer)
                .expect("an empty lineage spends no sanitizer yet"),
            evidence: crate::audience::AudienceEvidence::default(),
        },
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: derived,
            provenance: Provenance::ToolResult {
                dispatch: dispatch.clone(),
            },
        },
    ];
    let declared = Fact::BasisAdvanced {
        trajectory: traj(),
        act: crate::basis::DecidedAct::Outcome(dispatch),
        advance: crate::transition::Sequence::advance_of(&e, &viewing(&e, &log), &records),
        revision: e.revision(),
    };
    assert_eq!(
        e.validate_replay(&[log, vec![declared], records].concat()),
        Err(TransitionRefusal::ForgedLabel)
    );
}

#[test]
fn a_mark_cannot_spell_a_deferred_fan_out() {
    let single = serde_json::to_value(SpawnMark::deferred(0)).expect("a mark serializes");
    assert_eq!(single, json!({ "index": 0, "kind": "Single", "deferral": true }));
    assert_eq!(
        serde_json::to_value(SpawnMark::at(0)).expect("a mark serializes"),
        json!({ "index": 0, "kind": "Single" }),
        "a mark without the deferral is recorded as it was before deferral existed"
    );
    assert!(serde_json::from_value::<SpawnMark>(json!({ "index": 0, "kind": "FanOut", "deferral": true })).is_err());
}
