use super::*;
use crate::authority::{Attends, Authority, DeclaredTransition, Mandate, Sanitizer, SanitizerPoints, Scope};
use crate::intercept::{Interceptions, InterceptorAnswer, InterceptorDeclaration, Verdict};
use crate::names::{AuthorityName, InterceptorName, SanitizerName, TagName};

fn judged_by_orlando() -> TagName {
    TagName::new(format!("{}orlando", InterceptorDeclaration::TAG_PREFIX))
}

/// `commit` is judged by the `orlando` Interceptor and bound to `strip` (a sanitizer that
/// rewrites without a label change) and to `reviewer` (an authority with no mandate of its
/// own). `officer` attends every mark but is bound to no tool. `push` is judged by nothing.
fn intercepting_engine() -> Engine {
    let commit = ToolAnnotation {
        tags: vec![
            Scope::sanitizer_unblock_tag("strip"),
            Scope::authority_unblock_tag("reviewer"),
            judged_by_orlando(),
        ],
        ..plain_tool("commit")
    };
    let cfg = RegistryConfig {
        authorities: vec![
            Authority {
                name: AuthorityName::new("reviewer"),
                mandate: Mandate::default(),
                scope: Scope {
                    tags: vec![Scope::authority_unblock_tag("reviewer")],
                },
                hint: None,
            },
            Authority {
                name: AuthorityName::new("officer"),
                mandate: Mandate {
                    attends: Attends::Any,
                    ..Mandate::default()
                },
                scope: Scope::default(),
                hint: None,
            },
        ],
        sanitizers: vec![Sanitizer {
            name: SanitizerName::new("strip"),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: DeclaredTransition::Unchanged,
            scope: Scope {
                tags: vec![Scope::sanitizer_unblock_tag("strip")],
            },
            hint: None,
        }],
        interceptors: vec![InterceptorDeclaration {
            name: InterceptorName::new("orlando"),
            scope: Scope {
                tags: vec![judged_by_orlando()],
            },
        }],
        ..test_config(vec![commit, plain_tool("push")])
    };
    open_engine(cfg)
}

fn judged(call: ResolvedCall, verdict: Verdict) -> ResolvedCall {
    let digest = call.digest();
    call.with_interceptions(Some(Interceptions::new(
        digest,
        [(
            InterceptorName::new("orlando"),
            InterceptorAnswer {
                verdict,
                reason: Some("the message names Orlando".into()),
            },
        )]
        .into(),
    )))
}

fn commit(message: &str) -> ResolvedCall {
    call("commit", json!({ "message": message }))
}

fn decided(decision: &EngineDecision) -> (&[Released], &[Blocked]) {
    answered(decision)
}

#[test]
fn a_yes_releases_the_call() {
    let e = intercepting_engine();
    let decision = proposed(
        &e,
        &internal_log(&e),
        "b1",
        nonce(),
        judged(commit("fix"), Verdict::Yes),
    )
    .unwrap();
    let (released, blocked) = decided(&decision);
    assert_eq!(released.len(), 1);
    assert!(blocked.is_empty());
}

#[test]
fn a_no_blocks_with_the_bound_unblockers_only() {
    let e = intercepting_engine();
    let decision = proposed(
        &e,
        &internal_log(&e),
        "b1",
        nonce(),
        judged(commit("fix for Orlando"), Verdict::No),
    )
    .unwrap();
    let (released, blocked) = decided(&decision);
    assert!(released.is_empty());
    assert_eq!(
        blocked[0].block.raw.requirement_gaps,
        vec![Gap::Intercepted {
            interceptor: InterceptorName::new("orlando"),
            verdict: Verdict::No,
        }]
    );
    let plans = opened_offers(&appended_facts(decision));
    let hops: Vec<_> = plans.iter().filter_map(|(_, plan)| plan.hop()).collect();
    assert_eq!(hops, vec![&SanitizerName::new("strip")]);
    let rulings: Vec<_> = plans
        .iter()
        .flat_map(|(_, plan)| plan.required.iter().map(|ruling| ruling.authority.as_str()))
        .collect();
    // `officer` attends every mark, but an Interceptor's answer is not a mark: only the
    // authority bound to the tool lifts it.
    assert_eq!(rulings, vec!["reviewer"]);
}

#[test]
fn an_ask_is_lifted_by_a_bound_authority_and_never_by_a_rewrite() {
    let e = intercepting_engine();
    let decision = proposed(
        &e,
        &internal_log(&e),
        "b1",
        nonce(),
        judged(commit("fix"), Verdict::Ask),
    )
    .unwrap();
    let plans = opened_offers(&appended_facts(decision));
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].1.hop(), None);
    assert_eq!(plans[0].1.required[0].authority.as_str(), "reviewer");
}

