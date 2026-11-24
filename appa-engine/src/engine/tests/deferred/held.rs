use super::*;
use crate::fact::{BelowFloor, CrossingReturn, ReturnPolicy};
use crate::profile::{DeploymentName, PolicyRevision};
use crate::transition::{
    ChildFollowUp, ChildReport, ChildSubmission, EngineEvent, FollowUp, ForkBinding, ReturnClass, ReturnDecision,
    ReturnKind, ReturnSettlement, SettlementFollowUp, SpawnMark, TransitionError,
};
use crate::value::ChildReturnId;

fn returned(log: &[Fact], child: &TrajectoryId, body: &str, kind: ReturnKind) -> EngineEvent {
    EngineEvent::ChildReturn(ChildReport {
        child: child.clone(),
        fork: fork_in(log, child),
        submission: ChildSubmission::Value {
            body: ValueBody::new(body),
        },
        kind,
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    })
}

fn settle_as(parent: &TrajectoryId, child: &TrajectoryId, occurrence: u32, decision: ReturnDecision) -> EngineEvent {
    EngineEvent::SettleReturn(ReturnSettlement {
        trajectory: parent.clone(),
        id: ChildReturnId::new(child.clone(), occurrence),
        decision,
    })
}

fn settle(child: &TrajectoryId, occurrence: u32, decision: ReturnDecision) -> EngineEvent {
    settle_as(&traj(), child, occurrence, decision)
}

fn label_of(e: &Engine, log: &[Fact], trajectory: &TrajectoryId) -> Label {
    viewing(e, log)
        .views(trajectory)
        .expect("the trajectory is opened")
        .current_label()
}

fn has_ended(e: &Engine, log: &[Fact], child: &TrajectoryId) -> bool {
    let views = viewing(e, log);
    let root = traj();
    views.views(&root).expect("the parent is opened").has_ended(child)
}

fn decided(e: &Engine, log: &mut Vec<Fact>, event: impl FnOnce(&[Fact]) -> EngineEvent) -> FollowUp {
    let decision = e.handle(&viewing(e, log), event(log)).expect("the event decides");
    let follow_up = decision.follow_up.clone();
    log.extend(appended_facts(decision));
    follow_up
}

fn held_at(child: &TrajectoryId, occurrence: u32) -> FollowUp {
    FollowUp::Child(ChildFollowUp::Deferred {
        id: ChildReturnId::new(child.clone(), occurrence),
    })
}

fn deferrals(log: &[Fact]) -> Vec<(u32, bool)> {
    log.iter()
        .filter_map(|fact| match fact {
            Fact::ReturnDeferred { id, held, .. } => Some((id.occurrence(), *held)),
            _ => None,
        })
        .collect()
}

/// A parent with `child` spawned under a deferred return, which read `tool` and returned `body`
/// without ending.
fn background_turn(e: &Engine, child: &TrajectoryId, tool: &str, body: &str) -> Vec<Fact> {
    let parent = vec![opened(e)];
    let forked = spawn_family_marked(
        e,
        &parent,
        &traj(),
        child,
        ReturnPolicy::Deferred,
        SpawnMark::deferred(0),
    );
    let mut log = [parent, forked].concat();
    reads(e, &mut log, child, tool);
    assert_eq!(
        decided(e, &mut log, |log| returned(log, child, body, ReturnKind::Turn)),
        held_at(child, 0)
    );
    log
}

/// [`background_turn`], then the parent steers the child, which reads `read_internal` and
/// returns again: two held occurrences.
fn two_held_turns(e: &Engine, child: &TrajectoryId) -> Vec<Fact> {
    let mut log = background_turn(e, child, "read_suspicious", "turn one");
    let steer = EngineEvent::BindFork(ForkBinding {
        fork: fork_in(&log, child),
        child: child.clone(),
    });
    decided(e, &mut log, |_| steer);
    reads(e, &mut log, child, "read_internal");
    assert_eq!(
        decided(e, &mut log, |log| returned(log, child, "turn two", ReturnKind::Turn)),
        held_at(child, 1)
    );
    log
}

