use super::*;

#[test]
fn one_call_carries_one_current_approval() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let first = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let one = opened_offers(&first)[0].0;
    let log = [log, first].concat();
    let second = appended_facts(blocked_batch(&e, &log, "b2", crate::value::OfferNonce::new([3u8; 32])));
    let other = opened_offers(&second)[0].0;
    let log = [log, second].concat();
    assert_ne!(one, other, "each block surfaces its own offer for the same call");

    let approved = appended_facts(
        execute_offer(&e, &log, one, OfferOutcome::Approved(Vec::new())).expect("the first offer executes"),
    );
    let log = [log, approved].concat();
    assert_eq!(
        execute_offer(&e, &log, other, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::ApprovalPending)
    );
}

#[test]
fn a_denial_inside_an_offer_execution_is_held_to_that_offer() {
    let e = two_officer_engine();
    let opening = vec![opened(&e)];
    let decision = proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
    let opened = appended_facts(decision);
    let offers = opened_offers(&opened);
    let authority = offers[0].1.required[0].authority.clone();
    let opening = [opening, opened].concat();
    let denial = appended_facts(
        execute_offer(&e, &opening, offers[0].0, OfferOutcome::Denied { authority }).expect("the denial records"),
    );
    assert_eq!(e.validate_replay(&[opening.clone(), denial.clone()].concat()), Ok(()));

    let position = denial
        .iter()
        .position(|fact| matches!(fact, Fact::Denial { .. }))
        .expect("the execution recorded one");
    let mut elsewhere = denial;
    if let Fact::Denial { digest, .. } = &mut elsewhere[position] {
        *digest = call("wire", json!({ "to": "someone" })).digest();
    }
    assert_eq!(
        e.validate_replay(&[opening, elsewhere].concat()),
        Err(TransitionRefusal::UnbackedDenial)
    );
}

#[test]
fn a_release_that_declares_no_advance_cannot_spend_its_approval() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let opening = [log, approved].concat();
    let release =
        appended_facts(proposed(&e, &opening, "b2", nonce(), call("get_ticket", json!({}))).expect("it releases"));

    let undeclared: Vec<Fact> = release
        .into_iter()
        .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
        .collect();
    assert_eq!(
        e.validate_replay(&[opening, undeclared].concat()),
        Err(TransitionRefusal::MisdecidedBatch)
    );
}

#[test]
fn an_output_sanitizer_plan_binds_its_sanitizer_when_the_approval_releases() {
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
    let e = open_engine(RegistryConfig {
        sanitizers: vec![declassify],
        ..test_config(vec![crm_tool()])
    });
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let (offer, plan) = opened_offers(&opened)
        .into_iter()
        .find(|(_, plan)| plan.sanitizer().is_some())
        .expect("a confined result point offers the sanitize settlement");
    let log = [log, opened].concat();

    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    match approved.iter().find(|fact| matches!(fact, Fact::CallApproved { .. })) {
        Some(Fact::CallApproved { output, .. }) => assert_eq!(output, &plan.output_remedy()),
        other => panic!("the approval carries its binding, not {other:?}"),
    }
    let log = [log, approved].concat();

    let release = appended_facts(
        proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases"),
    );
    let bound = release
        .iter()
        .position(|fact| matches!(fact, Fact::OutputSanitizerBound { .. }))
        .expect("the release binds the sanitizer");
    let opening = release
        .iter()
        .position(|fact| matches!(fact, Fact::DispatchOpened { .. }))
        .expect("and then opens");
    assert!(
        bound < opening,
        "the binding lands before the dispatch it withholds for"
    );
    assert_eq!(e.validate_replay(&[log, release].concat()), Ok(()));
}

#[test]
fn a_sibling_offer_is_held_to_the_same_menu_as_the_offer_that_derived_it() {
    let e = two_officer_engine();
    let opening = vec![opened(&e)];
    let batch =
        appended_facts(proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
    assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

    let sibling = batch
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
        .map(|(at, _)| at)
        .nth(1)
        .expect("the block opened two");
    let rewritten = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = batch.clone();
        mutate(&mut facts[sibling]);
        e.validate_replay(&[opening.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::OfferOpened { plan, .. } = fact {
                plan.required.clear();
            }
        }),
        Err(TransitionRefusal::UnbackedOffer)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::OfferOpened { call: rendered, .. } = fact {
                *rendered = call("wire", json!({ "to": "elsewhere" })).digest();
            }
        }),
        Err(TransitionRefusal::UnbackedOffer)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::OfferOpened { basis, .. } = fact {
                basis.family = basis.family.next();
            }
        }),
        Err(TransitionRefusal::ForgedBasis)
    );
}

