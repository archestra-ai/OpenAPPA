use super::*;

fn audience_engine(authorities: Vec<crate::authority::Authority>, starting: Label) -> Engine {
    let mut send = plain_tool("send");
    send.parameters = crate::params::test_string_argument_schema("to");
    send.requires = Requires {
        label: LabelRequirements {
            trust_floor: None,
            audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
        },
        ..Requires::default()
    };
    open_engine_at(
        RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(vec![send]),
            authorities,
            sanitizers: vec![],
            audience: slack_groups(&["team", "wide", "nobody"]),
        },
        starting,
    )
}

fn send_to(to: &str) -> crate::transition::ProposedCall {
    raw(&call("send", json!({ "to": to })))
}

fn evidenced_batch(
    id: &str,
    proposals: Vec<crate::transition::ProposedCall>,
    audience: crate::audience::AudienceEvidence,
) -> EngineEvent {
    EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new(id),
        trajectory: traj(),
        provider_results: Vec::new(),
        proposals,
        spawn: None,
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience,
    })
}

fn chain_audience(level: crate::label::ChainAudience) -> DeclaredAudience {
    DeclaredAudience::Union(crate::label::Clause::new([level], [], []).expect("a chain clause names no reader"))
}

/// A confined `read` narrowing to `self` beside the output sanitizer `mask` moving
/// `from` to `internal`; `groups` are the named audiences the policy declares.
fn masking_config(from: DeclaredAudience, groups: &[&str]) -> RegistryConfig {
    let mut read = plain_tool("read");
    read.delta = Delta {
        trust: None,
        audience: Some(crate::contract::DeltaAudience::Static(chain_audience(
            crate::label::ChainAudience::Self_,
        ))),
    };
    RegistryConfig {
        annotators: vec![],
        trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
        tools: declared(vec![read]),
        authorities: vec![],
        sanitizers: vec![crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("mask"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: from,
                to: chain_audience(crate::label::ChainAudience::Internal),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }],
        audience: if groups.is_empty() {
            crate::audience::AudienceConfig::default()
        } else {
            slack_groups(groups)
        },
    }
}

fn internal_session() -> Label {
    known(
        TRUSTED,
        Audience::of_declared(&chain_audience(crate::label::ChainAudience::Internal)),
    )
}

#[test]
fn a_chain_only_output_sanitizer_is_offered_and_admitted_without_membership() {
    let e = open_engine_at(
        masking_config(chain_audience(crate::label::ChainAudience::Self_), &[]),
        internal_session(),
    );
    let call = call("read", json!({}));
    // No audience source, no answers: the block offers the sanitize settlement, the offer
    // executes and the release opens the dispatch.
    let (log, dispatch) = released_under_output_sanitizer(&e, vec![opened(&e)], &call);

    let raw = ValueBody::new("TOKEN=abc");
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
    let asked = e
        .handle(&viewing(&e, &log), EngineEvent::Outcome(report(Vec::new())))
        .expect("the bound sanitizer asks for its derivation, not for members");
    let log = [log, asked.append.expect("the effects commit first").facts().to_vec()].concat();

    let derived = ValueBody::new("TOKEN=***");
    let crossed = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("mask"),
                source,
                derived: derived.clone(),
            }])),
        )
        .expect("`self ⊇ self` derives from the chain: the derivation admits without a member list");
    assert_eq!(
        crossed.follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Closed {
            admitted: Some(derived),
            withheld: false,
        })
    );
    let log = [log, crossed.append.expect("the derivation appends").facts().to_vec()].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn a_group_from_output_sanitizer_still_asks_for_exactly_the_members_its_derivation_needs() {
    let e = open_engine_at(masking_config(group_audience("team"), &["team"]), internal_session());
    let call = call("read", json!({}));
    let log = vec![opened(&e)];
    assert_eq!(
        proposed(&e, &log, "b1", nonce(), call.clone()),
        Err(TransitionError::MembershipNeeded {
            needed: vec![group_atom("team")]
        }),
        "`self ⊇ @team` derives from no declared fact, so the enumeration asks for the group"
    );
    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b2",
                vec![raw(&call)],
                source_evidence(vec![user_group("team", vec![])])
            )
        ),
        Err(TransitionError::MembershipNeeded {
            needed: vec![crate::label::SymbolicAtom::Chain(crate::label::ChainAudience::Self_)]
        }),
        "with the group answered, the exact comparison needs `self`'s members next — one ask at a time"
    );
}

fn public_ceiling_officer() -> crate::authority::Authority {
    crate::authority::Authority {
        name: AuthorityName::new("officer"),
        mandate: crate::authority::Mandate {
            reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
            ..crate::authority::Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    }
}

#[test]
fn a_public_placeholder_argument_is_the_public_audience() {
    let auditor = || Audience::restricted([ReaderId::new("auditor")]);
    for (case, authorities, label, released, offers) in [
        (
            "no authority holds a reader ceiling, so nothing covers a Public recipient",
            vec![],
            auditor(),
            vec![],
            0,
        ),
        ("a public label releases", vec![], Audience::public(), vec!["send"], 0),
        (
            "a public reader ceiling offers one ruling plan",
            vec![public_ceiling_officer()],
            auditor(),
            vec![],
            1,
        ),
    ] {
        let e = audience_engine(authorities, known(TRUSTED, label));
        let log = vec![opened(&e)];
        let decision = e
            .handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![send_to("public")]))
            .expect("the batch decides");
        let (released_calls, blocked) = answered(&decision);
        assert_eq!(tool_names(released_calls), released, "{case}");
        match blocked {
            [] => assert_eq!(offers, 0, "{case}"),
            [block] => {
                assert_eq!(
                    block.block.raw.requirement_gaps,
                    vec![crate::check::Gap::Includes {
                        recipients: DeclaredAudience::Public
                    }],
                    "{case}"
                );
                assert_eq!(block.offers.len(), offers, "{case}");
            }
            more => panic!("{case}: one proposal blocks at most once, got {more:?}"),
        }
    }
}

#[test]
fn a_group_placeholder_reads_the_acts_pinned_answer() {
    let e = audience_engine(
        vec![],
        known(
            TRUSTED,
            Audience::restricted([corp_reader("alice"), corp_reader("bob")]),
        ),
    );
    let log = vec![opened(&e)];
    let evidence = source_evidence(vec![
        user_group("team", vec![slack_member("slack:UA", Some("alice@corp.com"))]),
        user_group(
            "wide",
            vec![
                slack_member("slack:UA", Some("alice@corp.com")),
                slack_member("slack:UC", Some("carol@other.com")),
            ],
        ),
        user_group("nobody", vec![]),
    ]);
    let decision = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b1",
                vec![send_to("@team"), send_to("@wide"), send_to("@nobody")],
                evidence.clone(),
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert_eq!(
        tool_names(released),
        ["send", "send"],
        "a covered group and an empty one release"
    );
    assert_eq!(blocked.len(), 1);
    assert_eq!(
        blocked[0].block.raw.requirement_gaps,
        vec![crate::check::Gap::Includes {
            recipients: group_audience("wide")
        }],
        "the gap names the audience symbolically, never a resolved reader list"
    );
    let facts = appended_facts(decision);
    for fact in &facts {
        if let Fact::DispatchOpened { evidence: pinned, .. } = fact {
            assert_eq!(pinned, &evidence, "each opening pins the act's answers");
        }
    }
    assert!(facts.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

#[test]
fn an_unanswered_group_refuses_the_batch_with_no_fact() {
    let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let needed = e.handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![send_to("@team")]));
    assert_eq!(
        needed,
        Err(TransitionError::MembershipNeeded {
            needed: vec![group_atom("team")]
        })
    );

    // An engine whose policy registers no audience source still asks — the atom is the
    // engine's question — and routing the ask is where the runtime fails operationally.
    let unregistered = engine_at(
        vec![{
            let mut send = plain_tool("send");
            send.parameters = crate::params::test_string_argument_schema("to");
            send.requires = Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            };
            send
        }],
        known(TRUSTED, Audience::restricted([corp_reader("alice")])),
    );
    assert!(matches!(
        unregistered.handle(
            &viewing(&unregistered, &[opened(&unregistered)]),
            batch("b1", Vec::new(), vec![send_to("@team")])
        ),
        Err(TransitionError::MembershipNeeded { .. })
    ));
    assert!(matches!(
        unregistered
            .registry
            .audience()
            .needed_primitives(&[group_atom("team")], None),
        Err(crate::audience::Unroutable::UnknownGroup(_))
    ));

    let decision = e
        .handle(&viewing(&e, &log), batch("b4", Vec::new(), vec![send_to("@")]))
        .expect("a malformed spelling still decides");
    assert!(answered(&decision).0.is_empty());
}