#[test]
fn a_narrowing_return_made_without_ending_is_held_and_the_child_runs_on() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let standing = label_of(&e, &[opened(&e)], &traj());
    let log = background_turn(&e, &child, "read_suspicious", "turn one");
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(deferrals(&log), vec![(0, true)]);
    assert!(!has_ended(&e, &log, &child), "a held return leaves the child running");
    assert_eq!(label_of(&e, &log, &traj()), standing, "nothing crossed");

    // The same words again are the held return again; other words are the child's next return.
    let again = e
        .handle(&viewing(&e, &log), returned(&log, &child, "turn one", ReturnKind::Turn))
        .expect("a repeated return decides");
    assert_eq!((again.append, again.follow_up), (None, held_at(&child, 0)));
    let mut log = log;
    assert_eq!(
        decided(&e, &mut log, |log| returned(
            log,
            &child,
            "turn one, revised",
            ReturnKind::Turn
        )),
        held_at(&child, 1)
    );
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn a_turn_after_a_steer_is_held_at_its_own_occurrence() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let mut log = two_held_turns(&e, &child);
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(deferrals(&log), vec![(0, true), (1, true)]);

    // A stop that would narrow is deferred at the next occurrence and ends the errand.
    reads(&e, &mut log, &child, "read_suspicious_internal");
    assert_eq!(
        decided(&e, &mut log, |log| returned(log, &child, "done", ReturnKind::Stop)),
        held_at(&child, 2)
    );
    assert_eq!(deferrals(&log), vec![(0, true), (1, true), (2, false)]);
    assert!(has_ended(&e, &log, &child));
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn held_returns_settle_in_any_order_and_each_crosses_at_most_once() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = two_held_turns(&e, &child);
    let from = label_of(&e, &log, &traj());
    let second = label_of(&e, &log, &child);

    let mut later_first = log.clone();
    assert_eq!(
        decided(&e, &mut later_first, |_| settle(&child, 1, ReturnDecision::Accept)),
        FollowUp::Settlement(SettlementFollowUp::Accepted {
            admitted: ValueBody::new("turn two")
        })
    );
    assert_eq!(
        decided(&e, &mut later_first, |_| settle(&child, 0, ReturnDecision::Discard)),
        FollowUp::Settlement(SettlementFollowUp::Discarded)
    );
    assert_eq!(e.validate_replay(&later_first), Ok(()));
    assert_eq!(label_of(&e, &later_first, &traj()), from.combine(&second));
    let narrowings: Vec<_> = later_first
        .iter()
        .filter_map(|fact| match fact {
            Fact::ReturnAccepted { narrowing, .. } => Some(narrowing.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        narrowings,
        vec![crate::check::Narrowing {
            from: from.clone(),
            to: from.combine(&second)
        }],
        "the parent is lowered once, to the later turn's label"
    );

    // Every repeat answers the recorded outcome and appends nothing.
    for (occurrence, outcome) in [
        (
            1,
            SettlementFollowUp::Accepted {
                admitted: ValueBody::new("turn two"),
            },
        ),
        (0, SettlementFollowUp::Discarded),
    ] {
        for repeat in [ReturnDecision::Accept, ReturnDecision::Discard] {
            let again = e
                .handle(&viewing(&e, &later_first), settle(&child, occurrence, repeat))
                .expect("a settled return answers from the log");
            assert_eq!(
                (again.append, again.follow_up),
                (None, FollowUp::Settlement(outcome.clone()))
            );
        }
    }
    assert_eq!(
        e.handle(&viewing(&e, &later_first), settle(&child, 2, ReturnDecision::Accept))
            .err(),
        Some(TransitionError::NoDeferredReturn),
        "settlement names an occurrence the child returned"
    );

    // Accepting both joins both labels, whichever is settled first.
    let accept_both = |order: [u32; 2]| {
        let mut settled = log.clone();
        for occurrence in order {
            decided(&e, &mut settled, |_| settle(&child, occurrence, ReturnDecision::Accept));
        }
        assert_eq!(e.validate_replay(&settled), Ok(()));
        label_of(&e, &settled, &traj())
    };
    assert_eq!(accept_both([0, 1]), accept_both([1, 0]));
}

/// A child `parent` spawned under no return, which spawned `grandchild` under a deferred one.
fn nested_family(e: &Engine, child: &TrajectoryId, grandchild: &TrajectoryId) -> Vec<Fact> {
    let root = vec![opened(e)];
    let mut log = [root.clone(), spawn_family_at(e, &root, child, ReturnPolicy::Void)].concat();
    log.extend(spawn_family_marked(
        e,
        &log,
        child,
        grandchild,
        ReturnPolicy::Deferred,
        SpawnMark::deferred(0),
    ));
    log
}

#[test]
fn a_held_return_never_crosses_live_into_a_parent_that_ended() {
    let e = open_engine(returning_registry(vec![]));
    let (child, grandchild) = (TrajectoryId::new("child"), TrajectoryId::new("grandchild"));
    let mut log = nested_family(&e, &child, &grandchild);
    reads(&e, &mut log, &grandchild, "read_suspicious");
    assert_eq!(
        decided(&e, &mut log, |log| returned(
            log,
            &grandchild,
            "found",
            ReturnKind::Turn
        )),
        held_at(&grandchild, 0)
    );
    let pending = log.clone();
    decided(&e, &mut log, |log| child_report(log, &child, ChildSubmission::Void));
    assert!(has_ended(&e, &log, &child));
    assert_eq!(e.validate_replay(&log), Ok(()));

    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            settle_as(&child, &grandchild, 0, ReturnDecision::Accept)
        )
        .err(),
        Some(TransitionError::BranchEnded)
    );
    let accepted = appended_facts(
        e.handle(
            &viewing(&e, &pending),
            settle_as(&child, &grandchild, 0, ReturnDecision::Accept),
        )
        .expect("the live parent accepts"),
    );
    assert_eq!(
        e.validate_replay(&[log, accepted].concat()),
        Ok(()),
        "a recorded acceptance replays by its ownership and its pending standing, as a discard does"
    );
}