#[test]
fn a_surfacing_offers_its_whole_menu_exactly_once_under_one_block() {
    let e = two_officer_engine();
    let opening = vec![opened(&e)];
    let batch =
        appended_facts(proposed(&e, &opening, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
    let offers: Vec<usize> = batch
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(offers.len(), 2, "two officers, two plans, two offers");
    assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

    for at in &offers {
        let mut facts = batch.clone();
        facts.remove(*at);
        assert_eq!(
            e.validate_replay(&[opening.clone(), facts].concat()),
            Err(TransitionRefusal::IncompleteMenu)
        );
    }
    let mut doubled = batch.clone();
    let mut again = batch[offers[1]].clone();
    if let Fact::OfferOpened { offer, .. } = &mut again {
        *offer = crate::value::OfferId::of_plan(
            &crate::value::BlockId::of_proposal(
                &nonce(),
                &traj(),
                &crate::transition::ProposalBatchId::new("b1"),
                0,
                &call("wire", json!({})).digest(),
            ),
            7,
            b"again",
        );
    }
    doubled.push(again);
    assert_eq!(
        e.validate_replay(&[opening.clone(), doubled].concat()),
        Err(TransitionRefusal::PlanReoffered)
    );
    let mut split = batch.clone();
    if let Fact::OfferOpened { block, .. } = &mut split[offers[1]] {
        *block = crate::value::BlockId::of_proposal(
            &crate::value::OfferNonce::new([9u8; 32]),
            &traj(),
            &crate::transition::ProposalBatchId::new("b1"),
            0,
            &call("wire", json!({})).digest(),
        );
    }
    assert_eq!(
        e.validate_replay(&[opening.clone(), split].concat()),
        Err(TransitionRefusal::SplitBlock)
    );
    let log = [opening.clone(), batch.clone()].concat();
    let first_block = match &batch[offers[0]] {
        Fact::OfferOpened { block, .. } => *block,
        _ => unreachable!(),
    };
    let mut second = appended_facts(
        proposed(
            &e,
            &log,
            "b2",
            crate::value::OfferNonce::new([5u8; 32]),
            call("wire", json!({ "to": "bob" })),
        )
        .expect("the batch decides"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), second.clone()].concat()), Ok(()));
    for fact in &mut second {
        if let Fact::OfferOpened { block, .. } = fact {
            *block = first_block;
        }
    }
    assert_eq!(
        e.validate_replay(&[log, second].concat()),
        Err(TransitionRefusal::BlockReused)
    );
}

#[test]
fn sibling_blocks_of_one_batch_are_each_held_to_their_own_menu() {
    let e = two_officer_engine();
    let opening = vec![opened(&e)];
    let view = e.view(&traj(), opening.clone(), 1).expect("the log replays");
    let batch = appended_facts(
        e.handle(
            &view,
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                provider_results: Vec::new(),
                proposals: vec![
                    raw(&call("wire", json!({ "to": "a" }))),
                    raw(&call("wire", json!({ "to": "b" }))),
                ],
                spawn: None,
                offer_nonce: nonce(),
                evidence: Vec::new(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the batch decides"),
    );
    let offers: Vec<usize> = batch
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, Fact::OfferOpened { .. }))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(offers.len(), 4, "two blocks of two plans");
    let head = &batch[..offers[0]];
    let [a1, a2, b1, b2] = [
        batch[offers[0]].clone(),
        batch[offers[1]].clone(),
        batch[offers[2]].clone(),
        batch[offers[3]].clone(),
    ];
    let replay = |offers: Vec<Fact>| e.validate_replay(&[opening.clone(), head.to_vec(), offers].concat());
    assert_eq!(replay(vec![a1.clone(), a2.clone(), b1.clone(), b2.clone()]), Ok(()));
    assert_eq!(replay(vec![b1.clone(), b2.clone(), a1.clone(), a2.clone()]), Ok(()));
    assert_eq!(
        replay(vec![a1.clone(), b1.clone(), b2.clone(), a2.clone()]),
        Err(TransitionRefusal::IncompleteMenu)
    );
    let mut a3 = a2.clone();
    if let Fact::OfferOpened { offer, block, .. } = &mut a3 {
        *offer = crate::value::OfferId::of_plan(block, 3, b"third");
    }
    assert_eq!(replay(vec![a1, a2, b1, b2, a3]), Err(TransitionRefusal::BlockReused));
}

