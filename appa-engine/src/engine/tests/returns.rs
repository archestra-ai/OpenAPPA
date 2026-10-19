use super::*;

fn return_steps(facts: &[Fact]) -> Vec<crate::fact::ReturnRoute> {
    opened_offers(facts)
        .iter()
        .map(|(_, plan)| {
            plan.return_step()
                .expect("a marked spawn's plan ends in a return declaration")
                .clone()
        })
        .collect()
}

fn named(sanitizer: &str) -> Option<ReturnSanitizer> {
    Some(ReturnSanitizer::Named(SanitizerName::new(sanitizer)))
}

fn report_with(log: &[Fact], child: &TrajectoryId, body: &str, evidence: Vec<Evidence>) -> EngineEvent {
    EngineEvent::ChildReturn(ChildReport {
        child: child.clone(),
        fork: fork_in(log, child),
        submission: ChildSubmission::Value {
            body: ValueBody::new(body),
        },
        evidence,
        audience: crate::audience::AudienceEvidence::default(),
    })
}

fn derived_by(sanitizer: &str, source: &str, derived: &str) -> Vec<Evidence> {
    vec![Evidence::Sanitizer {
        sanitizer: SanitizerName::new(sanitizer),
        source: RawResultDigest::of(source.as_bytes()),
        derived: ValueBody::new(derived),
    }]
}

