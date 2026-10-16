use super::*;

#[test]
fn the_opening_batch_carries_the_identity_and_derived_vectors() {
    let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
    let t = traj();
    let key = crate::profile::PolicyFileKey::of(b"the policy file");
    let batch = e
        .open_trajectory(&t, key.clone(), None)
        .expect("a fresh root's opening seals");
    assert_eq!(batch.basis(), 0, "the opening stands on the empty log");
    match batch.facts() {
        [
            Fact::TrajectoryOpened(crate::fact::TrajectoryOpening {
                trajectory,
                dialect,
                profile,
                policy_digest,
                policy_file_key,
                open_vectors,
                forked_from,
                principal,
            }),
        ] => {
            assert_eq!(forked_from, &None, "a fresh root opens as no fork");
            assert_eq!(principal, &None, "an opening no host named a principal for has none");
            assert_eq!(policy_file_key, &key, "the opening names the file it opened under");
            assert_eq!(trajectory, &t);
            assert_eq!(*dialect, PolicyDialectVersion::new(1));
            assert_eq!(profile, e.profile());
            assert_eq!(*policy_digest, e.identity());
            assert_eq!(open_vectors, &e.open_vectors());
            assert_eq!(open_vectors.len(), 1);
        }
        other => panic!("expected exactly the opening record, got {other:?}"),
    }
    let wire = serde_json::to_string(batch.facts()).unwrap();
    assert_eq!(serde_json::from_str::<Vec<Fact>>(&wire).unwrap(), batch.facts());
}

#[test]
fn cold_replay_verifies_the_opening_strictly() {
    use crate::transition::OpeningTransitionRefusal;
    let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
    let t = traj();
    let opening = opened_root(&e, &t);
    let mut valid = vec![opening.clone()];
    reads(&e, &mut valid, &t, "send");
    let admitted = valid[1].clone();
    let replay =
        |family: &TrajectoryId, facts: Vec<Fact>| e.view(family, facts.clone(), facts.len() as u64).map(|_| ());

    assert_eq!(replay(&t, valid.clone()), Ok(()));
    assert_eq!(replay(&t, vec![admitted.clone()]), Err(TransitionRefusal::Unopened));
    assert_eq!(
        replay(&t, vec![admitted.clone(), opening.clone()]),
        Err(TransitionRefusal::Unopened)
    );
    assert_eq!(
        replay(&t, vec![opening.clone(), opening.clone()]),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::Duplicate))
    );
    assert_eq!(
        replay(&t, [valid.clone(), vec![opening.clone()]].concat()),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::Duplicate))
    );
    assert_eq!(
        replay(&TrajectoryId::new("other"), vec![opening.clone()]),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::WrongTrajectory {
            found: "t".to_string()
        }))
    );

    let mutated = |mutate: &dyn Fn(&mut Fact)| {
        let mut fact = opening.clone();
        mutate(&mut fact);
        replay(&t, vec![fact])
    };
    assert_eq!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { dialect, .. }) = fact {
                *dialect = PolicyDialectVersion::new(9);
            }
        }),
        Err(TransitionRefusal::Opening(
            OpeningTransitionRefusal::UnsupportedDialect { found: 9 }
        ))
    );
    assert_eq!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { policy_digest, .. }) = fact {
                let other = engine(vec![plain_tool("send")]);
                *policy_digest = other.identity();
            }
        }),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::DigestMismatch))
    );
    // An opening an earlier engine recorded carries the identity its algorithm gave this
    // policy, and names this policy as surely as the current identity does.
    let earlier = *e
        .earlier_identities()
        .first()
        .expect("a policy that forwards nothing has a v1 identity");
    assert_ne!(earlier, e.identity(), "the algorithms differ in their domain tag");
    assert!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { policy_digest, .. }) = fact {
                *policy_digest = earlier;
            }
        })
        .is_ok(),
        "the v1 identity of this policy reopens its trajectory"
    );
    assert_eq!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { policy_digest, .. }) = fact {
                let other = engine(vec![plain_tool("send")]);
                *policy_digest = other.earlier_identities()[0];
            }
        }),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::DigestMismatch)),
        "the v1 identity of another policy does not"
    );
    assert_eq!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { profile, .. }) = fact {
                let other = engine(vec![plain_tool("send")]);
                *profile = other.profile().clone();
            }
        }),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::ProfileMismatch))
    );
    assert_eq!(
        mutated(&|fact| {
            if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { open_vectors, .. }) = fact {
                open_vectors.clear();
            }
        }),
        Err(TransitionRefusal::Opening(OpeningTransitionRefusal::VectorMismatch))
    );
}