#[test]
fn evidence_is_scoped_to_the_asked_atoms_and_the_registered_sources() {
    let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];

    // Evidence outside the registered sources is refused, not ignored.
    let foreign = source_evidence(vec![crate::audience::SourceClaims {
        provider: "github".to_string(),
        selector: "org/x/members".to_string(),
        members: vec![],
    }]);
    assert!(matches!(
        e.handle(
            &viewing(&e, &log),
            evidenced_batch("b2", vec![send_to("@team")], foreign)
        ),
        Err(TransitionError::ForeignEvidence(
            crate::audience::EvidenceRefusal::UnroutableSelector { .. }
        ))
    ));

    // Routable answers beyond the asked atoms are refused, not carried along: evidence
    // is scoped to the operation's own asks and inherited pins, never pre-loaded.
    let surplus = source_evidence(vec![
        user_group("team", vec![slack_member("slack:UA", Some("alice@corp.com"))]),
        user_group("nobody", vec![]),
    ]);
    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            evidenced_batch("b3", vec![send_to("@team")], surplus),
        ),
        Err(TransitionError::Invalid(TransitionRefusal::UnrequestedEvidence {
            entry: "source slack:user-group/nobody".to_string()
        }))
    );
    let exact = source_evidence(vec![user_group(
        "team",
        vec![slack_member("slack:UA", Some("alice@corp.com"))],
    )]);
    let decision = e
        .handle(&viewing(&e, &log), evidenced_batch("b3", vec![send_to("@team")], exact))
        .expect("the asked answers decide");
    assert_eq!(tool_names(answered(&decision).0), ["send"]);
}

#[test]
fn replay_refuses_pinned_evidence_no_operation_requested() {
    let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let exact = source_evidence(vec![user_group(
        "team",
        vec![slack_member("slack:UA", Some("alice@corp.com"))],
    )]);
    let decision = e
        .handle(&viewing(&e, &log), evidenced_batch("b1", vec![send_to("@team")], exact))
        .expect("the asked answers decide");
    let mut facts = appended_facts(decision);
    assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));
    // Tamper: a routable-but-unrequested answer smuggled consistently into every pinned
    // evidence field of the act, so only the operation-scope audit can catch it.
    for fact in &mut facts {
        match fact {
            Fact::ProposalBatchDecided { evidence, .. } | Fact::DispatchOpened { evidence, .. } => {
                evidence.sources.push(user_group("nobody", vec![]));
            }
            _ => {}
        }
    }
    assert_eq!(
        e.validate_replay(&[log, facts].concat()),
        Err(TransitionRefusal::UnrequestedEvidence {
            entry: "source slack:user-group/nobody".to_string()
        })
    );
}

#[test]
fn audience_evidence_is_batch_payload() {
    let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let team = |member: ReaderId| source_evidence(vec![user_group("team", vec![member])]);
    let first = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b1",
                vec![send_to("@team")],
                team(slack_member("slack:UA", Some("alice@corp.com"))),
            ),
        )
        .expect("the batch decides");
    let ran = answered(&first).0[0].dispatch.clone();
    let log = [log, appended_facts(first)].concat();
    let repeat = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b1",
                vec![send_to("@team")],
                team(slack_member("slack:UA", Some("alice@corp.com"))),
            ),
        )
        .expect("the repeat answers");
    assert_eq!(repeat.append, None);
    assert_eq!(answered(&repeat).0[0].dispatch, ran);
    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b1",
                vec![send_to("@team")],
                team(slack_member("slack:UB", Some("bob@corp.com"))),
            ),
        ),
        Err(TransitionError::BatchIdentityConflict)
    );
}

#[test]
fn replay_refuses_tampered_audience_evidence() {
    let e = audience_engine(vec![], known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let records = vec![opened(&e)];
    let decision = e
        .handle(
            &viewing(&e, &records),
            evidenced_batch(
                "b1",
                vec![send_to("@team")],
                source_evidence(vec![user_group(
                    "team",
                    vec![slack_member("slack:UA", Some("alice@corp.com"))],
                )]),
            ),
        )
        .expect("the batch decides");
    let facts = appended_facts(decision);
    assert_eq!(e.validate_replay(&[records.clone(), facts.clone()].concat()), Ok(()));
    let tampered = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = facts.clone();
        for fact in &mut facts {
            mutate(fact);
        }
        e.validate_replay(&[records.clone(), facts].concat())
    };
    // An opening whose pins differ from what its act consumed is forged.
    assert_eq!(
        tampered(&|fact| {
            if let Fact::DispatchOpened { evidence, .. } = fact {
                *evidence = crate::audience::AudienceEvidence::default();
            }
        }),
        Err(TransitionRefusal::ForgedEvidence)
    );
    // A decision recorded under answers that cannot back it is refused.
    assert_eq!(
        tampered(&|fact| {
            if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                *evidence = source_evidence(vec![user_group(
                    "team",
                    vec![slack_member("slack:UB", Some("bob@corp.com"))],
                )]);
            }
        }),
        Err(TransitionRefusal::MisdecidedBatch),
        "the recorded release does not follow from the substituted answer"
    );
    assert_eq!(
        tampered(&|fact| {
            if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                *evidence = crate::audience::AudienceEvidence::default();
            }
        }),
        Err(TransitionRefusal::UnansweredDecision {
            needed: vec![group_atom("team")]
        }),
        "a decision over an unanswered symbolic audience claims answers it does not pin"
    );
}

