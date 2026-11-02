use super::*;

#[test]
fn permuted_effect_declarations_produce_byte_identical_dispatch_facts() {
    let pay = |emits: [&str; 2]| ToolAnnotation {
        emits: EffectSet::new(emits.map(EffectKind::new)).unwrap(),
        ..plain_tool("pay")
    };
    let release = |contract: ToolAnnotation| {
        let e = engine(vec![contract]);
        let mut log = vec![opened(&e)];
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch_on(&traj(), "b1", Vec::new(), vec![raw(&call("pay", json!({})))], None),
            )
            .expect("the neutral call decides");
        let facts = appended_facts(decision);
        log.extend(facts.clone());
        (facts, log)
    };
    let (ab, log_ab) = release(pay(["spend", "audit"]));
    let (ba, log_ba) = release(pay(["audit", "spend"]));
    assert_eq!(serde_json::to_string(&ab).unwrap(), serde_json::to_string(&ba).unwrap());
    let revision = log_ab.len() as u64;
    assert_eq!(
        Projection::build(&log_ab, revision),
        Projection::build(&log_ba, revision)
    );
}

#[test]
fn clean_call_allows() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    match check_call(&e, &log, &call("get_ticket", json!({}))) {
        CheckOutcome::Block(b) => {
            assert!(b.narrowing.is_some());
            assert!(b.requirement_gaps.is_empty());
        }
        other => panic!("expected narrowing block, got {other:?}"),
    }
}

#[test]
fn the_boundary_releases_an_allowed_proposal_with_its_dispatch() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let records = vec![opened(&e)];
    let view = e.view(&traj(), records, 1).unwrap();
    let call = call("get_ticket", json!({}));

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

    let released = match &decision.follow_up {
        FollowUp::Proposals { released, blocked, .. } if blocked.is_empty() => released.clone(),
        other => panic!("expected a release, got {other:?}"),
    };
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].call, call);
    let appended = decision.append.expect("an allowed call opens a dispatch");
    let records = declared_records(appended.facts());
    assert!(matches!(
        &records[0],
        Fact::ProposalBatchDecided { batch, .. } if batch.as_str() == "b1"
    ));
    match &records[1] {
        Fact::DispatchOpened { dispatch, subject, .. } => {
            assert_eq!(dispatch, &released[0].dispatch);
            assert_eq!(
                subject,
                &crate::basis::SubjectKey::Call {
                    trajectory: traj(),
                    batch: crate::transition::ProposalBatchId::new("b1"),
                    position: 0,
                }
            );
        }
        other => panic!("the decision's first release record is its opening, got {other:?}"),
    }
    assert_eq!(records.len(), 2);
}

#[test]
fn a_repeated_batch_identity_returns_its_recorded_decision_and_a_reused_one_is_refused() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let records = vec![opened(&e)];
    let view = e.view(&traj(), records.clone(), 1).unwrap();
    let batch = |proposals: Vec<ResolvedCall>| {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new("b1"),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: proposals.iter().map(raw).collect(),
            spawn: None,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };
    let proposal = call("get_ticket", json!({}));
    let other = call("get_ticket", json!({ "id": "2" }));
    let call = proposal;

    let first = e.handle(&view, batch(vec![call.clone()])).unwrap();
    let appended_facts = first
        .append
        .clone()
        .expect("the first decision records itself")
        .into_unsealed();
    let decided = [records, appended_facts.clone()].concat();
    let after = e.view(&traj(), decided, 2).unwrap();

    let repeat = e.handle(&after, batch(vec![call.clone()])).unwrap();
    assert_eq!(repeat.append, None);
    assert_eq!(repeat.follow_up, first.follow_up);

    assert_eq!(
        e.handle(&after, batch(vec![other.clone()])),
        Err(crate::transition::TransitionError::BatchIdentityConflict)
    );

    let decision = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new("b1"),
        proposals: vec![call.clone()],
        spawn: None,
        released,
        evidence: crate::audience::AudienceEvidence::default(),
    };
    let allowed = |records: Vec<Fact>| [vec![opened(&e)], records].concat();
    assert_eq!(
        e.validate_replay(&allowed(vec![decision(vec![]), decision(vec![])])),
        Err(TransitionRefusal::MisdecidedBatch)
    );
    let FollowUp::Proposals { released, .. } = &first.follow_up else {
        panic!("a proposal batch answers with proposals")
    };
    let dispatch = released[0].dispatch.clone();
    assert_eq!(
        e.validate_replay(&allowed(vec![decision(vec![])])),
        Err(TransitionRefusal::MisdecidedBatch)
    );
    assert_eq!(
        e.validate_replay(&allowed(vec![
            decision(vec![dispatch.clone()]),
            opening_of(&first),
            decision(vec![dispatch.clone()])
        ])),
        Err(TransitionRefusal::BatchIdentityConflict)
    );
    assert_eq!(
        e.validate_replay(&allowed(vec![decision(vec![dispatch.clone()])])),
        Err(TransitionRefusal::UnbackedDecision)
    );
    let other_id = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new("b2"),
        proposals: vec![call.clone()],
        spawn: None,
        released,
        evidence: crate::audience::AudienceEvidence::default(),
    };
    let opening = appended_facts[1].clone();
    assert_eq!(
        e.validate_replay(&allowed(vec![
            decision(vec![dispatch.clone()]),
            other_id(vec![dispatch.clone()]),
            opening.clone()
        ])),
        Err(TransitionRefusal::UnbackedDecision)
    );
    assert_eq!(
        e.validate_replay(&allowed(vec![
            decision(vec![dispatch.clone(), dispatch.clone()]),
            opening.clone()
        ])),
        Err(TransitionRefusal::MisdecidedBatch)
    );
    assert_eq!(
        e.validate_replay(&allowed(vec![
            Fact::ProposalBatchDecided {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b3"),
                proposals: vec![other.clone()],
                spawn: None,
                released: vec![dispatch],
                evidence: crate::audience::AudienceEvidence::default(),
            },
            opening
        ])),
        Err(TransitionRefusal::MisdecidedBatch)
    );
}

