use super::*;

#[test]
fn a_proposal_naming_a_provider_run_tool_is_malformed_at_every_fresh_entry_point() {
    let e = engine_with_provider_run(vec![plain_tool("search")], &["search"]);
    let log = vec![opened(&e)];
    let p = Projection::build(&log, 1);
    let t = traj();
    let views = p.view(&t);
    let proposed = call("search", json!({}));
    assert!(matches!(
        e.check(&views, &proposed),
        Err(EngineError::ProviderRunTool(name)) if name == "search"
    ));
    let raw = crate::check::RawBlock {
        requirement_gaps: vec![],
        narrowing: None,
    };
    assert!(matches!(
        e.plan(&views, &proposed, &raw),
        Err(EngineError::ProviderRunTool(_))
    ));
    assert!(matches!(
        e.resolve_call(ToolName::new("search"), b"{}"),
        Err(EngineError::ProviderRunTool(_))
    ));
    // A provider-run name has no checkable contract at all, so a persisted batch naming it at
    // ordinal zero is not an undeclared tool's batch: replay refuses the name itself.
    let forged = vec![
        opened(&e),
        Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            proposals: vec![proposed],
            spawn: None,
            released: vec![],
            evidence: crate::audience::AudienceEvidence::default(),
        },
    ];
    assert!(matches!(
        e.validate_replay(&forged),
        Err(TransitionRefusal::UnknownTool(name)) if name == "search"
    ));
}

/// A released tool nothing covers is refused by name on replay. Under a wildcard the name
/// is covered, but the wildcard prescribes its annotator's mandate: a record carrying a
/// static pin for it is forged, and so is a record naming any other ordinal.
#[test]
fn replay_refuses_a_released_tool_nothing_covers_and_a_forged_wildcard_record() {
    let e = engine(vec![crm_tool()]);
    let ghost = call("ghost", json!({}));
    let forged = |proposal: ResolvedCall| {
        let dispatch = DispatchId::new(traj(), proposal.digest(), 0);
        vec![
            opened(&e),
            Fact::ProposalBatchDecided {
                trajectory: traj(),
                batch: crate::transition::ProposalBatchId::new("b1"),
                proposals: vec![proposal.clone()],
                spawn: None,
                released: vec![dispatch.clone()],
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch,
                tool: proposal.tool().clone(),
                arguments: proposal.canonical_arguments().clone(),
                proposed_label: Label::new(TRUSTED, Audience::public()),
                receiving: Label::new(TRUSTED, Audience::public()),
                proposed_effects: EffectSet::default(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj()),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
            },
        ]
    };
    // No wildcard: the name has no contract at all, and replay refuses it as such.
    assert_eq!(
        e.validate_replay(&forged(ghost.clone())),
        Err(TransitionRefusal::UnknownTool("ghost".to_string()))
    );

    // A wildcard covers the name, but the record's static pin is not its annotator's.
    let mut cfg = test_config(vec![crm_tool()]);
    cfg.tools.push(wildcard("any"));
    cfg.annotators.push(annotator("any"));
    let covered = open_engine(cfg);
    let forged_static = {
        let mut log = forged(ghost.clone());
        log[0] = opened(&covered);
        log
    };
    assert_eq!(
        covered.validate_replay(&forged_static),
        Err(TransitionRefusal::ForgedEvidence)
    );
}

#[test]
fn provider_run_tools_leave_every_plan_family() {
    let mut target = plain_tool("wire");
    target.requires = Requires {
        history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
        ..Requires::default()
    };
    let mut emitter = plain_tool("emit");
    emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
    let log = vec![crate::profile::opening_at(
        traj(),
        Label::new(TRUSTED, Audience::public()),
    )];
    let offered_tools = |e: &Engine| -> Vec<String> {
        let p = Projection::build(&log, 1);
        let t = traj();
        let wire = call("wire", json!({}));
        let raw = match e.check(&p.view(&t), &wire).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        e.plan(&p.view(&t), &wire, &raw)
            .unwrap()
            .plans
            .iter()
            .filter_map(|plan| match plan {
                plan::RemedyPlan::Redispatch(redispatch) => Some(redispatch.tool().as_str().to_string()),
                plan::RemedyPlan::Executable(_) => None,
            })
            .collect()
    };
    let enforced = engine(vec![target.clone(), emitter.clone()]);
    assert_eq!(offered_tools(&enforced), ["emit"]);
    let split = engine_with_provider_run(vec![target, emitter], &["emit"]);
    assert_eq!(offered_tools(&split), Vec::<String>::new());
}

