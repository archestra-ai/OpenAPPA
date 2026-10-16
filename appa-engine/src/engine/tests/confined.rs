use super::*;

#[test]
fn a_bound_sanitizer_checkpoints_before_it_asks_for_the_derivation() {
    let redactor = crate::authority::Sanitizer {
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
    };
    let fetch = ToolAnnotation {
        delta: Delta {
            trust: Some(SUSPICIOUS),
            audience: None,
        },
        emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
        ..plain_tool("fetch")
    };
    let e = open_engine(RegistryConfig {
        sanitizers: vec![redactor],
        ..test_config(vec![fetch])
    });
    let call = call("fetch", json!({}));
    let (log, dispatch) = released_under_output_sanitizer(&e, vec![opened(&e)], &call);
    let view = e.view(&traj(), log.clone(), log.len() as u64).unwrap();

    let raw = ValueBody::new("page bytes");
    let source = crate::value::RawResultDigest::of(raw.as_str().as_bytes());
    let report = |evidence: Vec<crate::transition::Evidence>| ToolReport {
        dispatch: dispatch.clone(),
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available(raw.clone()),
        },
        evidence,
        offer_nonce: nonce(),
        audience: crate::audience::AudienceEvidence::default(),
    };

    let asked = e.handle(&view, EngineEvent::Outcome(report(Vec::new()))).unwrap();
    assert_eq!(
        asked.follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Resolve(
            crate::transition::EvidenceRequest::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redactor"),
                source,
                body: raw.clone(),
            }
        ))
    );
    let checkpoint = asked.append.expect("the effects commit before the external step");
    assert!(matches!(
        checkpoint.facts(),
        [
            Fact::BasisAdvanced { .. },
            Fact::DispatchSucceeded {
                observed: crate::fact::ObservedResult::Available(recorded),
                ..
            }
        ] if recorded == &source
    ));

    let checkpointed = e
        .view(&traj(), [log.clone(), checkpoint.facts().to_vec()].concat(), 3)
        .unwrap();
    let again = e
        .handle(&checkpointed, EngineEvent::Outcome(report(Vec::new())))
        .unwrap();
    assert_eq!(again.append, None);
    assert_eq!(again.follow_up, asked.follow_up);

    assert_eq!(
        e.handle(
            &checkpointed,
            EngineEvent::Outcome(ToolReport {
                dispatch: dispatch.clone(),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("other bytes")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::ObservationMismatch)
    );
    assert_eq!(
        e.handle(
            &checkpointed,
            EngineEvent::Outcome(ToolReport {
                dispatch: dispatch.clone(),
                outcome: ToolOutcome::Failure,
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::ContradictedSuccess)
    );

    let derived = ValueBody::new("page bytes, redacted");
    let crossed = e
        .handle(
            &checkpointed,
            EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redactor"),
                source,
                derived: derived.clone(),
            }])),
        )
        .expect("the derivation admits");
    assert_eq!(
        crossed.follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Closed {
            admitted: Some(derived),
            withheld: false,
        })
    );
    let facts = crossed.append.expect("the derivation appends").facts().to_vec();
    assert!(matches!(
        facts.as_slice(),
        [
            Fact::BasisAdvanced { .. },
            Fact::DispatchClosed {
                outcome: crate::fact::CloseOutcome::Success { effects },
                ..
            },
            Fact::CandidateDerived { .. },
            Fact::ValueAdmitted { .. }
        ] if effects.is_empty()
    ));

    let whole = [log.clone(), checkpoint.facts().to_vec(), facts.clone()].concat();
    assert_eq!(e.view(&traj(), whole.clone(), 4).map(|_| ()), Ok(()));
    assert_eq!(
        e.view(&traj(), whole[..whole.len() - 1].to_vec(), 4)
            .map(|_| ())
            .unwrap_err(),
        crate::transition::TransitionRefusal::UnadmittedDerivation
    );

    let settled = e.view(&traj(), whole, 4).unwrap();
    let derived = ValueBody::new("page bytes, redacted");
    assert_eq!(
        e.handle(
            &settled,
            EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redactor"),
                source,
                derived: derived.clone(),
            }])),
        )
        .expect("the same report answers from the record")
        .follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Closed {
            admitted: Some(derived),
            withheld: false,
        })
    );
    assert_eq!(
        e.handle(
            &settled,
            EngineEvent::Outcome(ToolReport {
                dispatch: dispatch.clone(),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("other bytes")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::ObservationMismatch)
    );
    assert_eq!(
        e.handle(
            &settled,
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Failure,
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::ContradictedSuccess)
    );
}

/// `fetch` lowers trust and confines its result; `redactor` raises trust only. `taint` is a
/// provider-run result that narrows the audience. The trajectory starts trusted and public.
fn file_redacting_engine() -> Engine {
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
        ..test_config(vec![
            read_tool(
                "fetch",
                Delta {
                    trust: Some(SUSPICIOUS),
                    audience: None,
                },
            ),
            restrictable_tool("taint"),
        ])
    };
    let mut profile = crate::profile::ProfileDeclaration {
        starting_label: Label::new(TRUSTED, Audience::public()),
        ..crate::profile::covering_declaration(&cfg)
    };
    profile
        .executor_exceptions
        .insert(ToolName::new("taint"), crate::profile::ExecutorClass::ProviderRun);
    profile.confined_results.remove(&ToolName::new("taint"));
    Engine::open(DeploymentPolicy {
        registry: cfg,
        planner_cap: crate::registry::PlannerCap::default(),
        dialect: PolicyDialectVersion::new(1),
        profile,
    })
    .unwrap()
}

