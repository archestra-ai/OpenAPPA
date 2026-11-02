use super::*;

/// The complete annotation `acl` produces for a `post_dyn` call: the declaration's
/// operational metadata with the recipients the answer requires.
fn post_dyn_annotation(readers: &[&str]) -> ToolAnnotation {
    let mut produced = post_tool("post_dyn", vec![crate::names::TagName::new("outbound")]);
    produced.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
        DeclaredAudience::literal(Audience::restricted(
            readers.iter().map(|reader| ReaderId::new(*reader)),
        )),
    ))];
    produced
}

#[test]
fn an_input_hop_is_offered_before_the_ruling_that_covers_the_same_gap() {
    let e = substituting_engine(TRUSTED);
    let log = internal_log(&e);
    let plans = |tool: &str| {
        opened_offers(&appended_facts(
            proposed(&e, &log, "b1", nonce(), call(tool, json!({ "body": "ssn 123" }))).expect("the batch decides"),
        ))
        .into_iter()
        .map(|(_, plan)| plan)
        .collect::<Vec<_>>()
    };
    let tagged = plans("post");
    assert_eq!(
        tagged.iter().map(plan::ExecutableRemedyPlan::hop).collect::<Vec<_>>(),
        vec![Some(&crate::names::SanitizerName::new("redact")), None]
    );
    assert_eq!(
        tagged[1]
            .required
            .iter()
            .map(|required| required.authority.clone())
            .collect::<Vec<_>>(),
        vec![AuthorityName::new("officer")]
    );
    assert!(
        plans("post_untagged").iter().all(|plan| plan.hop().is_none()),
        "a sanitizer scoped to a tag the callee does not carry has no jurisdiction over it"
    );
}