fn raw_call(tool: &str, arguments: &[u8]) -> crate::transition::ProposedCall {
    crate::transition::ProposedCall {
        tool: ToolName::new(tool),
        arguments: arguments.to_vec(),
        annotation: None,
    }
}

fn blocked_names(blocked: &[Blocked]) -> Vec<&str> {
    blocked.iter().map(|block| block.call.tool().as_str()).collect()
}

#[test]
fn an_exposed_provider_run_result_is_history_for_every_sibling() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("wire", json!({}))), raw(&call("guard", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["wire"]);
    assert_eq!(blocked_names(blocked), ["guard"]);
    assert!(
        blocked[0]
            .block
            .raw
            .requirement_gaps
            .contains(&Gap::NoPrior(EffectKind::new("k")))
    );

    let facts = appended_facts(decision);
    match &facts[..2] {
        [
            Fact::BasisAdvanced { .. },
            Fact::ValueAdmitted { value, provenance, .. },
        ] => {
            assert_eq!(value.body, ValueBody::new("the provider ran it"));
            assert_eq!(value.label, Delta::NONE.output_label());
            assert_eq!(
                provenance,
                &Provenance::ProviderRun {
                    tool: ToolName::new("seen"),
                    batch: crate::transition::ProposalBatchId::new("b1"),
                    position: 0,
                    effects: EffectSet::new([EffectKind::new("k")]).unwrap(),
                    evidence: crate::audience::AudienceEvidence::default(),
                }
            );
        }
        other => panic!("the admissions open the batch, not {other:?}"),
    }
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::DispatchSucceeded { .. } | Fact::DispatchClosed { .. }))
    );
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn a_provider_run_result_the_response_hides_establishes_no_effect() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                Vec::new(),
                vec![raw(&call("wire", json!({}))), raw(&call("guard", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["guard"]);
    assert_eq!(blocked_names(blocked), ["wire"]);
    assert!(
        blocked[0]
            .block
            .raw
            .requirement_gaps
            .contains(&Gap::Prior(EffectKind::new("k")))
    );
}

#[test]
fn a_malformed_sibling_mediates_none_of_them_and_the_admissions_stand() {
    let e = batch_engine();
    let log = opening_log(&e);
    // A sibling nothing covers is one of the malformed shapes: no contract, no decision,
    // and the provider-run admissions stand.
    for (position, malformed) in [
        (1, raw_call("seen", b"{}")),
        (1, raw_call("quiet", b"not json")),
        (1, raw_call("nowhere", b"{}")),
    ] {
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch(
                    "b1",
                    vec![exposed("seen", "the provider ran it")],
                    vec![raw(&call("quiet", json!({}))), malformed],
                ),
            )
            .expect("the batch answers");
        match &decision.follow_up {
            FollowUp::Malformed { position: at, .. } => assert_eq!(*at, position),
            other => panic!("a malformed batch answers with its refusal, not {other:?}"),
        }
        let facts = appended_facts(decision);
        assert!(matches!(&facts[0], Fact::BasisAdvanced { .. }));
        assert!(matches!(
            &facts[1],
            Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { .. },
                ..
            }
        ));
        assert_eq!(facts.len(), 2, "nothing beyond the admissions: {facts:?}");
        assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));
    }
}