/// A symbolic audience an annotation produces is the act's membership question, exactly
/// as a declaration writing it: the check asks for the atoms, decides under the pinned
/// answers, and replay reads the pin — a record without the answers, or with answers the
/// decision does not follow from, is refused.
#[test]
fn a_produced_symbolic_audience_reads_membership_per_act_and_replays_from_the_pin() {
    let notify = plain_tool("notify");
    let mut cfg = test_config(vec![]);
    cfg.tools.push(annotated(notify.clone(), "acl"));
    cfg.annotators.push(annotator("acl"));
    cfg.audience = slack_groups(&["team"]);
    let e = open_engine_at(cfg, known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let requiring = {
        let mut produced = notify.clone();
        produced.requires.label.audience = vec![AudienceRequirement::Includes(RecipientSpec::Static(group_audience(
            "team",
        )))];
        let unpinned = call("notify", json!({}));
        raw(&unpinned
            .clone()
            .with_annotation(Some(pinned_for(produced, "acl", &unpinned))))
    };
    let batch_with =
        |evidence: crate::audience::AudienceEvidence| evidenced_batch("b1", vec![requiring.clone()], evidence);

    assert_eq!(
        e.handle(
            &viewing(&e, &log),
            batch_with(crate::audience::AudienceEvidence::default())
        ),
        Err(TransitionError::MembershipNeeded {
            needed: vec![group_atom("team")]
        }),
        "the produced `contains` is read extensionally, so the act asks for exactly its group"
    );

    let team = |members: Vec<ReaderId>| source_evidence(vec![user_group("team", members)]);
    let evidence = team(vec![slack_member("slack:UA", Some("alice@corp.com"))]);
    let decision = e
        .handle(&viewing(&e, &log), batch_with(evidence.clone()))
        .expect("the batch decides under the pinned answer");
    let (released, blocked) = answered(&decision);
    assert_eq!(released.len(), 1, "the team is alice alone, so the label covers it");
    assert!(blocked.is_empty());
    let facts = appended_facts(decision);
    assert!(
        facts.iter().any(|fact| matches!(
            fact,
            Fact::ProposalBatchDecided { evidence: pinned, .. } if pinned == &evidence
        )),
        "the decision pins the answer it consumed"
    );
    assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));

    let tampered = |mutate: &dyn Fn(&mut Fact)| {
        let mut facts = facts.clone();
        for fact in &mut facts {
            mutate(fact);
        }
        e.validate_replay(&[log.clone(), facts].concat())
    };
    assert_eq!(
        tampered(&|fact| {
            if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                *evidence = crate::audience::AudienceEvidence::default();
            }
        }),
        Err(TransitionRefusal::UnansweredDecision {
            needed: vec![group_atom("team")]
        }),
        "replay reads the pin, never a source: without it the decision is unanswered"
    );
    assert_eq!(
        tampered(&|fact| {
            if let Fact::ProposalBatchDecided { evidence, .. } = fact {
                *evidence = team(vec![slack_member("slack:UB", Some("bob@corp.com"))]);
            }
        }),
        Err(TransitionRefusal::MisdecidedBatch),
        "a membership the directory would report today does not rewrite the recorded decision"
    );
}

/// A produced delta that narrows to a symbolic audience narrows symbolically: no `requires`
/// reads it, so the act asks for no membership, and the narrowing the agent is offered
/// carries the chain word unexpanded.
#[test]
fn a_produced_symbolic_delta_narrows_without_a_membership_read() {
    let notify = plain_tool("notify");
    let mut cfg = test_config(vec![]);
    cfg.tools.push(annotated(notify.clone(), "acl"));
    cfg.annotators.push(annotator("acl"));
    let e = open_engine_at(cfg, known(TRUSTED, Audience::public()));
    let log = vec![opened(&e)];
    let internal = DeclaredAudience::Union(
        crate::label::Clause::new([crate::label::ChainAudience::Internal], [], []).expect("a chain clause"),
    );
    let narrowing = {
        let mut produced = notify.clone();
        produced.delta.audience = Some(DeltaAudience::Static(internal.clone()));
        let unpinned = call("notify", json!({}));
        raw(&unpinned
            .clone()
            .with_annotation(Some(pinned_for(produced, "acl", &unpinned))))
    };
    let decision = e
        .handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![narrowing]))
        .expect("a symbolic delta needs no directory answer to decide");
    let (released, blocked) = answered(&decision);
    assert!(released.is_empty());
    assert_eq!(blocked.len(), 1);
    let narrowing = blocked[0]
        .block
        .raw
        .narrowing
        .as_ref()
        .expect("a narrowing delta blocks on the narrowing it proposes");
    assert!(blocked[0].block.raw.requirement_gaps.is_empty());
    assert_eq!(
        narrowing.to,
        known(TRUSTED, Audience::of_declared(&internal)),
        "the chain word reaches the offered narrowing unexpanded"
    );
}