#[test]
fn a_fully_denied_block_re_plans_to_an_empty_menu_that_replays() {
    let e = two_officer_engine();
    let mut log = vec![opened(&e)];
    let opened = appended_facts(proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides"));
    log.extend(opened.clone());
    let mut offers = opened_offers(&opened);
    for round in 0..2 {
        let (offer, plan) = offers.remove(0);
        let authority = plan.required[0].authority.clone();
        let done = execute_offer(&e, &log, offer, OfferOutcome::Denied { authority }).expect("the denial is recorded");
        let fresh = match offer_answer(&done) {
            OfferFollowUp::Denied { block } => block.offers.clone(),
            other => panic!("a denial answers with the re-planned block, not {other:?}"),
        };
        let facts = appended_facts(done);
        log.extend(facts.clone());
        assert_eq!(fresh.len(), 1 - round);
        offers = opened_offers(&facts);
    }
    assert!(offers.is_empty(), "nothing left to offer");
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn evidence_gathered_for_one_offer_cannot_approve_another() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let first = appended_facts(
        proposed(&e, &log, "b1", nonce(), call("wire", json!({ "to": "alice" }))).expect("the batch decides"),
    );
    let (one, plan) = opened_offers(&first)[0].clone();
    let log = [log, first].concat();
    let second = appended_facts(
        proposed(
            &e,
            &log,
            "b2",
            crate::value::OfferNonce::new([5u8; 32]),
            call("wire", json!({ "to": "mallory" })),
        )
        .expect("the second batch decides"),
    );
    let (other, other_plan) = opened_offers(&second)[0].clone();
    let log = [log, second].concat();

    assert_eq!(plan.required, other_plan.required);
    let fold = Label::new(SUSPICIOUS, Audience::public());
    assert_eq!(
        evidence_for(one, &plan, "wire", fold.clone())
            .into_iter()
            .map(|given| (given.authority, given.covers, given.reviewed))
            .collect::<Vec<_>>(),
        evidence_for(other, &other_plan, "wire", fold.clone())
            .into_iter()
            .map(|given| (given.authority, given.covers, given.reviewed))
            .collect::<Vec<_>>(),
        "only the offer distinguishes them"
    );

    let gathered_for_one = evidence_for(one, &plan, "wire", fold.clone());
    assert!(
        matches!(
            execute_offer(&e, &log, other, OfferOutcome::Approved(gathered_for_one)),
            Err(TransitionError::Plan(PlanError::EvidenceOfferMismatch))
        ),
        "one offer's approval does not admit another's call"
    );
    let approved = appended_facts(
        execute_offer(
            &e,
            &log,
            other,
            OfferOutcome::Approved(evidence_for(other, &other_plan, "wire", fold)),
        )
        .expect("its own evidence approves it"),
    );
    let position = approved
        .iter()
        .position(|fact| matches!(fact, Fact::CallApproved { .. }))
        .expect("the acceptance prepared one");
    let mut swapped = approved;
    if let Fact::CallApproved { rulings, .. } = &mut swapped[position] {
        rulings[0].offer = one;
    }
    assert_eq!(
        e.validate_replay(&[log, swapped].concat()),
        Err(TransitionRefusal::UnbackedApproval)
    );
}

#[test]
fn a_repeat_returns_the_recorded_outcome_of_a_dispatch_that_already_ran() {
    let e = engine(vec![plain_tool("note")]);
    let log = vec![opened(&e)];
    let note = call("note", json!({}));
    let release = proposed(&e, &log, "b1", nonce(), note.clone()).expect("the note releases");
    let dispatch = match &release.follow_up {
        FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    };
    let log = [log, appended_facts(release)].concat();
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    let closed = e
        .handle(
            &view,
            EngineEvent::Outcome(ToolReport {
                dispatch: dispatch.clone(),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("result")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the outcome closes it");
    let log = [log, appended_facts(closed)].concat();

    let repeat = proposed(&e, &log, "b1", nonce(), note.clone()).expect("the repeat answers");
    assert!(repeat.append.is_none(), "a repeat appends nothing");
    match &repeat.follow_up {
        FollowUp::Proposals { released, settled, .. } => {
            assert!(
                released.is_empty(),
                "a dispatch that already ran is not handed back again"
            );
            assert_eq!(
                settled,
                &vec![Settled {
                    dispatch,
                    call: note,
                    outcome: SettledOutcome::Closed {
                        admitted: Some(ValueBody::new("result")),
                    },
                }]
            );
        }
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    }
}

#[test]
fn a_matching_proposal_consumes_its_approval_and_opens_the_dispatch() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let (offer, plan) = opened_offers(&opened)[0].clone();
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();

    let release = proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases");
    let dispatch = match &release.follow_up {
        FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    };
    let facts = appended_facts(release);
    match &facts[..] {
        [
            Fact::BasisAdvanced { .. },
            Fact::ProposalBatchDecided { released, .. },
            Fact::CallApprovalConsumed {
                offer: spent,
                dispatch: opened_for,
                ..
            },
            Fact::Acceptance {
                plan: accepted_under,
                narrowing,
                ..
            },
            Fact::DispatchOpened { dispatch: opening, .. },
        ] => {
            assert_eq!(released, &vec![dispatch.clone()]);
            assert_eq!((spent, opened_for, opening), (&offer, &dispatch, &dispatch));
            assert_eq!(accepted_under, &plan.id);
            assert_eq!(Some(narrowing), plan.narrowing());
        }
        other => panic!("unexpected release batch {other:?}"),
    }
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn an_approval_releases_once() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();
    let ticket = call("get_ticket", json!({}));
    let first = appended_facts(proposed(&e, &log, "b2", nonce(), ticket.clone()).expect("the approval releases"));
    let log = [log, first].concat();

    let second = proposed(&e, &log, "b3", nonce(), ticket).expect("the second proposal decides");
    match &second.follow_up {
        FollowUp::Proposals { released, blocked, .. } => {
            assert!(released.is_empty(), "a spent approval releases nothing");
            assert_eq!(blocked.len(), 1);
        }
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    }
}

#[test]
fn only_a_moved_label_stales_an_approval() {
    let prepared = |e: &Engine, log: Vec<Fact>| {
        let opened = appended_facts(blocked_batch(e, &log, "b1", nonce()));
        let offer = opened_offers(&opened)[0].0;
        let log = [log, opened].concat();
        let approved = appended_facts(
            execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
        );
        [log, approved].concat()
    };
    let releases = |e: &Engine, log: &[Fact], id: &str| {
        let decision = proposed(e, log, id, nonce(), call("get_ticket", json!({}))).expect("the batch decides");
        match &decision.follow_up {
            FollowUp::Proposals { released, .. } => !released.is_empty(),
            other => panic!("a proposal batch answers with proposals, not {other:?}"),
        }
    };

    let e = engine(vec![crm_tool(), plain_tool("read_note")]);
    let log = prepared(&e, vec![opened(&e)]);
    let neutral = appended_facts(
        proposed(&e, &log, "b2", nonce(), call("read_note", json!({}))).expect("the neutral call releases"),
    );
    assert!(
        releases(&e, &[log, neutral].concat(), "b3"),
        "a neutral release stales nothing"
    );

    let e = engine(vec![crm_tool(), strict_tool("send")]);
    let log = prepared(&e, vec![opened(&e)]);
    let elsewhere = appended_facts(
        proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({ "id": "other" })))
            .expect("the other proposal decides"),
    );
    assert!(releases(&e, &[log, elsewhere].concat(), "b3"), "a block stales nothing");

    let e = engine(vec![crm_tool(), restrictable_tool("note")]);
    let log = prepared(&e, vec![opened(&e)]);
    let restricting =
        appended_facts(proposed(&e, &log, "b2", nonce(), call("note", json!({}))).expect("the note releases"));
    assert!(
        releases(&e, &[log.clone(), restricting].concat(), "b3"),
        "a release stales nothing while its result is unseen"
    );
    let mut log = log;
    reads(&e, &mut log, &traj(), "note");
    assert!(
        !releases(&e, &log, "b4"),
        "a result that narrows the trajectory stales the approval taken before it"
    );
}

/// An Annotator-routed release is held to the same law as a static one: the approval stands
/// until a result moves the label, whoever declared the delta.
#[test]
fn an_annotated_release_leaves_an_approval_standing() {
    let classified = plain_tool("read_note");
    let pin = pinned_for(
        {
            let mut produced = classified.clone();
            produced.delta = Delta {
                trust: None,
                audience: Some(DeltaAudience::Static(DeclaredAudience::literal(Audience::public()))),
            };
            produced
        },
        "classify",
        &call("read_note", json!({})),
    );

    let mut cfg = test_config(vec![crm_tool()]);
    cfg.tools.push(annotated(classified, "classify"));
    cfg.annotators.push(annotator("classify"));
    let e = open_engine(cfg);
    let log = vec![opened(&e)];
    let blocked = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&blocked)[0].0;
    let log = [log, blocked].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();
    let note = proposed(
        &e,
        &log,
        "b2",
        nonce(),
        call("read_note", json!({})).with_annotation(Some(pin)),
    )
    .expect("the classified note decides");
    assert!(
        matches!(&note.follow_up, FollowUp::Proposals { released, .. } if !released.is_empty()),
        "a public answer narrows nothing, so the note releases: {:?}",
        note.follow_up
    );
    let log = [log, appended_facts(note)].concat();

    let decision = proposed(&e, &log, "b3", nonce(), call("get_ticket", json!({}))).expect("the batch decides");
    let released = match &decision.follow_up {
        FollowUp::Proposals { released, .. } => released.clone(),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    };
    assert_eq!(released.len(), 1, "the pinned release moved no label");
}