#[test]
fn a_retry_of_a_malformed_batch_admits_its_results_once_and_then_decides() {
    let e = batch_engine();
    let log = opening_log(&e);
    let malformed = || {
        batch(
            "b1",
            vec![exposed("seen", "the provider ran it")],
            vec![raw_call("quiet", b"not json")],
        )
    };
    let first = e.handle(&viewing(&e, &log), malformed()).expect("the batch answers");
    let log = [log, appended_facts(first)].concat();

    let repeat = e.handle(&viewing(&e, &log), malformed()).expect("the repeat answers");
    assert!(matches!(repeat.follow_up, FollowUp::Malformed { position: 0, .. }));
    assert_eq!(repeat.append, None, "a repeat admits nothing a second time");

    let corrected = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("wire", json!({})))],
            ),
        )
        .expect("the corrected batch decides");
    let (released, _) = answered(&corrected);
    assert_eq!(tool_names(released), ["wire"], "the admission is history for the retry");
    let facts = appended_facts(corrected);
    assert!(
        !facts.iter().any(|fact| matches!(fact, Fact::ValueAdmitted { .. })),
        "the results were admitted by the first attempt: {facts:?}"
    );
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn a_batch_identity_is_bound_to_its_exposed_results_and_their_trajectory() {
    let e = batch_engine();
    let log = opening_log(&e);
    let first = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw_call("quiet", b"not json")],
            ),
        )
        .expect("the batch answers");
    let log = [log, appended_facts(first)].concat();

    let other_body = e.handle(
        &viewing(&e, &log),
        batch(
            "b1",
            vec![exposed("seen", "something else")],
            vec![raw(&call("quiet", json!({})))],
        ),
    );
    assert_eq!(other_body.unwrap_err(), TransitionError::BatchIdentityConflict);

    let dropped = e.handle(
        &viewing(&e, &log),
        batch("b1", Vec::new(), vec![raw(&call("quiet", json!({})))]),
    );
    assert_eq!(dropped.unwrap_err(), TransitionError::BatchIdentityConflict);

    let other = TrajectoryId::new("other");
    let with_other = [log.clone(), forked_child(&e, &log, &other)].concat();
    let elsewhere = e.handle(
        &viewing(&e, &with_other),
        batch_on(
            &other,
            "b1",
            vec![exposed("seen", "the provider ran it")],
            vec![raw(&call("quiet", json!({})))],
            None,
        ),
    );
    assert_eq!(elsewhere.unwrap_err(), TransitionError::BatchIdentityConflict);

    let decided = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch("b2", Vec::new(), vec![raw(&call("quiet", json!({})))]),
        )
        .expect("the batch decides"),
    );
    let log = [log, decided].concat();
    let smuggled = e.handle(
        &viewing(&e, &log),
        batch(
            "b2",
            vec![exposed("seen", "the provider ran it")],
            vec![raw(&call("quiet", json!({})))],
        ),
    );
    assert_eq!(smuggled.unwrap_err(), TransitionError::BatchIdentityConflict);

    let swapped = e.handle(
        &viewing(&e, &log),
        batch(
            "b1",
            vec![exposed("nowhere", "the provider ran it")],
            vec![raw(&call("quiet", json!({})))],
        ),
    );
    assert_eq!(swapped.unwrap_err(), TransitionError::BatchIdentityConflict);
}

#[test]
fn an_exposed_result_of_a_tool_this_deployment_releases_is_refused() {
    let e = batch_engine();
    let log = opening_log(&e);
    for (tool, expected) in [
        ("quiet", EngineError::NotProviderRun("quiet".to_string())),
        ("nowhere", EngineError::UnknownTool("nowhere".to_string())),
    ] {
        let refusal = e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed(tool, "a body")],
                vec![raw(&call("quiet", json!({})))],
            ),
        );
        assert_eq!(refusal.unwrap_err(), TransitionError::Call(expected));
    }
}

#[test]
fn a_provider_admission_advances_flow_only_when_it_moves_the_label_and_family_only_for_its_effects() {
    let e = engine_with_provider_run(
        vec![
            plain_tool("quiet"),
            {
                let mut emitting = plain_tool("loud");
                emitting.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
                emitting
            },
            {
                let mut narrowing = plain_tool("insider");
                narrowing.delta = Delta {
                    trust: None,
                    audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                        "insider",
                    )]))),
                };
                narrowing
            },
        ],
        &["quiet", "loud", "insider"],
    );
    let log = opening_log(&e);
    let declared = |tool: &str| {
        let decision = e
            .handle(
                &viewing(&e, &log),
                batch("b1", vec![exposed(tool, "a body")], Vec::new()),
            )
            .expect("an admission-only batch decides");
        match &appended_facts(decision)[0] {
            Fact::BasisAdvanced { advance, .. } => advance.clone(),
            other => panic!("the declaration opens the batch, not {other:?}"),
        }
    };
    let quiet = declared("quiet");
    assert!(
        quiet.flows.is_empty(),
        "an admission at the identity label moves no flow: nothing an open offer reads changed"
    );
    assert!(
        !quiet.family,
        "an observation with no declared effects moves no family state"
    );
    let loud = declared("loud");
    assert!(loud.family);
    assert!(loud.flows.is_empty(), "effects move the family, not the flow");
    assert!(
        declared("insider").flows.contains(&traj()),
        "an admission that narrows the label moves the flow"
    );
}

