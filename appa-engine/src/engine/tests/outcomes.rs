use super::*;

#[test]
fn a_rewritten_admitted_label_is_refused_at_every_provenance() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal.clone()));
    let call = call("get_ticket", json!({}));
    let mut log = vec![opened(&e)];
    let dispatch = open(&e, &mut log, &call);
    close(
        &e,
        &mut log,
        &dispatch,
        &call,
        crate::admit::ResultAdmission::SuccessRaw {
            body: ValueBody::new("ticket"),
        },
    );
    assert_eq!(e.validate_replay(&log), Ok(()));

    let widened = |label: Label| {
        let mut forged = log.clone();
        let last = forged.len() - 1;
        forged[last] = Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new("ticket"), label),
            provenance: Provenance::ToolResult {
                dispatch: dispatch.clone(),
            },
        };
        e.validate_replay(&forged)
    };
    assert_eq!(
        widened(Label::new(TRUSTED, Audience::public())),
        Err(TransitionRefusal::ForgedLabel)
    );
    let admitted = match log.last() {
        Some(Fact::ValueAdmitted { value, .. }) => value.label.clone(),
        other => panic!("the raw result admits a value, got {other:?}"),
    };
    assert_eq!(widened(admitted), Ok(()));

    let child = TrajectoryId::new("child");
    let e = engine_at(vec![crm_tool()], Label::new(SUSPICIOUS, internal.clone()));
    let mut branched = vec![opened(&e)];
    branched.extend(forked_child(&e, &branched.clone(), &child));
    let crossing = crossed_report(&e, &branched.clone(), &child, "done");
    branched.extend(crossing);
    assert_eq!(e.validate_replay(&branched), Ok(()));

    let forge = |index: usize, label: Label| {
        let mut forged = branched.clone();
        forged[index] = match &forged[index] {
            Fact::ChildReturn {
                id, value, derivation, ..
            } => Fact::ChildReturn {
                trajectory: child.clone(),
                id: id.clone(),
                value: LabeledValue::new(value.body.clone(), label),
                derivation: derivation.clone(),
                evidence: crate::audience::AudienceEvidence::default(),
            },
            Fact::ValueAdmitted { value, provenance, .. } => Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(value.body.clone(), label),
                provenance: provenance.clone(),
            },
            other => panic!("unexpected record at {index}: {other:?}"),
        };
        e.validate_replay(&forged)
    };
    let crossing_at = branched
        .iter()
        .position(|fact| matches!(fact, Fact::ChildReturn { .. }))
        .expect("the return records its crossing");
    assert_eq!(
        forge(crossing_at, Label::new(TRUSTED, Audience::public())),
        Err(TransitionRefusal::ForgedLabel)
    );
    assert_eq!(
        forge(crossing_at + 1, Label::new(TRUSTED, Audience::public())),
        Err(TransitionRefusal::ForgedLabel)
    );
}

/// An indeterminate close records no observation and leaves the reservation
/// standing, because the call may have executed. A report that arrives afterwards is
/// refused on that ground, not on a contradiction with an observation there is none of.
#[test]
fn a_report_after_an_indeterminate_close_is_refused_on_its_own_ground() {
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
    let FollowUp::Proposals { released, .. } = decision.follow_up else {
        panic!("a proposal batch answers with proposals")
    };
    let dispatch = released[0].dispatch.clone();
    let log = [records, decision.append.unwrap().facts().to_vec()].concat();
    let released_view = e.view(&traj(), log.clone(), 2).unwrap();

    let report = |outcome: ToolOutcome| ToolReport {
        dispatch: dispatch.clone(),
        outcome,
        evidence: Vec::new(),
        offer_nonce: nonce(),
        audience: crate::audience::AudienceEvidence::default(),
    };
    let closed = e
        .handle(&released_view, EngineEvent::Outcome(report(ToolOutcome::Indeterminate)))
        .expect("an indeterminate outcome closes the dispatch");
    let facts = closed.append.expect("the close appends").facts().to_vec();
    assert!(
        matches!(
            declared_records(&facts),
            [Fact::DispatchClosed {
                outcome: crate::fact::CloseOutcome::Indeterminate,
                ..
            }]
        ),
        "the close records the indeterminate outcome and no observation: {facts:?}"
    );

    let after = e.view(&traj(), [log, facts].concat(), 3).unwrap();
    assert_eq!(
        e.handle(
            &after,
            EngineEvent::Outcome(report(ToolOutcome::Success {
                body: OutcomeBody::Available(ValueBody::new("the ticket")),
            }))
        ),
        Err(crate::transition::TransitionError::ClosedUnobserved)
    );
}

