use super::*;
use crate::fact::{BelowFloor, CrossingReturn, ReturnPolicy};
use crate::profile::{DeploymentName, PolicyRevision};
use crate::transition::{
    ChildFollowUp, ChildReport, ChildSubmission, EngineEvent, FollowUp, ForkBinding, ReturnDecision, ReturnKind,
    ReturnSettlement, SettlementFollowUp, SpawnMark, TransitionError, TransitionRefusal,
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
fn a_held_return_never_crosses_into_a_parent_that_ended() {
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
        Err(TransitionRefusal::BranchEnded),
        "a log that crosses a held return after its parent ended does not replay"
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
    let mut log = [root.clone(), spawn_family_at(&first, &root, &child, policy)].concat();
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
        e.check_return_declaration(&viewing(&e, log), trajectory, &policy)
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

    // A child under its parent's floor keeps the as-spoken return for its own children.
    let log = [root.clone(), forked_child(&e, &root, &child)].concat();
    let own = CrossingReturn {
        floor: label_of(&e, &log, &child),
        ..floor_crossing(&e, &root)
    };
    assert_eq!(check(&log, &child, ReturnPolicy::Crossing(own.clone())), Ok(()));
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
