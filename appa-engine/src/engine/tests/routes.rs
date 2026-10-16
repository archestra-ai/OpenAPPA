use super::*;
use crate::route::{Certainty, Contingency, RecoveryRoute, RouteDepth, RouteError, RouteOutcome, RouteStep};
use std::collections::BTreeSet;

fn subject(batch: &str) -> crate::basis::SubjectKey {
    crate::basis::SubjectKey::Call {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new(batch),
        position: 0,
    }
}

#[test]
fn depth_one_through_the_engine_is_the_offers_the_block_opened() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let wire = call("wire", json!({}));
    let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), wire.clone()).expect("the call blocks"));
    let offered: Vec<RecoveryRoute> = opened_offers(&blocked)
        .into_iter()
        .map(|(_, plan)| RecoveryRoute {
            steps: plan
                .required
                .iter()
                .map(|required| RouteStep::Authorize {
                    authority: required.authority.clone(),
                    covers: required.covers.clone(),
                    call: wire.digest(),
                })
                .collect(),
            outcome: RouteOutcome::Complete,
        })
        .collect();
    assert_eq!(offered.len(), 2);
    for route in &offered {
        let [RouteStep::Authorize { authority, .. }] = &route.steps[..] else {
            panic!("one ruling per offer: {route:?}");
        };
        assert_eq!(
            route.certainty(),
            Certainty::Contingent(BTreeSet::from([Contingency::AuthorityDecision {
                authority: authority.clone(),
            }]))
        );
    }
    let log = [log, blocked].concat();
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");

    for depth in [RouteDepth::ONE, RouteDepth::new(4).unwrap()] {
        let found = e
            .recovery_routes(
                &view,
                &subject("b1"),
                &crate::audience::AudienceEvidence::default(),
                depth,
            )
            .expect("a blocked call has a route search");
        assert_eq!(found, offered, "no tool clears a trust floor, so depth adds nothing");
    }
    assert_eq!(
        e.view(&traj(), log.clone(), log.len() as u64).map(|_| ()),
        Ok(()),
        "the search appended nothing"
    );
}

#[test]
fn only_a_blocked_standing_call_has_routes() {
    let e = two_officer_engine();
    let log = vec![opened(&e)];
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    let at = |subject: &crate::basis::SubjectKey| {
        e.recovery_routes(
            &view,
            subject,
            &crate::audience::AudienceEvidence::default(),
            RouteDepth::ONE,
        )
    };

    assert_eq!(at(&subject("never")), Err(RouteError::UnknownSubject));
    assert_eq!(
        engine(vec![neutral_tool()]).recovery_routes(
            &view,
            &subject("never"),
            &crate::audience::AudienceEvidence::default(),
            RouteDepth::ONE
        ),
        Err(RouteError::ForeignView),
        "a view built under another policy is refused before anything is read"
    );
    assert_eq!(
        at(&crate::basis::SubjectKey::Return(ChildReturnId::new(
            TrajectoryId::new("child"),
            0
        ))),
        Err(RouteError::NotACallSubject)
    );

    let e = engine_at(
        vec![crm_tool()],
        known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let log = vec![opened(&e)];
    let released =
        appended_facts(proposed(&e, &log, "b1", nonce(), call("get_ticket", json!({}))).expect("the batch decides"));
    assert!(released.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    let log = [log, released].concat();
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    assert_eq!(
        e.recovery_routes(
            &view,
            &subject("b1"),
            &crate::audience::AudienceEvidence::default(),
            RouteDepth::ONE
        ),
        Err(RouteError::NotBlocked),
        "a released call stands decided but passes its check, so there is nothing to plan"
    );
}

#[test]
fn a_route_reads_the_ordered_contract_the_standing_call_selected() {
    let private = ToolAnnotation {
        requires: Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                    DeclaredAudience::restricted([ReaderId::new("partner")]),
                ))],
            },
            ..Requires::default()
        },
        ..plain_tool("read(path:private/*)")
    };
    let e = engine_at(
        vec![plain_tool("read(path:public/*)"), private],
        known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let log = vec![opened(&e)];
    let proposal = e
        .resolve_call(ToolName::new("read"), br#"{"path":"private/q3.md"}"#)
        .expect("the call resolves");
    let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), proposal).expect("the batch decides"));
    assert!(!blocked.iter().any(|fact| matches!(fact, Fact::DispatchOpened { .. })));
    let log = [log, blocked].concat();
    let view = e.view(&traj(), log.clone(), log.len() as u64).expect("the log replays");
    assert!(
        e.recovery_routes(
            &view,
            &subject("b1"),
            &crate::audience::AudienceEvidence::default(),
            RouteDepth::ONE
        )
        .is_ok(),
        "the standing call is blocked under the contract it selected; the first contract's verdict is not its verdict"
    );
}