#[test]
fn a_later_basis_change_does_not_revoke_an_open_dispatch() {
    let e = engine(vec![crm_tool(), plain_tool("note")]);
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();
    let release = proposed(&e, &log, "b2", nonce(), call("get_ticket", json!({}))).expect("the approval releases");
    let dispatch = match &release.follow_up {
        FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    };
    let log = [log, appended_facts(release)].concat();
    let moved = appended_facts(proposed(&e, &log, "b3", nonce(), call("note", json!({}))).expect("the note releases"));
    let log = [log, moved].concat();

    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    let closed = e
        .handle(
            &view,
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Failure,
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the dispatch is still the engine's to close");
    assert_eq!(e.validate_replay(&[log, appended_facts(closed)].concat()), Ok(()));
}

#[test]
fn a_forged_release_cannot_depart_from_the_approval_it_spends() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let opening = [log, approved].concat();
    let release =
        appended_facts(proposed(&e, &opening, "b2", nonce(), call("get_ticket", json!({}))).expect("it releases"));
    assert_eq!(e.validate_replay(&[opening.clone(), release.clone()].concat()), Ok(()));

    let position = release
        .iter()
        .position(|fact| matches!(fact, Fact::Acceptance { .. }))
        .expect("the plan carried a narrowing");
    let mut relabelled = release.clone();
    if let Fact::Acceptance { plan, .. } = &mut relabelled[position] {
        *plan = plan::PlanId::new(plan.value() + 1);
    }
    assert_eq!(
        e.validate_replay(&[opening.clone(), relabelled].concat()),
        Err(TransitionRefusal::UnbackedApproval)
    );
    let mut dropped = release;
    dropped.remove(position);
    assert_eq!(
        e.validate_replay(&[opening, dropped].concat()),
        Err(TransitionRefusal::UnbackedApproval)
    );
}