fn child_returns(log: &[Fact], child: &TrajectoryId) -> Vec<LabeledValue> {
    log.iter()
        .filter_map(|fact| match fact {
            Fact::ChildReturn { id, value, .. } if id.child() == child => Some(value.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_marked_spawn_blocks_with_one_return_declaration_per_untagged_output_sanitizer() {
    let mut scoped = lifting_sanitizer("scoped");
    scoped.scope = crate::authority::Scope {
        tags: vec![crate::names::TagName::new("outbound")],
    };
    let mut tagged = plain_tool("post");
    tagged.tags = vec![crate::names::TagName::new("outbound")];
    let mut cfg = returning_registry(vec![lifting_sanitizer("redactor"), scoped]);
    cfg.tools.push(crate::contract::ToolDeclaration::Declared(tagged));
    let e = open_engine(cfg);
    let log = vec![opened(&e)];

    let blocked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("a marked spawn blocks alone, got {:?}", answered(&blocked))
    };
    assert!(
        block.block.raw.requirement_gaps.is_empty() && block.block.raw.narrowing.is_none(),
        "nothing but the declaration stands between the spawn and its release"
    );
    let facts = appended_facts(blocked);
    assert_eq!(
        return_steps(&facts),
        vec![
            crate::fact::ReturnRoute::Raw,
            crate::fact::ReturnRoute::Sanitized(SanitizerName::new("redactor")),
            crate::fact::ReturnRoute::Void,
        ],
        "the bare floor, each untagged output sanitizer, then no return; a scoped sanitizer never reaches a \
         return"
    );
    assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));

    let released = e
        .handle(&viewing(&e, &log), spawn_batch("unmarked", None))
        .expect("an unmarked spawn is an ordinary call");
    assert!(matches!(answered(&released), ([release], []) if release.fork.is_none()));
}

#[test]
fn a_return_declaration_is_held_to_its_plan_and_to_the_parents_label() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = open_engine_at(
        returning_registry(vec![lifting_sanitizer("redactor")]),
        Label::new(SUSPICIOUS, internal.clone()),
    );
    let log = vec![opened(&e)];
    let blocked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks");
    let log = [log, appended_facts(blocked)].concat();
    let offer_for = |route: crate::fact::ReturnRoute| {
        opened_offers(&log)
            .into_iter()
            .find_map(|(offer, plan)| (plan.return_step() == Some(&route)).then_some(offer))
            .expect("the declaration is offered")
    };
    let execute = |offer, return_policy| {
        e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy,
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
    };
    let bare = offer_for(crate::fact::ReturnRoute::Raw);
    let redacting = offer_for(crate::fact::ReturnRoute::Sanitized(SanitizerName::new("redactor")));
    let floor = Label::new(SUSPICIOUS, internal.clone());

    assert!(matches!(
        execute(bare, None),
        Err(crate::transition::TransitionError::PlanOutcomeMismatch)
    ));
    assert!(matches!(
        execute(
            bare,
            Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                floor: floor.clone(),
                sanitizer: named("redactor"),
            }))
        ),
        Err(crate::transition::TransitionError::ReturnPolicy(
            plan::ReturnPolicyRefusal::RouteMismatch
        ))
    ));
    assert!(matches!(
        execute(
            redacting,
            Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                floor: floor.clone(),
                sanitizer: None,
            }))
        ),
        Err(crate::transition::TransitionError::ReturnPolicy(
            plan::ReturnPolicyRefusal::RouteMismatch
        ))
    ));
    assert!(
        matches!(
            execute(
                bare,
                Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    floor: Label::new(TRUSTED, internal.clone()),
                    sanitizer: None,
                }))
            ),
            Err(crate::transition::TransitionError::ReturnPolicy(
                plan::ReturnPolicyRefusal::FloorAboveLabel
            ))
        ),
        "a child starts at its parent's label and cannot rise to a floor above it"
    );
    assert!(
        matches!(
            execute(
                bare,
                Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    floor: Label::new(SUSPICIOUS, Audience::public()),
                    sanitizer: None,
                }))
            ),
            Err(crate::transition::TransitionError::ReturnPolicy(
                plan::ReturnPolicyRefusal::FloorAboveLabel
            ))
        ),
        "the audience dimension is held to the parent's label too"
    );
    let approved = execute(
        bare,
        Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
            floor: Label::new(SUSPICIOUS, internal),
            sanitizer: None,
        })),
    )
    .expect("a floor at the parent's label approves the spawn");
    assert!(matches!(
        approved.follow_up,
        FollowUp::Offer(OfferFollowUp::Approved { .. })
    ));
    let log = [log, appended_facts(approved)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn an_approval_declaring_a_return_policy_spends_only_at_the_marked_spawn() {
    let e = open_engine(returning_registry(vec![]));
    let log = vec![opened(&e)];
    let blocked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks");
    let log = [log, appended_facts(blocked)].concat();
    let (offer, _) = opened_offers(&log)
        .into_iter()
        .next()
        .expect("the bare declaration is offered");
    let approved = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(floor_policy(&e, &log)),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the declaration approves");
    let log = [log, appended_facts(approved)].concat();

    let unmarked = e
        .handle(&viewing(&e, &log), spawn_batch("unmarked", None))
        .expect("an ordinary spawn releases on its own");
    assert!(matches!(answered(&unmarked), ([release], []) if release.fork.is_none()));
    let facts = appended_facts(unmarked);
    assert!(
        !facts.iter().any(|fact| matches!(fact, Fact::ForkPrepared { .. })),
        "an ordinary release prepares no fork"
    );
    let log = [log, facts].concat();

    let marked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked-again", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("the approval still stands for the marked spawn");
    assert!(matches!(answered(&marked), ([release], []) if release.fork.is_some()));
    let log = [log, appended_facts(marked)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let again = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked-spent", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a spent approval leaves the next marked spawn blocked");
    assert!(matches!(answered(&again), ([], [_])));
}

#[test]
fn the_floor_removes_acceptances_below_it_from_the_childs_menu() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);
    let read = call("read_suspicious", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&child, "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(
        block
            .block
            .plans
            .iter()
            .all(|plan| plan.executable().is_none_or(|plan| plan.narrowing().is_none())),
        "under a floor at the parent's rank no plan accepts the drop to suspicious"
    );
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Below,
            sanitized_return: true,
            preserving_output: true,
        }),
        "the child is told a grandchild under the bare floor cannot take the drop either, but one with a \
         return sanitizer can"
    );

    // Under a sanitizer that raises trust, the trust dimension is unbound: the child may
    // descend there, because the return climbs back through the sanitizer.
    let parent = vec![opened(&e)];
    let lifted = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: named("redactor"),
        ..floor_crossing(&e, &parent)
    });
    let log = [parent.clone(), spawn_family_at(&e, &parent, &child, lifted)].concat();
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&child, "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(
        block
            .block
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .any(|plan| plan.narrowing().is_some() && plan.sanitizer().is_none()),
        "the acceptance is offered again"
    );
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Within,
            sanitized_return: true,
            preserving_output: true,
        }),
        "a drop the floor permits is to be accepted here, not delegated again, unless the offered output \
         sanitizer keeps this trajectory's label"
    );
    let internal = call("read_internal", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&child, "read-internal", Vec::new(), vec![raw(&internal)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(
        block
            .block
            .plans
            .iter()
            .all(|plan| plan.executable().is_none_or(|plan| plan.narrowing().is_none())),
        "the audience dimension stays bound: no plan accepts the drop to internal"
    );
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Below,
            sanitized_return: false,
            preserving_output: false,
        }),
        "a trust-raising sanitizer lifts no audience drop, so no sanitized delegation is advised"
    );
}