#[test]
fn a_call_the_interceptor_judges_needs_its_answer() {
    let e = intercepting_engine();
    assert!(matches!(
        proposed(&e, &internal_log(&e), "b1", nonce(), commit("fix")),
        Err(TransitionError::InterceptionNeeded { interceptors }) if interceptors == [InterceptorName::new("orlando")]
    ));
}

#[test]
fn answers_about_another_call_or_from_a_stranger_are_refused() {
    let e = intercepting_engine();
    let elsewhere = commit("fix").with_interceptions(judged(commit("other"), Verdict::Yes).interceptions().cloned());
    assert!(matches!(
        proposed(&e, &internal_log(&e), "b1", nonce(), elsewhere),
        Err(TransitionError::ForeignInterceptions { .. })
    ));
    assert!(matches!(
        proposed(
            &e,
            &internal_log(&e),
            "b2",
            nonce(),
            judged(call("push", json!({})), Verdict::Yes)
        ),
        Err(TransitionError::ForeignInterceptions { .. })
    ));
}

#[test]
fn a_bound_authority_approval_releases_the_judged_call() {
    let e = intercepting_engine();
    let mut log = internal_log(&e);
    let blocked = judged(commit("fix for Orlando"), Verdict::No);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), blocked.clone()).unwrap());
    log.extend(facts.clone());
    let (offer, plan) = opened_offers(&facts)
        .into_iter()
        .find(|(_, plan)| plan.hop().is_none())
        .expect("the ruling plan is offered");
    let approved = execute_offer(
        &e,
        &log,
        offer,
        OfferOutcome::Approved(evidence_for(offer, &plan, "commit", log_label(&e, &log))),
    )
    .expect("the bound authority's ruling is admitted");
    log.extend(appended_facts(approved));
    let decision = proposed(&e, &log, "b2", nonce(), blocked).unwrap();
    let (released, _) = decided(&decision);
    assert_eq!(released.len(), 1, "the approval releases the exact judged call");
    log.extend(appended_facts(decision));
    assert_eq!(e.validate_replay(&log), Ok(()), "the pinned answers replay");
}

#[test]
fn a_rewrite_the_interceptor_accepts_approves_the_rewritten_call() {
    let e = intercepting_engine();
    let mut log = internal_log(&e);
    let blocked = judged(commit("fix for Orlando"), Verdict::No);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), blocked.clone()).unwrap());
    log.extend(facts.clone());
    let offer = hop_named(&facts, "strip");
    let rewritten = judged(commit("fix for the client"), Verdict::Yes);
    let decision = execute_offer(&e, &log, offer, rewrite(&blocked, &rewritten)).expect("the rewrite is admitted");
    assert_eq!(approved_by(&decision).arguments(), rewritten.arguments());
}

#[test]
fn a_rewrite_the_interceptor_still_refuses_does_not_approve() {
    let e = intercepting_engine();
    let mut log = internal_log(&e);
    let blocked = judged(commit("fix for Orlando"), Verdict::No);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), blocked.clone()).unwrap());
    log.extend(facts.clone());
    let offer = hop_named(&facts, "strip");
    let still = judged(commit("fix for ORLANDO"), Verdict::No);
    let outcome = execute_offer(&e, &log, offer, rewrite(&blocked, &still));
    assert!(
        !matches!(outcome.as_ref().map(offer_answer), Ok(OfferFollowUp::Approved { .. })),
        "a rewrite that clears nothing approves nothing: {outcome:?}"
    );
}

#[test]
fn a_sanitizer_claiming_no_label_change_must_name_its_tools() {
    let cfg = RegistryConfig {
        sanitizers: vec![Sanitizer {
            name: SanitizerName::new("strip"),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: DeclaredTransition::Unchanged,
            scope: Scope::default(),
            hint: None,
        }],
        ..test_config(vec![plain_tool("commit")])
    };
    assert!(matches!(
        crate::registry::Registry::build_covered(cfg),
        Err(crate::registry::LoadError::UnchangedSanitizer(name)) if name == "strip"
    ));
}

fn rewrite(blocked: &ResolvedCall, rewritten: &ResolvedCall) -> OfferOutcome {
    OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
        sanitizer: SanitizerName::new("strip"),
        source: crate::value::RawResultDigest::of(blocked.canonical_arguments().canonical_bytes()),
        derived: ValueBody::new(String::from_utf8(rewritten.canonical_arguments().canonical_bytes().to_vec()).unwrap()),
        annotation: None,
        interceptions: rewritten.interceptions().cloned(),
    })
}

fn log_label(e: &Engine, log: &[Fact]) -> Label {
    viewing(e, log)
        .views(&traj())
        .expect("the trajectory is opened")
        .current_label()
}