fn insider_file() -> crate::value::FileSource {
    crate::value::FileSource {
        version: "v1".into(),
        digest: "content".into(),
        label: Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    }
}

/// `fetch` over `basis` blocks, the agent binds `redactor` to its result, and the approved
/// call releases: the log, and the released dispatch.
fn released_file_fetch(e: &Engine, basis: &crate::value::FileBasis) -> (Vec<Fact>, DispatchId) {
    let fetch = call("fetch", json!({}));
    let proposed = |log: &[Fact], id: &str| {
        appended_facts(
            e.handle(
                &viewing(e, log),
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new(id),
                    trajectory: traj(),
                    provider_results: Vec::new(),
                    proposals: vec![raw(&fetch)],
                    spawn: None,
                    offer_nonce: nonce(),
                    evidence: vec![Evidence::File {
                        position: 0,
                        basis: basis.clone(),
                    }],
                    audience: crate::audience::AudienceEvidence::default(),
                }),
            )
            .expect("the batch decides"),
        )
    };
    let mut log = vec![opened(e)];
    let blocked = proposed(&log, "b1");
    let (offer, _) = opened_offers(&blocked)
        .into_iter()
        .find(|(_, plan)| plan.sanitizer().is_some())
        .expect("the confined result offers its output sanitizer");
    log.extend(blocked);
    log.extend(appended_facts(
        execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"),
    ));
    let released = proposed(&log, "b2");
    let dispatch = released
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
            _ => None,
        })
        .expect("the approved call releases");
    log.extend(released);
    (log, dispatch)
}

/// The result's raw bytes checkpoint, then `redactor`'s derivation of them is reported: the
/// log through the checkpoint, and the derivation's decision.
fn redacted_result(e: &Engine, mut log: Vec<Fact>, dispatch: &DispatchId) -> (Vec<Fact>, EngineDecision) {
    let raw = ValueBody::new("file bytes");
    let report = |evidence: Vec<Evidence>| {
        EngineEvent::Outcome(ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(raw.clone()),
            },
            evidence,
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };
    log.extend(appended_facts(
        e.handle(&viewing(e, &log), report(Vec::new()))
            .expect("the bound result asks for its derivation"),
    ));
    let derived = e
        .handle(
            &viewing(e, &log),
            report(vec![Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redactor"),
                source: crate::value::RawResultDigest::of(raw.as_str().as_bytes()),
                derived: ValueBody::new("file bytes, redacted"),
            }]),
        )
        .expect("the derivation decides");
    (log, derived)
}

