use super::*;

#[test]
fn a_child_return_crosses_and_a_repeat_answers_from_the_record() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![], Label::new(SUSPICIOUS, internal.clone()));
    let child = TrajectoryId::new("child");
    let mut log = vec![opened(&e)];
    log.extend(forked_child(&e, &log.clone(), &child));
    let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

    let body = ValueBody::new("the child's answer");
    let report = |submission: ChildSubmission| child_report(&log, &child, submission);
    let merged = e
        .handle(&view, report(ChildSubmission::Value { body: body.clone() }))
        .expect("a non-narrowing crossing merges");
    assert_eq!(
        merged.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { admitted: body.clone() })
    );
    let crossed_log = [
        log.clone(),
        merged.append.expect("the crossing appends").facts().to_vec(),
    ]
    .concat();
    let after = e.view(&traj(), crossed_log.clone(), crossed_log.len() as u64).unwrap();

    let repeat = e
        .handle(&after, report(ChildSubmission::Value { body: body.clone() }))
        .expect("a repeat of the latest crossing answers from the record");
    assert_eq!(repeat.append, None);
    assert_eq!(
        repeat.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { admitted: body })
    );
    let ended = e
        .handle(&after, report(ChildSubmission::Void))
        .expect("a void return ends the branch");
    let ended_log = [crossed_log, ended.append.expect("the void appends").facts().to_vec()].concat();
    let after = e.view(&traj(), ended_log.clone(), ended_log.len() as u64).unwrap();
    assert_eq!(
        e.handle(
            &after,
            report(ChildSubmission::Value {
                body: ValueBody::new("a second answer")
            })
        ),
        Err(crate::transition::TransitionError::BranchEnded)
    );
    assert_eq!(
        e.handle(&after, report(ChildSubmission::Void)),
        Err(crate::transition::TransitionError::BranchEnded)
    );

    assert_eq!(
        e.handle(
            &after,
            EngineEvent::ChildReturn(ChildReport {
                child: TrajectoryId::new("stranger"),
                fork: fork_in(&log, &child),
                submission: ChildSubmission::Void,
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::UnopenedTrajectory)
    );
}

#[test]
fn a_fan_out_child_starts_at_the_parents_label_including_earlier_siblings_returns() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine(vec![plain_tool("spawn"), internal_read()]);
    let (mut log, fan_out) = fan_out_family(&e, Label::new(TRUSTED, internal.clone()));
    let (first, second) = (TrajectoryId::new("first"), TrajectoryId::new("second"));

    member_started(&e, &mut log, &fan_out, &first);
    let seed = |log: &[Fact], child: &TrajectoryId| {
        viewing(&e, log)
            .views(child)
            .expect("the member's child is opened")
            .current_label()
    };
    assert_eq!(seed(&log, &first), Label::new(TRUSTED, Audience::public()));

    reads(&e, &mut log, &first, "read_internal");
    let crossing = crossed_report(&e, &log, &first, "what the first child read");
    log.extend(crossing);
    member_started(&e, &mut log, &fan_out, &second);

    assert_eq!(
        seed(&log, &second).audience,
        internal,
        "a child started after a sibling's return crossed inherits what it carried"
    );
    assert_eq!(e.validate_replay(&log), Ok(()));

    // A log preparing the second member before the crossing, at the basis it stood at then,
    // would bind it at a seed that misses what the crossing carried.
    let prepared = |log: &[Fact], child: &TrajectoryId| {
        log.iter()
            .position(|fact| matches!(fact, Fact::ForkPrepared { fork, .. } if fork == &crate::value::ForkId::member(fan_out.dispatch(), child)))
            .expect("the member is prepared")
    };
    let mut early = log.clone();
    let mut second_prepared = early.remove(prepared(&early, &second));
    let first_prepared = prepared(&early, &first);
    if let (Fact::ForkPrepared { snapshot, .. }, Fact::ForkPrepared { snapshot: before, .. }) =
        (&mut second_prepared, &early[first_prepared])
    {
        *snapshot = before.clone();
    }
    early.insert(first_prepared + 2, second_prepared);
    assert_eq!(e.validate_replay(&early), Err(TransitionRefusal::ForkBasisMismatch));
}