#[test]
fn an_admission_only_batch_decides_and_an_empty_one_is_no_event() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch("b1", vec![exposed("seen", "the provider ran it")], Vec::new()),
        )
        .expect("an admission-only batch decides");
    let (released, blocked) = answered(&decision);
    assert!(released.is_empty() && blocked.is_empty());
    let facts = appended_facts(decision);
    assert!(matches!(
        &facts[2],
        Fact::ProposalBatchDecided { proposals, released, .. } if proposals.is_empty() && released.is_empty()
    ));
    assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));

    let empty = e.handle(&viewing(&e, &log), batch("b2", Vec::new(), Vec::new()));
    assert_eq!(empty.unwrap_err(), TransitionError::EmptyBatch);

    let marked = e.handle(
        &viewing(&e, &log),
        batch_on(&traj(), "b3", Vec::new(), Vec::new(), Some(SpawnMark::at(0))),
    );
    assert_eq!(marked.unwrap_err(), TransitionError::SpawnMarkOutOfRange);
}

#[test]
fn a_siblings_release_is_state_for_the_siblings_after_it() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                Vec::new(),
                vec![
                    raw(&call("quiet", json!({}))),
                    raw(&call("quiet", json!({}))),
                    raw(&call("emit", json!({}))),
                    raw(&call("guard", json!({}))),
                ],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["quiet", "quiet", "emit"]);
    assert_eq!(
        (released[0].dispatch.occurrence(), released[1].dispatch.occurrence()),
        (0, 1),
        "two identical siblings are two occurrences of one call"
    );
    assert_eq!(blocked_names(blocked), ["guard"]);
    assert!(
        blocked[0]
            .block
            .raw
            .requirement_gaps
            .contains(&Gap::NoPrior(EffectKind::new("k")))
    );
    let facts = appended_facts(decision);
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn a_refused_sibling_is_planned_against_the_batchs_final_state() {
    let mut strict = plain_tool("strict");
    strict.requires = Requires {
        label: floor(TRUSTED),
        history: vec![HistoryRequirement::NoPrior(EffectKind::new("k"))],
        ..Requires::default()
    };
    let mut emit = plain_tool("emit");
    emit.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
    let e = engine_at(vec![strict, emit], Label::new(SUSPICIOUS, Audience::public()));

    let log = vec![opened(&e)];
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                Vec::new(),
                vec![raw(&call("strict", json!({}))), raw(&call("emit", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["emit"]);
    assert_eq!(blocked_names(blocked), ["strict"]);
    let gaps = &blocked[0].block.raw.requirement_gaps;
    assert!(
        gaps.contains(&Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS
        }),
        "the gap it was refused for: {gaps:?}"
    );
    assert!(
        gaps.contains(&Gap::NoPrior(EffectKind::new("k"))),
        "the gap the sibling's own release opened: {gaps:?}"
    );
    let facts = appended_facts(decision);
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn a_marked_spawn_prepares_its_fork_from_its_own_position() {
    let mut spawn = plain_tool("spawn");
    spawn.requires = Requires {
        history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
        ..Requires::default()
    };
    let e = engine(vec![plain_tool("quiet"), spawn]);
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch_on(
                &traj(),
                "b1",
                Vec::new(),
                vec![raw(&call("quiet", json!({}))), raw(&call("spawn", json!({})))],
                Some(SpawnMark::at(1)),
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["quiet"]);
    assert_eq!(blocked_names(blocked), ["spawn"]);
    assert!(released[0].fork.is_none(), "the unmarked sibling prepares nothing");
    let facts = appended_facts(decision);
    assert!(
        !facts.iter().any(|fact| matches!(fact, Fact::ForkPrepared { .. })),
        "a refused spawn prepares no fork: {facts:?}"
    );

    let log = [log, facts].concat();
    let quiet_pair = e
        .handle(
            &viewing(&e, &log),
            batch_on(
                &traj(),
                "b2",
                Vec::new(),
                vec![raw(&call("quiet", json!({}))), raw(&call("quiet", json!({})))],
                Some(SpawnMark::at(1)),
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&quiet_pair);
    assert_eq!(tool_names(released), ["quiet"]);
    assert!(released[0].fork.is_none(), "the unmarked sibling prepares nothing");
    assert_eq!(blocked_names(blocked), ["quiet"]);
    assert!(
        blocked[0].block.raw.requirement_gaps.is_empty(),
        "the marked sibling waits only for its return declaration"
    );
    let facts = appended_facts(quiet_pair);
    let (offer, _) = opened_offers(&facts)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
        .expect("the bare declaration is offered");
    let log = [log, facts].concat();
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
        .expect("the declaration approves the marked quiet call");
    let log = [log, appended_facts(approved)].concat();
    let released_spawn = e
        .handle(
            &viewing(&e, &log),
            batch_on(
                &traj(),
                "b3",
                Vec::new(),
                vec![raw(&call("quiet", json!({})))],
                Some(SpawnMark::at(0)),
            ),
        )
        .expect("the batch decides");
    let (released, _) = answered(&released_spawn);
    let fork = released[0].fork.clone().expect("the marked call prepared its fork");
    assert_eq!(fork, ForkId::of(&released[0].dispatch));
    let facts = appended_facts(released_spawn);
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn an_approval_is_spent_only_by_a_singleton_batch() {
    let e = engine(vec![crm_tool(), plain_tool("read_note")]);
    let log = opening_log(&e);
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();

    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b2",
                Vec::new(),
                vec![raw(&call("get_ticket", json!({}))), raw(&call("read_note", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(tool_names(released), ["read_note"]);
    assert_eq!(blocked_names(blocked), ["get_ticket"]);
    let facts = appended_facts(decision);
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::CallApprovalConsumed { .. })),
        "a sibling batch spends no approval: {facts:?}"
    );
    let alone = e
        .handle(
            &viewing(&e, &[log.clone(), facts].concat()),
            batch("b3", Vec::new(), vec![raw(&call("get_ticket", json!({})))]),
        )
        .expect("the singleton decides");
    let (released, _) = answered(&alone);
    assert_eq!(tool_names(released), ["get_ticket"]);
}

#[test]
fn an_identity_admission_leaves_the_approval_its_own_batch_spends_current() {
    let e = engine_with_provider_run(vec![crm_tool(), plain_tool("seen")], &["seen"]);
    let log = opening_log(&e);
    let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
    let offer = opened_offers(&opened)[0].0;
    let log = [log, opened].concat();
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();

    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b2",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("get_ticket", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(
        tool_names(released),
        ["get_ticket"],
        "an admission at the identity label leaves the approval current, and the batch spends it"
    );
    assert!(blocked.is_empty());
    assert_eq!(
        e.validate_replay(&[log.clone(), appended_facts(decision)].concat()),
        Ok(())
    );

    let release = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch("b3", Vec::new(), vec![raw(&call("get_ticket", json!({})))]),
        )
        .expect("the singleton releases"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), release.clone()].concat()), Ok(()));
    let mut spliced = release;
    spliced.insert(
        1,
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new("the provider ran it"), Delta::NONE.output_label()),
            provenance: Provenance::ProviderRun {
                tool: ToolName::new("seen"),
                batch: crate::transition::ProposalBatchId::new("b3"),
                position: 0,
                effects: EffectSet::default(),
                evidence: crate::audience::AudienceEvidence::default(),
            },
        },
    );
    // Under its own declared act, at the next position, with the contract's label and
    // effects, an identity-label provider admission is a record the engine could have
    // produced from a batch that carried the result: nothing distinguishes the splice from
    // that log, and it moves no basis, so replay has nothing to refuse.
    assert_eq!(e.validate_replay(&[log, spliced].concat()), Ok(()));
}