#[test]
fn a_marked_spawn_is_offered_no_input_hop_while_its_identical_sibling_is() {
    let e = substituting_engine(TRUSTED);
    let log = internal_log(&e);
    let post = call("post", json!({ "body": "ssn 123" }));
    let batch = || {
        batch_on(
            &traj(),
            "b1",
            Vec::new(),
            vec![raw(&post), raw(&post)],
            Some(SpawnMark::at(1)),
        )
    };
    let decision = e.handle(&viewing(&e, &log), batch()).expect("the batch decides");
    let hops = |blocked: &[Blocked]| -> Vec<Vec<bool>> {
        blocked
            .iter()
            .map(|block| {
                block
                    .block
                    .plans
                    .iter()
                    .filter_map(plan::RemedyPlan::executable)
                    .map(|plan| plan.hop().is_some())
                    .collect()
            })
            .collect()
    };
    let (_, blocked) = answered(&decision);
    assert_eq!(hops(blocked), vec![vec![true, false], vec![false, false]]);
    assert_eq!(blocked[0].offers.len(), 2);
    assert_eq!(blocked[1].offers.len(), 2);
    let facts = appended_facts(decision);
    let log = [log, facts.clone()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let repeat = e.handle(&viewing(&e, &log), batch()).expect("the repeat answers");
    assert!(repeat.append.is_none());
    let (_, blocked) = answered(&repeat);
    assert_eq!(hops(blocked), vec![vec![true, false], vec![false, false]]);

    let (unmarked_hop, marked_offer) = {
        let mut opened = facts.iter().filter(|fact| matches!(fact, Fact::OfferOpened { .. }));
        (
            opened.next().expect("the sibling's hop").clone(),
            opened.nth(1).expect("the marked plan").clone(),
        )
    };
    let mut forged = marked_offer;
    if let (Fact::OfferOpened { plan, offer, block, .. }, Fact::OfferOpened { plan: hop, .. }) =
        (&mut forged, &unmarked_hop)
    {
        *plan = hop.clone();
        *offer = crate::value::OfferId::of_plan(block, 9, b"forged");
    }
    let mut with_hop = log.clone();
    with_hop.push(forged);
    assert_eq!(e.validate_replay(&with_hop), Err(TransitionRefusal::UnbackedOffer));
}

#[test]
fn a_substitution_that_clears_the_last_gap_approves_the_call_the_model_then_proposes() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let offers = opened_offers(&facts);
    let log = [log, facts].concat();

    let hopped = execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs");
    let approved = approved_by(&hopped);
    assert_eq!(approved.canonical_arguments().canonical_text(), REDACTED);
    let facts = appended_facts(hopped);
    assert!(
        matches!(
            facts.as_slice(),
            [
                Fact::BasisAdvanced { .. },
                Fact::OfferAccepted { .. },
                Fact::OfferInvalidated { offer, .. },
                Fact::CandidateDerived {
                    derived: DerivedCandidate::Call { call, .. },
                    ..
                },
            ] if offer == &offers[1].0 && call == &approved
        ),
        "the hop commits its candidate and ends the sibling standing on the predecessor it \
         replaced, and opens no dispatch: {facts:?}"
    );
    let before = basis_of(&e, &log);
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        basis_of(&e, &log).family,
        before.family,
        "nothing ran, so the hop reserves no effect"
    );

    // The derivation releases the bytes it staged and nothing else: the same block's
    // original arguments are still blocked.
    let plain = proposed(&e, &log, "b2", nonce(), proposal.clone()).expect("the batch decides");
    match &plain.follow_up {
        FollowUp::Proposals { released, blocked, .. } => {
            assert!(released.is_empty());
            assert_eq!(
                blocked[0].block.raw.requirement_gaps,
                vec![Gap::Includes {
                    recipients: DeclaredAudience::restricted([ReaderId::new("partner")])
                }]
            );
        }
        other => panic!("a fresh proposal decides as proposals, got {other:?}"),
    }

    let facts = taken(&e, &log, "b3", &approved);
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn a_forged_answer_neither_persists_on_a_candidate_nor_releases_one() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let offers = opened_offers(&facts);
    let log = [log, facts].concat();
    let facts =
        appended_facts(execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs"));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let answer = || pinned_for(plain_tool("post"), "ghost", &proposal);

    let mut forged = log.clone();
    let Some(Fact::CandidateDerived {
        derived: DerivedCandidate::Call { call, .. },
        ..
    }) = forged.last_mut()
    else {
        panic!("the hop's batch ends with its candidate")
    };
    let substituted = call.clone();
    *call = call.clone().with_annotation(Some(answer()));
    assert_eq!(
        e.validate_replay(&forged),
        Err(crate::transition::TransitionRefusal::ForgedEvidence),
        "a static declaration is its own annotation: a pinned candidate under it is forged"
    );

    // The opening the taking proposal earns is held to the same rule.
    let log = [log.clone(), taken(&e, &log, "b2", &substituted)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    let mut forged = log.clone();
    let Some(Fact::DispatchOpened { annotation, .. }) = forged
        .iter_mut()
        .find(|fact| matches!(fact, Fact::DispatchOpened { .. }))
    else {
        panic!("the taking batch opens the dispatch")
    };
    *annotation = Some(answer());
    assert_eq!(
        e.validate_replay(&forged),
        Err(crate::transition::TransitionRefusal::UnbackedDecision),
        "a static dispatch record carrying a pin diverges from the decision that released it"
    );
}

#[test]
fn a_repeat_of_a_hop_answers_with_the_same_call_until_a_proposal_takes_it() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);

    let hop_of = |log: &Vec<Fact>, batch: &str| {
        let facts = appended_facts(proposed(&e, log, batch, nonce(), proposal.clone()).expect("the batch decides"));
        let offer = opened_offers(&facts)[0].0;
        ([log.clone(), facts].concat(), offer)
    };
    let run = |log: &Vec<Fact>, offer| {
        execute_offer(&e, log, offer, substitution(&proposal, REDACTED)).expect("the hop runs")
    };

    let (log, first) = hop_of(&log, "b1");
    let approved = approved_by(&run(&log, first));
    let log = [log.clone(), appended_facts(run(&log, first))].concat();

    // A second block over the same bytes derives its own candidate on its own subject.
    let (log, second) = hop_of(&log, "b2");
    assert_eq!(approved_by(&run(&log, second)), approved);
    let log = [log.clone(), appended_facts(run(&log, second))].concat();

    let repeat = |log: &Vec<Fact>, offer| {
        let decision = execute_offer(&e, log, offer, substitution(&proposal, REDACTED)).expect("the repeat answers");
        assert_eq!(decision.append, None, "a repeat writes nothing");
        offer_answer(&decision).clone()
    };
    let same = OfferFollowUp::Approved {
        call: Box::new(approved.clone()),
    };
    assert_eq!(repeat(&log, first), same);
    assert_eq!(
        repeat(&log, second),
        same,
        "an unspent derivation answers its repeat with the same call"
    );

    // One proposal takes one derivation. The hop whose derivation it spent no longer stands;
    // the other still answers, so one rewrite never releases the call twice.
    let facts = taken(&e, &log, "b3", &approved);
    let spent = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::CandidateConsumed { subject, dispatch, .. } => Some((subject.clone(), dispatch.clone())),
            _ => None,
        })
        .expect("the batch spends a derivation");
    let opened = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
            _ => None,
        })
        .expect("the batch opens its dispatch");
    assert_eq!(
        spent.1, opened,
        "the record ties the emission to the derivation it stood on"
    );
    assert!(
        matches!(
            &spent.0,
            crate::basis::SubjectKey::Call { batch, .. }
                if batch == &crate::transition::ProposalBatchId::new("b1")
                    || batch == &crate::transition::ProposalBatchId::new("b2")
        ),
        "the record names the original call the derivation came from: {:?}",
        spent.0
    );

    let log = [log, facts].concat();
    let answers = [repeat(&log, first), repeat(&log, second)];
    assert_eq!(
        answers
            .iter()
            .filter(|answer| **answer == OfferFollowUp::Invalidated)
            .count(),
        1,
        "exactly one hop's derivation was spent: {answers:?}"
    );
}