#[test]
fn one_fan_out_declaration_backs_a_member_fork_per_child() {
    let e = engine(vec![plain_tool("spawn")]);
    let (mut log, fan_out) = fan_out_family(&e, Label::new(TRUSTED, Audience::public()));
    let children = ["a", "b", "c"].map(TrajectoryId::new);
    for child in &children {
        member_started(&e, &mut log, &fan_out, child);
    }
    let view = viewing(&e, &log);
    for child in &children {
        let dispatch = fan_out.dispatch();
        assert_eq!(
            e.fork_of(&view, child),
            Some(crate::value::ForkId::member(dispatch, child))
        );
        assert_eq!(
            e.return_policy_of(&view, child).map(|policy| policy.route()),
            Some(crate::fact::ReturnRoute::Raw)
        );
    }
    assert_eq!(e.fork_status(&view, &fan_out), ForkStatus::FanOut);
    assert!(
        e.forks_in_flight(&view).is_empty(),
        "a fan-out fork never waits for a single child to bind it"
    );

    let again = e
        .handle(
            &view,
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fan_out.clone(),
                child: children[0].clone(),
            }),
        )
        .expect("a started child addressed again resumes");
    assert!(matches!(
        declared_records(&appended_facts(again)),
        [Fact::Boundary {
            kind: BoundaryKind::Resume { .. },
            ..
        }]
    ));
    assert_eq!(e.validate_replay(&log), Ok(()));
}

/// A spawn's fork is prepared as the kind its release's spawn mark named: a log that flips
/// the kind stands on no release.
#[test]
fn a_spawn_fork_prepared_as_another_kind_than_its_release_named_is_refused() {
    let flipped = |log: &[Fact], to: crate::transition::SpawnKind| {
        let prepared = log
            .iter()
            .position(|fact| {
                matches!(
                    fact,
                    Fact::ForkPrepared {
                        fork: crate::value::ForkId::Spawn(_),
                        ..
                    }
                )
            })
            .expect("the release prepares the spawn's fork");
        let honest = log[..=prepared].to_vec();
        let mut forged = honest.clone();
        if let Some(Fact::ForkPrepared { kind, .. }) = forged.last_mut() {
            *kind = to;
        }
        (honest, forged)
    };

    let e = engine(vec![plain_tool("spawn")]);
    let (single, forged) = flipped(
        &spawn_family(&e, &TrajectoryId::new("child")),
        crate::transition::SpawnKind::FanOut,
    );
    assert_eq!(e.validate_replay(&single), Ok(()));
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::UnbackedDecision));

    let (fan_out, _) = fan_out_family(&e, Label::new(TRUSTED, Audience::public()));
    let (fan_out, forged) = flipped(&fan_out, crate::transition::SpawnKind::Single);
    assert_eq!(e.validate_replay(&fan_out), Ok(()));
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::UnbackedDecision));
}

#[test]
fn a_member_fork_replays_only_under_its_fan_out_spawn_and_for_its_own_child() {
    let e = engine(vec![plain_tool("spawn")]);
    let (mut log, fan_out) = fan_out_family(&e, Label::new(TRUSTED, Audience::public()));
    let child = TrajectoryId::new("child");
    let before = log.len();
    member_started(&e, &mut log, &fan_out, &child);
    let (records, started) = log.split_at(before);
    let tampered = |mutate: &dyn Fn(&mut Vec<Fact>)| {
        let mut facts = started.to_vec();
        mutate(&mut facts);
        e.validate_replay(&[records, &facts].concat())
    };

    assert_eq!(
        tampered(&|facts| {
            facts.retain(|fact| !matches!(fact, Fact::ForkPrepared { .. }));
            for fact in facts.iter_mut() {
                if let Fact::ForkOpened { fork, .. } = fact {
                    *fork = fan_out.clone();
                }
            }
        }),
        Err(TransitionRefusal::UnknownFork),
        "a fan-out fork binds no child itself"
    );
    assert_eq!(
        tampered(&|facts| {
            for fact in facts.iter_mut() {
                if let Fact::ForkOpened { trajectory, .. } = fact {
                    *trajectory = TrajectoryId::new("someone-else");
                }
            }
        }),
        Err(TransitionRefusal::UnknownFork),
        "a member binds only the child it names"
    );
    assert_eq!(
        tampered(&|facts| {
            for fact in facts.iter_mut() {
                if let Fact::ForkPrepared {
                    return_policy: ReturnPolicy::Crossing(crossing),
                    ..
                } = fact
                {
                    crossing.floor = Label::bottom();
                }
            }
        }),
        Err(TransitionRefusal::UnbackedReturnPolicy),
        "a member carries exactly its fan-out spawn's return policy"
    );
}