#[test]
fn a_void_child_accepts_any_narrowing_and_ends_only_with_an_empty_stop() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let log = [parent.clone(), spawn_family_at(&e, &parent, &child, ReturnPolicy::Void)].concat();
    let read = call("read_suspicious", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&child, "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(
        block
            .block
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .any(|plan| plan.narrowing().is_some()),
        "no floor bounds a child whose parent takes no return, so the drop is offered"
    );
    let log = [log, appended_facts(blocked)].concat();
    let parent_label = viewing(&e, &log)
        .views(&traj())
        .expect("the parent is opened")
        .current_label();

    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            child_report(
                &log,
                &child,
                ChildSubmission::Value {
                    body: ValueBody::new("Done.")
                }
            )
        )
        .map(|_| ()),
        Err(TransitionError::ReturnVoid)
    );
    let ended = e
        .handle(&viewing(&e, &log), child_report(&log, &child, ChildSubmission::Void))
        .expect("an empty stop ends the child");
    assert!(matches!(ended.follow_up, FollowUp::Child(ChildFollowUp::Ended)));
    let log = [log, appended_facts(ended)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        viewing(&e, &log)
            .views(&traj())
            .expect("the parent is opened")
            .current_label(),
        parent_label
    );
}

#[test]
fn a_crossing_recorded_under_a_void_fork_does_not_replay() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let floored = spawn_family(&e, &child);
    let crossing = crossed_report(&e, &floored, &child, "Done.");
    let void = [parent.clone(), spawn_family_at(&e, &parent, &child, ReturnPolicy::Void)].concat();
    assert_eq!(
        e.validate_replay(&[void, crossing].concat()),
        Err(TransitionRefusal::ReturnVoid)
    );
}

#[test]
fn a_void_declaration_answers_only_the_void_plan() {
    let e = open_engine(returning_registry(vec![]));
    let log = vec![opened(&e)];
    let blocked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks");
    let log = [log, appended_facts(blocked)].concat();
    let offer_for = |route: crate::fact::ReturnRoute| {
        opened_offers(&log)
            .into_iter()
            .find_map(|(offer, plan)| (plan.return_step() == Some(&route)).then_some(offer))
            .expect("the declaration is offered")
    };
    let execute = |offer, return_policy| {
        e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(return_policy),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .map(|_| ())
    };
    let mismatch = Err(crate::transition::TransitionError::ReturnPolicy(
        plan::ReturnPolicyRefusal::RouteMismatch,
    ));
    assert_eq!(
        execute(offer_for(crate::fact::ReturnRoute::Void), floor_policy(&e, &log)),
        mismatch
    );
    assert_eq!(
        execute(offer_for(crate::fact::ReturnRoute::Raw), ReturnPolicy::Void),
        mismatch
    );
    assert!(execute(offer_for(crate::fact::ReturnRoute::Void), ReturnPolicy::Void).is_ok());
}

#[test]
fn a_child_boxed_by_its_floor_with_no_return_sanitizer_is_told_not_to_delegate() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);
    let read = call("read_suspicious", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&child, "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(block.block.plans.is_empty(), "nothing lifts a drop below the floor");
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Below,
            sanitized_return: false,
            preserving_output: false,
        }),
        "with no return sanitizer registered, no grandchild can take the drop either"
    );
}