/// Two derivations can stand for one proposal: the same bytes under sanitizers that widen to
/// different audiences. Only `Includes` reads a derivation's frozen label, and a group named
/// there resolves through the membership answered for the act that asks — so a group that has
/// since lost the recipient leaves one derivation covering the requirement and the other not.
/// The proposal is judged under each in turn: the one that no longer clears must not starve
/// the one that does, and the record must name the derivation that actually permitted the
/// emission.
#[test]
fn a_derivation_that_no_longer_covers_does_not_starve_the_one_that_does() {
    let e = open_engine_at(
        RegistryConfig {
            sanitizers: vec![
                // Sorted so the group-labelled hop is the earlier subject's: `by_group` runs
                // on b1, `by_name` on b2, and subject order puts b1 first.
                input_sanitizer_to("by_group", &["insider"], group_audience("team")),
                input_sanitizer_to(
                    "by_name",
                    &["insider"],
                    DeclaredAudience::restricted([corp_reader("partner")]),
                ),
            ],
            audience: slack_groups(&["team"]),
            ..test_config(vec![ToolAnnotation {
                requires: Requires {
                    label: LabelRequirements {
                        trust_floor: Some(TRUSTED),
                        audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                            DeclaredAudience::restricted([corp_reader("partner")]),
                        ))],
                    },
                    ..Requires::default()
                },
                ..post_tool("post", vec![crate::names::TagName::new("outbound")])
            }])
        },
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);

    // While the group holds the partner desk, the group-labelled rewrite clears the block.
    let holding = source_evidence(vec![user_group(
        "team",
        vec![slack_member("slack:UP", Some("partner@corp.com"))],
    )]);
    let hop_of = |log: &Vec<Fact>, batch: &str, sanitizer: &str| {
        let facts = appended_facts(
            proposed_with(&e, log, batch, nonce(), proposal.clone(), holding.clone()).expect("the batch decides"),
        );
        let offer = hop_named(&facts, sanitizer);
        ([log.clone(), facts].concat(), offer)
    };
    let rewrite_by = |sanitizer: &str| {
        OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
            sanitizer: crate::names::SanitizerName::new(sanitizer),
            source: crate::value::RawResultDigest::of(proposal.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(REDACTED),
            annotation: None,
        })
    };
    let (log, by_group) = hop_of(&log, "b1", "by_group");
    let hopped = execute_offer_with(&e, &log, by_group, rewrite_by("by_group"), holding.clone())
        .expect("the group-labelled hop runs");
    let approved = approved_by(&hopped);
    let log = [log, appended_facts(hopped)].concat();

    let (log, by_name) = hop_of(&log, "b2", "by_name");
    let hopped = execute_offer(&e, &log, by_name, rewrite_by("by_name")).expect("the named hop runs");
    assert_eq!(approved_by(&hopped), approved, "both derivations stage the same bytes");
    let log = [log, appended_facts(hopped)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    // The group no longer holds the partner desk. The b1 derivation's label covers nobody the
    // contract asks for; the b2 derivation names the desk outright.
    let emptied = source_evidence(vec![user_group(
        "team",
        vec![slack_member("slack:UQ", Some("someone@corp.com"))],
    )]);
    let decided = proposed_with(&e, &log, "b3", nonce(), approved.clone(), emptied).expect("the batch decides");
    let (released, blocked) = answered(&decided);
    assert!(blocked.is_empty(), "the derivation that still covers releases the call");
    assert_eq!(released[0].call, approved);

    let facts = appended_facts(decided);
    let spent = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::CandidateConsumed { subject, .. } => Some(subject.clone()),
            _ => None,
        })
        .expect("the batch spends the derivation it stood on");
    assert_eq!(
        spent,
        crate::basis::SubjectKey::Call {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b2"),
            position: 0,
        },
        "the record names the derivation that permitted the emission, not the first in order"
    );
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