#[test]
fn a_single_spawn_releases_while_a_fan_out_fork_is_open() {
    let e = engine(vec![plain_tool("spawn")]);
    let (log, _) = fan_out_family(&e, Label::new(TRUSTED, Audience::public()));
    let view = viewing(&e, &log);
    assert!(e.forks_in_flight(&view).is_empty());
    let single = [log.clone(), declared_spawn(&e, &log, "single")].concat();
    let released = e
        .handle(
            &viewing(&e, &single),
            spawn_batch("single-approved", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("the single spawn releases");
    assert!(matches!(answered(&released), ([_], [])));
}

#[test]
fn a_marked_spawn_prepares_its_fork_and_the_child_binds_to_it() {
    let e = engine(vec![plain_tool("spawn")]);
    let call = call("spawn", json!({}));
    let records = [vec![opened(&e)], declared_spawn(&e, &[opened(&e)], "b0")].concat();
    let view = e.view(&traj(), records.clone(), records.len() as u64).unwrap();
    let batch = |spawn: Option<crate::transition::SpawnMark>| {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new("b1"),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(&call)],
            spawn,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };

    let decision = e
        .handle(&view, batch(Some(crate::transition::SpawnMark::at(0))))
        .expect("the declared marked spawn releases and prepares");
    let FollowUp::Proposals { released, .. } = &decision.follow_up else {
        panic!("a proposal batch answers with proposals")
    };
    let fork = released[0].fork.clone().expect("the release carries its fork");
    assert_eq!(fork, crate::value::ForkId::of(&released[0].dispatch));
    let facts = decision.append.clone().expect("the release appends").facts().to_vec();
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProposalBatchDecided { .. }))
    );
    assert!(matches!(
        facts.as_slice(),
        [.., Fact::DispatchOpened { .. }, Fact::ForkPrepared { .. }]
    ));
    let log = [records, facts].concat();
    let prepared = e.view(&traj(), log.clone(), log.len() as u64).unwrap();
    assert_eq!(
        e.handle(&prepared, batch(None)),
        Err(crate::transition::TransitionError::BatchIdentityConflict)
    );
    let child = TrajectoryId::new("child");
    let bind = |fork: crate::value::ForkId, child: TrajectoryId| {
        EngineEvent::BindFork(crate::transition::ForkBinding { fork, child })
    };
    let bound = e
        .handle(&prepared, bind(fork.clone(), child.clone()))
        .expect("the child binds");
    assert_eq!(bound.follow_up, FollowUp::Fork { child: child.clone() });
    let opened = bound.append.expect("the binding appends").facts().to_vec();
    assert!(matches!(declared_records(&opened), [Fact::ForkOpened { .. }]));

    let after = e
        .view(&traj(), [log.clone(), opened].concat(), log.len() as u64 + 1)
        .unwrap();
    let child_views = after.views(&child).expect("the bound child is opened");
    assert_eq!(child_views.current_label(), Label::new(TRUSTED, Audience::public()));
    assert_eq!(child_views.parent_of(&child), Some(&traj()));

    let resumed = e.handle(&after, bind(fork.clone(), child.clone())).unwrap();
    assert!(
        matches!(
            resumed.append.as_ref().map(|batch| declared_records(batch.facts())),
            Some([Fact::Boundary {
                kind: crate::fact::BoundaryKind::Resume { .. },
                ..
            }])
        ),
        "the same pair bound again is the parent addressing its child once more"
    );
    assert_eq!(
        e.handle(&after, bind(fork.clone(), TrajectoryId::new("other"))),
        Err(crate::transition::TransitionError::UnbindableFork)
    );
    let unprepared = crate::value::ForkId::of(&DispatchId::new(traj(), call.digest(), 9));
    assert_eq!(
        e.handle(&after, bind(unprepared, TrajectoryId::new("other"))),
        Err(crate::transition::TransitionError::UnbindableFork)
    );
    assert_eq!(
        e.handle(&prepared, bind(fork.clone(), traj())),
        Err(crate::transition::TransitionError::ChildAlreadyUsed)
    );

    let ran = [
        log.clone(),
        vec![Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: fork.dispatch().clone(),
            outcome: crate::fact::CloseOutcome::Success {
                effects: crate::fact::EffectSet::default(),
            },
        }],
    ]
    .concat();
    let after_run = e.view(&traj(), ran.clone(), ran.len() as u64).unwrap();
    let repeat = e
        .handle(&after_run, batch(Some(crate::transition::SpawnMark::at(0))))
        .expect("the repeat answers from the record");
    assert_eq!(repeat.append, None);
    match repeat.follow_up {
        FollowUp::Proposals { released, .. } => {
            assert!(released.is_empty(), "an invoked call is not re-released");
            assert_eq!(
                e.fork_status(&after_run, &fork),
                ForkStatus::Prepared,
                "its fork still awaits a child"
            );
        }
        other => panic!("expected a proposal answer, got {other:?}"),
    }

    let failed = [
        log,
        vec![Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: fork.dispatch().clone(),
            outcome: crate::fact::CloseOutcome::Failure,
        }],
    ]
    .concat();
    let after_failure = e.view(&traj(), failed.clone(), failed.len() as u64).unwrap();
    assert_eq!(
        e.handle(&after_failure, bind(fork, child)),
        Err(crate::transition::TransitionError::UnbindableFork)
    );
}