/// The view of `root`'s own family log.
fn viewing_root(e: &Engine, root: &TrajectoryId, log: &[Fact]) -> EngineView {
    e.view(root, log.to_vec(), log.len() as u64)
        .expect("the root's log replays")
}

/// The whole log of `fork`, an independent root fork of `parent` where `view` stands: its
/// one opening record.
fn root_fork_log(e: &Engine, view: &EngineView, parent: &TrajectoryId, fork: &TrajectoryId) -> Vec<Fact> {
    let origin = view
        .root_fork_origin(parent)
        .expect("an open trajectory can be root-forked");
    e.open_root_fork(fork, crate::profile::PolicyFileKey::of(b"policy"), origin)
        .expect("a fork of another family's trajectory seals")
        .into_unsealed()
}

/// The requirement gaps `tool` blocks on as `root`'s next proposal, or `None` when it is
/// released.
fn gaps_of(e: &Engine, root: &TrajectoryId, log: &[Fact], tool: &str) -> Option<Vec<Gap>> {
    let decision = e
        .handle(
            &viewing_root(e, root, log),
            batch_on(root, tool, Vec::new(), vec![raw(&call(tool, json!({})))], None),
        )
        .expect("the call decides");
    match answered(&decision) {
        ([_], []) => None,
        ([], [blocked]) => Some(blocked.block.raw.requirement_gaps.clone()),
        other => panic!("one call decides one way, got {other:?}"),
    }
}