fn derived_result_label(facts: &[Fact]) -> Label {
    facts
        .iter()
        .find_map(|fact| match fact {
            Fact::CandidateDerived {
                derived: DerivedCandidate::Result { value, .. },
                ..
            } => Some(value.label.clone()),
            _ => None,
        })
        .expect("the derivation records its candidate")
}

/// A trust-only output sanitizer on a file read derives from what the read returns, file
/// label included: the file's audience survives the derivation, and the result reaches the
/// trajectory only through an acceptance of exactly that audience.
#[test]
fn a_sanitized_file_read_keeps_the_files_audience() {
    let e = file_redacting_engine();
    let insider = Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")]));
    let (log, dispatch) = released_file_fetch(&e, &crate::value::FileBasis::Read(insider_file()));
    assert!(log.iter().any(|fact| matches!(
        fact,
        Fact::OutputSanitizerBound { contribution, .. } if contribution == &insider
    )));

    let (log, derived) = redacted_result(&e, log, &dispatch);
    let confined = confined_of(&derived.follow_up).clone();
    let facts = appended_facts(derived);
    assert_eq!(derived_result_label(&facts), insider);
    assert_eq!(confined.residual.to, insider);
    assert!(!facts.iter().any(|fact| matches!(fact, Fact::ValueAdmitted { .. })));
    let log = [log, facts].concat();

    let accept = confined.offers.last().expect("the stage offers its acceptance").0;
    let log = [
        log.clone(),
        appended_facts(execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("it crosses")),
    ]
    .concat();
    assert_eq!(
        viewing(&e, &log)
            .views(&traj())
            .expect("the trajectory is opened")
            .current_label(),
        insider
    );
    assert_eq!(e.validate_replay(&log), Ok(()));
}

/// An edit's result label reads the label its dispatch received. An admission between the
/// release and the result moves the trajectory, not that bound, so the live derivation and
/// its replay both read the bound.
#[test]
fn a_sanitized_file_edit_derives_from_the_bound_its_dispatch_received() {
    let e = file_redacting_engine();
    let (mut log, dispatch) = released_file_fetch(&e, &crate::value::FileBasis::Edit(insider_file()));
    log.extend(appended_facts(
        e.handle(
            &viewing(&e, &log),
            batch("b3", vec![exposed("taint", "news")], Vec::new()),
        )
        .expect("the provider-run result admits"),
    ));
    assert_eq!(
        viewing(&e, &log)
            .views(&traj())
            .expect("the trajectory is opened")
            .current_label()
            .audience,
        Audience::restricted([ReaderId::new("reviewer")])
    );

    let (log, derived) = redacted_result(&e, log, &dispatch);
    let facts = appended_facts(derived);
    assert_eq!(
        derived_result_label(&facts),
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")]))
    );
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

/// A log that bound a file read's sanitizer, or derived its first hop, from the tool's
/// declared output alone dropped the file's audience; replay refuses both records.
#[test]
fn a_file_derivation_recorded_without_the_files_label_is_refused() {
    let e = file_redacting_engine();
    let declared_only = Label::new(TRUSTED, Audience::public());
    let (log, dispatch) = released_file_fetch(&e, &crate::value::FileBasis::Read(insider_file()));
    let mut forged = log.clone();
    for fact in forged.iter_mut() {
        if let Fact::OutputSanitizerBound { contribution, .. } = fact {
            *contribution = declared_only.clone();
        }
    }
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::UnbackedApproval));

    let (log, derived) = redacted_result(&e, log, &dispatch);
    let mut facts = appended_facts(derived);
    let at = facts
        .iter()
        .position(|fact| matches!(fact, Fact::CandidateDerived { .. }))
        .expect("the derivation records its candidate");
    facts.truncate(at + 1);
    if let Some(Fact::CandidateDerived {
        derived: DerivedCandidate::Result { value, residual, .. },
        ..
    }) = facts.last_mut()
    {
        value.label = declared_only.clone();
        *residual = None;
    }
    assert_eq!(
        e.validate_replay(&[log, facts].concat()),
        Err(TransitionRefusal::ForgedLabel)
    );
}

