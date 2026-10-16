use super::*;

fn emitting_tool() -> ToolAnnotation {
    ToolAnnotation {
        emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
        ..plain_tool("send_note")
    }
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