/// An engine whose chain names a middle rank, and a read that lowers trust to it.
fn middle_rank_engine() -> Engine {
    let mut cfg = returning_registry(vec![]);
    cfg.trust_chain = TrustChain::new(vec!["suspicious".into(), "middle".into(), "trusted".into()]);
    cfg.tools.extend(declared(vec![read_tool(
        "read_middle",
        crate::contract::Delta {
            trust: Some(Trust::new(1)),
            audience: None,
        },
    )]));
    open_engine(cfg)
}

/// [`spawn_family_at`] for a spawn whose host declared that its parent decides.
fn deciding_family(e: &Engine, log: &[Fact], child: &TrajectoryId, policy: ReturnPolicy) -> Vec<Fact> {
    spawn_family_marked(e, log, &traj(), child, policy, SpawnMark::decides(0))
}

/// The child returns at its floor under revision 1, whose chain names the middle rank. Revision
/// 2 drops that rank: the child's label falls to the lowest rank and the floor rises to the
/// highest, so the same return stands below the floor there.
fn below_floor_return(below_floor: BelowFloor) -> Result<(EngineDecision, Vec<Fact>), TransitionError> {
    let first = middle_rank_engine();
    let second = open_engine(returning_registry(vec![])).at(DeploymentName::new("default"), PolicyRevision::new(2));
    let child = TrajectoryId::new("child");
    let root = vec![opened(&first)];
    let crossing = floor_crossing(&first, &root);
    let policy = ReturnPolicy::Crossing(CrossingReturn {
        floor: Label::new(Trust::new(1), crossing.floor.audience.clone()),
        below_floor,
        ..crossing
    });
    let mark = match below_floor {
        BelowFloor::Hold => SpawnMark::decides(0),
        BelowFloor::Refuse => SpawnMark::at(0),
    };
    let mut log = [
        root.clone(),
        spawn_family_marked(&first, &root, &traj(), &child, policy, mark),
    ]
    .concat();
    reads(&first, &mut log, &child, "read_middle");
    let batches = [StampedBatch {
        revision: Some(PolicyRevision::FIRST),
        facts: &log,
    }];
    let view = second.replay(&traj(), &batches, &[&first]).expect("the log replays");
    let decision = second.handle(&view, returned(&log, &child, "at the floor", ReturnKind::Turn))?;
    let facts = appended_facts(decision.clone());
    let replayed = second.replay(
        &traj(),
        &[
            batches[0],
            StampedBatch {
                revision: Some(PolicyRevision::new(2)),
                facts: &facts,
            },
        ],
        &[&first],
    );
    assert!(replayed.is_ok(), "the held return replays: {:?}", replayed.err());
    Ok((decision, facts))
}