#[test]
fn a_call_approval_extends_the_offered_calls_pins() {
    let officer = crate::authority::Authority {
        name: AuthorityName::new("officer"),
        mandate: crate::authority::Mandate {
            reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
            ..crate::authority::Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let alice = Audience::restricted([corp_reader("alice")]);
    let e = audience_engine(vec![officer], known(TRUSTED, alice.clone()));
    let opening = vec![opened(&e)];
    let evidence = source_evidence(vec![user_group(
        "wide",
        vec![
            slack_member("slack:UA", Some("alice@corp.com")),
            slack_member("slack:UC", Some("carol@other.com")),
        ],
    )]);
    let blocked = appended_facts(
        e.handle(
            &viewing(&e, &opening),
            evidenced_batch("b1", vec![send_to("@wide")], evidence.clone()),
        )
        .expect("the batch decides"),
    );
    let (offer, plan) = opened_offers(&blocked)[0].clone();
    let log = [opening, blocked].concat();
    let approved = appended_facts(
        execute_offer(
            &e,
            &log,
            offer,
            OfferOutcome::Approved(evidence_for(offer, &plan, "send", partial(TRUSTED, alice))),
        )
        .expect("the officer's ruling arms the call"),
    );
    assert!(approved.iter().any(|fact| matches!(
        fact,
        Fact::CallApproved { evidence: pinned, .. } if pinned.contains(&evidence)
    )));
    assert_eq!(e.validate_replay(&[log.clone(), approved.clone()].concat()), Ok(()));
    let mut tampered = approved;
    for fact in &mut tampered {
        if let Fact::CallApproved { evidence, .. } = fact {
            *evidence = crate::audience::AudienceEvidence::default();
        }
    }
    assert_eq!(
        e.validate_replay(&[log, tampered].concat()),
        Err(TransitionRefusal::ForgedEvidence),
        "an approval that drops the offer's pins is forged"
    );
}

/// A record pinning evidence the act could not have consumed names why: a selector
/// answered twice is the duplicate, not an unspecified forgery.
#[test]
fn replay_names_the_refusal_of_a_duplicated_pin() {
    let officer = crate::authority::Authority {
        name: AuthorityName::new("officer"),
        mandate: crate::authority::Mandate {
            reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
            ..crate::authority::Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let e = audience_engine(
        vec![officer],
        known(TRUSTED, Audience::restricted([corp_reader("alice")])),
    );
    let opening = vec![opened(&e)];
    let mut blocked = appended_facts(
        e.handle(
            &viewing(&e, &opening),
            evidenced_batch(
                "b1",
                vec![send_to("@wide")],
                source_evidence(vec![user_group("wide", wide_as_reported())]),
            ),
        )
        .expect("the batch decides"),
    );
    for fact in &mut blocked {
        if let Fact::ProposalBatchDecided { evidence, .. } = fact {
            let again = evidence.sources[0].clone();
            evidence.sources.push(again);
        }
    }
    assert_eq!(
        e.validate_replay(&[opening, blocked].concat()),
        Err(TransitionRefusal::ForeignEvidence(
            crate::audience::EvidenceRefusal::DuplicateSelector {
                provider: "slack".to_string(),
                selector: "user-group/wide".to_string(),
            }
        ))
    );
}

fn wide_as_reported() -> Vec<ReaderId> {
    vec![
        slack_member("slack:UA", Some("alice@corp.com")),
        slack_member("slack:UC", Some("carol@other.com")),
    ]
}

/// The officer's approval, armed over `wide` reported as alice and an outsider, for a
/// proposal whose own evidence the test supplies: the spend reads that evidence under the
/// approval's pins.
fn spending_under_pins(fresh: crate::audience::AudienceEvidence) -> Result<EngineDecision, TransitionError> {
    let officer = crate::authority::Authority {
        name: AuthorityName::new("officer"),
        mandate: crate::authority::Mandate {
            reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
            ..crate::authority::Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let alice = Audience::restricted([corp_reader("alice")]);
    let e = audience_engine(vec![officer], known(TRUSTED, alice.clone()));
    let opening = vec![opened(&e)];
    let pinned = source_evidence(vec![user_group("wide", wide_as_reported())]);
    let blocked = appended_facts(
        e.handle(
            &viewing(&e, &opening),
            evidenced_batch("b1", vec![send_to("@wide")], pinned),
        )
        .expect("the batch decides"),
    );
    let (offer, plan) = opened_offers(&blocked)[0].clone();
    let log = [opening, blocked].concat();
    let approved = appended_facts(
        execute_offer(
            &e,
            &log,
            offer,
            OfferOutcome::Approved(evidence_for(offer, &plan, "send", partial(TRUSTED, alice))),
        )
        .expect("the officer's ruling arms the call"),
    );
    let log = [log, approved].concat();
    e.handle(&viewing(&e, &log), evidenced_batch("b2", vec![send_to("@wide")], fresh))
}

/// A fresh answer for a key the approval pinned is read under the pin: the same answer
/// again is fine, a different one is a contradiction the spend refuses — never an entry
/// silently dropped in favour of the pin.
#[test]
fn a_spend_refuses_a_fresh_answer_that_contradicts_a_pinned_key() {
    let same = source_evidence(vec![user_group("wide", wide_as_reported())]);
    assert!(
        spending_under_pins(same).is_ok(),
        "restating the pin is not a contradiction"
    );
    let contradicting = source_evidence(vec![user_group(
        "wide",
        vec![slack_member("slack:UB", Some("bob@corp.com"))],
    )]);
    assert!(matches!(
        spending_under_pins(contradicting),
        Err(TransitionError::ForeignEvidence(
            crate::audience::EvidenceRefusal::ContradictedPin { .. }
        ))
    ));
}

mod symbolic_audiences {
    use super::*;

    use crate::label::SymbolicAtom;
    use crate::names::GroupName;

    fn grouped(readers: &[&str], groups: &[&str]) -> DeclaredAudience {
        DeclaredAudience::Union(
            crate::label::Clause::new(
                [],
                groups
                    .iter()
                    .map(|group| crate::label::GroupRef::Named(GroupName::new(*group))),
                readers.iter().map(|reader| ReaderId::new(*reader)),
            )
            .expect("literal readers beside groups"),
        )
    }

    fn readers(names: &[&str]) -> Audience {
        Audience::restricted(names.iter().map(|name| ReaderId::new(*name)))
    }

    fn team_delta(name: &str) -> ToolAnnotation {
        let mut tool = plain_tool(name);
        tool.delta = Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(grouped(&[], &["team"]))),
        };
        tool
    }

    fn team_send() -> ToolAnnotation {
        let mut send = plain_tool("send");
        send.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(grouped(
                    &[],
                    &["team"],
                )))],
            },
            ..Requires::default()
        };
        send
    }

    fn config(tools: Vec<ToolAnnotation>, authorities: Vec<crate::authority::Authority>) -> RegistryConfig {
        RegistryConfig {
            annotators: vec![],
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: declared(tools),
            authorities,
            sanitizers: vec![],
            audience: slack_groups(&["team", "board", "officers"]),
        }
    }

    fn grouped_engine(cfg: RegistryConfig, provider_run: &[&str], starting: Label) -> Engine {
        let mut declaration = crate::profile::ProfileDeclaration {
            starting_label: starting,
            ..crate::profile::covering_declaration(&cfg)
        };
        for name in provider_run {
            declaration
                .executor_exceptions
                .insert(ToolName::new(*name), crate::profile::ExecutorClass::ProviderRun);
            declaration.confined_results.remove(&ToolName::new(*name));
        }
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            profile: declaration,
        })
        .unwrap()
    }

    fn act_batch(
        id: &str,
        provider_results: Vec<crate::transition::ProviderResult>,
        proposals: Vec<crate::transition::ProposedCall>,
        audience: crate::audience::AudienceEvidence,
    ) -> EngineEvent {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(id),
            trajectory: traj(),
            provider_results,
            proposals,
            spawn: None,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience,
        })
    }

    fn no_answers() -> crate::audience::AudienceEvidence {
        crate::audience::AudienceEvidence::default()
    }

    fn symbolic(handle: &str) -> Audience {
        Audience::of_declared(&group_audience(handle))
    }

    #[test]
    fn a_planned_state_that_reads_an_unanswered_group_refuses_the_search_instead_of_resolving_it() {
        use crate::route::{RouteDepth, RouteError, RouteOutcome, RouteStep};

        let mut backup = plain_tool("backup");
        backup.emits = EffectSet::new([EffectKind::new("backup")]).unwrap();
        backup.delta = Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::literal(readers(&["insider"])))),
        };
        let mut wire = plain_tool("wire");
        wire.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(grouped(
                    &[],
                    &["team"],
                )))],
            },
            history: vec![HistoryRequirement::Prior(EffectKind::new("backup"))],
            ..Requires::default()
        };
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(grouped(&[], &["board"])),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = grouped_engine(
            config(vec![backup, wire], vec![officer]),
            &[],
            known(TRUSTED, Audience::public()),
        );
        let log = vec![opened(&e)];
        let decided = e
            .handle(
                &viewing(&e, &log),
                act_batch("b1", vec![], vec![raw(&call("wire", json!({})))], no_answers()),
            )
            .expect("a `prior` gap alone reads no symbolic audience");
        let log = [log, appended_facts(decided)].concat();
        let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
        let subject = crate::basis::SubjectKey::Call {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            position: 0,
        };
        let routes = |answers: &crate::audience::AudienceEvidence| {
            e.recovery_routes(&view, &subject, answers, RouteDepth::new(2).unwrap())
        };

        assert!(
            matches!(routes(&no_answers()), Err(RouteError::MembershipNeeded(_))),
            "after `backup` narrows the audience, the `includes` gap makes symbolic audiences reads: {:?}",
            routes(&no_answers())
        );
        let team = user_group("team", vec![slack_member("slack:UC", Some("carol@corp.com"))]);
        assert_eq!(
            routes(&source_evidence(vec![team.clone()])),
            Err(RouteError::MembershipNeeded(vec![SymbolicAtom::Group(
                crate::label::GroupRef::Named(GroupName::new("board"))
            )])),
            "planning the block reads the officer's grouped ceiling"
        );
        let planned = routes(&source_evidence(vec![
            team,
            user_group("board", vec![slack_member("slack:UC", Some("carol@corp.com"))]),
        ]))
        .expect("the answers let the state plan");
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].outcome, RouteOutcome::Complete);
        assert!(
            matches!(
                &planned[0].steps[..],
                [RouteStep::Precede { tool, .. }, RouteStep::Authorize { authority, .. }]
                    if tool.as_str() == "backup" && authority.as_str() == "officer"
            ),
            "{:?}",
            planned[0].steps
        );
    }

    #[test]
    fn a_route_reads_the_answers_the_block_recorded_over_any_fresh_answer() {
        use crate::route::{RouteDepth, RouteOutcome, RouteStep};

        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(grouped(&[], &["team"])),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let mut send = plain_tool("send");
        send.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                    DeclaredAudience::restricted([corp_reader("bob")]),
                ))],
            },
            ..Requires::default()
        };
        let e = grouped_engine(
            config(vec![send], vec![officer]),
            &[],
            known(TRUSTED, Audience::restricted([corp_reader("alice")])),
        );
        let log = vec![opened(&e)];
        let bob_in_team = || user_group("team", vec![slack_member("slack:UB", Some("bob@corp.com"))]);
        let decided = e
            .handle(
                &viewing(&e, &log),
                act_batch(
                    "b1",
                    vec![],
                    vec![raw(&call("send", json!({})))],
                    source_evidence(vec![bob_in_team()]),
                ),
            )
            .expect("the officer's ceiling plans the block, so the batch answers `team`");
        let log = [log, appended_facts(decided)].concat();
        let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
        let subject = crate::basis::SubjectKey::Call {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            position: 0,
        };
        let routes = |answers: crate::audience::AudienceEvidence| {
            e.recovery_routes(&view, &subject, &answers, RouteDepth::new(2).unwrap())
                .expect("the recorded answers stand")
        };

        let recorded = routes(no_answers());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].outcome, RouteOutcome::Complete);
        assert!(
            matches!(
                &recorded[0].steps[..],
                [RouteStep::Authorize { authority, .. }] if authority.as_str() == "officer"
            ),
            "the recorded `team` answer puts bob under the officer's ceiling: {:?}",
            recorded[0].steps
        );
        assert_eq!(
            routes(source_evidence(vec![bob_in_team()])),
            recorded,
            "restating the answers the block consumed reads them once"
        );
        assert!(
            matches!(
                e.recovery_routes(
                    &view,
                    &subject,
                    &source_evidence(vec![user_group("team", vec![])]),
                    RouteDepth::ONE
                ),
                Err(crate::route::RouteError::Evidence(
                    crate::audience::EvidenceRefusal::ContradictedPin { .. }
                ))
            ),
            "a fresh answer that would drop the route contradicts the answer the block consumed"
        );
        assert!(matches!(
            e.recovery_routes(
                &view,
                &subject,
                &source_evidence(vec![user_group("officers", vec![]), user_group("officers", vec![])]),
                RouteDepth::ONE
            ),
            Err(crate::route::RouteError::Evidence(
                crate::audience::EvidenceRefusal::DuplicateSelector { .. }
            ))
        ));
    }

    #[test]
    fn a_provider_run_delta_writing_a_group_admits_at_the_symbolic_audience() {
        let e = grouped_engine(
            config(vec![team_delta("seen")], vec![]),
            &["seen"],
            known(TRUSTED, Audience::public()),
        );
        let log = vec![opened(&e)];
        let seen = || vec![exposed("seen", "what the provider saw")];

        // The delta stays symbolic: admitting it reads no membership at all.
        let admitted = e
            .handle(&viewing(&e, &log), act_batch("b1", seen(), vec![], no_answers()))
            .expect("a symbolic delta admits without an answer");
        let facts = appended_facts(admitted);
        let Some(label) = facts.iter().find_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.label.clone()),
            _ => None,
        }) else {
            panic!("the exposed result admits, got {facts:?}");
        };
        assert_eq!(
            label.audience,
            symbolic("team"),
            "the durable record carries the audience symbolically, never a reader snapshot"
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let repeat = e
            .handle(&viewing(&e, &log), act_batch("b1", seen(), vec![], no_answers()))
            .expect("the repeat answers from the record");
        assert_eq!(repeat.append, None);
    }

    #[test]
    fn a_contains_written_with_a_group_decides_by_the_answer_and_an_empty_answer_is_valid() {
        let e = grouped_engine(
            config(vec![team_send()], vec![]),
            &[],
            known(TRUSTED, Audience::restricted([corp_reader("alice")])),
        );
        let log = vec![opened(&e)];
        let send = || vec![raw(&call("send", json!({})))];

        assert_eq!(
            e.handle(&viewing(&e, &log), act_batch("b1", vec![], send(), no_answers())),
            Err(TransitionError::MembershipNeeded {
                needed: vec![group_atom("team")]
            })
        );

        let outsider = source_evidence(vec![user_group(
            "team",
            vec![slack_member("slack:UB", Some("bob@corp.com"))],
        )]);
        let decided = e
            .handle(&viewing(&e, &log), act_batch("b1", vec![], send(), outsider))
            .expect("a member answer decides");
        let (released, blocked) = answered(&decided);
        assert!(released.is_empty());
        assert_eq!(
            blocked[0].block.raw.requirement_gaps,
            vec![crate::check::Gap::Includes {
                recipients: grouped(&[], &["team"])
            }]
        );
        assert_eq!(
            e.validate_replay(&[log.clone(), appended_facts(decided)].concat()),
            Ok(())
        );

        let empty = source_evidence(vec![user_group("team", vec![])]);
        let released = e
            .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), empty.clone()))
            .expect("an empty answer decides");
        assert_eq!(tool_names(answered(&released).0), ["send"]);
        let facts = appended_facts(released);
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::DispatchOpened { evidence, .. } if evidence == &empty
        )));
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn one_act_reads_one_answer_for_a_group_two_sites_read() {
        // A recipient the current symbolic audience does not structurally cover: reading it
        // takes the group's members.
        let mut to_alice = plain_tool("send");
        to_alice.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                    DeclaredAudience::literal(Audience::restricted([corp_reader("alice")])),
                ))],
            },
            ..Requires::default()
        };
        let e = grouped_engine(
            config(vec![team_delta("seen"), to_alice], vec![]),
            &["seen"],
            known(TRUSTED, Audience::public()),
        );
        let log = vec![opened(&e)];
        let act = |audience| {
            act_batch(
                "b1",
                vec![exposed("seen", "what the provider saw")],
                vec![raw(&call("send", json!({})))],
                audience,
            )
        };
        assert_eq!(
            e.handle(&viewing(&e, &log), act(no_answers())),
            Err(TransitionError::MembershipNeeded {
                needed: vec![group_atom("team")]
            })
        );
        let answer = source_evidence(vec![user_group(
            "team",
            vec![slack_member("slack:UA", Some("alice@corp.com"))],
        )]);
        let decided = e
            .handle(&viewing(&e, &log), act(answer))
            .expect("the answered act decides");
        assert_eq!(tool_names(answered(&decided).0), ["send"]);
        let facts = appended_facts(decided);
        assert!(
            facts.iter().any(|fact| matches!(
                fact,
                Fact::ValueAdmitted { value, .. } if value.label.audience == symbolic("team")
            )),
            "the admitted label stays symbolic while `contains` reads the same act's answer"
        );
        assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
    }

    #[test]
    fn a_reader_ceiling_writing_a_group_is_read_when_the_block_is_planned() {
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                reader_ceiling: Some(grouped(&[], &["officers"])),
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let mut send = plain_tool("send");
        send.requires = Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(grouped(
                    &[],
                    &["team"],
                )))],
            },
            ..Requires::default()
        };
        let e = grouped_engine(
            config(vec![send], vec![officer]),
            &[],
            known(TRUSTED, Audience::restricted([corp_reader("alice")])),
        );
        let log = vec![opened(&e)];
        let send = || vec![raw(&call("send", json!({})))];

        assert_eq!(
            e.handle(&viewing(&e, &log), act_batch("b1", vec![], send(), no_answers())),
            Err(TransitionError::MembershipNeeded {
                needed: vec![group_atom("team")]
            })
        );
        let team = user_group("team", vec![slack_member("slack:UC", Some("carol@corp.com"))]);
        assert_eq!(
            e.handle(
                &viewing(&e, &log),
                act_batch("b1", vec![], send(), source_evidence(vec![team.clone()]))
            ),
            Err(TransitionError::MembershipNeeded {
                needed: vec![group_atom("officers")]
            }),
            "the block's plans read the officer's grouped ceiling"
        );
        let uncovered = e
            .handle(
                &viewing(&e, &log),
                act_batch(
                    "b1",
                    vec![],
                    send(),
                    source_evidence(vec![
                        team.clone(),
                        user_group("officers", vec![slack_member("slack:UD", Some("dave@corp.com"))]),
                    ]),
                ),
            )
            .expect("the answered act decides");
        assert!(answered(&uncovered).1[0].offers.is_empty());
        let covering = source_evidence(vec![
            team,
            user_group("officers", vec![slack_member("slack:UC", Some("carol@corp.com"))]),
        ]);
        let blocked = e
            .handle(&viewing(&e, &log), act_batch("b1", vec![], send(), covering.clone()))
            .expect("the answered act decides");
        let (offer, plan) = {
            let facts = appended_facts(blocked);
            let opened = opened_offers(&facts);
            assert_eq!(opened.len(), 1, "one ruling plan names the officer");
            (opened[0].0, opened[0].1.clone())
        };
        let blocked = e
            .handle(&viewing(&e, &log), act_batch("b1", vec![], send(), covering.clone()))
            .expect("the answered act decides");
        let log = [log, appended_facts(blocked)].concat();

        let evidence = evidence_for(
            offer,
            &plan,
            "send",
            partial(TRUSTED, Audience::restricted([corp_reader("alice")])),
        );
        let approved = execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the offer approves");
        assert!(matches!(
            approved.follow_up,
            FollowUp::Offer(OfferFollowUp::Approved { .. })
        ));
        let facts = appended_facts(approved);
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::CallApproved { evidence, .. } if evidence.contains(&covering)
        )));
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let released = e
            .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), no_answers()))
            .expect("the approved call releases");
        assert_eq!(tool_names(answered(&released).0), ["send"]);
        let log = [log, appended_facts(released)].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let repeated = e
            .handle(&viewing(&e, &log), act_batch("b2", vec![], send(), no_answers()))
            .expect("a decided batch repeats");
        assert!(repeated.append.is_none());
        assert_eq!(tool_names(answered(&repeated).0), ["send"]);
    }

    #[test]
    fn the_audience_configuration_is_policy_identity() {
        let identity = |cfg: &RegistryConfig| {
            let profile = crate::profile::DeploymentProfile::declare(crate::profile::covering_declaration(cfg))
                .expect("the covering declaration declares");
            crate::profile::identity_of(cfg, &profile)
        };
        let forward = config(vec![team_send(), team_delta("read")], vec![]);
        let backward = config(vec![team_delta("read"), team_send()], vec![]);
        let mut remapped = config(vec![team_send(), team_delta("read")], vec![]);
        remapped.audience.groups[0].from = vec![crate::audience::SelectorSpec {
            provider: "slack".to_string(),
            selector: "user-group/other".to_string(),
        }];
        assert_ne!(
            identity(&forward),
            identity(&remapped),
            "which sources feed an audience is part of what the policy means"
        );
        assert_eq!(identity(&forward), identity(&backward));
    }

    fn declassify_to(to: DeclaredAudience) -> crate::authority::Sanitizer {
        crate::authority::Sanitizer {
            name: SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([]),
                to,
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        }
    }

    /// A return declared through `declassify` at the spawn, derived, echoed and merged,
    /// then read back from its wire form.
    fn sanitized_return_log(e: &Engine) -> Vec<Fact> {
        let child = TrajectoryId::new("child");
        let opened_log = vec![opened(e)];
        let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
            sanitizer: Some(ReturnSanitizer::Named(SanitizerName::new("declassify"))),
            ..floor_crossing(e, &opened_log)
        });
        let log = [opened_log.clone(), spawn_family_at(e, &opened_log, &child, policy)].concat();
        let body = ValueBody::new("what I found");
        let evidence = vec![Evidence::Sanitizer {
            sanitizer: SanitizerName::new("declassify"),
            source: RawResultDigest::of(body.as_str().as_bytes()),
            derived: ValueBody::new("clean"),
        }];
        let derived = e
            .handle(
                &viewing(e, &log),
                child_report_with(&log, &child, &body, evidence, no_answers()),
            )
            .expect("the derivation stands as the candidate");
        let log = [log, appended_facts(derived)].concat();
        let echoed = e
            .handle(
                &viewing(e, &log),
                child_report_with(&log, &child, &ValueBody::new("clean"), vec![], no_answers()),
            )
            .expect("the echoed derivation crosses");
        let log = [log, appended_facts(echoed)].concat();
        let wire = serde_json::to_string(&log).expect("facts serialize");
        serde_json::from_str(&wire).expect("facts deserialize")
    }

    /// A derivation record names its sanitizer and the transition it applied is the
    /// registry's: the same policy replays the record from its wire form, and a policy
    /// whose sanitizer transitions differ is another policy, refused as a whole.
    #[test]
    fn a_sanitizers_transition_is_policy_identity_not_a_records_claim() {
        let identity = |cfg: &RegistryConfig| {
            let profile = crate::profile::DeploymentProfile::declare(crate::profile::covering_declaration(cfg))
                .expect("the covering declaration declares");
            crate::profile::identity_of(cfg, &profile)
        };
        let to_public = returning_with_sources(vec![declassify_to(DeclaredAudience::Public)]);
        let to_alice = returning_with_sources(vec![declassify_to(DeclaredAudience::restricted([corp_reader(
            "alice",
        )]))]);
        assert_ne!(identity(&to_public), identity(&to_alice));

        let e = open_engine(to_public);
        let log = sanitized_return_log(&e);
        assert!(log.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { .. },
                ..
            }
        )));
        assert_eq!(e.validate_replay(&log), Ok(()));
        assert_ne!(
            open_engine(to_alice).validate_replay(&log),
            Ok(()),
            "a changed transition is another policy"
        );
    }

    fn child_report_with(
        log: &[Fact],
        child: &TrajectoryId,
        body: &ValueBody,
        evidence: Vec<Evidence>,
        audience: crate::audience::AudienceEvidence,
    ) -> EngineEvent {
        EngineEvent::ChildReturn(ChildReport {
            child: child.clone(),
            fork: fork_in(log, child),
            submission: ChildSubmission::Value { body: body.clone() },
            evidence,
            audience,
        })
    }

    fn returning_with_sources(sanitizers: Vec<crate::authority::Sanitizer>) -> RegistryConfig {
        RegistryConfig {
            audience: slack_groups(&["team"]),
            ..returning_registry(sanitizers)
        }
    }

    #[test]
    fn a_sanitizer_to_writing_a_group_derives_at_the_symbolic_audience() {
        let declassify = crate::authority::Sanitizer {
            name: SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::restricted([]),
                to: grouped(&[], &["team"]),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let e = open_engine(returning_with_sources(vec![declassify]));
        let child = TrajectoryId::new("child");
        let opened_log = vec![opened(&e)];
        let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
            sanitizer: Some(ReturnSanitizer::Named(SanitizerName::new("declassify"))),
            ..floor_crossing(&e, &opened_log)
        });
        let log = [opened_log.clone(), spawn_family_at(&e, &opened_log, &child, policy)].concat();
        let body = ValueBody::new("what I found");
        let raw_digest = RawResultDigest::of(body.as_str().as_bytes());

        let asked = e
            .handle(
                &viewing(&e, &log),
                child_report_with(&log, &child, &body, vec![], no_answers()),
            )
            .expect("a symbolic transition target needs no answer to ask for the derivation");
        assert_eq!(asked.append, None);
        assert!(matches!(
            asked.follow_up,
            FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer { .. }))
        ));

        let clean = ValueBody::new("clean");
        let evidence = vec![Evidence::Sanitizer {
            sanitizer: SanitizerName::new("declassify"),
            source: raw_digest,
            derived: clean.clone(),
        }];
        let staged = e
            .handle(
                &viewing(&e, &log),
                child_report_with(&log, &child, &body, evidence, no_answers()),
            )
            .expect("the derivation stands as the candidate");
        assert_eq!(
            staged.follow_up,
            FollowUp::Child(ChildFollowUp::Staged { derived: clean.clone() })
        );
        let facts = appended_facts(staged);
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::CandidateDerived { sanitizer, .. } if sanitizer == &SanitizerName::new("declassify")
        )));
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
        let subject = crate::basis::SubjectKey::Return(ChildReturnId::new(child.clone(), 0));
        assert!(
            matches!(
                viewing(&e, &log).views(&traj()).expect("the parent is opened").candidate(&subject),
                Some(DerivedCandidate::Return { value, .. }) if value.label.audience == symbolic("team")
            ),
            "the derived candidate carries the transition's audience symbolically"
        );

        let echoed = e
            .handle(
                &viewing(&e, &log),
                child_report_with(&log, &child, &clean, vec![], no_answers()),
            )
            .expect("the child's echo of the derivation crosses it");
        assert_eq!(
            echoed.follow_up,
            FollowUp::Child(ChildFollowUp::Merged {
                admitted: clean.clone()
            })
        );
        let facts = appended_facts(echoed);
        assert!(facts.iter().any(|fact| matches!(
            fact,
            Fact::ChildReturn { value, .. } if value.label.audience == symbolic("team")
        )));
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
    }

    #[test]
    fn a_staged_derivation_seals_and_replays_with_its_pinned_answers() {
        // Deriving the return reads @team to judge the sanitizer's
        // applicability, so the staging act pins the answer, and the
        // echo that crosses the derivation inherits it: both batches must
        // seal live and replay with that evidence.
        let declassify = crate::authority::Sanitizer {
            name: SanitizerName::new("declassify"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: grouped(&[], &["team"]),
                to: DeclaredAudience::literal(Audience::public()),
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        let mut cfg = returning_with_sources(vec![declassify]);
        cfg.tools.push(crate::contract::ToolDeclaration::Declared(read_tool(
            "read_member",
            Delta {
                trust: None,
                audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                    "slack:U1",
                )]))),
            },
        )));
        // Confine no result, so the child's own read never asks the
        // sanitizer's atom and the derivation is the one reader.
        let mut declaration = crate::profile::covering_declaration(&cfg);
        declaration.confined_results = std::collections::BTreeSet::new();
        let e = Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            profile: declaration,
        })
        .expect("a context-controlled deployment opens");
        let child = TrajectoryId::new("child");
        let opened_log = vec![opened(&e)];
        // The declaration binds only the trust dimension, the one the sanitizer leaves alone.
        let policy = ReturnPolicy::Crossing(crate::fact::CrossingReturn {
            floor: Label::new(SUSPICIOUS, Audience::public()),
            sanitizer: Some(ReturnSanitizer::Named(SanitizerName::new("declassify"))),
        });
        let mut log = [opened_log.clone(), spawn_family_at(&e, &opened_log, &child, policy)].concat();
        reads(&e, &mut log, &child, "read_member");
        let body = ValueBody::new("what I found");
        let clean = ValueBody::new("clean");
        let evidence = vec![Evidence::Sanitizer {
            sanitizer: SanitizerName::new("declassify"),
            source: RawResultDigest::of(body.as_str().as_bytes()),
            derived: clean.clone(),
        }];
        let answers = crate::audience::AudienceEvidence {
            lookups: vec![crate::audience::MemberLookup {
                provider: "slack".to_string(),
                member: "slack:U1".to_string(),
                principal: Some(slack_member("slack:U1", None)),
            }],
            ..source_evidence(vec![user_group("team", vec![slack_member("slack:U1", None)])])
        };

        let staged = e
            .handle(
                &viewing(&e, &log),
                child_report_with(&log, &child, &body, evidence, answers.clone()),
            )
            .expect("the answered derivation stages");
        assert_eq!(
            staged.follow_up,
            FollowUp::Child(ChildFollowUp::Staged { derived: clean.clone() })
        );
        let facts = appended_facts(staged);
        assert!(
            facts
                .iter()
                .any(|fact| matches!(fact, Fact::CandidateDerived { evidence, .. } if *evidence == answers)),
            "the staging pins the answers it read"
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));

        let echoed = e
            .handle(
                &viewing(&e, &log),
                child_report_with(&log, &child, &clean, vec![], no_answers()),
            )
            .expect("the echo crosses the staged derivation without reading again");
        assert_eq!(
            echoed.follow_up,
            FollowUp::Child(ChildFollowUp::Merged {
                admitted: clean.clone()
            })
        );
        let facts = appended_facts(echoed);
        assert!(
            facts.iter().any(|fact| matches!(
                fact,
                Fact::ChildReturn { value, .. } if value.body == clean && value.label.audience == Audience::public()
            )),
            "the crossing carries the derivation at the sanitizer's audience"
        );
        let log = [log, facts].concat();
        assert_eq!(e.validate_replay(&log), Ok(()));
    }
}