fn confined_of(follow_up: &FollowUp) -> &Confined {
    match follow_up {
        FollowUp::Outcome(OutcomeFollowUp::Staged(confined)) | FollowUp::Offer(OfferFollowUp::Staged(confined)) => {
            confined
        }
        other => panic!("a residual-bearing derivation stages, got {other:?}"),
    }
}

#[test]
fn a_narrowing_derivation_stages_its_candidate_instead_of_admitting_it() {
    let e = staged_engine();
    let (log, dispatch, derived, staged) = staged_candidate(&e);
    let confined = confined_of(&staged.follow_up);
    assert_eq!(confined.dispatch, dispatch);
    assert_eq!(confined.candidate.body, derived);
    assert_eq!(
        confined.residual,
        crate::check::Narrowing {
            from: Label::new(Trust::new(2), Audience::public()),
            to: Label::new(Trust::new(1), Audience::public()),
        }
    );
    let facts = appended_facts(staged.clone());
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ValueAdmitted { .. } | Fact::DispatchClosed { .. })),
        "a staged candidate admits nothing and closes nothing: {facts:?}"
    );
    let stage: Vec<_> = opened_offers(&facts).into_iter().map(|(_, plan)| plan).collect();
    assert_eq!(
        stage
            .iter()
            .filter_map(plan::ExecutableRemedyPlan::hop)
            .collect::<Vec<_>>(),
        vec![&crate::names::SanitizerName::new("scrubber")],
        "the stage offers the sanitizer that only became applicable once the first hop widened the candidate"
    );
    assert_eq!(
        stage
            .iter()
            .filter_map(plan::ExecutableRemedyPlan::narrowing)
            .collect::<Vec<_>>(),
        vec![&confined.residual],
        "and acceptance of exactly the residual, never a guess made before the derivation existed"
    );

    let log = [log, facts].concat();
    let again = e
        .handle(
            &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("page bytes")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the repeat hears the stage");
    assert_eq!(again.append, None);
    assert_eq!(confined_of(&again.follow_up), confined);
}

#[test]
fn a_spend_of_an_offer_the_log_never_accepted_is_refused() {
    let without_acceptance = |log: &[Fact], batch: Vec<Fact>| {
        let kept: Vec<Fact> = batch
            .into_iter()
            .filter(|fact| !matches!(fact, Fact::OfferAccepted { .. }))
            .collect();
        [log.to_vec(), kept].concat()
    };

    let e = staged_engine();
    let (log, _, derived, staged) = staged_candidate(&e);
    let confined = confined_of(&staged.follow_up).clone();
    let log = [log, appended_facts(staged)].concat();
    let hop = confined.offers[0].0;
    let accept = confined.offers[1].0;

    let hopped = appended_facts(
        execute_offer(
            &e,
            &log,
            hop,
            OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("scrubber"),
                source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
                derived: ValueBody::new("page bytes, redacted, scrubbed"),
            }),
        )
        .expect("the hop runs"),
    );
    assert_eq!(
        e.validate_replay(&without_acceptance(&log, hopped)),
        Err(crate::transition::TransitionRefusal::OfferEnded),
        "a confined hop's successor stands on the acceptance that authorised it"
    );

    let accepted = appended_facts(
        execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("the acceptance runs"),
    );
    assert_eq!(
        e.validate_replay(&without_acceptance(&log, accepted)),
        Err(crate::transition::TransitionRefusal::OfferEnded),
        "a candidate crosses at a cost the agent accepted, never one the log asserts"
    );

    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let offers = opened_offers(&facts);
    let log = [log, facts].concat();
    let substituted =
        appended_facts(execute_offer(&e, &log, offers[0].0, substitution(&proposal, REDACTED)).expect("the hop runs"));
    assert_eq!(
        e.validate_replay(&without_acceptance(&log, substituted)),
        Err(crate::transition::TransitionRefusal::OfferEnded),
        "a substituted candidate stands on the acceptance that authorised it"
    );
}