#[test]
fn a_return_below_the_floor_is_held_where_the_fork_holds_it_and_refused_otherwise() {
    let child = TrajectoryId::new("child");
    let (held, facts) = below_floor_return(BelowFloor::Hold).expect("the return is held");
    assert_eq!(held.follow_up, held_at(&child, 0));
    assert_eq!(deferrals(&facts), vec![(0, true)]);

    assert_eq!(
        below_floor_return(BelowFloor::Refuse).err(),
        Some(TransitionError::ReturnBelowFloor {
            floor: Label::new(TRUSTED, Audience::public())
        })
    );
}

#[test]
fn a_return_below_the_floor_is_held_only_for_a_raw_return_under_no_floor_of_the_parents_own() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("scrubber")]));
    let child = TrajectoryId::new("child");
    let root = vec![opened(&e)];
    let holding = |crossing: CrossingReturn| {
        ReturnPolicy::Crossing(CrossingReturn {
            below_floor: BelowFloor::Hold,
            ..crossing
        })
    };
    let check = |log: &[Fact], trajectory: &TrajectoryId, policy: ReturnPolicy| {
        e.check_return_declaration(&viewing(&e, log), trajectory, &policy, ReturnClass::Decides)
    };
    assert_eq!(check(&root, &traj(), holding(floor_crossing(&e, &root))), Ok(()));
    assert_eq!(
        check(
            &root,
            &traj(),
            holding(CrossingReturn {
                sanitizer: Some(crate::fact::ReturnSanitizer::Named(SanitizerName::new("scrubber"))),
                ..floor_crossing(&e, &root)
            })
        ),
        Err(crate::plan::ReturnPolicyRefusal::HoldUnavailable)
    );
    // A hold is declared exactly for the spawn whose parent decides, and that spawn takes
    // nothing but a hold.
    for (policy, returns) in [
        (holding(floor_crossing(&e, &root)), ReturnClass::Planned),
        (holding(floor_crossing(&e, &root)), ReturnClass::Deferred),
        (ReturnPolicy::Crossing(floor_crossing(&e, &root)), ReturnClass::Decides),
        (ReturnPolicy::Void, ReturnClass::Decides),
        (ReturnPolicy::Deferred, ReturnClass::Decides),
    ] {
        assert_eq!(
            e.check_return_declaration(&viewing(&e, &root), &traj(), &policy, returns),
            Err(crate::plan::ReturnPolicyRefusal::HoldUndeclared),
            "{returns:?}"
        );
    }

    // A child under its parent's floor keeps the as-spoken return for its own children.
    let log = [root.clone(), forked_child(&e, &root, &child)].concat();
    let own = CrossingReturn {
        floor: label_of(&e, &log, &child),
        ..floor_crossing(&e, &root)
    };
    assert_eq!(
        e.check_return_declaration(
            &viewing(&e, &log),
            &child,
            &ReturnPolicy::Crossing(own.clone()),
            ReturnClass::Planned
        ),
        Ok(())
    );
    assert_eq!(
        check(&log, &child, holding(own)),
        Err(crate::plan::ReturnPolicyRefusal::HoldUnavailable)
    );
}

