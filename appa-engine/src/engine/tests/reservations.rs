use super::*;

fn emitting(name: &str, kind: &str) -> ToolAnnotation {
    ToolAnnotation {
        emits: EffectSet::new([EffectKind::new(kind)]).unwrap(),
        ..plain_tool(name)
    }
}

fn history_guarded(name: &str, requirement: HistoryRequirement) -> ToolAnnotation {
    ToolAnnotation {
        requires: Requires {
            history: vec![requirement],
            ..Requires::default()
        },
        ..plain_tool(name)
    }
}

fn reservation_tools() -> Vec<ToolAnnotation> {
    vec![
        emitting("send", "email.sent"),
        history_guarded("guard", HistoryRequirement::NoPrior(EffectKind::new("email.sent"))),
        history_guarded("wants", HistoryRequirement::Prior(EffectKind::new("email.sent"))),
    ]
}

#[test]
fn an_open_dispatch_reserves_its_emits_for_no_prior_only() {
    let e = engine(reservation_tools());
    let mut log = vec![opened(&e)];
    assert_eq!(check_call(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
    let send = call("send", json!({}));
    let dispatch = open(&e, &mut log, &send);
    match check_call(&e, &log, &call("guard", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected a reservation-failed no_prior, got {other:?}"),
    }
    match check_call(&e, &log, &call("wants", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected prior unfulfilled by a reservation, got {other:?}"),
    }
    close(
        &e,
        &mut log,
        &dispatch,
        &send,
        crate::admit::ResultAdmission::SuccessRaw {
            body: ValueBody::new("sent"),
        },
    );
    match check_call(&e, &log, &call("guard", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected a committed-effect no_prior failure, got {other:?}"),
    }
    assert_eq!(check_call(&e, &log, &call("wants", json!({}))), CheckOutcome::Allow);
}

#[test]
fn a_failed_dispatch_evaporates_its_reservation() {
    let e = engine(reservation_tools());
    let mut log = vec![opened(&e)];
    let send = call("send", json!({}));
    let dispatch = open(&e, &mut log, &send);
    close(&e, &mut log, &dispatch, &send, crate::admit::ResultAdmission::Failure);
    assert_eq!(check_call(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
    match check_call(&e, &log, &call("wants", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected prior still unmet, got {other:?}"),
    }
}

#[test]
fn an_indeterminate_close_keeps_the_reservation() {
    let e = engine(reservation_tools());
    let mut log = vec![opened(&e)];
    let send = call("send", json!({}));
    let dispatch = open(&e, &mut log, &send);
    close(
        &e,
        &mut log,
        &dispatch,
        &send,
        crate::admit::ResultAdmission::Indeterminate,
    );
    let p = Projection::build(&log, log.len() as u64);
    assert!(!p.view(&traj()).is_open(&dispatch), "the dispatch is closed");
    match check_call(&e, &log, &call("guard", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected the reservation to outlive the close, got {other:?}"),
    }
    match check_call(&e, &log, &call("wants", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected prior unmet, got {other:?}"),
    }
}

#[test]
fn two_reservations_of_one_kind_settle_independently() {
    let e = engine(reservation_tools());
    let mut log = vec![opened(&e)];
    let send = call("send", json!({}));
    let first = open(&e, &mut log, &send);
    let second = open(&e, &mut log, &send);
    assert_ne!(first, second, "a repeat call is a new dispatch occurrence");
    close(&e, &mut log, &first, &send, crate::admit::ResultAdmission::Failure);
    match check_call(&e, &log, &call("guard", json!({}))) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected the second reservation to hold, got {other:?}"),
    }
    close(&e, &mut log, &second, &send, crate::admit::ResultAdmission::Failure);
    assert_eq!(check_call(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
}

#[test]
fn a_calls_own_emits_never_fail_its_own_check() {
    let selfguard = ToolAnnotation {
        emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
        requires: Requires {
            history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
            ..Requires::default()
        },
        ..plain_tool("selfguard")
    };
    let e = engine(vec![selfguard]);
    let mut log = vec![opened(&e)];
    let c = call("selfguard", json!({}));
    assert_eq!(check_call(&e, &log, &c), CheckOutcome::Allow);
    let _dispatch = open(&e, &mut log, &c);
    match check_call(&e, &log, &c) {
        CheckOutcome::Block(b) => {
            assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
        }
        other => panic!("expected the open dispatch to reserve, got {other:?}"),
    }
}

#[test]
fn a_success_checkpoint_settles_while_the_dispatch_stays_open() {
    let scan = ToolAnnotation {
        emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
        ..plain_tool("scan")
    };
    let tools = vec![
        scan,
        history_guarded("guard_read", HistoryRequirement::NoPrior(EffectKind::new("read"))),
        history_guarded("wants_read", HistoryRequirement::Prior(EffectKind::new("read"))),
    ];
    let e = engine(tools);
    let mut log = vec![opened(&e)];
    let scan_call = call("scan", json!({}));
    let dispatch = open(&e, &mut log, &scan_call);
    assert!(matches!(
        check_call(&e, &log, &call("guard_read", json!({}))),
        CheckOutcome::Block(_)
    ));
    assert!(matches!(
        check_call(&e, &log, &call("wants_read", json!({}))),
        CheckOutcome::Block(_)
    ));
    let p = Projection::build(&log, log.len() as u64);
    let batch = admit::observe_success(
        &e.registry,
        &p.view(&traj()),
        &dispatch,
        &scan_call,
        ObservedResult::Unavailable,
    )
    .unwrap();
    log.extend(batch);
    let p = Projection::build(&log, log.len() as u64);
    assert!(p.view(&traj()).is_open(&dispatch));
    assert_eq!(
        check_call(&e, &log, &call("wants_read", json!({}))),
        CheckOutcome::Allow
    );
    assert!(matches!(
        check_call(&e, &log, &call("guard_read", json!({}))),
        CheckOutcome::Block(_)
    ));
}