#[test]
fn offer_consults_names_the_external_work_each_offer_needs() {
    use crate::transition::{OfferConsult, OfferOutcome};

    let e = staged_engine();
    let (log, _dispatch, derived, staged) = staged_candidate(&e);
    let confined = confined_of(&staged.follow_up).clone();
    let log = [log, appended_facts(staged)].concat();
    let hop = confined.offers[0].0;
    let accept = confined.offers[1].0;

    assert_eq!(
        e.offer_consults(&viewing(&e, &log), &traj(), &hop),
        Ok(OfferConsult::Sanitizer {
            sanitizer: crate::names::SanitizerName::new("scrubber"),
            source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
            body: derived.clone(),
            tool: Some(ToolName::new("fetch")),
        }),
    );
    assert_eq!(
        e.offer_consults(&viewing(&e, &log), &traj(), &accept),
        Ok(OfferConsult::Accept { sanitizer: None }),
    );

    let crossed = [
        log.clone(),
        appended_facts(execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("it crosses")),
    ]
    .concat();
    assert_eq!(
        e.offer_consults(&viewing(&e, &crossed), &traj(), &accept),
        Ok(OfferConsult::Replay(OfferOutcome::Approved(Vec::new()))),
    );

    let e = substituting_engine(TRUSTED);
    let proposal = call("post", json!({ "body": "ssn 123" }));
    let log = internal_log(&e);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let input_hop = opened_offers(&facts)[0].0;
    let log = [log, facts].concat();
    assert_eq!(
        e.offer_consults(&viewing(&e, &log), &traj(), &input_hop),
        Ok(OfferConsult::Rewrite {
            sanitizer: crate::names::SanitizerName::new("redact"),
            call: proposal.clone(),
        }),
    );

    // A spent input hop answers from the record as the rewrite it was, and only for one.
    let hopped =
        appended_facts(execute_offer(&e, &log, input_hop, substitution(&proposal, REDACTED)).expect("the hop runs"));
    let spent = [log, hopped].concat();
    assert_eq!(
        e.offer_consults(&viewing(&e, &spent), &traj(), &input_hop),
        Ok(OfferConsult::Replay(OfferOutcome::Derived(Evidence::Rewrite {
            sanitizer: crate::names::SanitizerName::new("redact"),
            source: crate::value::RawResultDigest::of(&[]),
            derived: ValueBody::new(""),
            annotation: None,
        }))),
    );
    assert_eq!(
        execute_offer(
            &e,
            &spent,
            input_hop,
            OfferOutcome::Derived(Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redact"),
                source: crate::value::RawResultDigest::of(&[]),
                derived: ValueBody::new(""),
            }),
        )
        .err(),
        Some(TransitionError::PlanOutcomeMismatch),
        "an output derivation names another kind of offer"
    );
}