/// Logs recorded before returns could be held: two terminal deferrals, one discarded and one
/// accepted, and a raw return under a floor. Each decodes with no held deferral and no hold,
/// writes back byte for byte, and replays.
#[test]
fn logs_recorded_before_held_returns_read_and_write_back_unchanged() {
    let e = open_engine(returning_registry(vec![]));
    for stored in [
        include_str!("../fixtures/terminal-deferral-log.json"),
        include_str!("../fixtures/floored-return-log.json"),
    ] {
        let log: Vec<Fact> = serde_json::from_str(stored).expect("a log recorded at the base commit decodes");
        assert!(log.iter().all(|fact| match fact {
            Fact::ReturnDeferred { held, .. } => !held,
            Fact::ForkPrepared {
                return_policy: ReturnPolicy::Crossing(crossing),
                ..
            } => crossing.below_floor == BelowFloor::Refuse,
            _ => true,
        }));
        assert_eq!(
            serde_json::to_string_pretty(&log).expect("the log serializes"),
            stored.trim_end()
        );
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    let log: Vec<Fact> =
        serde_json::from_str(include_str!("../fixtures/terminal-deferral-log.json")).expect("the stored log decodes");
    assert_eq!(deferrals(&log), vec![(0, false), (0, false)]);
    for child in ["discarded", "accepted"] {
        let child = TrajectoryId::new(child);
        assert!(has_ended(&e, &log, &child), "a terminal deferral still ends its child");
        assert_eq!(
            e.handle(&viewing(&e, &log), settle(&child, 1, ReturnDecision::Accept))
                .err(),
            Some(TransitionError::NoDeferredReturn)
        );
    }
}

/// [`decided`] for an event that may answer from the log: whether it appended, and its answer.
fn answered_by(e: &Engine, log: &mut Vec<Fact>, event: impl FnOnce(&[Fact]) -> EngineEvent) -> (bool, FollowUp) {
    let decision = e.handle(&viewing(e, log), event(log)).expect("the event decides");
    let follow_up = decision.follow_up.clone();
    let appended = decision.append.is_some();
    log.extend(appended_facts(decision));
    (appended, follow_up)
}

#[test]
fn a_stop_in_a_held_turns_words_freezes_its_own_occurrence_and_ends_the_child() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let mut log = background_turn(&e, &child, "read_suspicious", "turn one");
    assert_eq!(
        answered_by(&e, &mut log, |log| returned(log, &child, "turn one", ReturnKind::Stop)),
        (true, held_at(&child, 1)),
        "a stop never answers as a held turn"
    );
    assert_eq!(deferrals(&log), vec![(0, true), (1, false)]);
    assert!(has_ended(&e, &log, &child));
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn a_keyed_turn_never_answers_from_an_earlier_return_in_its_words() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let mut log = background_turn(&e, &child, "read_suspicious", "turn one");
    assert_eq!(
        answered_by(&e, &mut log, |log| returned(
            log,
            &child,
            "turn one",
            ReturnKind::KeyedTurn
        )),
        (true, held_at(&child, 1))
    );
    assert_eq!(deferrals(&log), vec![(0, true), (1, true)]);
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn the_answer_after_a_steer_is_its_own_occurrence_whatever_the_parent_decided_before() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let base = background_turn(&e, &child, "read_suspicious", "done");
    let steered = |decision: Option<ReturnDecision>| {
        let mut log = base.clone();
        decided(&e, &mut log, |log| {
            EngineEvent::BindFork(ForkBinding {
                fork: fork_in(log, &child),
                child: child.clone(),
            })
        });
        if let Some(decision) = decision {
            decided(&e, &mut log, |_| settle(&child, 0, decision));
        }
        assert!(
            !e.is_quiescent(&viewing(&e, &log)),
            "{decision:?}: the steered errand is unanswered"
        );
        reads(&e, &mut log, &child, "read_suspicious_internal");
        let answered = answered_by(&e, &mut log, |log| returned(log, &child, "done", ReturnKind::Turn));
        assert_eq!(e.validate_replay(&log), Ok(()));
        answered
    };
    for decision in [None, Some(ReturnDecision::Discard), Some(ReturnDecision::Accept)] {
        assert_eq!(steered(decision), (true, held_at(&child, 1)), "{decision:?}");
    }
}