fn opening_of(decision: &EngineDecision) -> Fact {
    decision
        .append
        .as_ref()
        .expect("the decision records itself")
        .facts()
        .iter()
        .find(|fact| matches!(fact, Fact::DispatchOpened { .. }))
        .expect("a released decision opens its dispatch")
        .clone()
}

#[test]
fn a_decision_cannot_record_a_release_the_check_refuses() {
    let e = engine(vec![crm_tool()]);
    let call = call("get_ticket", json!({}));
    let public = opened(&e);
    let dispatch = DispatchId::new(traj(), call.digest(), 0);
    let forged = vec![
        public,
        Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            proposals: vec![call.clone()],
            spawn: None,
            released: vec![dispatch.clone()],
            evidence: crate::audience::AudienceEvidence::default(),
        },
        Fact::DispatchOpened {
            trajectory: traj(),
            dispatch,
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            receiving: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            proposed_effects: crate::fact::EffectSet::default(),
            annotation: None,
            file_basis: None,
            subject: crate::basis::fixture_subject(&traj()),
            evidence: crate::audience::AudienceEvidence::default(),
            terms: crate::fact::ReleaseTerms::none(),
        },
    ];
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::MisdecidedBatch));

    assert_eq!(
        e.validate_replay(&[forged[0].clone(), forged[2].clone()]),
        Err(TransitionRefusal::UnbackedDecision)
    );
}

#[test]
fn a_repeat_of_a_block_that_has_lifted_reports_a_spent_identity() {
    let e = engine(vec![crm_tool(), internal_read()]);
    let public = vec![opened(&e)];
    let call = call("get_ticket", json!({}));
    let event = EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new("b1"),
        trajectory: traj(),
        provider_results: Vec::new(),
        proposals: vec![raw(&call)],
        spawn: None,
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    });

    let view = e.view(&traj(), public.clone(), 1).unwrap();
    let decision = e.handle(&view, event.clone()).unwrap();
    let decided = decision.append.expect("the block records its decision").into_unsealed();

    let mut later = [public, decided].concat();
    reads(&e, &mut later, &traj(), "read_internal");
    let revision = later.len() as u64;
    let after = e.view(&traj(), later, revision).unwrap();

    let FollowUp::Proposals {
        released,
        blocked,
        spent,
        ..
    } = e.handle(&after, event).unwrap().follow_up
    else {
        panic!("a proposal batch answers with proposals")
    };
    assert!(released.is_empty() && blocked.is_empty());
    assert_eq!(spent, vec![call]);
}

#[test]
fn the_boundary_plans_a_blocked_proposal_and_opens_nothing() {
    let e = engine(vec![crm_tool(), plain_tool("send")]);
    let records = vec![opened(&e)];
    let view = e.view(&traj(), records, 1).unwrap();
    let call = call("get_ticket", json!({}));

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

    let appended = decision.append.clone().expect("the decision boundary is recorded");
    let offers_opened = appended
        .facts()
        .iter()
        .filter(|fact| matches!(fact, Fact::OfferOpened { .. }))
        .count();
    assert!(matches!(
        &appended.facts()[..2],
        [
            Fact::BasisAdvanced { advance, .. },
            Fact::ProposalBatchDecided { .. }
        ] if advance.is_empty()
    ));
    assert_eq!(offers_opened, appended.facts().len() - 2);
    assert!(offers_opened > 0);
    match &decision.follow_up {
        FollowUp::Proposals { released, blocked, .. } if released.is_empty() => {
            assert_eq!(blocked.len(), 1);
            assert_eq!(blocked[0].call, call);
            assert!(blocked[0].block.raw.narrowing.is_some());
        }
        other => panic!("expected a planned block, got {other:?}"),
    }
}