fn channel_source() -> crate::audience::AudienceConfig {
    crate::audience::AudienceConfig {
        sources: vec![crate::audience::SourceRegistration {
            provider: crate::names::ProviderName::new("slack"),
            templates: vec![crate::audience::DeclaredTemplate::named("channel/<id>").expect("a well-formed template")],
        }],
        ..crate::audience::AudienceConfig::default()
    }
}

fn channel_placeholder() -> crate::contract::SelectorPlaceholder {
    crate::contract::SelectorPlaceholder::parse("slack:channel/$channel").expect("a placeholder spelling")
}

fn channel_group(id: &str) -> DeclaredAudience {
    DeclaredAudience::Union(
        crate::label::Clause::new(
            [],
            [crate::label::GroupRef::Source {
                provider: "slack".to_string(),
                selector: format!("channel/{id}"),
            }],
            [],
        )
        .expect("a group clause names no reader"),
    )
}

/// A delta placeholder labels the result with the one collection the call spells — folded
/// symbolically, so no membership is read: on a trajectory already at that collection the
/// call releases under it, on any other it narrows to it. Minting refuses a call whose
/// argument cannot spell a selector segment.
#[test]
fn a_delta_placeholder_labels_the_result_with_the_collection_the_call_spells() {
    let mut read = plain_tool("read");
    read.parameters = crate::params::test_string_argument_schema("channel");
    read.delta = Delta {
        trust: None,
        audience: Some(DeltaAudience::Selector(channel_placeholder())),
    };
    let mut cfg = test_config(vec![read]);
    cfg.audience = channel_source();
    let at_channel = |id: &str| known(TRUSTED, Audience::of_declared(&channel_group(id)));
    let e = open_engine_at(cfg, at_channel("C1"));
    let log = vec![opened(&e)];
    let to_channel = |id: &str| {
        e.resolve_call(ToolName::new("read"), format!(r#"{{"channel":"{id}"}}"#).as_bytes())
            .expect("the call fills the placeholder")
    };

    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), to_channel("C1")).expect("the batch decides"));
    let label = facts
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened { proposed_label, .. } => Some(proposed_label.clone()),
            _ => None,
        })
        .expect("a call to the channel the trajectory is at releases");
    assert_eq!(label, at_channel("C1"));
    assert_eq!(e.validate_replay(&[log.clone(), facts.clone()].concat()), Ok(()));

    // A recorded opening whose arguments fill no placeholder was never minted here.
    let forged: Vec<Fact> = facts
        .iter()
        .cloned()
        .map(|fact| match fact {
            Fact::DispatchOpened {
                trajectory,
                dispatch,
                tool,
                declaration,
                arguments: _,
                proposed_label,
                receiving,
                proposed_effects,
                annotation,
                file_basis,
                subject,
                evidence,
            } => Fact::DispatchOpened {
                trajectory,
                dispatch,
                tool,
                declaration,
                arguments: crate::params::test_arguments(&json!({ "channel": "a/b" })),
                proposed_label,
                receiving,
                proposed_effects,
                annotation,
                file_basis,
                subject,
                evidence,
            },
            other => other,
        })
        .collect();
    assert!(e.validate_replay(&[log.clone(), forged].concat()).is_err());

    match check_call(&e, &log, &to_channel("C2")) {
        CheckOutcome::Block(block) => {
            assert!(block.requirement_gaps.is_empty());
            assert_eq!(
                block.narrowing.map(|narrowing| narrowing.to),
                Some(at_channel("C1").combine(&at_channel("C2"))),
                "another channel is another collection"
            );
        }
        other => panic!("a call to another channel narrows, got {other:?}"),
    }

    for unwritable in [br#"{"channel":"a/b"}"#.as_slice(), br#"{"channel":"$x"}"#.as_slice()] {
        assert!(matches!(
            e.resolve_call(ToolName::new("read"), unwritable),
            Err(EngineError::InvalidCall(ArgumentError::UnfilledPlaceholder(unfilled))) if unfilled.argument == "channel"
        ));
    }
}

