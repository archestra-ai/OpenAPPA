use super::*;

fn emitting_tool() -> ToolAnnotation {
    ToolAnnotation {
        emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
        ..plain_tool("send_note")
    }
}

/// An engine deciding at a revision declares the same neutral release, moving nothing, so
/// the batch carries the revision that decided it.
#[test]
fn a_neutral_release_at_a_revision_declares_its_stamp() {
    let e = engine(vec![plain_tool("read_note")]).at(DeploymentName::new("default"), PolicyRevision::new(3));
    let log = vec![opened(&e)];
    let facts = appended_facts(decide(&e, &log, "b1", &call("read_note", json!({}))));
    assert!(matches!(
        facts.first(),
        Some(Fact::BasisAdvanced { advance, revision, .. })
            if advance.is_empty() && *revision == Some(PolicyRevision::new(3))
    ));
    assert!(
        e.view(&traj(), [log, facts].concat(), 2).is_ok(),
        "a declaration moving nothing replays"
    );
}

/// An offer and an approval a revision recorded are not current under a later one: executing
/// the offer consults nothing and is refused as stale, the approval releases nothing, and the
/// call is decided afresh at the later revision. Under the revision that recorded them both
/// still stand.
#[test]
fn a_later_revision_retires_every_offer_and_approval_an_earlier_one_recorded() {
    let line = DeploymentName::new("default");
    let first = engine(vec![suspicious_internal_read()]).at(line.clone(), PolicyRevision::FIRST);
    let second = engine(vec![suspicious_internal_read()]).at(line, PolicyRevision::new(2));
    let read = call("read_suspicious_internal", json!({ "who": "someone" }));
    let mut log: Vec<(u64, Vec<Fact>)> = vec![(1, vec![opened(&first)])];
    let under_first = |log: &[(u64, Vec<Fact>)]| {
        let facts: Vec<Fact> = log.iter().flat_map(|(_, facts)| facts.clone()).collect();
        viewing(&first, &facts)
    };
    let under_second = |log: &[(u64, Vec<Fact>)]| {
        let batches: Vec<StampedBatch<'_>> = log
            .iter()
            .map(|(revision, facts)| StampedBatch {
                revision: Some(PolicyRevision::new(*revision)),
                facts,
            })
            .collect();
        second.replay(&traj(), &batches, &[&first]).expect("the log replays")
    };
    let execute = |offer| {
        EngineEvent::ExecuteOffer(OfferExecution {
            trajectory: traj(),
            offer,
            outcome: OfferOutcome::Approved(Vec::new()),
            return_policy: None,
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };
    let propose = |id: &str| batch_on(&traj(), id, Vec::new(), vec![raw(&read)], None);
    let acceptance = |decision: &EngineDecision| match answered(decision) {
        ([], [block]) => {
            let accepting = block
                .block
                .plans
                .iter()
                .filter_map(crate::plan::RemedyPlan::executable)
                .find(|plan| plan.narrowing().is_some() && plan.required.is_empty())
                .expect("a narrowing read offers its acceptance")
                .id;
            block
                .offers
                .iter()
                .find_map(|(offer, plan)| (*plan == accepting).then_some(*offer))
                .expect("the acceptance plan is offered")
        }
        other => panic!("the narrowing read blocks, got {other:?}"),
    };

    let blocked = first
        .handle(&under_first(&log), propose("b1"))
        .expect("the read decides");
    let offer = acceptance(&blocked);
    log.push((1, appended_facts(blocked)));
    assert_eq!(
        second.offer_consults(&under_second(&log), &traj(), &offer),
        Ok(crate::transition::OfferConsult::Stale),
        "no authority is consulted for an offer an earlier revision opened"
    );
    assert!(matches!(
        second.handle(&under_second(&log), execute(offer)),
        Err(TransitionError::StaleOffer)
    ));

    let approved = first
        .handle(&under_first(&log), execute(offer))
        .expect("the offer executes under the revision that opened it");
    log.push((1, appended_facts(approved)));
    assert!(
        matches!(
            answered(&first.handle(&under_first(&log), propose("b2")).unwrap()),
            ([_], [])
        ),
        "the approval releases the call under its own revision"
    );
    let afresh = second
        .handle(&under_second(&log), propose("b2"))
        .expect("the read decides at the later revision");
    let reoffered = acceptance(&afresh);
    log.push((2, appended_facts(afresh)));
    let approved = second
        .handle(&under_second(&log), execute(reoffered))
        .expect("the later revision's own offer executes");
    log.push((2, appended_facts(approved)));
    let released = second
        .handle(&under_second(&log), propose("b3"))
        .expect("the read decides");
    assert!(matches!(answered(&released), ([_], [])));
    log.push((2, appended_facts(released)));
    under_second(&log);
}

#[test]
fn a_neutral_release_advances_no_basis_component() {
    let e = engine(vec![plain_tool("read_note")]);
    let log = vec![opened(&e)];
    let before = basis_of(&e, &log);
    let facts = appended_facts(decide(&e, &log, "b1", &call("read_note", json!({}))));
    assert!(!facts.iter().any(|fact| matches!(fact, Fact::BasisAdvanced { .. })));
    assert_eq!(basis_of(&e, &[log, facts].concat()), before);
}

#[test]
fn a_release_advances_the_family_and_leaves_the_flow_to_its_admission() {
    let effects = engine(vec![emitting_tool()]);
    let log = vec![opened(&effects)];
    let before = basis_of(&effects, &log);
    let facts = appended_facts(decide(&effects, &log, "b1", &call("send_note", json!({}))));
    let after = basis_of(&effects, &[log.clone(), facts].concat());
    assert_eq!(after.family, before.family.next());
    assert_eq!(after.flow, before.flow, "a `delta = {{}}` result restricts nothing");

    let restricting = engine_at(
        vec![crm_tool()],
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let internal = vec![opened(&restricting)];
    let before = basis_of(&restricting, &internal);
    let facts = appended_facts(decide(&restricting, &internal, "b1", &call("get_ticket", json!({}))));
    assert_eq!(
        basis_of(&restricting, &[internal, facts].concat()),
        before,
        "a result is unseen until it is admitted, and it reserves no effect"
    );
}

#[test]
fn a_blocked_proposal_leaves_every_basis_component_where_it_was() {
    let e = engine(vec![crm_tool()]);
    let log = vec![opened(&e)];
    let before = basis_of(&e, &log);
    let facts = appended_facts(decide(&e, &log, "b1", &call("get_ticket", json!({}))));
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::BasisAdvanced { advance, .. } if advance.is_empty()))
    );
    assert_eq!(basis_of(&e, &[log, facts].concat()), before);
}

#[test]
fn a_declaration_that_disagrees_with_its_records_is_refused() {
    let e = engine(vec![emitting_tool()]);
    let opening = vec![opened(&e)];
    let batch = appended_facts(decide(&e, &opening, "b1", &call("send_note", json!({}))));
    let log = [opening.clone(), batch.clone()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let rewritten = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = batch.clone();
        mutate(&mut facts[0]);
        e.validate_replay(&[opening.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::BasisAdvanced { advance, .. } = fact {
                advance.flows.insert(traj());
            }
        }),
        Err(TransitionRefusal::UnbackedAdvance)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::BasisAdvanced { advance, .. } = fact {
                advance.family = false;
            }
        }),
        Err(TransitionRefusal::UndeclaredAdvance)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::BasisAdvanced { act, .. } = fact {
                *act = crate::basis::DecidedAct::Proposals(crate::transition::ProposalBatchId::new("other"));
            }
        }),
        Err(TransitionRefusal::UnbackedAdvance)
    );
}