#[test]
fn a_repeat_of_a_multi_sibling_batch_answers_each_position_from_the_record() {
    let e = batch_engine();
    let log = opening_log(&e);
    let proposals = || {
        vec![
            raw(&call("quiet", json!({}))),
            raw(&call("quiet", json!({}))),
            raw(&call("wire", json!({}))),
        ]
    };
    let first = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
        .expect("the batch decides");
    let (released, _) = answered(&first);
    let dispatches: Vec<DispatchId> = released.iter().map(|release| release.dispatch.clone()).collect();
    let log = [log, appended_facts(first)].concat();

    let repeat = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
        .expect("the repeat answers");
    assert_eq!(repeat.append, None);
    let (released, blocked) = answered(&repeat);
    assert_eq!(
        released.iter().map(|r| r.dispatch.clone()).collect::<Vec<_>>(),
        dispatches
    );
    assert_eq!(blocked_names(blocked), ["wire"]);
}

#[test]
fn a_repeat_matches_each_sibling_to_the_dispatch_its_own_annotation_opened() {
    let mut notify = plain_tool("notify");
    notify.parameters = crate::params::test_string_argument_schema("room");
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let mut cfg = test_config(vec![]);
    cfg.tools.push(annotated(notify.clone(), "acl"));
    cfg.annotators
        .push(annotator_with_readers("acl", &["insider", "outsider"]));
    let e = open_engine_at(cfg, Label::new(TRUSTED, internal.clone()));
    let log = vec![opened(&e)];
    let arguments = json!({ "room": "lobby" });
    let pinned = |audience: &Audience| {
        let mut produced = notify.clone();
        produced.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(
            DeclaredAudience::literal(audience.clone()),
        ))];
        let unpinned = call("notify", arguments.clone());
        raw(&unpinned
            .clone()
            .with_annotation(Some(pinned_for(produced, "acl", &unpinned))))
    };
    let outsider = Audience::restricted([ReaderId::new("outsider")]);
    let proposals = || vec![pinned(&outsider), pinned(&internal)];

    let first = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
        .expect("the batch decides");
    let (released, blocked) = answered(&first);
    assert_eq!(released.len(), 1);
    assert_eq!(blocked.len(), 1);
    let ran = released[0].dispatch.clone();
    let required_includes = |call: &ResolvedCall| match call
        .annotation()
        .expect("an annotated proposal carries its pin")
        .produced()
        .requires
        .label
        .audience
        .as_slice()
    {
        [AudienceRequirement::Includes(RecipientSpec::Static(recipients))] => Audience::of_declared(recipients),
        other => panic!("one produced includes requirement, got {other:?}"),
    };
    assert_eq!(required_includes(&released[0].call), internal.clone());
    let log = [log, appended_facts(first)].concat();

    let repeat = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), proposals()))
        .expect("the repeat answers");
    assert_eq!(repeat.append, None);
    let (released, blocked) = answered(&repeat);
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].dispatch, ran);
    assert_eq!(
        required_includes(&released[0].call),
        internal.clone(),
        "the repeat names the sibling that actually ran"
    );
    assert_eq!(blocked.len(), 1);
    assert_eq!(required_includes(&blocked[0].call), outsider.clone());
}