#[test]
fn a_reported_outcome_closes_once_and_repeats_answer_from_the_record() {
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
    let FollowUp::Proposals { released, .. } = decision.follow_up else {
        panic!("a proposal batch answers with proposals")
    };
    let dispatch = released[0].dispatch.clone();
    let log = [records, decision.append.unwrap().facts().to_vec()].concat();
    let released_view = e.view(&traj(), log.clone(), 2).unwrap();

    let report = |outcome: ToolOutcome| ToolReport {
        dispatch: dispatch.clone(),
        outcome,
        evidence: Vec::new(),
        offer_nonce: nonce(),
        audience: crate::audience::AudienceEvidence::default(),
    };
    let body = ValueBody::new("the ticket");
    let success = || {
        report(ToolOutcome::Success {
            body: OutcomeBody::Available(body.clone()),
        })
    };

    let closed = e
        .handle(&released_view, EngineEvent::Outcome(success()))
        .expect("the outcome closes the dispatch");
    assert_eq!(
        closed.follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Closed {
            admitted: Some(body.clone()),
            withheld: false,
        })
    );
    let facts = closed.append.expect("the close appends").facts().to_vec();
    assert!(
        matches!(
            facts.as_slice(),
            [Fact::BasisAdvanced { advance, .. }, Fact::DispatchClosed { .. }, Fact::ValueAdmitted { .. }]
                if advance.is_empty()
        ),
        "a result that leaves the label where it was moves no basis, so the close declares no advance: {facts:?}"
    );

    let after = e.view(&traj(), [log, facts].concat(), 3).unwrap();
    let repeat = e
        .handle(&after, EngineEvent::Outcome(success()))
        .expect("a repeat of a closed report answers from the record");
    assert_eq!(repeat.append, None);
    assert_eq!(
        repeat.follow_up,
        FollowUp::Outcome(OutcomeFollowUp::Closed {
            admitted: Some(body),
            withheld: false,
        })
    );
    assert_eq!(
        e.handle(
            &after,
            EngineEvent::Outcome(report(ToolOutcome::Success {
                body: OutcomeBody::Available(ValueBody::new("another ticket")),
            }))
        ),
        Err(crate::transition::TransitionError::ObservationMismatch)
    );
    assert_eq!(
        e.handle(&after, EngineEvent::Outcome(report(ToolOutcome::Failure))),
        Err(crate::transition::TransitionError::ContradictedSuccess)
    );
    assert_eq!(
        e.handle(
            &after,
            EngineEvent::Outcome(report(ToolOutcome::Success {
                body: OutcomeBody::Unavailable
            }))
        ),
        Err(crate::transition::TransitionError::ObservationMismatch)
    );
    assert_eq!(
        e.handle(
            &after,
            EngineEvent::Outcome(ToolReport {
                dispatch: DispatchId::new(traj(), call.digest(), 7),
                outcome: ToolOutcome::Failure,
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            })
        ),
        Err(crate::transition::TransitionError::UnknownDispatch)
    );
}

#[test]
fn a_failure_body_on_a_call_that_touches_no_file_is_refused() {
    let e = engine(vec![plain_tool("quiet")]);
    let mut log = vec![opened(&e)];
    let dispatch = open(&e, &mut log, &call("quiet", json!({})));
    let reported = e.handle(
        &viewing(&e, &log),
        EngineEvent::Outcome(ToolReport {
            dispatch,
            outcome: ToolOutcome::FailureWithBody {
                body: ValueBody::new("error text"),
            },
            evidence: Vec::new(),
            offer_nonce: nonce(),
            audience: crate::audience::AudienceEvidence::default(),
        }),
    );
    assert_eq!(reported.unwrap_err(), TransitionError::FailureBodyOutsideFile);
}