#[test]
fn a_return_sanitizer_that_does_not_admit_the_narrowed_value_is_no_sanitized_route() {
    let closed_to_suspicious = crate::authority::Sanitizer {
        transition: crate::authority::DeclaredTransition::Trust {
            from_floor: TRUSTED,
            to: TRUSTED,
        },
        ..lifting_sanitizer("trusted-only")
    };
    let e = open_engine(returning_registry(vec![closed_to_suspicious]));
    let log = vec![opened(&e)];
    let read = call("read_suspicious", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&traj(), "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Unbound,
            sanitized_return: false,
            preserving_output: false,
        }),
        "a sanitizer whose `from` the suspicious value fails would refuse the child's return"
    );

    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let log = vec![opened(&e)];
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&traj(), "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert_eq!(
        block.block.fork_advice,
        Some(crate::plan::ForkAdvice::Narrowing {
            standing: crate::plan::FloorStanding::Unbound,
            sanitized_return: true,
            preserving_output: true,
        }),
        "a sanitizer admitting the suspicious value and raising it back to trusted is the sanitized route, \
         and offered on the read's output it keeps this trajectory's label"
    );
}

#[test]
fn an_output_sanitizer_that_leaves_a_narrowing_does_not_preserve_the_label() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let log = vec![opened(&e)];
    let read = call("read_suspicious_internal", json!({ "who": "someone" }));
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(&traj(), "read", Vec::new(), vec![raw(&read)], None),
        )
        .expect("a narrowing read blocks");
    let ([], [block]) = answered(&blocked) else {
        panic!("the read blocks, got {:?}", answered(&blocked))
    };
    assert!(
        block
            .block
            .plans
            .iter()
            .filter_map(crate::plan::RemedyPlan::executable)
            .any(|plan| plan.sanitizer().is_some()),
        "the trust-lifting sanitizer is offered on the read's output"
    );
    assert!(
        matches!(
            block.block.fork_advice,
            Some(crate::plan::ForkAdvice::Narrowing {
                preserving_output: false,
                ..
            })
        ),
        "the offer still narrows the audience, so it keeps no label: {:?}",
        block.block.fork_advice
    );
}

#[test]
fn the_lowest_declarable_return_trust_is_the_own_floors_unless_its_sanitizer_raises_trust() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let root_log = vec![opened(&e)];
    assert_eq!(
        e.lowest_return_trust(&viewing(&e, &root_log).views(&traj()).expect("the root is opened")),
        SUSPICIOUS,
        "a root is bound by no floor"
    );

    let bare = TrajectoryId::new("bare");
    let log = [root_log.clone(), forked_child(&e, &root_log, &bare)].concat();
    assert_eq!(
        e.lowest_return_trust(&viewing(&e, &log).views(&bare).expect("the child is opened")),
        TRUSTED,
        "under the bare floor the child declares nothing below its parent's trust"
    );

    let lifted = TrajectoryId::new("lifted");
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: named("redactor"),
        ..floor_crossing(&e, &root_log)
    });
    let log = [root_log.clone(), spawn_family_at(&e, &root_log, &lifted, policy)].concat();
    assert_eq!(
        e.lowest_return_trust(&viewing(&e, &log).views(&lifted).expect("the child is opened")),
        SUSPICIOUS,
        "a trust-raising return sanitizer leaves the trust dimension unbound"
    );
}

