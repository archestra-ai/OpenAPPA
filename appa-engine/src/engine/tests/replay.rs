use super::*;

#[test]
fn a_release_replays_only_as_the_records_its_decision_obliged() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let call = call("get_ticket", json!({}));
    let records = vec![opened(&e)];
    let view = e.view(&traj(), records.clone(), 1).unwrap();
    let decision = e
        .handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .unwrap();
    let batch = decision.append.expect("the release appends").facts().to_vec();
    let log = [records.clone(), batch.clone()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let interleaved = [
        records.clone(),
        vec![
            batch[0].clone(),
            stray_admission(&traj(), Label::new(SUSPICIOUS, Audience::public())),
            batch[1].clone(),
        ],
    ]
    .concat();
    assert_eq!(
        e.validate_replay(&interleaved),
        Err(TransitionRefusal::UnbackedDecision)
    );

    let tampered = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = batch.clone();
        mutate(&mut facts[1]);
        e.validate_replay(&[records.clone(), facts].concat())
    };
    assert_eq!(
        tampered(&|fact| {
            if let Fact::DispatchOpened { annotation, .. } = fact {
                // A pin on a statically declared dispatch record cannot restate the
                // decision that released it: the decided call carries none.
                let ghost = ResolvedCall::new(
                    crate::value::ToolName::new("forged"),
                    crate::params::test_arguments(&json!({})),
                );
                *annotation = Some(pinned_for(plain_tool("forged"), "ghost", &ghost));
            }
        }),
        Err(TransitionRefusal::UnbackedDecision)
    );
    assert_eq!(
        tampered(&|fact| {
            if let Fact::DispatchOpened { proposed_effects, .. } = fact {
                *proposed_effects = EffectSet::new([EffectKind::new("email.sent")]).unwrap();
            }
        }),
        Err(TransitionRefusal::EffectsMismatch)
    );
    assert_eq!(
        tampered(&|fact| {
            if let Fact::DispatchOpened { proposed_label, .. } = fact {
                *proposed_label = Label::top();
            }
        }),
        Err(TransitionRefusal::ForgedLabel)
    );
}

#[test]
fn a_crossing_replays_only_with_its_admission_and_its_merge() {
    let child = TrajectoryId::new("child");
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![], Label::new(SUSPICIOUS, internal.clone()));
    let mut log = vec![opened(&e)];
    log.extend(forked_child(&e, &log.clone(), &child));
    let crossing = crossed_report(&e, &log, &child, "the answer");
    let whole = [log.clone(), crossing.clone()].concat();
    assert_eq!(e.validate_replay(&whole), Ok(()));

    assert_eq!(
        e.validate_replay(&[log.clone(), vec![crossing[0].clone()]].concat()),
        Err(TransitionRefusal::UnmergedCrossing)
    );
    assert_eq!(
        e.validate_replay(&[log.clone(), vec![crossing[0].clone(), crossing[2].clone()]].concat()),
        Err(TransitionRefusal::RepeatAdmission)
    );
    let mut forged = whole.clone();
    if let Fact::ValueAdmitted { value, .. } = &mut forged[log.len() + 1] {
        *value = LabeledValue::new(ValueBody::new("something else"), value.label.clone());
    }
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ForgedLabel));
}

#[test]
fn a_result_replays_only_for_a_successful_close() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let call = call("get_ticket", json!({}));
    let mut log = vec![opened(&e)];
    let dispatch = open(&e, &mut log, &call);
    let admitted = Fact::ValueAdmitted {
        trajectory: traj(),
        value: LabeledValue::new(
            ValueBody::new("ticket"),
            e.registry()
                .tool(call.tool())
                .unwrap()
                .declared()
                .expect("a static declaration")
                .output_label(),
        ),
        provenance: Provenance::ToolResult {
            dispatch: dispatch.clone(),
        },
    };
    let closed = |outcome: crate::fact::CloseOutcome| Fact::DispatchClosed {
        trajectory: traj(),
        dispatch: dispatch.clone(),
        outcome,
    };
    assert_eq!(
        e.validate_replay(
            &[
                log.clone(),
                vec![
                    closed(crate::fact::CloseOutcome::Success {
                        effects: EffectSet::default()
                    }),
                    admitted.clone()
                ]
            ]
            .concat()
        ),
        Ok(())
    );
    for refused in [
        crate::fact::CloseOutcome::Failure,
        crate::fact::CloseOutcome::Indeterminate,
    ] {
        assert_eq!(
            e.validate_replay(&[log.clone(), vec![closed(refused), admitted.clone()]].concat()),
            Err(TransitionRefusal::DispatchNotOpen)
        );
    }
}

#[test]
fn an_ended_branch_releases_nothing_more() {
    let child = TrajectoryId::new("child");
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let mut log = vec![opened(&e)];
    log.extend(forked_child(&e, &log.clone(), &child));
    let ended = branch::submit_void_return(&Projection::build(&log, log.len() as u64).view(&traj()), &child)
        .expect("the child ends with no value");
    log.extend(ended);
    let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

    let call = call("get_ticket", json!({}));
    assert_eq!(
        e.handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: child.clone(),
                provider_results: Vec::new(),
                proposals: vec![raw(&call)],
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::BranchEnded)
    );
    let forged = [
        log,
        vec![Fact::DispatchOpened {
            trajectory: child.clone(),
            dispatch: DispatchId::new(child.clone(), call.digest(), 0),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            receiving: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            proposed_effects: EffectSet::default(),
            annotation: None,
            file_basis: None,
            subject: crate::basis::fixture_subject(&child),
            evidence: crate::audience::AudienceEvidence::default(),
            terms: None,
        }],
    ]
    .concat();
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::BranchEnded));
}