/// A `contains` placeholder asks for the collection the call spells and requires every
/// member of it among the current readers.
#[test]
fn a_contains_placeholder_requires_the_collection_the_call_spells() {
    let mut send = plain_tool("send");
    send.parameters = crate::params::test_string_argument_schema("channel");
    send.requires = Requires {
        label: LabelRequirements {
            trust_floor: None,
            audience: vec![AudienceRequirement::Includes(RecipientSpec::Selector(
                channel_placeholder(),
            ))],
        },
        ..Requires::default()
    };
    let mut cfg = test_config(vec![send]);
    cfg.audience = channel_source();
    let e = open_engine_at(cfg, known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let to_channel = |id: &str| raw(&call("send", json!({ "channel": id })));
    let atom = SymbolicAtom::Group(crate::label::GroupRef::Source {
        provider: "slack".to_string(),
        selector: "channel/C1".to_string(),
    });
    assert_eq!(
        e.handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![to_channel("C1")])),
        Err(TransitionError::MembershipNeeded { needed: vec![atom] })
    );
    let members = |readers: Vec<ReaderId>| {
        source_evidence(vec![crate::audience::SourceClaims {
            provider: "slack".to_string(),
            selector: "channel/C1".to_string(),
            members: readers,
        }])
    };
    let decision = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch("b2", vec![to_channel("C1")], members(vec![corp_reader("alice")])),
        )
        .expect("the batch decides");
    assert_eq!(
        answered(&decision).0.len(),
        1,
        "every member of the channel already reads"
    );
    let decision = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch(
                "b3",
                vec![to_channel("C1")],
                members(vec![corp_reader("alice"), corp_reader("bob")]),
            ),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert!(released.is_empty());
    assert_eq!(
        blocked[0].block.raw.requirement_gaps,
        vec![Gap::Includes {
            recipients: channel_group("C1")
        }]
    );
}