#[test]
fn an_executed_offer_prepares_its_call_and_releases_nothing() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let decision = blocked_batch(&e, &log, "b1", nonce());
    let opened = appended_facts(decision);
    let (offer, plan) = opened_offers(&opened)[0].clone();
    let log = [log, opened].concat();

    let done = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes");
    let proposal = match offer_answer(&done) {
        OfferFollowUp::Approved { call } => (**call).clone(),
        other => panic!("a full approval prepares a call, not {other:?}"),
    };
    assert_eq!(proposal, call("get_ticket", json!({})));
    let facts = appended_facts(done);
    assert!(
        !facts.iter().any(|fact| matches!(
            fact,
            Fact::DispatchOpened { .. } | Fact::Ruling { .. } | Fact::Acceptance { .. }
        )),
        "an approval reserves nothing and lets no executor run"
    );
    match &facts[..] {
        [
            Fact::BasisAdvanced { .. },
            Fact::OfferAccepted { offer: accepted, .. },
            Fact::CallApproved {
                offer: approved,
                call: approved_call,
                plan: approved_plan,
                acceptance,
                rulings,
                ..
            },
        ] => {
            assert_eq!((accepted, approved), (&offer, &offer));
            assert_eq!(approved_call, &proposal);
            assert_eq!(approved_plan, &plan.id);
            assert_eq!(acceptance.as_ref(), plan.narrowing());
            assert!(rulings.is_empty());
        }
        other => panic!("unexpected approval batch {other:?}"),
    }
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn accepting_one_offer_ends_every_sibling_on_its_candidate() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let wire = call("wire", json!({}));
    let decision = proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the batch decides");
    let opened = appended_facts(decision);
    let offers = opened_offers(&opened);
    assert_eq!(offers.len(), 2, "each officer's grouped assignment is its own offer");
    let (chosen, plan) = offers[0].clone();
    let sibling = offers[1].0;
    let log = [log, opened].concat();

    let evidence = evidence_for(chosen, &plan, "wire", Label::new(SUSPICIOUS, Audience::public()));
    let done = execute_offer(&e, &log, chosen, OfferOutcome::Approved(evidence)).expect("the offer executes");
    let facts = appended_facts(done);
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::OfferInvalidated { offer, .. } if offer == &sibling)),
        "the sibling on the taken candidate ends in the same batch"
    );
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    assert!(matches!(
        execute_offer(&e, &log, sibling, OfferOutcome::Approved(Vec::new())),
        Ok(decision) if offer_answer(&decision) == &OfferFollowUp::Invalidated
    ));
    let repeat = proposed(&e, &log, "b1", nonce(), wire).expect("the repeat answers");
    assert!(offers_of(&repeat).is_empty());
}

#[test]
fn a_repeated_selection_answers_from_the_record() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let decision = blocked_batch(&e, &log, "b1", nonce());
    let opened = appended_facts(decision);
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let first = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes");
    let prepared = offer_answer(&first).clone();
    let log = [log, appended_facts(first)].concat();

    let repeat = execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the repeat answers");
    assert!(repeat.append.is_none(), "a repeat appends nothing");
    assert_eq!(offer_answer(&repeat), &prepared);
    assert_eq!(
        execute_offer(
            &e,
            &log,
            offer,
            OfferOutcome::Denied {
                authority: AuthorityName::new("officer-a"),
            },
        ),
        Err(TransitionError::TerminalOffer)
    );
}

#[test]
fn an_offer_whose_basis_moved_is_refused() {
    let e = engine(vec![crm_tool(), restrictable_tool("note")]);
    let log = vec![opened(&e)];
    let decision = blocked_batch(&e, &log, "b1", nonce());
    let opened = appended_facts(decision);
    let offer = opened_offers(&opened)[0].0;
    let mut log = [log, opened].concat();

    let release = proposed(&e, &log, "b2", nonce(), call("note", json!({}))).expect("the note releases");
    assert!(
        execute_offer(
            &e,
            &[log.clone(), appended_facts(release)].concat(),
            offer,
            OfferOutcome::Approved(Vec::new())
        )
        .is_ok(),
        "a release moves no label, so the offer stands beside it"
    );
    reads(&e, &mut log, &traj(), "note");
    assert_eq!(
        execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::StaleOffer)
    );
}