#[test]
fn a_raw_return_crosses_at_the_fold_and_the_child_may_return_again() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);

    let first = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "done", vec![]))
        .expect("a return at the fold crosses");
    assert_eq!(
        first.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("done")
        })
    );
    let facts = appended_facts(first);
    assert!(matches!(
        declared_records(&facts),
        [
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                ..
            },
            Fact::ValueAdmitted { .. },
            Fact::Boundary {
                kind: crate::fact::BoundaryKind::Merge { .. },
                ..
            }
        ]
    ));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    let views = viewing(&e, &log);
    let root = traj();
    let parent = views.views(&root).expect("the parent is opened");
    assert!(!parent.has_ended(&child), "a crossing ends nothing");
    assert_eq!(parent.returns_by(&child), 1);

    let replay = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "done", vec![]))
        .expect("a repeat of the latest crossing answers from the record");
    assert_eq!(replay.append, None);
    assert_eq!(
        replay.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("done")
        })
    );

    let second = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "more", vec![]))
        .expect("a later return crosses at the next occurrence");
    assert!(matches!(
        second.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { .. })
    ));
    let log = [log, appended_facts(second)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        child_returns(&log, &child)
            .iter()
            .map(|value| value.body.as_str().to_string())
            .collect::<Vec<_>>(),
        vec!["done", "more"]
    );

    let ended = e
        .handle(&viewing(&e, &log), child_report(&log, &child, ChildSubmission::Void))
        .expect("a void return ends the branch");
    assert_eq!(ended.follow_up, FollowUp::Child(ChildFollowUp::Ended));
    let log = [log, appended_facts(ended)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        e.handle(&viewing(&e, &log), report_with(&log, &child, "late", vec![])),
        Err(crate::transition::TransitionError::BranchEnded)
    );
    assert_eq!(
        e.handle(&viewing(&e, &log), child_report(&log, &child, ChildSubmission::Void)),
        Err(crate::transition::TransitionError::BranchEnded)
    );
}

#[test]
fn a_fan_out_spawn_is_offered_every_return_route_but_the_attestation() {
    use crate::fact::ReturnRoute;
    let e = open_engine(returning_registry(vec![
        lifting_sanitizer("attest-schema"),
        lifting_sanitizer("scrub"),
    ]));
    let log = vec![opened(&e)];
    let routes = |mark| {
        let blocked = e
            .handle(&viewing(&e, &log), spawn_batch("marked", Some(mark)))
            .expect("a marked spawn blocks");
        return_steps(&appended_facts(blocked))
    };
    let sanitized = |name: &str| ReturnRoute::Sanitized(SanitizerName::new(name));
    assert_eq!(
        routes(crate::transition::SpawnMark::at(0)),
        vec![
            ReturnRoute::Raw,
            sanitized("attest-schema"),
            sanitized("scrub"),
            ReturnRoute::Void
        ]
    );
    assert_eq!(
        routes(crate::transition::SpawnMark::fan_out(0)),
        vec![ReturnRoute::Raw, sanitized("scrub"), ReturnRoute::Void]
    );
}

#[test]
fn the_reserved_route_is_declared_only_with_its_shape() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")]));
    let log = vec![opened(&e)];
    let blocked = e
        .handle(
            &viewing(&e, &log),
            spawn_batch("marked", Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks");
    let log = [log, appended_facts(blocked)].concat();
    let attest = SanitizerName::new(SanitizerName::ATTEST_SCHEMA);
    let offer = opened_offers(&log)
        .into_iter()
        .find_map(|(offer, plan)| {
            (plan.return_step() == Some(&crate::fact::ReturnRoute::Sanitized(attest.clone()))).then_some(offer)
        })
        .expect("the attestation is offered");
    let execute = |sanitizer| {
        e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    sanitizer,
                    ..floor_crossing(&e, &log)
                })),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
    };

    assert!(matches!(
        execute(named("attest-schema")),
        Err(crate::transition::TransitionError::ReturnPolicy(
            plan::ReturnPolicyRefusal::RouteMismatch
        ))
    ));
    assert!(execute(Some(ReturnSanitizer::Attest(attest_shape()))).is_ok());
}