/// A placeholder filled from an array asks for every collection the call names, and the
/// requirement holds only when the members of all of them already read.
#[test]
fn a_contains_placeholder_filled_from_an_array_requires_every_collection() {
    let mut send = plain_tool("send");
    send.parameters = crate::params::ToolParameters::compile(&json!({
        "type": "object",
        "properties": { "channel": { "type": "array", "items": { "type": "string" } } },
        "required": ["channel"],
    }))
    .expect("an array-of-strings schema is dialect-valid");
    send.requires = Requires {
        label: LabelRequirements {
            trust_floor: None,
            audience: vec![AudienceRequirement::Includes(RecipientSpec::Selector(
                channel_placeholder(),
            ))],
        },
        ..Requires::default()
    };
    let mut cfg = test_config(vec![send]);
    cfg.audience = channel_source();
    let e = open_engine_at(cfg, known(TRUSTED, Audience::restricted([corp_reader("alice")])));
    let log = vec![opened(&e)];
    let to_channels = raw(&call("send", json!({ "channel": ["C1", "C2"] })));
    let atom = |id: &str| {
        SymbolicAtom::Group(crate::label::GroupRef::Source {
            provider: "slack".to_string(),
            selector: format!("channel/{id}"),
        })
    };
    assert_eq!(
        e.handle(&viewing(&e, &log), batch("b1", Vec::new(), vec![to_channels.clone()])),
        Err(TransitionError::MembershipNeeded {
            needed: vec![atom("C1"), atom("C2")]
        })
    );
    let members = |c2: Vec<ReaderId>| {
        source_evidence(vec![
            crate::audience::SourceClaims {
                provider: "slack".to_string(),
                selector: "channel/C1".to_string(),
                members: vec![corp_reader("alice")],
            },
            crate::audience::SourceClaims {
                provider: "slack".to_string(),
                selector: "channel/C2".to_string(),
                members: c2,
            },
        ])
    };
    let decision = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch("b2", vec![to_channels.clone()], members(vec![corp_reader("alice")])),
        )
        .expect("the batch decides");
    assert_eq!(answered(&decision).0.len(), 1, "both channels' members already read");
    let decision = e
        .handle(
            &viewing(&e, &log),
            evidenced_batch("b3", vec![to_channels], members(vec![corp_reader("bob")])),
        )
        .expect("the batch decides");
    let (released, blocked) = answered(&decision);
    assert!(released.is_empty(), "a reader of the second channel does not read yet");
    assert_eq!(blocked.len(), 1);
}