/// An independent root fork carries its parent family's history as it stood at the fork. A committed effect
/// answers `prior(k)` there and blocks `no_prior(k)`. An unsettled reservation, whether its
/// call is still open or closed indeterminate, blocks `no_prior(k)` in the fork as it does in
/// the parent and never counts as committed. No call of the fork can settle it: it stands
/// there after the parent's call fails, and a fork of the fork carries it on.
#[test]
fn a_root_fork_carries_its_parent_familys_effects_and_unsettled_reservations() {
    let e = batch_engine();
    let k = EffectKind::new("k");
    let emitted = |log: &[Fact], id: &str| {
        let decision = e
            .handle(
                &viewing(&e, log),
                batch(id, Vec::new(), vec![raw(&call("emit", json!({})))]),
            )
            .expect("the emit decides");
        let dispatch = match answered(&decision) {
            ([release], []) => release.dispatch.clone(),
            other => panic!("a neutral emit releases, got {other:?}"),
        };
        ([log, &appended_facts(decision)].concat(), dispatch)
    };
    let settled = |log: &[Fact], dispatch: &DispatchId, outcome: ToolOutcome| {
        let closed = e
            .handle(
                &viewing(&e, log),
                EngineEvent::Outcome(ToolReport {
                    dispatch: dispatch.clone(),
                    outcome,
                    evidence: Vec::new(),
                    offer_nonce: nonce(),
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the outcome closes the emit");
        [log, &appended_facts(closed)].concat()
    };
    let success = || ToolOutcome::Success {
        body: OutcomeBody::Available(ValueBody::new("emitted")),
    };
    let (open, dispatch) = emitted(&opening_log(&e), "emit");
    let indeterminate = settled(&open, &dispatch, ToolOutcome::Indeterminate);
    let failed = settled(&open, &dispatch, ToolOutcome::Failure);
    let committed = settled(&open, &dispatch, success());
    let (again, repeat) = emitted(&committed, "emit-again");
    let committed_twice = settled(&again, &repeat, success());
    let (parent, no_prior, prior) = (
        traj(),
        Some(vec![Gap::NoPrior(k.clone())]),
        Some(vec![Gap::Prior(k.clone())]),
    );

    assert_eq!(gaps_of(&e, &parent, &indeterminate, "guard"), no_prior);
    for (log, name) in [(&open, "open"), (&indeterminate, "indeterminate")] {
        let fork = TrajectoryId::new(name);
        let forked = root_fork_log(&e, &viewing(&e, log), &parent, &fork);
        assert_eq!(
            gaps_of(&e, &fork, &forked, "guard"),
            no_prior,
            "the parent's {name} reservation blocks no_prior(k) in the fork"
        );
        assert_eq!(
            gaps_of(&e, &fork, &forked, "wire"),
            prior,
            "a reservation is never a committed effect"
        );
    }
    let fork = TrajectoryId::new("committed");
    let forked = root_fork_log(&e, &viewing(&e, &committed), &parent, &fork);
    assert_eq!(
        gaps_of(&e, &fork, &forked, "guard"),
        no_prior,
        "a committed k blocks too"
    );
    assert_eq!(gaps_of(&e, &fork, &forked, "wire"), None, "and answers prior(k)");

    // The emit fails after the first fork was taken. The parent is free of k again, but that
    // fork never learns how the call ended: it stays blocked, as checked above, and a fork of
    // it carries the reservation on. A fork taken after the failure carries nothing.
    assert_eq!(gaps_of(&e, &parent, &failed, "guard"), None);
    let fork = TrajectoryId::new("open");
    let forked = root_fork_log(&e, &viewing(&e, &open), &parent, &fork);
    let second = TrajectoryId::new("second");
    let refork = root_fork_log(&e, &viewing_root(&e, &fork, &forked), &fork, &second);
    assert_eq!(gaps_of(&e, &second, &refork, "guard"), no_prior);
    let after = TrajectoryId::new("after");
    let forked = root_fork_log(&e, &viewing(&e, &failed), &parent, &after);
    assert_eq!(
        gaps_of(&e, &after, &forked, "guard"),
        None,
        "a settled call carries nothing"
    );

    // The origin holds each kind once, however often the family committed it, and keeps
    // what is committed apart from what is only reserved.
    let origin = |log: &[Fact]| viewing(&e, log).root_fork_origin(&parent).expect("the parent is open");
    let just_k = EffectSet::new([k.clone()]).unwrap();
    assert_eq!(origin(&committed_twice).effects, just_k);
    assert!(origin(&committed_twice).reservations.is_empty());
    assert_eq!(origin(&open).reservations, just_k);
    assert!(origin(&open).effects.is_empty());
}

/// An independent root fork can be taken from any open trajectory of a family. A spawned
/// child's root fork starts
/// at the child's own label: neither its root's, which moved on after the spawn, nor the
/// deployment's starting label. A trajectory the family never opened, or a child that has
/// ended, has no origin to fork from.
#[test]
fn a_root_fork_starts_at_the_forked_trajectorys_label_while_it_is_open() {
    let e = engine(vec![internal_read(), suspicious_read()]);
    let (root, child) = (traj(), TrajectoryId::new("child"));
    let mut log = vec![opened(&e)];
    reads(&e, &mut log, &root, "read_internal");
    log.extend(forked_child(&e, &log, &child));
    reads(&e, &mut log, &root, "read_suspicious");
    let view = viewing(&e, &log);
    let label = |trajectory: &TrajectoryId| {
        view.views(trajectory)
            .expect("the trajectory is opened")
            .current_label()
    };
    let fresh = viewing_root(&e, &root, &[opened(&e)])
        .views(&root)
        .expect("the root is opened")
        .current_label();
    assert_ne!(label(&child), label(&root), "the root narrowed after the spawn");
    assert_ne!(label(&child), fresh, "the child inherited the root's earlier narrowing");

    let origin = view.root_fork_origin(&child).expect("a live child can be root-forked");
    assert!(origin.is_from(&root, &child));
    let fork = TrajectoryId::new("fork");
    let forked = root_fork_log(&e, &view, &child, &fork);
    assert_eq!(
        viewing_root(&e, &fork, &forked)
            .views(&fork)
            .expect("the fork is opened")
            .current_label(),
        label(&child)
    );

    assert_eq!(view.root_fork_origin(&TrajectoryId::new("stranger")), None);
    let ended = e
        .handle(&view, child_report(&log, &child, ChildSubmission::Void))
        .expect("a void return ends the child");
    let ended = viewing(&e, &[log, appended_facts(ended)].concat());
    assert_eq!(
        ended.root_fork_origin(&child),
        None,
        "an ended child cannot be root-forked"
    );
    assert!(ended.root_fork_origin(&root).is_some(), "its root still can");
}

/// A root fork's opening names another family. The engine seals no opening whose origin forks the
/// root from itself, and a replay refuses one found in a log, whether the origin names the
/// root as the parent's family or as the parent trajectory.
#[test]
fn a_root_fork_opening_that_forks_the_root_from_itself_is_refused() {
    use crate::transition::OpeningTransitionRefusal;
    let e = batch_engine();
    let child = TrajectoryId::new("child");
    let view = viewing(&e, &spawn_family(&e, &child));
    let own = view.root_fork_origin(&traj()).expect("the root can be root-forked");
    let childs = view.root_fork_origin(&child).expect("the child can be root-forked");
    let key = crate::profile::PolicyFileKey::of(b"policy");
    let self_fork = Err(TransitionRefusal::Opening(OpeningTransitionRefusal::SelfFork));

    let sealed = |root: &TrajectoryId, origin: &crate::fact::RootForkOrigin| {
        e.open_root_fork(root, key.clone(), origin.clone()).map(|_| ())
    };
    assert_eq!(sealed(&traj(), &own), self_fork);
    assert_eq!(sealed(&child, &childs), self_fork);

    let replayed = |root: &TrajectoryId, origin: &crate::fact::RootForkOrigin| {
        let mut opening = opened_root(&e, root);
        if let Fact::TrajectoryOpened(crate::fact::TrajectoryOpening { forked_from, .. }) = &mut opening {
            *forked_from = Some(origin.clone());
        }
        e.view(root, vec![opening], 1).map(|_| ())
    };
    assert_eq!(replayed(&traj(), &own), self_fork);
    assert_eq!(replayed(&child, &childs), self_fork);
    assert_eq!(
        replayed(&TrajectoryId::new("fork"), &childs),
        Ok(()),
        "the same origin opens a root of another name"
    );
}

#[test]
fn a_fork_records_the_return_policy_its_spawn_approved() {
    let cfg = RegistryConfig {
        sanitizers: vec![crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }],
        ..test_config(vec![plain_tool("spawn")])
    };
    let e = open_engine(cfg);
    let child = TrajectoryId::new("child");
    let log = vec![opened(&e)];
    let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
        sanitizer: Some(ReturnSanitizer::Named(crate::names::SanitizerName::new("redactor"))),
        ..floor_crossing(&e, &log)
    });
    let forked = spawn_family_at(&e, &log, &child, policy.clone());
    let prepared = forked
        .iter()
        .find_map(|fact| match fact {
            Fact::ForkPrepared { return_policy, .. } => Some(return_policy.clone()),
            _ => None,
        })
        .expect("the release prepares a fork");
    assert_eq!(prepared, policy);
    assert_eq!(
        e.return_policy_of(&viewing(&e, &[log.clone(), forked.clone()].concat()), &child),
        Some(policy)
    );
    assert_eq!(e.validate_replay(&[log, forked].concat()), Ok(()));
}