/// One derivation releases one call. A sibling's take does not reach the working projection —
/// only the batch's own `BasisAdvanced` does — so the batch has to exclude what it already
/// took, or two identical proposals would both stand on the one rewrite the model paid for.
#[test]
fn a_derivation_releases_one_of_two_identical_siblings_and_blocks_the_other() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();
    let hopped = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the hop runs");
    let approved = approved_by(&hopped);
    let log = [log, appended_facts(hopped)].concat();

    let decided = e
        .handle(
            &viewing(&e, &log),
            batch("b2", Vec::new(), vec![raw(&approved), raw(&approved)]),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decided);
    assert_eq!(released.len(), 1, "the one derivation released one of the two");
    assert_eq!(blocked.len(), 1, "the other is judged at the origin stage and blocks");
    assert_eq!(blocked[0].call, approved);

    let facts = appended_facts(decided);
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact, Fact::CandidateConsumed { .. }))
            .count(),
        1,
        "the derivation is spent once: {facts:?}"
    );
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn an_opening_names_the_position_the_decision_released_it_for() {
    let e = engine(vec![plain_tool("ping")]);
    let log = vec![opened(&e)];
    let ping = call("ping", json!({}));
    let decided = e
        .handle(
            &viewing(&e, &log),
            batch("b1", Vec::new(), vec![raw(&ping), raw(&ping)]),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decided);
    assert!(blocked.is_empty());
    assert_eq!(released.len(), 2, "identical siblings take their own occurrences");
    let log = [log, appended_facts(decided)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let subject = |position: u32| crate::basis::SubjectKey::Call {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new("b1"),
        position,
    };
    let openings: Vec<_> = log
        .iter()
        .filter_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, subject, .. } => Some((dispatch.clone(), subject.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(openings[0].1, subject(0));
    assert_eq!(openings[1].1, subject(1));

    let mut forged = log.clone();
    let second = forged
        .iter_mut()
        .filter_map(|fact| match fact {
            Fact::DispatchOpened { subject, .. } => Some(subject),
            _ => None,
        })
        .nth(1)
        .expect("the batch opened two dispatches");
    *second = subject(0);
    assert_eq!(
        e.validate_replay(&forged),
        Err(crate::transition::TransitionRefusal::UnbackedDecision)
    );
}

#[test]
fn a_substitution_that_leaves_a_gap_re_plans_over_the_derived_call() {
    let e = substituting_engine(SUSPICIOUS);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();

    let hopped = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the hop runs");
    let block = match offer_answer(&hopped) {
        OfferFollowUp::Substituted { block } => (**block).clone(),
        other => panic!("a substitution that still blocks re-plans, got {other:?}"),
    };
    assert_eq!(block.call.canonical_arguments().canonical_text(), REDACTED);
    assert_eq!(
        block.block.raw.requirement_gaps,
        vec![Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        }],
        "the substitution cleared the recipients its `to` declared, and nothing else"
    );
    let facts = appended_facts(hopped);
    let stage = opened_offers(&facts);
    assert_eq!(
        stage.len(),
        1,
        "the spent sanitizer is not offered again on its own successor"
    );
    assert!(stage[0].1.hop().is_none());
    let log = [log, facts].concat();

    let (offer, plan) = stage[0].clone();
    let evidence = evidence_for(
        offer,
        &plan,
        "post",
        Label::new(SUSPICIOUS, Audience::restricted([ReaderId::new("insider")])),
    );
    let approved = execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the officer answers");
    assert_eq!(
        offer_answer(&approved),
        &OfferFollowUp::Approved {
            call: Box::new(block.call.clone())
        }
    );
    let log = [log, appended_facts(approved)].concat();

    let released = proposed(&e, &log, "b2", nonce(), block.call).expect("the approved call releases");
    match &released.follow_up {
        FollowUp::Proposals { released, .. } => assert_eq!(released.len(), 1),
        other => panic!("the approved proposal releases, got {other:?}"),
    }
    let log = [log, appended_facts(released)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    match &proposed(&e, &log, "b3", nonce(), proposal)
        .expect("the batch decides")
        .follow_up
    {
        FollowUp::Proposals { released, blocked, .. } => {
            assert!(released.is_empty());
            assert_eq!(blocked.len(), 1);
        }
        other => panic!("a fresh proposal decides as proposals, got {other:?}"),
    }
}

/// An approval is admitted only as what an accepted offer owes. A hop offer owes its
/// derivation, so an approval appended for it — here over a rewrite that kept the original
/// bytes, which the hop's empty rulings would otherwise release despite the trust gap — is
/// refused.
#[test]
fn an_approval_no_accepted_offer_owes_is_refused_on_replay() {
    let e = substituting_engine(SUSPICIOUS);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let (hop, plan) = opened_offers(&facts)[0].clone();
    assert!(plan.hop().is_some());
    let log = [log, facts].concat();
    let same_bytes = proposal.canonical_arguments().canonical_text().to_string();
    let hopped =
        appended_facts(execute_offer(&e, &log, hop, substitution(&proposal, &same_bytes)).expect("the hop runs"));
    let log = [log, hopped].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let basis = Projection::build(&log, log.len() as u64)
        .view(&traj())
        .basis_for(&crate::basis::SubjectKey::Approval(hop));
    let forged = Fact::CallApproved {
        trajectory: traj(),
        offer: hop,
        call: proposal,
        plan: plan.id,
        acceptance: None,
        rulings: Vec::new(),
        output: None,
        return_policy: None,
        basis,
        evidence: crate::audience::AudienceEvidence::default(),
    };
    assert_eq!(
        e.validate_replay(&[log, vec![forged]].concat()),
        Err(TransitionRefusal::UnbackedApproval)
    );
}

#[test]
fn a_replacement_the_engine_cannot_bind_leaves_the_hop_unspent() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();
    let run = |outcome: OfferOutcome| execute_offer(&e, &log, hop, outcome).map(|_| ());

    for unbindable in [
        r#"{"body":"a"} {"body":"b"}"#,
        r#"["body"]"#,
        r#"{"body":7}"#,
        r#"{"other":"a"}"#,
        "",
    ] {
        assert!(
            matches!(
                run(substitution(&proposal, unbindable)),
                Err(TransitionError::Call(EngineError::InvalidCall(_)))
            ),
            "{unbindable:?} was bound as a replacement call"
        );
    }
    assert_eq!(
        run(OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
            sanitizer: crate::names::SanitizerName::new("officer"),
            source: crate::value::RawResultDigest::of(proposal.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(REDACTED),
        })),
        Err(TransitionError::EvidenceMismatch)
    );
    assert_eq!(
        run(substitution(&call("post", json!({ "body": "other" })), REDACTED)),
        Err(TransitionError::EvidenceMismatch),
        "a derivation of other bytes is not a derivation of this candidate's arguments"
    );
    assert_eq!(
        run(OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::PlanOutcomeMismatch)
    );
    assert!(execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).is_ok());
}