/// A delta placeholder filled from an array labels the result with the union of every
/// collection the call lists — no more, no fewer.
#[test]
fn a_delta_placeholder_filled_from_an_array_binds_the_union() {
    let mut read = plain_tool("read");
    read.delta = Delta {
        trust: None,
        audience: Some(DeltaAudience::Selector(channel_placeholder())),
    };
    let group = |id: &str| crate::label::GroupRef::Source {
        provider: "slack".to_string(),
        selector: format!("channel/{id}"),
    };
    let bound = read
        .bound_to(&json!({ "channel": ["C1", "C2"] }))
        .expect("the array fills the placeholder")
        .expect("a placeholder delta binds");
    assert_eq!(
        bound.delta.audience,
        Some(DeltaAudience::Static(DeclaredAudience::Union(
            crate::label::Clause::new([], [group("C1"), group("C2")], []).expect("a group clause names no reader")
        )))
    );
}

/// A mandate placeholder admits, per call, exactly the collection the call's arguments
/// spell. The policy refuses to route a tool without the argument, or the wildcard, through
/// such a mandate.
#[test]
fn a_mandate_placeholder_admits_the_collection_each_call_spells() {
    let acl = crate::registry::AnnotatorDeclaration {
        name: crate::names::AnnotatorName::new("acl"),
        trust: None,
        audiences: Some(
            crate::registry::AudienceVocabulary::parse_entries(&["@slack:channel/$channel".to_string()])
                .expect("a placeholder mandate parses"),
        ),
        marks: None,
        effects: None,
    };
    let mut lookup = plain_tool("lookup");
    lookup.parameters = crate::params::test_string_argument_schema("channel");
    let mut cfg = test_config(vec![]);
    cfg.annotators = vec![acl];
    cfg.tools = vec![annotated(lookup, "acl")];
    cfg.audience = channel_source();
    let e = open_engine_at(cfg.clone(), known(TRUSTED, Audience::public()));
    let declaration = e.registry().tool(&ToolName::new("lookup")).expect("lookup registers");
    let unpinned = call("lookup", json!({ "channel": "C1" }));
    let pinned = |id: &str| {
        let mut produced = plain_tool("lookup");
        produced.delta = Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(channel_group(id))),
        };
        unpinned
            .clone()
            .with_annotation(Some(pinned_for(produced, "acl", &unpinned)))
    };
    assert_eq!(
        crate::check::validate_annotation(e.registry(), declaration, &pinned("C1")),
        Ok(())
    );
    assert!(matches!(
        crate::check::validate_annotation(e.registry(), declaration, &pinned("C2")),
        Err(crate::check::AnnotationRefusal::OutsidePolicy(_))
    ));

    let mut through_wildcard = cfg.clone();
    through_wildcard.tools = vec![wildcard("acl")];
    assert!(matches!(
        crate::registry::Registry::build_covered(through_wildcard),
        Err(crate::registry::LoadError::WildcardPlaceholderMandate(name)) if name == "acl"
    ));
    // A tool that declares no `parameters` still binds the mandate's argument: the
    // registry makes `channel` a required string of its schema.
    let mut unbound = cfg;
    unbound.tools = vec![annotated(plain_tool("lookup"), "acl")];
    let registry = crate::registry::Registry::build_covered(unbound).expect("the argument implies its schema");
    let Some(crate::contract::ToolDeclaration::Annotated { parameters, .. }) = registry.tool(&ToolName::new("lookup"))
    else {
        panic!("lookup is Annotator-routed");
    };
    assert!(parameters.validate(&serde_json::json!({ "channel": "C1" })).is_ok());
    assert!(parameters.validate(&serde_json::json!({})).is_err());
    assert!(parameters.validate(&serde_json::json!({ "channel": 1 })).is_err());
}