#[test]
fn an_attested_return_crosses_in_canonical_form_at_the_raised_rank() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("attest-schema")]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: Some(ReturnSanitizer::Attest(attest_shape())),
        ..floor_crossing(&e, &parent)
    });
    let mut log = [parent.clone(), spawn_family_at(&e, &parent, &child, policy)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    // The attestation raises trust, so the child may descend there.
    reads(&e, &mut log, &child, "read_suspicious");
    assert_eq!(
        viewing(&e, &log)
            .views(&child)
            .expect("the child is opened")
            .current_label()
            .trust,
        SUSPICIOUS
    );

    assert!(matches!(
        e.handle(&viewing(&e, &log), report_with(&log, &child, "free text", vec![])),
        Err(crate::transition::TransitionError::ReturnShapeMismatch(_))
    ));
    assert!(matches!(
        e.handle(
            &viewing(&e, &log),
            report_with(&log, &child, r#"{"verdict":"allow","confidence":101}"#, vec![])
        ),
        Err(crate::transition::TransitionError::ReturnShapeMismatch(_))
    ));

    let canonical = ValueBody::new(r#"{"confidence":97,"verdict":"allow"}"#);
    let merged = e
        .handle(
            &viewing(&e, &log),
            report_with(&log, &child, "{ \"verdict\": \"allow\",  \"confidence\": 97 }", vec![]),
        )
        .expect("a conforming submission crosses");
    assert_eq!(
        merged.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: canonical.clone()
        })
    );
    let facts = appended_facts(merged);
    assert!(matches!(
        &declared_records(&facts)[0],
        Fact::ChildReturn {
            value,
            derivation: ReturnDerivation::Sanitized { sanitizer, .. },
            ..
        } if value.body == canonical && value.label.trust == TRUSTED && sanitizer.is_attest_schema()
    ));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let replay = e
        .handle(
            &viewing(&e, &log),
            report_with(&log, &child, r#"{"verdict": "allow", "confidence": 97}"#, vec![]),
        )
        .expect("a repeat of the crossing answers from the record");
    assert_eq!(replay.append, None);
    assert_eq!(
        replay.follow_up,
        FollowUp::Child(ChildFollowUp::Merged { admitted: canonical })
    );
}

#[test]
fn a_named_sanitizer_stages_its_derivation_until_the_child_echoes_it() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: named("redactor"),
        ..floor_crossing(&e, &parent)
    });
    let mut log = [parent.clone(), spawn_family_at(&e, &parent, &child, policy)].concat();
    reads(&e, &mut log, &child, "read_suspicious");

    let asked = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "raw", vec![]))
        .expect("an underived submission asks for the derivation");
    assert_eq!(asked.append, None);
    assert!(matches!(
        asked.follow_up,
        FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { ref sanitizer, ref body, .. }))
            if sanitizer.as_str() == "redactor" && body.as_str() == "raw"
    ));

    let staged = e
        .handle(
            &viewing(&e, &log),
            report_with(&log, &child, "raw", derived_by("redactor", "raw", "clean")),
        )
        .expect("the derivation stages");
    assert_eq!(
        staged.follow_up,
        FollowUp::Child(ChildFollowUp::Staged {
            derived: ValueBody::new("clean")
        })
    );
    let facts = appended_facts(staged);
    assert!(facts.iter().any(|fact| matches!(
        fact,
        Fact::CandidateDerived {
            subject: crate::basis::SubjectKey::Return(_),
            derived: DerivedCandidate::Return { value, .. },
            ..
        } if value.body.as_str() == "clean" && value.label.trust == TRUSTED
    )));
    assert!(
        !facts.iter().any(|fact| matches!(fact, Fact::ChildReturn { .. })),
        "a derivation the child has not echoed crosses nothing"
    );
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert!(child_returns(&log, &child).is_empty(), "nothing crossed yet");

    let other = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "raw again", vec![]))
        .expect("a different stop is answered with the standing derivation");
    assert_eq!(other.append, None);
    assert_eq!(
        other.follow_up,
        FollowUp::Child(ChildFollowUp::Staged {
            derived: ValueBody::new("clean")
        })
    );

    let echoed = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "clean", vec![]))
        .expect("the echo crosses the derivation");
    assert_eq!(
        echoed.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("clean")
        })
    );
    let facts = appended_facts(echoed);
    assert!(matches!(
        &declared_records(&facts)[0],
        Fact::ChildReturn {
            value,
            derivation: ReturnDerivation::Sanitized { sanitizer, raw_digest, .. },
            ..
        } if value.body.as_str() == "clean"
            && value.label.trust == TRUSTED
            && sanitizer.as_str() == "redactor"
            && *raw_digest == RawResultDigest::of(b"raw")
    ));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    // A derivation equal to its submission needs no echo.
    let unchanged = e
        .handle(
            &viewing(&e, &log),
            report_with(&log, &child, "fine", derived_by("redactor", "fine", "fine")),
        )
        .expect("an unchanged derivation crosses at once");
    assert_eq!(
        unchanged.follow_up,
        FollowUp::Child(ChildFollowUp::Merged {
            admitted: ValueBody::new("fine")
        })
    );
    let log = [log, appended_facts(unchanged)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        viewing(&e, &log)
            .views(&traj())
            .expect("the parent is opened")
            .returns_by(&child),
        2
    );
}