fn without_interceptions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.remove("interceptions");
            fields.values_mut().for_each(without_interceptions);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(without_interceptions),
        _ => {}
    }
}

#[test]
fn replay_refuses_a_log_that_lost_the_answers() {
    let e = intercepting_engine();
    let mut log = internal_log(&e);
    let judged = judged(commit("fix"), Verdict::Yes);
    log.extend(appended_facts(proposed(&e, &log, "b1", nonce(), judged).unwrap()));
    assert_eq!(e.validate_replay(&log), Ok(()));
    let mut forged = serde_json::to_value(&log).unwrap();
    without_interceptions(&mut forged);
    let forged: Vec<Fact> = serde_json::from_value(forged).unwrap();
    assert!(
        e.validate_replay(&forged).is_err(),
        "a judged call without its answers does not replay"
    );
}

#[test]
fn a_bound_authority_attends_only_on_its_tools() {
    let restricted = ToolAnnotation {
        requires: Requires {
            attention: vec![MarkName::new("review")],
            ..Requires::default()
        },
        ..plain_tool("restricted")
    };
    let safe = ToolAnnotation {
        tags: vec![Scope::authority_unblock_tag("lead")],
        ..plain_tool("safe")
    };
    let e = open_engine(RegistryConfig {
        authorities: vec![Authority {
            name: AuthorityName::new("lead"),
            mandate: Mandate {
                attends: Attends::Any,
                ..Mandate::default()
            },
            scope: Scope {
                tags: vec![Scope::authority_unblock_tag("lead")],
            },
            hint: None,
        }],
        ..test_config(vec![restricted, safe])
    });
    let decision = proposed(&e, &internal_log(&e), "b1", nonce(), call("restricted", json!({}))).unwrap();
    assert!(
        opened_offers(&appended_facts(decision)).is_empty(),
        "an authority bound to `safe` approves nothing on `restricted`"
    );
}

#[test]
fn a_component_cannot_carry_another_components_binding() {
    let cfg = RegistryConfig {
        authorities: vec![Authority {
            name: AuthorityName::new("lead"),
            mandate: Mandate::default(),
            scope: Scope {
                tags: vec![Scope::sanitizer_unblock_tag("strip")],
            },
            hint: None,
        }],
        ..test_config(vec![plain_tool("commit")])
    };
    assert!(matches!(
        crate::registry::Registry::build_covered(cfg),
        Err(crate::registry::LoadError::ForeignUnblockTag(name)) if name == "lead"
    ));
}

#[test]
fn a_rewrite_cannot_leave_the_interceptor_behind() {
    // Only commits starting "release" are judged; a rewrite to any other message selects the
    // plain declaration, which no Interceptor judges.
    let judged_release = ToolAnnotation {
        name: ToolName::new("commit(message:release*)"),
        tags: vec![Scope::sanitizer_unblock_tag("strip"), judged_by_orlando()],
        ..plain_tool("commit")
    };
    let plain = ToolAnnotation {
        tags: vec![Scope::sanitizer_unblock_tag("strip")],
        ..plain_tool("commit")
    };
    let e = open_engine(RegistryConfig {
        sanitizers: vec![Sanitizer {
            name: SanitizerName::new("strip"),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: DeclaredTransition::Unchanged,
            scope: Scope {
                tags: vec![Scope::sanitizer_unblock_tag("strip")],
            },
            hint: None,
        }],
        interceptors: vec![InterceptorDeclaration {
            name: InterceptorName::new("orlando"),
            scope: Scope {
                tags: vec![judged_by_orlando()],
            },
        }],
        ..test_config(vec![judged_release, plain])
    });
    let mut log = internal_log(&e);
    let blocked = judged(commit("release for Orlando"), Verdict::No);
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), blocked.clone()).unwrap());
    log.extend(facts.clone());
    let offer = hop_named(&facts, "strip");
    let escaped = commit("chore");
    let outcome = execute_offer(&e, &log, offer, rewrite(&blocked, &escaped));
    assert!(
        !matches!(outcome.as_ref().map(offer_answer), Ok(OfferFollowUp::Approved { .. })),
        "a rewrite onto a declaration no Interceptor judges is not approved: {outcome:?}"
    );
}

#[test]
fn an_interceptor_may_judge_tools_by_an_ordinary_tag() {
    let reviewed = ToolAnnotation {
        tags: vec![TagName::new("reviewed")],
        ..plain_tool("commit")
    };
    let cfg = RegistryConfig {
        interceptors: vec![InterceptorDeclaration {
            name: InterceptorName::new("check"),
            scope: Scope {
                tags: vec![TagName::new("reviewed")],
            },
        }],
        ..test_config(vec![reviewed])
    };
    assert!(crate::registry::Registry::build_covered(cfg).is_ok());
}