/// A produced `forwards` is held to the annotator's mandate exactly as its `delta` is: live,
/// the proposal is refused; on replay, a record carrying it is forged evidence.
#[test]
fn a_produced_forwards_outside_the_mandate_is_refused_live_and_on_replay() {
    let notify = plain_tool("notify");
    let mut cfg = test_config(vec![]);
    cfg.tools.push(annotated(notify.clone(), "acl"));
    cfg.annotators.push(annotator_with_readers("acl", &["insider"]));
    let e = open_engine_at(
        cfg,
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let log = vec![opened(&e)];
    let unpinned = call("notify", json!({ "room": "lobby" }));
    let forwarding = |reader: &str| {
        let mut produced = notify.clone();
        produced.forwards = Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                reader,
            )]))),
        };
        pinned_for(produced, "acl", &unpinned)
    };

    let outside = unpinned.clone().with_annotation(Some(forwarding("outsider")));
    assert!(matches!(
        e.handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![raw(&outside)])),
        Err(TransitionError::InvalidAnnotation { .. })
    ));

    let within = unpinned.clone().with_annotation(Some(forwarding("insider")));
    let decision = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![raw(&within)]))
        .expect("the batch decides");
    assert_eq!(tool_names(answered(&decision).0), ["notify"]);
    let mut facts = appended_facts(decision);
    assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));
    for fact in &mut facts {
        match fact {
            Fact::ProposalBatchDecided { proposals, .. } => {
                for proposal in proposals.iter_mut() {
                    *proposal = proposal.clone().with_annotation(Some(forwarding("outsider")));
                }
            }
            Fact::DispatchOpened { annotation, .. } => *annotation = Some(forwarding("outsider")),
            _ => {}
        }
    }
    assert_eq!(
        e.validate_replay(&[log, facts].concat()),
        Err(TransitionRefusal::ForgedEvidence)
    );
}