#[test]
fn a_return_after_a_resume_is_a_new_crossing_even_when_its_bytes_repeat() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);
    let first = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "done", vec![]))
        .expect("the first return crosses");
    let log = [log, appended_facts(first)].concat();
    let fork = fork_in(&log, &child);
    let resumed = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork,
                child: child.clone(),
            }),
        )
        .expect("the same pair binds again as a resume");
    let log = [log, appended_facts(resumed)].concat();

    let again = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "done", vec![]))
        .expect("the resumed child's return crosses");
    let facts = appended_facts(again);
    assert!(
        matches!(
            declared_records(&facts),
            [
                Fact::ChildReturn { .. },
                Fact::ValueAdmitted { .. },
                Fact::Boundary {
                    kind: crate::fact::BoundaryKind::Merge { .. },
                    ..
                }
            ]
        ),
        "a return after a resume is no replay: {facts:?}"
    );
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    let views = viewing(&e, &log);
    assert_eq!(
        views.views(&traj()).expect("the parent is opened").returns_by(&child),
        2
    );
}

#[test]
fn a_resume_folds_the_parents_label_into_the_child_and_drops_its_staged_derivation() {
    let e = open_engine(returning_registry(vec![lifting_sanitizer("redactor")]));
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: named("redactor"),
        ..floor_crossing(&e, &parent)
    });
    let mut log = [parent.clone(), spawn_family_at(&e, &parent, &child, policy)].concat();
    let staged = e
        .handle(
            &viewing(&e, &log),
            report_with(&log, &child, "raw", derived_by("redactor", "raw", "clean")),
        )
        .expect("the derivation stages");
    log.extend(appended_facts(staged));

    // The parent narrows itself after the spawn, then addresses the child again.
    reads(&e, &mut log, &traj(), "read_internal");
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let fork = fork_in(&log, &child);
    let resumed = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fork.clone(),
                child: child.clone(),
            }),
        )
        .expect("the same pair binds again as a resume");
    assert_eq!(resumed.follow_up, FollowUp::Fork { child: child.clone() });
    let facts = appended_facts(resumed);
    assert!(matches!(
        facts.last(),
        Some(Fact::Boundary {
            kind: crate::fact::BoundaryKind::Resume { seed },
            ..
        }) if seed.audience == internal
    ));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        viewing(&e, &log)
            .views(&child)
            .expect("the child is opened")
            .current_label()
            .audience,
        internal,
        "the parent's label as it stands now flows into the child"
    );

    let echoed = e
        .handle(&viewing(&e, &log), report_with(&log, &child, "clean", vec![]))
        .expect("the old derivation no longer stands");
    assert_eq!(echoed.append, None);
    assert!(matches!(
        echoed.follow_up,
        FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { .. }))
    ));

    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork,
                child: TrajectoryId::new("stranger"),
            }),
        ),
        Err(crate::transition::TransitionError::UnbindableFork)
    );
}