#[test]
fn fork_of_answers_the_same_advanced_or_rebuilt_and_a_refused_bind_leaves_it() {
    let e = engine(vec![plain_tool("spawn")]);
    let records = [vec![opened(&e)], declared_spawn(&e, &[opened(&e)], "b0")].concat();
    let mut held = e.view(&traj(), records.clone(), records.len() as u64).unwrap();
    let child = TrajectoryId::new("child");

    let prepared = e
        .handle(&held, spawn_batch("b1", Some(crate::transition::SpawnMark::at(0))))
        .expect("the declared marked spawn releases and prepares");
    let FollowUp::Proposals { released, .. } = &prepared.follow_up else {
        panic!("a proposal batch answers with proposals")
    };
    let fork = released[0].fork.clone().expect("the release carries its fork");
    assert_eq!(e.fork_of(&held, &child), None, "no binding yet, no fork for the child");
    let prepared_batch = prepared.append.clone().expect("the release appends");
    held.advance(&prepared_batch).unwrap();

    let bound = e
        .handle(
            &held,
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fork.clone(),
                child: child.clone(),
            }),
        )
        .expect("the child binds");
    let bound_batch = bound.append.clone().expect("the binding appends");
    held.advance(&bound_batch).unwrap();
    assert_eq!(e.fork_of(&held, &child), Some(fork.clone()));

    let whole = [records, prepared_batch.facts().to_vec(), bound_batch.facts().to_vec()].concat();
    let cold = e.view(&traj(), whole.clone(), whole.len() as u64).unwrap();
    assert_eq!(e.fork_of(&cold, &child), Some(fork.clone()));

    assert_eq!(
        e.handle(
            &held,
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fork.clone(),
                child: TrajectoryId::new("other"),
            }),
        ),
        Err(crate::transition::TransitionError::UnbindableFork),
    );
    assert_eq!(e.fork_of(&held, &child), Some(fork));
    assert_eq!(e.fork_of(&held, &TrajectoryId::new("other")), None);
}

#[test]
fn a_fork_preparation_replays_only_as_its_marked_release() {
    let e = engine(vec![plain_tool("spawn")]);
    let records = [vec![opened(&e)], declared_spawn(&e, &[opened(&e)], "b0")].concat();
    let view = e.view(&traj(), records.clone(), records.len() as u64).unwrap();
    let marked = e
        .handle(&view, spawn_batch("b1", Some(crate::transition::SpawnMark::at(0))))
        .unwrap();
    let batch = marked.append.expect("the release appends").facts().to_vec();
    assert_eq!(e.validate_replay(&[records.clone(), batch.clone()].concat()), Ok(()));
    let prepared_at = batch
        .iter()
        .position(|fact| matches!(fact, Fact::ForkPrepared { .. }))
        .expect("the release prepares the fork");

    let mut unmarked = batch.clone();
    let decided_at = unmarked
        .iter()
        .position(|fact| matches!(fact, Fact::ProposalBatchDecided { .. }))
        .expect("the release records its decision");
    if let Fact::ProposalBatchDecided { spawn, .. } = &mut unmarked[decided_at] {
        *spawn = None;
    }
    let unmarked = e.validate_replay(&[records.clone(), unmarked].concat());
    assert!(unmarked.is_err(), "an unmarked release backs no fork, got {unmarked:?}");
    let mut displaced = batch.clone();
    displaced.insert(
        prepared_at,
        stray_admission(&traj(), Label::new(SUSPICIOUS, Audience::public())),
    );
    let displaced = e.validate_replay(&[records, displaced].concat());
    assert!(
        displaced.is_err(),
        "a preparation lands in its release's batch, got {displaced:?}"
    );
}

#[test]
fn a_spawn_mark_takes_declared_context_control() {
    let config = test_config(vec![plain_tool("spawn")]);
    let mut declaration = crate::profile::covering_declaration(&config);
    declaration.context_control = false;
    let e = Engine::open(DeploymentPolicy {
        registry: config,
        planner_cap: crate::registry::PlannerCap::default(),
        profile: declaration,
    })
    .map(placed)
    .expect("an uncontrolled deployment opens");
    let view = e.view(&traj(), vec![opened(&e)], 1).unwrap();
    assert_eq!(
        e.handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call("spawn", json!({})))],
                spawn: Some(crate::transition::SpawnMark::at(0)),
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::SpawnUncontrolled)
    );
}