#[test]
fn returns_held_below_the_floor_settle_in_turn_and_the_child_returns_on() {
    let first = middle_rank_engine();
    let second = open_engine(returning_registry(vec![])).at(DeploymentName::new("default"), PolicyRevision::new(2));
    let child = TrajectoryId::new("child");
    let root = vec![opened(&first)];
    let crossing = floor_crossing(&first, &root);
    let policy = ReturnPolicy::Crossing(CrossingReturn {
        floor: Label::new(Trust::new(1), crossing.floor.audience.clone()),
        below_floor: BelowFloor::Hold,
        ..crossing
    });
    let mut old = [root.clone(), deciding_family(&first, &root, &child, policy)].concat();
    reads(&first, &mut old, &child, "read_middle");
    let replayed = |new: &[Fact]| {
        let mut batches = vec![StampedBatch {
            revision: Some(PolicyRevision::FIRST),
            facts: &old,
        }];
        if !new.is_empty() {
            batches.push(StampedBatch {
                revision: Some(PolicyRevision::new(2)),
                facts: new,
            });
        }
        second.replay(&traj(), &batches, &[&first])
    };
    let mut new: Vec<Fact> = Vec::new();
    let mut step = |event: &dyn Fn(&[Fact]) -> EngineEvent| {
        let view = replayed(&new).expect("the log replays");
        let decision = second
            .handle(&view, event(&[old.clone(), new.clone()].concat()))
            .expect("the event decides");
        let follow_up = decision.follow_up.clone();
        new.extend(appended_facts(decision));
        assert!(replayed(&new).is_ok(), "the log replays after {follow_up:?}");
        follow_up
    };
    let turn = |body: &'static str, kind| move |log: &[Fact]| returned(log, &TrajectoryId::new("child"), body, kind);
    assert_eq!(step(&turn("at the floor", ReturnKind::Turn)), held_at(&child, 0));
    assert_eq!(step(&turn("second words", ReturnKind::Turn)), held_at(&child, 1));
    assert!(matches!(
        step(&|_| settle(&child, 0, ReturnDecision::Accept)),
        FollowUp::Settlement(SettlementFollowUp::Accepted { .. })
    ));
    // The parent now stands at the child's label, so the child's next words narrow nothing.
    assert_eq!(
        step(&turn("third words", ReturnKind::Turn)),
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("third words")
        })
    );
    assert_eq!(
        step(&|_| settle(&child, 1, ReturnDecision::Accept)),
        FollowUp::Settlement(SettlementFollowUp::Accepted {
            admitted: ValueBody::new("second words")
        })
    );
    assert_eq!(deferrals(&new), vec![(0, true), (1, true)]);
}

#[test]
fn a_terminal_deferral_log_counts_each_return_once_and_replays_at_every_deferral() {
    let e = open_engine(returning_registry(vec![]));
    let log: Vec<Fact> =
        serde_json::from_str(include_str!("../fixtures/terminal-deferral-log.json")).expect("the stored log decodes");
    let views = viewing(&e, &log);
    let root = traj();
    let parent = views.views(&root).expect("the parent is opened");
    for child in ["discarded", "accepted"] {
        let child = TrajectoryId::new(child);
        assert_eq!(parent.returns_by(&child), 1, "{child:?}");
        assert_eq!(
            parent.latest_return(&child),
            None,
            "{child:?}: a deferral is no crossing to repeat"
        );
    }
    for (at, _) in log
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, Fact::ReturnDeferred { .. }))
    {
        assert_eq!(e.validate_replay(&log[..=at]), Ok(()), "through {at}");
    }
}

/// Logs recorded before returns could be held: a child under no return spawned a grandchild
/// under a deferred one, the grandchild's stop froze, the child ended by its void return, and
/// only then accepted or discarded the grandchild's return.
#[test]
fn a_return_settled_after_its_parent_ended_under_an_earlier_engine_replays() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    for stored in [
        include_str!("../fixtures/accept-after-parent-ended-log.json"),
        include_str!("../fixtures/discard-after-parent-ended-log.json"),
    ] {
        let log: Vec<Fact> = serde_json::from_str(stored).expect("a log recorded at the base commit decodes");
        assert_eq!(
            serde_json::to_string_pretty(&log).expect("the log serializes"),
            stored.trim_end()
        );
        let ended_at = log
            .iter()
            .position(|fact| matches!(fact, Fact::Boundary { trajectory, kind: crate::fact::BoundaryKind::VoidReturn } if *trajectory == child))
            .expect("the child ends by its void return");
        let settled_at = log
            .iter()
            .position(|fact| matches!(fact, Fact::ReturnAccepted { .. } | Fact::ReturnDiscarded { .. }))
            .expect("the grandchild's return is settled");
        assert!(ended_at < settled_at, "the settlement follows the parent's end");
        assert_eq!(e.validate_replay(&log), Ok(()));
    }
}

