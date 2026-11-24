use super::*;
use crate::engine::ReceiptRefusal;
use crate::transition::{AnswerReport, ReleasedAt};
use appa_runtime_api::ReleaseDigest;

fn internal() -> Label {
    Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")]))
}

fn answer(e: &Engine, log: &[Fact], body: &str) -> EngineDecision {
    e.handle(
        &viewing(e, log),
        EngineEvent::Answer(AnswerReport {
            trajectory: traj(),
            digest: ReleaseDigest::of(body.as_bytes()),
        }),
    )
    .expect("the root answers")
}

fn released(decision: &EngineDecision) -> ReleasedAt {
    match decision.follow_up {
        FollowUp::Released { at } => at,
        ref other => panic!("an answer follows up with its record, got {other:?}"),
    }
}

fn position(decision: &EngineDecision) -> u64 {
    released(decision).position
}

/// The root's answer, recorded: the log with it, and the position its receipt names.
fn answered_log(e: &Engine, body: &str) -> (Vec<Fact>, u64) {
    let mut log = vec![opened(e)];
    let decision = answer(e, &log, body);
    let at = position(&decision);
    log.extend(appended_facts(decision));
    (log, at)
}

#[test]
fn a_root_answer_is_recorded_at_the_roots_label_and_its_receipt_names_it() {
    let e = engine_at(vec![crm_tool()], internal());
    let (log, at) = answered_log(&e, "done");
    let digest = ReleaseDigest::of(b"done");
    assert!(matches!(
        &log[at as usize],
        Fact::AnswerReleased { trajectory, digest: recorded, label }
            if *trajectory == traj() && *recorded == digest && *label == internal()
    ));
    let view = viewing(&e, &log);
    assert_eq!(e.released_label(&view, at, &digest), Ok(internal()));
}

#[test]
fn an_empty_answer_is_an_answer() {
    let e = engine_at(vec![crm_tool()], internal());
    let (log, at) = answered_log(&e, "");
    assert_eq!(
        e.released_label(&viewing(&e, &log), at, &ReleaseDigest::of(b"")),
        Ok(internal())
    );
}

#[test]
fn the_same_answer_again_records_nothing_and_another_records_anew() {
    let e = engine_at(vec![crm_tool()], internal());
    let (mut log, first) = answered_log(&e, "done");
    let again = answer(&e, &log, "done");
    assert!(
        again.append.is_none(),
        "a retried answer with the same bytes appends nothing"
    );
    assert_eq!(
        released(&again),
        ReleasedAt {
            position: first,
            digest: ReleaseDigest::of(b"done")
        }
    );

    let changed = answer(&e, &log, "done, differently");
    let second = position(&changed);
    log.extend(appended_facts(changed));
    assert!(second > first);
    let view = viewing(&e, &log);
    let digest = ReleaseDigest::of(b"done, differently");
    assert_eq!(e.released_label(&view, second, &digest), Ok(internal()));
    assert_eq!(
        e.released_label(&view, second, &ReleaseDigest::of(b"done")),
        Err(ReceiptRefusal::OtherBytes),
        "the earlier bytes do not stand at the later answer's position"
    );
    assert_eq!(released(&answer(&e, &log, "done, differently")).position, second);
}

#[test]
fn a_receipt_naming_a_record_that_released_nothing_is_refused() {
    let e = engine_at(vec![crm_tool()], internal());
    let (log, at) = answered_log(&e, "done");
    let view = viewing(&e, &log);
    let digest = ReleaseDigest::of(b"done");
    assert_eq!(e.released_label(&view, 0, &digest), Err(ReceiptRefusal::NotReleased));
    assert_eq!(
        e.released_label(&view, at + 1, &digest),
        Err(ReceiptRefusal::NotReleased)
    );
}

#[test]
fn an_answer_recorded_at_another_label_than_the_roots_is_refused_on_replay() {
    let e = engine_at(vec![crm_tool()], internal());
    let (log, at) = answered_log(&e, "done");
    let mut forged = log.clone();
    if let Fact::AnswerReleased { label, .. } = &mut forged[at as usize] {
        *label = Label::top();
    }
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::AnswerLabelMismatch));
}

#[test]
fn an_answer_outside_its_own_act_or_off_the_root_is_refused_on_replay() {
    let e = engine_at(vec![crm_tool()], internal());
    let (log, at) = answered_log(&e, "done");
    let mut undeclared = log.clone();
    undeclared.remove(at as usize - 1);
    assert_eq!(e.validate_replay(&undeclared), Err(TransitionRefusal::AnswerOffRoot));

    let child = TrajectoryId::new("t/child");
    let log = spawn_family(&e, &child);
    let declared = e
        .handle(
            &viewing(&e, &log),
            EngineEvent::Answer(AnswerReport {
                trajectory: child,
                digest: ReleaseDigest::of(b"done"),
            }),
        )
        .map(|decision| decision.append.is_some());
    assert!(
        matches!(
            declared,
            Err(TransitionError::Invalid(TransitionRefusal::AnswerOffRoot))
        ),
        "a child records no answer, got {declared:?}"
    );
}