#[test]
fn a_denial_ends_the_plans_that_named_the_authority_and_re_offers_the_rest() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let wire = call("wire", json!({}));
    let decision = proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the batch decides");
    let opened = appended_facts(decision);
    let offers = opened_offers(&opened);
    let denied_authority = offers[0].1.required[0].authority.clone();
    let (target, survivor) = (offers[0].0, offers[1].0);
    let log = [log, opened].concat();

    let done = execute_offer(
        &e,
        &log,
        target,
        OfferOutcome::Denied {
            authority: denied_authority.clone(),
        },
    )
    .expect("the denial is recorded");
    let fresh = match offer_answer(&done) {
        OfferFollowUp::Denied { block } => block.offers.clone(),
        other => panic!("a denial answers with the re-planned block, not {other:?}"),
    };
    let facts = appended_facts(done);
    assert!(facts.iter().any(
        |fact| matches!(fact, Fact::Denial { digest, authority, .. } if digest == &wire.digest() && authority == &denied_authority)
    ));
    let ended: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::OfferDenied { offer, .. } => Some(*offer),
            _ => None,
        })
        .collect();
    assert_eq!(ended, vec![target], "only the plans naming the denying authority end");

    assert_eq!(fresh.len(), 1);
    assert_ne!(fresh[0].0, survivor);
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        execute_offer(&e, &log, survivor, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::StaleOffer),
        "the pre-denial offer cannot revive"
    );
    let (fresh_offer, fresh_plan) = opened_offers(&log)
        .into_iter()
        .find(|(offer, _)| offer == &fresh[0].0)
        .expect("the re-plan opened it");
    let evidence = evidence_for(
        fresh_offer,
        &fresh_plan,
        "wire",
        Label::new(SUSPICIOUS, Audience::public()),
    );
    let executed =
        execute_offer(&e, &log, fresh_offer, OfferOutcome::Approved(evidence)).expect("the fresh offer executes");
    assert!(matches!(offer_answer(&executed), OfferFollowUp::Approved { .. }));
}

#[test]
fn a_denial_by_an_unassigned_authority_is_refused() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let decision = proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
    let opened = appended_facts(decision);
    let offers = opened_offers(&opened);
    let other = offers[1].1.required[0].authority.clone();
    let log = [log, opened].concat();
    assert_eq!(
        execute_offer(&e, &log, offers[0].0, OfferOutcome::Denied { authority: other }),
        Err(TransitionError::UnassignedAuthority)
    );
}

#[test]
fn an_incomplete_or_misreviewed_evidence_set_is_refused() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let decision = proposed(&e, &log, "b1", nonce(), call("wire", json!({}))).expect("the batch decides");
    let opened = appended_facts(decision);
    let (offer, plan) = opened_offers(&opened)[0].clone();
    let log = [log, opened].concat();
    let complete = evidence_for(offer, &plan, "wire", Label::new(SUSPICIOUS, Audience::public()));

    assert!(matches!(
        execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::Plan(PlanError::RulingAssignmentMismatch))
    ));
    let mut rerouted = complete.clone();
    rerouted[0].authority = AuthorityName::new("officer-b");
    assert!(
        matches!(
            execute_offer(&e, &log, offer, OfferOutcome::Approved(rerouted)),
            Err(TransitionError::Plan(PlanError::RulingAssignmentMismatch))
        ),
        "an overlapping mandate cannot reroute the grouping the agent was shown"
    );
    let mut moved_fold = complete;
    moved_fold[0].reviewed.trajectory_label = Label::new(TRUSTED, Audience::public());
    assert!(matches!(
        execute_offer(&e, &log, offer, OfferOutcome::Approved(moved_fold)),
        Err(TransitionError::Plan(PlanError::ReviewMismatch))
    ));
}

#[test]
fn an_unknown_or_foreign_offer_is_refused() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let decision = blocked_batch(&e, &log, "b1", nonce());
    let opened = appended_facts(decision);
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();

    let unknown = crate::value::OfferId::of_plan(
        &crate::value::BlockId::of_proposal(
            &nonce(),
            &traj(),
            &crate::transition::ProposalBatchId::new("nowhere"),
            0,
            &call("get_ticket", json!({})).digest(),
        ),
        0,
        b"{}",
    );
    assert_eq!(
        execute_offer(&e, &log, unknown, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::UnknownOffer)
    );
    let elsewhere = TrajectoryId::new("elsewhere");
    let log = [log.clone(), forked_child(&e, &log, &elsewhere)].concat();
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    assert_eq!(
        e.handle(
            &view,
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: elsewhere,
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: None,
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        ),
        Err(TransitionError::OfferElsewhere)
    );
}