/// The declaration menu of a spawn whose parent decides offers only a route that can declare
/// its hold: the bare floor under no floor of the declarer's own, and nothing under one. A plain
/// spawn is offered every route.
#[test]
fn a_deciding_spawns_declaration_menu_offers_only_a_route_that_can_hold() {
    use crate::fact::ReturnRoute;
    let e = open_engine(returning_registry(vec![lifting_sanitizer("scrubber")]));
    let child = TrajectoryId::new("child");
    let root = vec![opened(&e)];
    let log = [root.clone(), forked_child(&e, &root, &child)].concat();
    let routes = |trajectory: &TrajectoryId, mark: SpawnMark| {
        let event = EngineEvent::Proposals(crate::transition::ProposalBatch {
            id: crate::transition::ProposalBatchId::new("marked"),
            trajectory: trajectory.clone(),
            provider_results: Vec::new(),
            proposals: vec![raw(&call("spawn", json!({})))],
            spawn: Some(mark),
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        });
        let blocked = e.handle(&viewing(&e, &log), event).expect("the marked spawn decides");
        opened_offers(&appended_facts(blocked))
            .into_iter()
            .filter_map(|(_, plan)| plan.return_step().cloned())
            .collect::<Vec<_>>()
    };
    assert_eq!(routes(&traj(), SpawnMark::decides(0)), vec![ReturnRoute::Raw]);
    assert_eq!(routes(&child, SpawnMark::decides(0)), Vec::new());
    assert_eq!(
        routes(&traj(), SpawnMark::at(0)),
        vec![
            ReturnRoute::Raw,
            ReturnRoute::Sanitized(SanitizerName::new("scrubber")),
            ReturnRoute::Void
        ]
    );
}

#[test]
fn a_floor_held_for_a_deciding_parent_covers_no_other_proposal_and_no_fan_out_holds() {
    let e = open_engine(returning_registry(vec![]));
    let parent = vec![opened(&e)];
    let declare = |mark: SpawnMark, policy: ReturnPolicy| {
        let blocked = e
            .handle(&viewing(&e, &parent), spawn_batch("marked", Some(mark)))
            .expect("a marked spawn blocks with its return declarations");
        let log = [parent.clone(), appended_facts(blocked)].concat();
        let (offer, _) = opened_offers(&log)
            .into_iter()
            .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
            .expect("the bare declaration is offered");
        let declared = e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(crate::transition::OfferExecution {
                initiator: appa_runtime_api::Initiator::Agent,
                trajectory: traj(),
                offer,
                outcome: crate::transition::OfferOutcome::Approved(Vec::new()),
                return_policy: Some(policy),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        );
        declared.map(|declared| [log, appended_facts(declared)].concat())
    };
    let refusing = ReturnPolicy::Crossing(floor_crossing(&e, &parent));
    let holding = ReturnPolicy::Crossing(CrossingReturn {
        below_floor: BelowFloor::Hold,
        ..floor_crossing(&e, &parent)
    });

    // Only the deciding single spawn declares the hold, and it declares nothing else.
    for mark in [SpawnMark::at(0), SpawnMark::fan_out(0)] {
        assert!(declare(mark, holding.clone()).is_err(), "{mark:?} holds nothing");
    }
    assert!(declare(SpawnMark::decides(0), refusing.clone()).is_err());
    assert!(
        serde_json::from_value::<SpawnMark>(serde_json::json!({ "index": 0, "kind": "FanOut", "decides": true }))
            .is_err(),
        "no fan-out is marked as deciding"
    );

    // A plain floor never releases the deciding spawn, and a held floor never releases another.
    let releases = |log: &[Fact], mark: SpawnMark| {
        let decided = e
            .handle(&viewing(&e, log), spawn_batch("again", Some(mark)))
            .expect("the proposal decides");
        matches!(answered(&decided), ([_], []))
    };
    let plain = declare(SpawnMark::at(0), refusing).expect("a plain spawn declares its floor");
    assert!(!releases(&plain, SpawnMark::decides(0)));
    assert!(releases(&plain, SpawnMark::at(0)));
    let deciding = declare(SpawnMark::decides(0), holding).expect("the deciding spawn declares its hold");
    for other in [SpawnMark::at(0), SpawnMark::fan_out(0)] {
        assert!(!releases(&deciding, other), "{other:?}");
    }
    assert!(releases(&deciding, SpawnMark::decides(0)));
}