#[test]
fn a_proposal_missing_its_annotation_refuses_the_event_and_appends_nothing() {
    let e = substituting_engine(TRUSTED);
    let log = internal_log(&e);
    let unpinned = call("post_dyn", json!({ "body": "ssn 123" }));
    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            batch_on(&traj(), "b1", Vec::new(), vec![raw(&unpinned), raw(&unpinned)], None)
        ),
        Err(TransitionError::AnnotationNeeded {
            annotators: vec![crate::names::AnnotatorName::new("acl")]
        })
    );
    let unpinned_post = call("post", json!({ "body": "ssn 123" }));
    let foreign = unpinned_post.clone().with_annotation(Some(pinned_for(
        post_dyn_annotation(&["partner"]),
        "acl",
        &unpinned_post,
    )));
    assert!(
        matches!(
            e.handle(
                &viewing(&e, &log),
                batch_on(&traj(), "b2", Vec::new(), vec![raw(&foreign)], None)
            ),
            Err(TransitionError::ForeignAnnotation { .. })
        ),
        "a static declaration is its own annotation; an annotator's pin on it is foreign"
    );
    let sibling = call("post_dyn", json!({ "body": "other" }));
    let restated = call("post_dyn", json!({ "body": "ssn 123" })).with_annotation(Some(pinned_for(
        post_dyn_annotation(&["partner"]),
        "acl",
        &sibling,
    )));
    assert!(
        matches!(
            e.handle(
                &viewing(&e, &log),
                batch_on(&traj(), "b3", Vec::new(), vec![raw(&restated)], None)
            ),
            Err(TransitionError::ForeignAnnotation { .. })
        ),
        "annotation evidence binds the exact call it judged: a sibling call cannot reuse it"
    );
}