#[test]
fn a_root_folds_from_its_openings_starting_label() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![plain_tool("send")], Label::new(SUSPICIOUS, internal.clone()));
    let t = traj();
    let starting = Label::new(SUSPICIOUS, internal.clone());

    let replayed = e.view(&t, vec![opened(&e)], 1).expect("the opened root replays");
    assert_eq!(
        replayed.views(&t).expect("the root is opened").current_label(),
        starting
    );
    assert_eq!(Projection::build(&[opened(&e)], 1).view(&t).current_label(), starting);
    let mut advanced = EngineView::validated(Projection::empty(0), e.identity(), t.clone());
    advanced
        .advance(
            &e.open_trajectory(&t, crate::profile::PolicyFileKey::of(b"policy"), None)
                .expect("the opening seals"),
        )
        .expect("the sealed opening advances the empty view");
    assert_eq!(
        advanced.views(&t).expect("the root is opened").current_label(),
        starting
    );

    let send = call("send", json!({}));
    let released = e
        .handle(&replayed, batch("b1", Vec::new(), vec![raw(&send)]))
        .expect("a neutral send releases");
    let dispatch = match &released.follow_up {
        FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
        other => panic!("the send releases, got {other:?}"),
    };
    let mut log = [vec![opened(&e)], appended_facts(released)].concat();
    let closed = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("sent")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the result admits");
    log.extend(appended_facts(closed));
    let after = viewing(&e, &log);
    let views = after.views(&t).expect("the root is opened");
    assert_eq!(
        views.current_label(),
        starting,
        "a neutral result leaves the fold in place"
    );
}

#[test]
fn an_unopened_trajectory_has_no_views_and_takes_no_event() {
    let e = engine(vec![plain_tool("send")]);
    let unopened = TrajectoryId::new("child");
    let view = viewing(&e, &[opened(&e)]);
    assert!(view.views(&traj()).is_some());
    assert!(view.views(&unopened).is_none());
    assert_eq!(
        e.handle(
            &view,
            batch_on(&unopened, "b1", Vec::new(), vec![raw(&call("send", json!({})))], None),
        ),
        Err(TransitionError::UnopenedTrajectory)
    );
    assert_eq!(
        e.validate_replay(&[
            opened(&e),
            stray_admission(&unopened, Label::new(SUSPICIOUS, Audience::public()))
        ]),
        Err(TransitionRefusal::ForeignTrajectory)
    );
}