#[test]
fn replay_refuses_a_provider_admission_no_act_declared() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decided = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch("b1", vec![exposed("seen", "the provider ran it")], Vec::new()),
        )
        .expect("an admission-only batch decides"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

    let undeclared: Vec<Fact> = decided
        .iter()
        .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
        .cloned()
        .collect();
    assert_eq!(
        e.validate_replay(&[log.clone(), undeclared].concat()),
        Err(TransitionRefusal::UndeclaredAdmission)
    );

    let child = TrajectoryId::new("child");
    let seeded = forked_child(&e, &log, &child);
    let after_seed = [log.clone(), seeded.clone()].concat();
    let ended = appended_facts(
        e.handle(
            &viewing(&e, &after_seed),
            child_report(&after_seed, &child, ChildSubmission::Void),
        )
        .expect("the child ends its errand"),
    );
    let forked = [log, seeded, ended].concat();
    let on_ended: Vec<Fact> = decided
        .into_iter()
        .map(|fact| match fact {
            Fact::BasisAdvanced {
                act, advance, revision, ..
            } => Fact::BasisAdvanced {
                trajectory: child.clone(),
                act,
                revision,
                advance: crate::basis::BasisAdvance {
                    flows: [child.clone()].into(),
                    ..advance
                },
            },
            Fact::ValueAdmitted { value, provenance, .. } => Fact::ValueAdmitted {
                trajectory: child.clone(),
                value,
                provenance,
            },
            other => other,
        })
        .filter(|fact| !matches!(fact, Fact::ProposalBatchDecided { .. }))
        .collect();
    assert_eq!(
        e.validate_replay(&[forked, on_ended].concat()),
        Err(TransitionRefusal::BranchEnded)
    );
}

#[test]
fn replay_refuses_one_batchs_admissions_split_across_two_decisions() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decided = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the first"), exposed("seen", "the second")],
                Vec::new(),
            ),
        )
        .expect("an admission-only batch decides"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

    let split = vec![
        decided[0].clone(),
        decided[1].clone(),
        decided[0].clone(),
        decided[2].clone(),
    ];
    assert_eq!(
        e.validate_replay(&[log, split].concat()),
        Err(TransitionRefusal::SplitAdmission)
    );
}

#[test]
fn replay_refuses_a_decision_whose_own_declaration_is_missing() {
    let e = batch_engine();
    let opening = opening_log(&e);
    let decide = |log: &[Fact], id: &str| {
        appended_facts(
            e.handle(
                &viewing(&e, log),
                batch(id, Vec::new(), vec![raw(&call("emit", json!({})))]),
            )
            .expect("the batch decides"),
        )
    };
    let log = [opening.clone(), decide(&opening, "b1")].concat();
    let second = decide(&log, "b2");
    assert_eq!(e.validate_replay(&[log.clone(), second.clone()].concat()), Ok(()));

    let undeclared: Vec<Fact> = second
        .into_iter()
        .filter(|fact| !matches!(fact, Fact::BasisAdvanced { .. }))
        .collect();
    assert_eq!(
        e.validate_replay(&[log, undeclared].concat()),
        Err(TransitionRefusal::UndeclaredAdvance)
    );
}