#[test]
fn a_confined_stage_is_left_by_a_hop_or_by_accepting_its_residual() {
    let e = staged_engine();
    let (log, dispatch, derived, staged) = staged_candidate(&e);
    let confined = confined_of(&staged.follow_up).clone();
    let log = [log, appended_facts(staged)].concat();
    let plan_of = |offer: &crate::value::OfferId| {
        confined
            .offers
            .iter()
            .find(|(id, _)| id == offer)
            .map(|(_, plan)| *plan)
            .expect("the stage's own offer")
    };
    let scrubbed = ValueBody::new("page bytes, redacted, scrubbed");
    let hop = confined.offers[0].0;
    let accept = confined.offers[1].0;
    assert_ne!(plan_of(&hop), plan_of(&accept));

    assert_eq!(
        execute_offer(
            &e,
            &log,
            hop,
            OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("scrubber"),
                source: crate::value::RawResultDigest::of(b"page bytes"),
                derived: scrubbed.clone(),
            })
        ),
        Err(TransitionError::EvidenceMismatch),
        "a derivation of the raw bytes is not a derivation of the candidate standing on them"
    );
    assert_eq!(
        execute_offer(&e, &log, hop, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::PlanOutcomeMismatch)
    );

    let hopped = execute_offer(
        &e,
        &log,
        hop,
        OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
            sanitizer: crate::names::SanitizerName::new("scrubber"),
            source: crate::value::RawResultDigest::of(derived.as_str().as_bytes()),
            derived: scrubbed.clone(),
        }),
    )
    .expect("the hop runs");
    assert_eq!(
        offer_answer(&hopped),
        &OfferFollowUp::Admitted {
            value: scrubbed.clone()
        }
    );
    let hopped = appended_facts(hopped);
    assert!(
        hopped
            .iter()
            .any(|fact| matches!(fact, Fact::OfferInvalidated { offer, .. } if offer == &accept)),
        "taking the candidate ends every other offer standing on it: {hopped:?}"
    );
    let after = e
        .view(&traj(), [log.clone(), hopped].concat(), (log.len() + 8) as u64)
        .expect("the hop's batch replays");
    assert_eq!(
        after.projection().view(&traj()).current_label(),
        Label::new(Trust::new(2), Audience::public()),
        "a candidate that narrows nothing costs the trajectory nothing"
    );
    assert!(
        after
            .projection()
            .view(&traj())
            .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
            .is_none(),
        "an admitted candidate leaves the staging model"
    );

    let accepted = execute_offer(&e, &log, accept, OfferOutcome::Approved(Vec::new())).expect("the acceptance runs");
    assert_eq!(offer_answer(&accepted), &OfferFollowUp::Admitted { value: derived });
    let accepted = appended_facts(accepted);
    assert!(
        accepted.iter().any(|fact| matches!(
            fact,
            Fact::CandidateAccepted { narrowing, .. } if narrowing == &confined.residual
        )),
        "the acceptance names exactly the residual the candidate owed: {accepted:?}"
    );
    let after = e
        .view(
            &traj(),
            [log.clone(), accepted.clone()].concat(),
            (log.len() + 8) as u64,
        )
        .expect("the acceptance's batch replays");
    assert_eq!(
        after.projection().view(&traj()).current_label(),
        Label::new(Trust::new(1), Audience::public())
    );
    assert!(
        after
            .projection()
            .view(&traj())
            .candidate(&crate::basis::SubjectKey::ConfinedResult(dispatch.clone()))
            .is_none(),
        "accepting the residual ends the stage as surely as hopping past it"
    );
    assert_eq!(
        e.view(
            &traj(),
            [log.clone(), accepted[..accepted.len() - 1].to_vec()].concat(),
            (log.len() + 8) as u64,
        )
        .map(|_| ())
        .unwrap_err(),
        crate::transition::TransitionRefusal::UnadmittedDerivation
    );
    let settled = [log, accepted].concat();
    assert_eq!(
        offer_answer(
            &execute_offer(&e, &settled, accept, OfferOutcome::Approved(Vec::new())).expect("the repeat answers")
        ),
        &OfferFollowUp::Admitted {
            value: ValueBody::new("page bytes, redacted")
        }
    );
    assert_eq!(
        execute_offer(
            &e,
            &settled,
            accept,
            OfferOutcome::Derived(crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("scrubber"),
                source: crate::value::RawResultDigest::of(b"page bytes, redacted"),
                derived: scrubbed,
            })
        )
        .map(|_| ()),
        Err(TransitionError::PlanOutcomeMismatch)
    );
    assert_eq!(
        execute_offer(
            &e,
            &settled,
            accept,
            OfferOutcome::Approved(vec![stray_evidence(accept)])
        )
        .map(|_| ()),
        Err(TransitionError::PlanOutcomeMismatch),
        "an acceptance carrying evidence is refused after the offer ends, as it is before"
    );
}