#[test]
fn an_admission_after_a_checkpoint_carries_the_bytes_it_observed() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let call = call("get_ticket", json!({}));
    let mut log = vec![opened(&e)];
    let dispatch = open(&e, &mut log, &call);
    let body = ValueBody::new("the ticket");
    let checkpoint = admit::observe_success(
        &e.registry,
        &Projection::build(&log, log.len() as u64).view(&traj()),
        &dispatch,
        &call,
        crate::fact::ObservedResult::Available(crate::value::RawResultDigest::of(body.as_str().as_bytes())),
    )
    .expect("an open dispatch checkpoints");
    log.extend(checkpoint);
    let views = Projection::build(&log, log.len() as u64);

    assert_eq!(
        admit::admit_result(
            &e.registry,
            &views.view(&traj()),
            &dispatch,
            &call,
            crate::admit::ResultAdmission::SuccessRaw {
                body: ValueBody::new("other bytes"),
            },
            &crate::label::TestContext::default().context(),
            &crate::audience::AudienceEvidence::default(),
        )
        .expect_err("other bytes are another observation"),
        crate::admit::AdmitError::ObservationMismatch
    );
    assert!(
        admit::admit_result(
            &e.registry,
            &views.view(&traj()),
            &dispatch,
            &call,
            crate::admit::ResultAdmission::SuccessRaw { body },
            &crate::label::TestContext::default().context(),
            &crate::audience::AudienceEvidence::default(),
        )
        .is_ok(),
        "the observed bytes admit"
    );
}

#[test]
fn a_sanitizer_settlement_replays_only_where_the_deployment_confines_the_result() {
    let declassify = crate::authority::Sanitizer {
        name: crate::names::SanitizerName::new("declassify"),
        on: crate::authority::SanitizerPoints {
            input: false,
            output: true,
        },
        transition: crate::authority::DeclaredTransition::Audience {
            from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
            to: DeclaredAudience::literal(Audience::public()),
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let config = RegistryConfig {
        sanitizers: vec![declassify],
        ..test_config(vec![crm_tool()])
    };
    let call = call("get_ticket", json!({}));

    let confining = open_engine(config.clone());
    let (log, _) = released_under_output_sanitizer(&confining, vec![opened(&confining)], &call);
    assert_eq!(confining.validate_replay(&log), Ok(()));

    let mut declaration = crate::profile::covering_declaration(&config);
    declaration.confined_results.clear();
    let unconfined = Engine::open(DeploymentPolicy {
        registry: config,
        planner_cap: crate::registry::PlannerCap::default(),
        dialect: PolicyDialectVersion::new(1),
        profile: declaration,
    })
    .expect("an unconfined deployment opens");
    let forged = [vec![opened(&unconfined)], log[1..].to_vec()].concat();
    assert_eq!(
        unconfined.validate_replay(&forged),
        Err(TransitionRefusal::UnbackedOffer)
    );
}

#[test]
fn replay_holds_a_fork_to_its_parents_frozen_basis() {
    let e = engine(vec![plain_tool("spawn"), suspicious_read()]);
    let child = TrajectoryId::new("child");
    let mut base = vec![opened(&e)];
    reads(&e, &mut base, &traj(), "read_suspicious");
    let basis_after = |log: &[Fact]| Projection::build(log, log.len() as u64).view(&traj()).freeze_basis();

    let seeded = forked_child(&e, &base, &child);
    let log = [base.clone(), seeded.clone()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let mut dropped = log.clone();
    let snapshot = dropped
        .iter_mut()
        .rev()
        .find_map(|fact| match fact {
            Fact::ForkPrepared { snapshot, .. } => Some(snapshot),
            _ => None,
        })
        .expect("the release prepared a fork");
    *snapshot = basis_after(&[opened(&e)]);
    assert_eq!(e.validate_replay(&dropped), Err(TransitionRefusal::ForkBasisMismatch));

    let fork = seeded
        .iter()
        .find_map(|fact| match fact {
            Fact::ForkPrepared { fork, .. } => Some(fork.clone()),
            _ => None,
        })
        .expect("the release prepared a fork");
    let prepared: Vec<Fact> = seeded
        .iter()
        .take_while(|fact| !matches!(fact, Fact::ForkOpened { .. }))
        .cloned()
        .collect();
    let self_bind = [
        base.clone(),
        prepared,
        vec![Fact::ForkOpened {
            trajectory: traj(),
            fork,
        }],
    ]
    .concat();
    assert_eq!(
        e.validate_replay(&self_bind),
        Err(TransitionRefusal::ChildActiveBeforeFork)
    );
}

#[test]
fn a_value_admitted_under_an_unopened_dispatch_is_refused_on_replay() {
    let e = engine(vec![suspicious_read()]);
    let read_call = crate::value::ResolvedCall::new(
        ToolName::new("read_suspicious"),
        crate::params::test_arguments(&serde_json::json!({})),
    );
    assert_eq!(
        e.validate_replay(&[
            opened(&e),
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: crate::value::LabeledValue::new(
                    crate::value::ValueBody::new("page"),
                    Label::new(SUSPICIOUS, Audience::public()),
                ),
                provenance: crate::value::Provenance::ToolResult {
                    dispatch: DispatchId::new(traj(), read_call.digest(), 7),
                },
            }
        ]),
        Err(TransitionRefusal::UnknownDispatch)
    );
}