#[test]
fn a_forged_approval_record_is_refused() {
    let e = engine(vec![crm_tool()]);
    let opening = vec![opened(&e)];
    let decision = blocked_batch(&e, &opening, "b1", nonce());
    let opened = appended_facts(decision);
    let offer = opened_offers(&opened)[0].0;
    let opening = [opening, opened].concat();
    let approval = appended_facts(
        execute_offer(&e, &opening, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
    );
    assert_eq!(e.validate_replay(&[opening.clone(), approval.clone()].concat()), Ok(()));

    let position = approval
        .iter()
        .position(|fact| matches!(fact, Fact::CallApproved { .. }))
        .expect("the acceptance prepared one");
    let rewritten = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = approval.clone();
        mutate(&mut facts[position]);
        e.validate_replay(&[opening.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::CallApproved { call: approved, .. } = fact {
                *approved = call("get_ticket", json!({ "id": "other" }));
            }
        }),
        Err(TransitionRefusal::UnbackedApproval)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::CallApproved { acceptance, .. } = fact {
                *acceptance = None;
            }
        }),
        Err(TransitionRefusal::UnbackedApproval)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::CallApproved { basis, .. } = fact {
                basis.subject = basis.subject.next();
            }
        }),
        Err(TransitionRefusal::ForgedBasis)
    );
    let orphan: Vec<Fact> = approval
        .iter()
        .filter(|fact| !matches!(fact, Fact::OfferAccepted { .. }))
        .cloned()
        .collect();
    assert_eq!(
        e.validate_replay(&[opening, orphan].concat()),
        Err(TransitionRefusal::OfferEnded)
    );
}

#[test]
fn an_acceptance_whose_batch_prepares_no_approval_is_refused() {
    let e = two_officer_engine();
    let opening = vec![opened(&e)];
    let wire = call("wire", json!({}));
    let opened = appended_facts(proposed(&e, &opening, "b1", nonce(), wire.clone()).expect("the batch decides"));
    let offers = opened_offers(&opened);
    let (chosen, plan) = offers[0].clone();
    let opening = [opening, opened].concat();
    let elsewhere = appended_facts(proposed(&e, &opening, "b2", nonce(), wire).expect("the batch decides"));
    let unrelated = opened_offers(&elsewhere)[0].0;
    let opening = [opening, elsewhere].concat();

    let evidence = evidence_for(chosen, &plan, "wire", Label::new(SUSPICIOUS, Audience::public()));
    let approval = appended_facts(
        execute_offer(&e, &opening, chosen, OfferOutcome::Approved(evidence)).expect("the offer executes"),
    );
    assert_eq!(e.validate_replay(&[opening.clone(), approval.clone()].concat()), Ok(()));

    let position = approval
        .iter()
        .position(|fact| matches!(fact, Fact::CallApproved { .. }))
        .expect("the acceptance prepared one");
    let mut truncated = approval.clone();
    truncated.remove(position);
    assert!(
        truncated
            .iter()
            .any(|fact| matches!(fact, Fact::OfferInvalidated { .. }))
    );
    let stopped = [opening.clone(), truncated].concat();
    assert_eq!(
        e.validate_replay(&stopped),
        Err(TransitionRefusal::UndischargedAcceptance)
    );
    assert_eq!(
        e.view(&traj(), stopped.clone(), stopped.len() as u64).err(),
        Some(TransitionRefusal::UndischargedAcceptance)
    );

    let mut deferred = approval.clone();
    deferred.insert(
        position,
        stray_admission(&traj(), Label::new(TRUSTED, Audience::public())),
    );
    assert_eq!(
        e.validate_replay(&[opening.clone(), deferred].concat()),
        Err(TransitionRefusal::UndischargedAcceptance)
    );

    let mut foreign = approval;
    foreign.insert(
        position,
        Fact::OfferInvalidated {
            trajectory: traj(),
            offer: unrelated,
        },
    );
    assert_eq!(
        e.validate_replay(&[opening, foreign].concat()),
        Err(TransitionRefusal::UndischargedAcceptance)
    );
}