#[test]
fn a_sink_judgment_reads_the_contract_against_no_history() {
    let audience_bound = ToolAnnotation {
        requires: Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(readers(&[
                    "outsider",
                ])))],
            },
            history: Vec::new(),
            attention: Vec::new(),
        },
        ..plain_tool("post")
    };
    let historied = ToolAnnotation {
        requires: Requires {
            history: vec![HistoryRequirement::NoPrior(EffectKind::new("spend"))],
            ..Requires::default()
        },
        ..plain_tool("audit_post")
    };
    let e = engine(vec![audience_bound, historied]);
    let judge = |tool: &str, label: &Label| {
        e.judge_release(
            label,
            ToolName::new(tool),
            b"{}",
            crate::audience::AudienceEvidence::default(),
            None,
            |_| false,
        )
    };
    assert_eq!(
        judge("post", &Label::top()).expect("judged"),
        ReleaseJudgment::Released {
            approved_by: Vec::new()
        }
    );
    assert!(matches!(
        judge("post", &internal()).expect("judged"),
        ReleaseJudgment::Withheld { gaps } if !gaps.is_empty()
    ));
    assert!(matches!(
        judge("audit_post", &Label::top()),
        Err(ReleaseError::Sink(SinkRefusal::HistoryRequirement))
    ));
    assert!(matches!(
        judge("undeclared", &Label::top()),
        Err(ReleaseError::Sink(SinkRefusal::Undeclared))
    ));
}

#[test]
fn a_sink_an_annotator_answers_for_is_not_judged() {
    let mut cfg = test_config(Vec::new());
    cfg.annotators.push(annotator("classifier"));
    cfg.tools.push(annotated(plain_tool("post"), "classifier"));
    let e = open_engine(cfg);
    assert!(matches!(
        e.judge_release(
            &Label::top(),
            ToolName::new("post"),
            b"{}",
            crate::audience::AudienceEvidence::default(),
            None,
            |_| true,
        ),
        Err(ReleaseError::Sink(SinkRefusal::Annotated))
    ));
}

#[test]
fn a_gap_is_lifted_only_by_an_authority_the_judgment_admits() {
    let mut cfg = test_config(vec![ToolAnnotation {
        requires: Requires {
            label: LabelRequirements {
                trust_floor: Some(TRUSTED),
                audience: Vec::new(),
            },
            ..Requires::default()
        },
        ..plain_tool("post")
    }]);
    cfg.authorities.push(crate::authority::Authority {
        name: crate::names::AuthorityName::new("auto"),
        mandate: crate::authority::Mandate {
            trust_ceiling: Some(TRUSTED),
            ..Default::default()
        },
        scope: crate::authority::Scope::default(),
        except: Vec::new(),
        hint: None,
    });
    let e = open_engine(cfg);
    let suspicious = Label::new(SUSPICIOUS, Audience::public());
    let judge = |admitted: bool| {
        e.judge_release(
            &suspicious,
            ToolName::new("post"),
            b"{}",
            crate::audience::AudienceEvidence::default(),
            None,
            |_| admitted,
        )
        .expect("judged")
    };
    assert_eq!(
        judge(true),
        ReleaseJudgment::Released {
            approved_by: vec![crate::names::AuthorityName::new("auto")]
        }
    );
    assert!(matches!(judge(false), ReleaseJudgment::Withheld { .. }));
}

fn review(
    e: &Engine,
    log: &[Fact],
    offer: crate::value::OfferId,
    body: &str,
    shown: bool,
) -> Result<EngineDecision, TransitionError> {
    e.handle(
        &viewing(e, log),
        EngineEvent::Review(crate::transition::ReviewReport {
            offer,
            digest: ReleaseDigest::of(body.as_bytes()),
            shown,
        }),
    )
}

#[test]
fn a_review_is_recorded_only_as_bytes_an_answer_returned_at_its_blocks_label() {
    let e = engine(vec![crm_tool(), internal_read()]);
    let mut log = vec![opened(&e)];
    let at_block = viewing(&e, &log).projection().view(&traj()).current_label();
    log.extend(appended_facts(blocked_batch(&e, &log, "b1", nonce())));
    let offer = opened_offers(&log)[0].0;
    reads(&e, &mut log, &traj(), "read_internal");
    let now = viewing(&e, &log).projection().view(&traj()).current_label();
    assert_ne!(now, at_block, "the read moved the root's label past the block's");

    assert_eq!(
        review(&e, &log, offer, "the review", false).map(|decision| decision.append.is_some()),
        Err(TransitionError::OtherReview),
        "bytes no answer returned release nothing"
    );

    let recorded = review(&e, &log, offer, "the review", true).expect("the review records");
    let at = position(&recorded);
    log.extend(appended_facts(recorded));
    let digest = ReleaseDigest::of(b"the review");
    assert!(matches!(
        &log[at as usize],
        Fact::ReviewReleased { trajectory, offer: named, digest: recorded, label }
            if *trajectory == traj() && *named == offer && *recorded == digest && *label == at_block
    ));
    assert_eq!(e.released_label(&viewing(&e, &log), at, &digest), Ok(at_block));

    let again = review(&e, &log, offer, "the review", false).expect("the standing review answers");
    assert!(again.append.is_none(), "a repeated review appends nothing");
    assert_eq!(released(&again), ReleasedAt { position: at, digest });

    let mut forged = log.clone();
    if let Fact::ReviewReleased { label, .. } = &mut forged[at as usize] {
        *label = now;
    }
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ReviewMismatch));
}
