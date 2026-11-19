use super::returns::{execute_offer_declaring, gapped_spawn_engine};
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
    let forked = spawn_family_marked(e, &parent, child, ReturnPolicy::Deferred, SpawnMark::deferred(0));
    [parent, forked].concat()
}

/// [`deferring_family`] after the child read what narrows it and stopped with `body`.
fn frozen_family(e: &Engine, child: &TrajectoryId, body: &str) -> Vec<Fact> {
    let mut log = deferring_family(e, child);
    reads(e, &mut log, child, "read_suspicious_internal");
    let deferred = e
        .handle(&viewing(e, &log), stop(&log, child, body))
        .expect("a narrowing return under a deferred fork is frozen");
    assert_eq!(deferred.follow_up, FollowUp::Child(ChildFollowUp::Deferred));
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
    assert_eq!(parent.returns_by(&child), 0);
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
    assert_eq!(
        accepted.follow_up,
        FollowUp::Settlement(SettlementFollowUp::Accepted {
            admitted: ValueBody::new("the answer")
        })
    );
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
        assert_eq!(
            again.follow_up,
            FollowUp::Settlement(SettlementFollowUp::Accepted {
                admitted: ValueBody::new("the answer")
            })
        );
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

#[test]
fn a_deferred_return_that_leaves_the_parent_where_it_is_crosses_at_the_stop() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = deferring_family(&e, &child);

    let crossed = e
        .handle(&viewing(&e, &log), stop(&log, &child, "done"))
        .expect("a return that narrows nothing crosses");
    assert_eq!(
        crossed.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("done")
        })
    );
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