/// A child blocked on `get_ticket`, its parent then lowered below the tool's trust floor by a
/// read, and the child resumed.
fn resumed_under_a_narrowed_parent(approve_first: bool) -> (Engine, Vec<Fact>, TrajectoryId, crate::value::OfferId) {
    let mut cfg = returning_registry(vec![]);
    cfg.tools.push(crate::contract::ToolDeclaration::Declared(crm_tool()));
    let e = open_engine(cfg);
    let child = TrajectoryId::new("child");
    let parent = vec![opened(&e)];
    let mut policy = floor_crossing(&e, &parent);
    policy.floor.audience = Audience::restricted([ReaderId::new("insider")]);
    let mut log = [
        parent.clone(),
        spawn_family_at(&e, &parent, &child, ReturnPolicy::Crossing(policy)),
    ]
    .concat();
    let blocked = e
        .handle(
            &viewing(&e, &log),
            batch_on(
                &child,
                "c1",
                Vec::new(),
                vec![raw(&call("get_ticket", json!({})))],
                None,
            ),
        )
        .expect("the child's batch decides");
    let facts = appended_facts(blocked);
    let offer = opened_offers(&facts)[0].0;
    log.extend(facts);
    if approve_first {
        let approved =
            executed_by(&e, &log, &child, offer).expect("the offer executes under the label it was opened on");
        log.extend(appended_facts(approved));
    }

    reads(&e, &mut log, &traj(), "read_suspicious");
    let resumed = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fork_in(&log, &child),
                child: child.clone(),
            }),
        )
        .expect("the same pair binds again as a resume");
    log.extend(appended_facts(resumed));
    assert_eq!(e.validate_replay(&log), Ok(()));
    (e, log, child, offer)
}

fn executed_by(
    e: &Engine,
    log: &[Fact],
    trajectory: &TrajectoryId,
    offer: crate::value::OfferId,
) -> Result<EngineDecision, TransitionError> {
    e.handle(
        &viewing(e, log),
        EngineEvent::ExecuteOffer(OfferExecution {
            trajectory: trajectory.clone(),
            offer,
            outcome: OfferOutcome::Approved(Vec::new()),
            return_policy: None,
            offer_nonce: crate::value::OfferNonce::new([11u8; 32]),
            audience: crate::audience::AudienceEvidence::default(),
        }),
    )
}

#[test]
fn a_resume_that_moves_the_childs_label_ends_the_offers_opened_under_the_old_one() {
    let (e, log, child, offer) = resumed_under_a_narrowed_parent(false);
    assert_eq!(
        executed_by(&e, &log, &child, offer).map(|_| ()),
        Err(TransitionError::StaleOffer)
    );
}

#[test]
fn a_resume_that_moves_the_childs_label_ends_the_approvals_granted_under_the_old_one() {
    let (e, log, child, _) = resumed_under_a_narrowed_parent(true);
    let again = e
        .handle(
            &viewing(&e, &log),
            batch_on(
                &child,
                "c2",
                Vec::new(),
                vec![raw(&call("get_ticket", json!({})))],
                None,
            ),
        )
        .expect("the child's batch decides");
    assert!(
        matches!(answered(&again), ([], [_])),
        "the approval stood on the label the resume replaced"
    );
}

#[test]
fn a_child_stop_carries_its_parents_declared_floor_to_a_grandchild() {
    let e = open_engine(returning_registry(vec![]));
    let child = TrajectoryId::new("child");
    let log = spawn_family(&e, &child);
    let floor = floor_crossing(&e, &log).floor;
    let views = viewing(&e, &log);
    assert_eq!(
        e.return_policy_of(&views, &child),
        Some(ReturnPolicy::Crossing(floor_crossing(&e, &log)))
    );
    // The child spawning its own child may declare no floor below the one it stands under.
    let batch = EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new("grandchild"),
        trajectory: child.clone(),
        provider_results: Vec::new(),
        proposals: vec![raw(&call("spawn", json!({})))],
        spawn: Some(crate::transition::SpawnMark::at(0)),
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    });
    let blocked = e.handle(&views, batch).expect("the child's marked spawn blocks");
    let facts = appended_facts(blocked);
    let (offer, _) = opened_offers(&facts)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
        .expect("the bare declaration is offered");
    let log = [log, facts].concat();
    let execute = |floor: Label| {
        e.handle(
            &viewing(&e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: child.clone(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    floor,
                    sanitizer: None,
                })),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
    };
    let below = execute(Label::new(SUSPICIOUS, floor.audience.clone()));
    assert!(
        matches!(
            below,
            Err(crate::transition::TransitionError::ReturnPolicy(
                plan::ReturnPolicyRefusal::FloorBelowOwn
            ))
        ),
        "got {below:?}"
    );
    assert!(execute(floor).is_ok());
}