#[test]
fn a_hop_goes_stale_with_its_basis_and_a_taken_one_stops_answering() {
    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();

    let mut moved = log.clone();
    reads(&e, &mut moved, &traj(), "ping");
    assert_eq!(
        execute_offer(&e, &moved, hop, substitution(&proposal, REDACTED)).map(|_| ()),
        Err(TransitionError::StaleOffer)
    );

    let hopped = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the hop runs");
    let answer = offer_answer(&hopped).clone();
    let approved = approved_by(&hopped);
    let log = [log, appended_facts(hopped)].concat();
    let repeat = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the repeat answers");
    assert_eq!(repeat.append, None);
    assert_eq!(offer_answer(&repeat), &answer);
    assert_eq!(
        execute_offer(&e, &log, hop, OfferOutcome::Approved(Vec::new())).map(|_| ()),
        Err(TransitionError::PlanOutcomeMismatch)
    );

    // The proposal that takes the derivation spends it. From here the hop has nothing left
    // to hand out, before the call runs and after it closes alike: a second run of the
    // substituted call needs a second rewrite.
    let facts = taken(&e, &log, "b3", &approved);
    let dispatch = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
            _ => None,
        })
        .expect("the taking batch opens the dispatch");
    let log = [log, facts].concat();
    let spent = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the repeat answers");
    assert_eq!(spent.append, None);
    assert_eq!(offer_answer(&spent), &OfferFollowUp::Invalidated);

    let reported = e
        .handle(
            &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("posted")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the raw result crosses");
    let log = [log, appended_facts(reported)].concat();
    let settled = execute_offer(&e, &log, hop, substitution(&proposal, REDACTED)).expect("the repeat answers");
    assert_eq!(settled.append, None);
    assert_eq!(offer_answer(&settled), &OfferFollowUp::Invalidated);
    assert_eq!(e.validate_replay(&log), Ok(()));
}