#[test]
fn a_decision_releases_exactly_what_the_ordered_composition_allows() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decided = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                Vec::new(),
                vec![
                    raw(&call("quiet", json!({}))),
                    raw(&call("emit", json!({}))),
                    raw(&call("guard", json!({}))),
                ],
            ),
        )
        .expect("the batch decides"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

    let rewritten = |mutate: &dyn Fn(&mut Vec<DispatchId>)| {
        let mut facts = decided.clone();
        for fact in &mut facts {
            if let Fact::ProposalBatchDecided { released, .. } = fact {
                mutate(released);
            }
        }
        e.validate_replay(&[log.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|released| {
            released.pop();
        }),
        Err(TransitionRefusal::MisdecidedBatch)
    );
    assert_eq!(
        rewritten(&|released| released.swap(0, 1)),
        Err(TransitionRefusal::MisdecidedBatch)
    );
    assert_eq!(
        rewritten(&|released| {
            let first = released[0].clone();
            released.push(first);
        }),
        Err(TransitionRefusal::MisdecidedBatch)
    );
}

#[test]
fn replay_refuses_a_rewritten_provider_admission() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decided = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the provider ran it")],
                vec![raw(&call("quiet", json!({})))],
            ),
        )
        .expect("the batch decides"),
    );
    assert_eq!(e.validate_replay(&[log.clone(), decided.clone()].concat()), Ok(()));

    let rewritten = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = decided.clone();
        mutate(&mut facts[1]);
        e.validate_replay(&[log.clone(), facts].concat())
    };
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::ValueAdmitted { value, .. } = fact {
                value.label = Label::new(TRUSTED, Audience::restricted([ReaderId::new("forged")]));
            }
        }),
        Err(TransitionRefusal::ForgedLabel)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { effects, .. },
                ..
            } = fact
            {
                *effects = EffectSet::default();
            }
        }),
        Err(TransitionRefusal::EffectsMismatch)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { position, .. },
                ..
            } = fact
            {
                *position = 1;
            }
        }),
        Err(TransitionRefusal::WrongAdmissionPosition)
    );
    assert_eq!(
        rewritten(&|fact| {
            if let Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { tool, .. },
                ..
            } = fact
            {
                *tool = ToolName::new("quiet");
            }
        }),
        Err(TransitionRefusal::NotProviderRun("quiet".to_string()))
    );
    let admission_only = appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch("b2", vec![exposed("seen", "the provider ran it")], Vec::new()),
        )
        .expect("an admission-only batch decides"),
    );
    let mut reordered = admission_only.clone();
    reordered.swap(1, 2);
    assert_eq!(e.validate_replay(&[log.clone(), admission_only].concat()), Ok(()));
    assert_eq!(
        e.validate_replay(&[log, reordered].concat()),
        Err(TransitionRefusal::AdmissionAfterDecision)
    );
}

#[test]
fn a_batch_admits_every_result_it_exposed() {
    let e = batch_engine();
    let log = opening_log(&e);
    let decision = e
        .handle(
            &viewing(&e, &log),
            batch(
                "b1",
                vec![exposed("seen", "the first"), exposed("seen", "the second")],
                vec![raw(&call("wire", json!({})))],
            ),
        )
        .expect("the batch decides");
    let (released, _) = answered(&decision);
    assert_eq!(tool_names(released), ["wire"]);
    let facts = appended_facts(decision);
    let slots: Vec<u32> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted {
                provenance: Provenance::ProviderRun { position, .. },
                ..
            } => Some(*position),
            _ => None,
        })
        .collect();
    assert_eq!(slots, [0, 1]);
    assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));

    let child = TrajectoryId::new("child");
    let seeded = forked_child(&e, &log, &child);
    let forked = [log, seeded].concat();
    let decided = appended_facts(
        e.handle(
            &viewing(&e, &forked),
            batch(
                "b2",
                vec![exposed("seen", "the first"), exposed("seen", "the second")],
                Vec::new(),
            ),
        )
        .expect("the batch decides"),
    );
    assert_eq!(e.validate_replay(&[forked.clone(), decided.clone()].concat()), Ok(()));
    let mut elsewhere = decided;
    if let Fact::ValueAdmitted { trajectory, .. } = &mut elsewhere[2] {
        *trajectory = child;
    }
    assert_eq!(
        e.validate_replay(&[forked, elsewhere].concat()),
        Err(TransitionRefusal::ForeignAdmission)
    );
}