#[test]
fn the_approval_window_admits_no_ending_from_before_the_offer_it_approves() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let wire = call("wire", json!({}));
    let opened = appended_facts(proposed(&e, &log, "b1", nonce(), wire).expect("the batch decides"));
    let offers = opened_offers(&opened);
    let authority = offers[0].1.required[0].authority.clone();
    let (target, survivor) = (offers[0].0, offers[1].0);
    let log = [log, opened].concat();

    let denial = appended_facts(
        execute_offer(&e, &log, target, OfferOutcome::Denied { authority }).expect("the denial is recorded"),
    );
    let log = [log, denial].concat();
    let (fresh, fresh_plan) = opened_offers(&log)
        .into_iter()
        .find(|(offer, _)| offer != &target && offer != &survivor)
        .expect("the denial re-offered the surviving plan");
    assert_eq!(
        execute_offer(&e, &log, survivor, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::StaleOffer),
        "the predecessor is stale, and no record ended it"
    );

    let evidence = evidence_for(fresh, &fresh_plan, "wire", Label::new(SUSPICIOUS, Audience::public()));
    let approval = appended_facts(
        execute_offer(&e, &log, fresh, OfferOutcome::Approved(evidence)).expect("the fresh offer executes"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), approval.clone()].concat()), Ok(()));

    let position = approval
        .iter()
        .position(|fact| matches!(fact, Fact::CallApproved { .. }))
        .expect("the acceptance prepared one");
    let mut forged = approval;
    forged.insert(
        position,
        Fact::OfferInvalidated {
            trajectory: traj(),
            offer: survivor,
        },
    );
    assert_eq!(
        e.validate_replay(&[log, forged].concat()),
        Err(TransitionRefusal::UndischargedAcceptance)
    );
}

#[test]
fn a_repeat_of_an_unapproved_acceptance_answers_a_refusal() {
    let e = engine(vec![crm_tool()]);
    let opening = vec![opened(&e)];
    let opened = appended_facts(blocked_batch(&e, &opening, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let approval = appended_facts(
        execute_offer(
            &e,
            &[opening.clone(), opened.clone()].concat(),
            offer,
            OfferOutcome::Approved(Vec::new()),
        )
        .expect("the offer executes"),
    );
    let unpaired: Vec<Fact> = [opening, opened, approval]
        .concat()
        .into_iter()
        .filter(|fact| !matches!(fact, Fact::CallApproved { .. }))
        .collect();

    let mut projection = Projection::empty(unpaired.len() as u64);
    for fact in &unpaired {
        projection.fold(fact);
    }
    let view = crate::transition::EngineView::validated(projection, e.identity, traj());
    assert_eq!(
        e.handle(
            &view,
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: None,
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        ),
        Err(TransitionError::Invalid(TransitionRefusal::UndischargedAcceptance))
    );
}

fn offers_of(decision: &EngineDecision) -> Vec<(crate::value::OfferId, plan::PlanId)> {
    match &decision.follow_up {
        FollowUp::Proposals { blocked, .. } => blocked[0].offers.clone(),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    }
}

#[test]
fn a_block_binds_one_durable_offer_to_each_of_its_plans() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let decision = blocked_batch(&e, &log, "b1", nonce());
    let offers = offers_of(&decision);
    assert!(!offers.is_empty());

    let facts = appended_facts(decision);
    let opened: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::OfferOpened { offer, basis, plan, .. } => Some((*offer, *basis, plan.id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened.iter().map(|(id, _, plan)| (*id, *plan)).collect::<Vec<_>>(),
        offers,
        "the follow-up names exactly the offers the batch opened"
    );
    assert_eq!(
        opened.iter().filter(|(_, basis, _)| basis != &opened[0].1).count(),
        0,
        "siblings opened together share one basis"
    );
    assert_eq!(
        opened.iter().map(|(id, ..)| *id).collect::<BTreeSet<_>>().len(),
        opened.len()
    );
    let elsewhere = offers_of(&blocked_batch(&e, &log, "b1", crate::value::OfferNonce::new([9u8; 32])));
    assert!(
        elsewhere
            .iter()
            .all(|(id, _)| !offers.iter().any(|(mine, _)| mine == id))
    );
}

#[test]
fn a_repeated_block_answers_with_the_offers_it_already_opened() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let first = blocked_batch(&e, &log, "b1", nonce());
    let opened = offers_of(&first);
    let log = [log, appended_facts(first)].concat();

    let repeat = blocked_batch(&e, &log, "b1", nonce());
    assert!(repeat.append.is_none(), "a repeat appends nothing");
    assert_eq!(offers_of(&repeat), opened);
}

#[test]
fn a_forged_offer_record_is_refused() {
    let e = engine(vec![crm_tool()]);
    let opening = vec![opened(&e)];
    let batch = appended_facts(blocked_batch(&e, &opening, "b1", nonce()));
    assert_eq!(e.validate_replay(&[opening.clone(), batch.clone()].concat()), Ok(()));

    let position = batch
        .iter()
        .position(|fact| matches!(fact, Fact::OfferOpened { .. }))
        .expect("the block opened an offer");
    let rewritten = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = batch.clone();
        mutate(&mut facts[position]);
        e.validate_replay(&[opening.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::OfferOpened { plan, .. } = fact {
                plan.steps.clear();
            }
        }),
        Err(TransitionRefusal::UnbackedOffer)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::OfferOpened { basis, .. } = fact {
                basis.flow = basis.flow.next();
            }
        }),
        Err(TransitionRefusal::ForgedBasis)
    );
    let doubled = [opening.clone(), batch.clone(), vec![batch[position].clone()]].concat();
    assert_eq!(e.validate_replay(&doubled), Err(TransitionRefusal::OfferReopened));
}