fn input_sanitizer_to(name: &str, from: &[&str], to: DeclaredAudience) -> crate::authority::Sanitizer {
    crate::authority::Sanitizer {
        transition: crate::authority::DeclaredTransition::Audience {
            from_includes: DeclaredAudience::literal(Audience::restricted(
                from.iter().map(|reader| ReaderId::new(*reader)),
            )),
            to,
        },
        ..input_sanitizer(name, from, from)
    }
}

/// `post` split into two declarations by its body: the `ssn*` bodies select the first, every
/// other body the second. `redact` has jurisdiction over both.
fn selecting_substitution_engine() -> Engine {
    open_engine_at(
        RegistryConfig {
            annotators: Vec::new(),
            authorities: Vec::new(),
            sanitizers: vec![input_sanitizer_to(
                "redact",
                &["insider"],
                DeclaredAudience::restricted([ReaderId::new("insider"), ReaderId::new("partner")]),
            )],
            ..test_config(vec![
                post_tool("post(body:ssn*)", vec![crate::names::TagName::new("outbound")]),
                post_tool("post", vec![crate::names::TagName::new("outbound")]),
            ])
        },
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    )
}

/// `proposal` blocked and hopped to [`REDACTED`]: the log up to the hop, and the hop's facts.
fn redacted_hop(e: &Engine, proposal: &ResolvedCall) -> (Vec<Fact>, Vec<Fact>) {
    let log = internal_log(e);
    let facts = appended_facts(proposed(e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();
    let hopped = appended_facts(execute_offer(e, &log, hop, substitution(proposal, REDACTED)).expect("the hop runs"));
    (log, hopped)
}

/// The hop's derived call, re-recorded with a host-pinned file basis.
fn with_pinned_file_basis(mut hopped: Vec<Fact>) -> Vec<Fact> {
    let basis = crate::value::FileBasis::Read(crate::value::FileSource {
        version: "v1".into(),
        digest: "content".into(),
        label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    });
    let mut pinned = false;
    for fact in &mut hopped {
        if let Fact::CandidateDerived {
            derived: DerivedCandidate::Call { call, .. },
            ..
        } = fact
        {
            *call = call.clone().with_file_basis(Some(basis.clone()));
            pinned = true;
        }
    }
    assert!(pinned, "the hop derives a call candidate");
    hopped
}

#[test]
fn a_same_declaration_rewrite_carrying_a_file_basis_is_refused() {
    let e = substituting_engine(TRUSTED);
    let (log, hopped) = redacted_hop(&e, &call("post", json!({ "body": "ssn 123" })));
    assert_eq!(e.validate_replay(&[log.clone(), hopped.clone()].concat()), Ok(()));
    assert_eq!(
        e.validate_replay(&[log, with_pinned_file_basis(hopped)].concat()),
        Err(TransitionRefusal::ForgedLabel)
    );
}

#[test]
fn a_rewrite_that_selects_another_declaration_may_carry_a_file_basis() {
    let e = selecting_substitution_engine();
    let proposal = e
        .resolve_call(ToolName::new("post"), br#"{"body":"ssn 123"}"#)
        .expect("the body selects a declaration");
    let (log, hopped) = redacted_hop(&e, &proposal);
    let derived = hopped
        .iter()
        .find_map(|fact| match fact {
            Fact::CandidateDerived {
                derived: DerivedCandidate::Call { call, .. },
                ..
            } => Some(call.clone()),
            _ => None,
        })
        .expect("the hop derives a call candidate");
    assert!(
        !e.registry.selects_alike(&derived, &proposal),
        "the rewrite selects the other declaration"
    );
    assert_eq!(e.validate_replay(&[log.clone(), hopped.clone()].concat()), Ok(()));
    assert_eq!(
        e.validate_replay(&[log, with_pinned_file_basis(hopped)].concat()),
        Ok(())
    );
}

/// A send to bob whose `redact` hop staged it at `@b` while bob sat in `@b`, then proposed
/// again after bob moved from `@b` to `@a`: the staged stage now blocks, the default stage
/// (the trajectory's own `@a`) allows. Returns the log before that proposal and its decision.
fn staged_call_after_membership_moves() -> (Engine, Vec<Fact>, Result<EngineDecision, TransitionError>) {
    let mut send = plain_tool("send");
    send.parameters = crate::params::test_string_argument_schema("to");
    send.requires = Requires {
        label: LabelRequirements {
            trust_floor: None,
            audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
        },
        ..Requires::default()
    };
    let redact = crate::authority::Sanitizer {
        name: crate::names::SanitizerName::new("redact"),
        on: crate::authority::SanitizerPoints {
            input: true,
            output: false,
        },
        transition: crate::authority::DeclaredTransition::Audience {
            from_includes: group_audience("a"),
            to: group_audience("b"),
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let e = open_engine_at(
        RegistryConfig {
            sanitizers: vec![redact],
            audience: slack_groups(&["a", "b"]),
            ..test_config(vec![send])
        },
        Label::new(TRUSTED, Audience::of_declared(&group_audience("a"))),
    );
    let bob = || slack_member("slack:UB", Some("bob@corp.com"));
    let bob_in_b = source_evidence(vec![user_group("a", vec![]), user_group("b", vec![bob()])]);
    let bob_in_a = source_evidence(vec![user_group("a", vec![bob()]), user_group("b", vec![])]);
    let proposal = call("send", json!({ "to": "bob@corp.com" }));

    let log = vec![opened(&e)];
    let blocked = appended_facts(
        proposed_with(&e, &log, "b1", nonce(), proposal.clone(), bob_in_b.clone()).expect("the batch decides"),
    );
    let hop = hop_named(&blocked, "redact");
    let log = [log, blocked].concat();
    let hopped = execute_offer_with(
        &e,
        &log,
        hop,
        OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
            sanitizer: crate::names::SanitizerName::new("redact"),
            source: crate::value::RawResultDigest::of(proposal.canonical_arguments().canonical_bytes()),
            derived: ValueBody::new(r#"{"to":"bob@corp.com"}"#),
            annotation: None,
        }),
        bob_in_b,
    )
    .expect("the hop runs");
    let log = [log, appended_facts(hopped)].concat();
    let decided = proposed_with(&e, &log, "b2", nonce(), proposal, bob_in_a);
    (e, log, decided)
}

#[test]
fn a_staged_call_the_default_stage_clears_releases_without_taking_the_derivation() {
    let (_, _, decided) = staged_call_after_membership_moves();
    let decided = decided.expect("the batch decides");
    let (released, blocked) = answered(&decided);
    assert_eq!((released.len(), blocked.len()), (1, 0));
    let facts = appended_facts(decided);
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::CandidateConsumed { .. } | Fact::CallApprovalConsumed { .. })),
        "the release stands on the default stage and spends nothing"
    );
}

#[test]
fn a_call_released_under_the_default_stage_over_a_standing_derivation_replays() {
    let (e, log, decided) = staged_call_after_membership_moves();
    let facts = appended_facts(decided.expect("the batch decides"));
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}