#[test]
fn repeat_at_same_label_is_not_a_narrowing() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal));
    let log = vec![opened(&e)];
    assert_eq!(
        check_call(&e, &log, &call("get_ticket", json!({}))),
        CheckOutcome::Allow
    );
}

fn gated(name: &str, delta: Delta, forwards: Delta, requires: LabelRequirements) -> ToolAnnotation {
    ToolAnnotation {
        delta,
        forwards,
        requires: Requires {
            label: requires,
            ..Requires::default()
        },
        ..plain_tool(name)
    }
}

fn contains(recipients: RecipientSpec) -> LabelRequirements {
    LabelRequirements {
        trust_floor: None,
        audience: vec![AudienceRequirement::Includes(recipients)],
    }
}

/// The delta labels what the call returns: a `contains` the session covers holds even when
/// the call's own result would narrow the audience below the recipient.
#[test]
fn a_delta_audience_labels_the_result_and_never_gates_the_call() {
    let narrowing = Delta {
        trust: None,
        audience: Some(DeltaAudience::Static(readers(&["a"]))),
    };
    let share = gated(
        "share",
        narrowing,
        Delta::NONE,
        contains(RecipientSpec::Static(readers(&["b"]))),
    );
    let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
    let e = engine_at(vec![share], Label::new(TRUSTED, both.clone()));
    let log = vec![opened(&e)];
    match check_call(&e, &log, &call("share", json!({}))) {
        CheckOutcome::Block(block) => {
            assert_eq!(block.requirement_gaps, vec![]);
            assert_eq!(
                block.narrowing,
                Some(crate::check::Narrowing {
                    from: Label::new(TRUSTED, both),
                    to: Label::new(TRUSTED, Audience::restricted([ReaderId::new("a")])),
                })
            );
        }
        other => panic!("expected the narrowing alone, got {other:?}"),
    }
}

#[test]
fn a_delta_trust_labels_the_result_and_never_gates_the_call() {
    let lowering = Delta {
        trust: Some(SUSPICIOUS),
        audience: None,
    };
    let e = engine(vec![gated("risky", lowering, Delta::NONE, floor(TRUSTED))]);
    let log = vec![opened(&e)];
    match check_call(&e, &log, &call("risky", json!({}))) {
        CheckOutcome::Block(block) => {
            assert_eq!(block.requirement_gaps, vec![]);
            assert!(block.narrowing.is_some());
        }
        other => panic!("expected the narrowing alone, got {other:?}"),
    }
}

/// What the call sends beyond the arguments gates it: a trusted session cannot lift
/// suspicious forwarded content over a trust floor, and the trajectory does not narrow.
#[test]
fn forwarded_content_below_the_floor_blocks_the_call() {
    let forwards = Delta {
        trust: Some(SUSPICIOUS),
        audience: None,
    };
    let e = engine(vec![gated("mail_report", Delta::NONE, forwards, floor(TRUSTED))]);
    let log = vec![opened(&e)];
    assert_eq!(
        check_call(&e, &log, &call("mail_report", json!({}))),
        CheckOutcome::Block(crate::check::RawBlock {
            requirement_gaps: vec![Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            }],
            narrowing: None,
        })
    );
}

#[test]
fn forwarded_content_bounds_the_recipients_a_call_reaches() {
    let forwards = Delta {
        trust: None,
        audience: Some(DeltaAudience::Static(readers(&["a"]))),
    };
    let share = ToolAnnotation {
        parameters: crate::params::test_string_argument_schema("to"),
        ..gated(
            "share_packet",
            Delta::NONE,
            forwards,
            contains(RecipientSpec::Placeholder("to".into())),
        )
    };
    let e = engine(vec![share]);
    let log = vec![opened(&e)];
    assert_eq!(
        check_call(&e, &log, &call("share_packet", json!({ "to": "a" }))),
        CheckOutcome::Allow
    );
    match check_call(&e, &log, &call("share_packet", json!({ "to": "b" }))) {
        CheckOutcome::Block(block) => {
            assert_eq!(
                block.requirement_gaps,
                vec![Gap::Includes {
                    recipients: readers(&["b"])
                }]
            );
            assert_eq!(block.narrowing, None);
        }
        other => panic!("expected the forwarded audience gap, got {other:?}"),
    }
}

#[test]
fn attention_is_always_a_gap() {
    let tool = ToolAnnotation {
        requires: Requires {
            attention: vec![MarkName::new("signoff")],
            ..Requires::default()
        },
        ..plain_tool("wire")
    };
    let e = engine(vec![tool]);
    let log = vec![opened(&e)];
    match check_call(&e, &log, &call("wire", json!({}))) {
        CheckOutcome::Block(b) => {
            assert!(b.requirement_gaps.contains(&Gap::Attention(MarkName::new("signoff"))))
        }
        other => panic!("expected attention gap, got {other:?}"),
    }
}