#[test]
fn a_confined_stage_goes_stale_with_its_basis_and_is_planned_again() {
    let e = staged_engine();
    let (log, dispatch, _, staged) = staged_candidate(&e);
    let confined = confined_of(&staged.follow_up).clone();
    let mut log = [log, appended_facts(staged)].concat();
    reads(&e, &mut log, &traj(), "ping");
    assert_eq!(
        execute_offer(&e, &log, confined.offers[1].0, OfferOutcome::Approved(Vec::new())),
        Err(TransitionError::StaleOffer)
    );

    let replanned = e
        .handle(
            &e.view(&traj(), log.clone(), log.len() as u64).unwrap(),
            EngineEvent::Outcome(ToolReport {
                dispatch,
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("page bytes")),
                },
                evidence: Vec::new(),
                offer_nonce: crate::value::OfferNonce::new([13u8; 32]),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the candidate is planned again");
    let fresh = confined_of(&replanned.follow_up);
    assert_eq!(fresh.candidate, confined.candidate, "the candidate itself is durable");
    assert_eq!(fresh.residual, confined.residual);
    assert!(
        fresh
            .offers
            .iter()
            .all(|(offer, _)| !confined.offers.iter().any(|(stale, _)| stale == offer)),
        "a stale offer never revives: the new stage carries identities of its own"
    );
    let fresh = fresh.offers[1].0;
    let log = [log, appended_facts(replanned)].concat();
    assert!(matches!(
        offer_answer(
            &execute_offer(&e, &log, fresh, OfferOutcome::Approved(Vec::new())).expect("the fresh offer runs")
        ),
        OfferFollowUp::Admitted { .. }
    ));
}

/// Authority evidence an `Approved` outcome may not carry: a plan that assigns no
/// authority is accepted only with an empty vector, so any entry is a mismatch.
fn stray_evidence(offer: crate::value::OfferId) -> crate::execute::AuthorityEvidence {
    crate::execute::AuthorityEvidence {
        offer,
        authority: crate::names::AuthorityName::new("officer"),
        covers: Vec::new(),
        reviewed: crate::execute::AuthorityReview {
            tool: ToolName::new("anything"),
            trajectory_label: Label::new(Trust::new(0), Audience::public()),
        },
    }
}

/// A bound output sanitizer derives from a success's bytes; a failure's body has no
/// derivation to cross through, so it closes the dispatch without entering the trajectory.
#[test]
fn a_failure_body_under_a_bound_sanitizer_closes_withheld() {
    let e = file_redacting_engine();
    let (log, dispatch) = released_file_fetch(&e, &crate::value::FileBasis::Read(insider_file()));
    let body = ValueBody::new("ENOENT");
    let report = || {
        EngineEvent::Outcome(ToolReport {
            dispatch: dispatch.clone(),
            outcome: ToolOutcome::FailureWithBody { body: body.clone() },
            evidence: Vec::new(),
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };
    let withheld = FollowUp::Outcome(OutcomeFollowUp::Closed {
        admitted: None,
        withheld: true,
    });

    let closed = e.handle(&viewing(&e, &log), report()).expect("the failure closes");
    assert_eq!(closed.follow_up, withheld);
    let facts = appended_facts(closed);
    assert!(facts.iter().any(|fact| matches!(
        fact,
        Fact::DispatchClosed {
            outcome: crate::fact::CloseOutcome::FailureWithBody { observed },
            ..
        } if observed == &crate::value::RawResultDigest::of(body.as_str().as_bytes())
    )));
    assert!(!facts.iter().any(|fact| matches!(fact, Fact::ValueAdmitted { .. })));
    let log = [log, facts].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let again = e.handle(&viewing(&e, &log), report()).expect("the repeat answers");
    assert_eq!((again.append, again.follow_up), (None, withheld));
}
