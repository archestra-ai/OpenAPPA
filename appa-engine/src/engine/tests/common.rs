use super::*;

pub(super) const SUSPICIOUS: Trust = Trust::new(0);

pub(super) const TRUSTED: Trust = Trust::new(1);

pub(super) fn nonce() -> crate::value::OfferNonce {
    crate::value::OfferNonce::new([7u8; 32])
}

pub(super) fn traj() -> TrajectoryId {
    TrajectoryId::new("t")
}

pub(super) fn engine(tools: Vec<ToolAnnotation>) -> Engine {
    open_engine(test_config(tools))
}

pub(super) fn engine_at(tools: Vec<ToolAnnotation>, starting: Label) -> Engine {
    open_engine_at(test_config(tools), starting)
}

pub(super) fn declared(tools: Vec<ToolAnnotation>) -> Vec<crate::contract::ToolDeclaration> {
    tools
        .into_iter()
        .map(crate::contract::ToolDeclaration::Declared)
        .collect()
}

/// A complete annotation as `annotator` produced it for `bound_to`'s exact canonical call.
pub(super) fn pinned_for(
    produced: ToolAnnotation,
    annotator: &str,
    bound_to: &ResolvedCall,
) -> crate::contract::PinnedAnnotation {
    crate::contract::PinnedAnnotation::new(
        crate::names::AnnotatorName::new(annotator),
        bound_to.digest(),
        crate::contract::ProducedAnnotation {
            delta: produced.delta,
            forwards: produced.forwards,
            emits: produced.emits,
            requires: produced.requires,
        },
    )
}

/// An Annotated declaration carrying `tool`'s operational metadata, routed through `annotator`.
pub(super) fn annotated(tool: ToolAnnotation, annotator: &str) -> crate::contract::ToolDeclaration {
    crate::contract::ToolDeclaration::Annotated {
        name: tool.name,
        tags: tool.tags,
        description: tool.description,
        parameters: tool.parameters,
        annotator: crate::names::AnnotatorName::new(annotator),
        on_no_answer: None,
    }
}

/// The policy's wildcard: `name = "*"`, no metadata, routed through `by`.
pub(super) fn wildcard(by: &str) -> crate::contract::ToolDeclaration {
    crate::contract::ToolDeclaration::Annotated {
        name: ToolName::new(crate::registry::WILDCARD_SPELLING),
        tags: vec![],
        description: None,
        parameters: crate::params::ToolParameters::open(),
        annotator: crate::names::AnnotatorName::new(by),
        on_no_answer: None,
    }
}

/// An Annotator registration with no bounds: every omitted bound resolves to the whole policy
/// vocabulary at load.
pub(super) fn annotator(name: &str) -> crate::registry::AnnotatorDeclaration {
    crate::registry::AnnotatorDeclaration {
        name: crate::names::AnnotatorName::new(name),
        trust: None,
        audiences: None,
        marks: None,
        effects: None,
    }
}

pub(super) fn annotator_with_readers(name: &str, readers: &[&str]) -> crate::registry::AnnotatorDeclaration {
    let entries: Vec<String> = readers.iter().map(|reader| reader.to_string()).collect();
    crate::registry::AnnotatorDeclaration {
        audiences: Some(
            crate::registry::AudienceVocabulary::parse_entries(&entries).expect("a fixture vocabulary parses"),
        ),
        ..annotator(name)
    }
}

pub(super) fn test_config(tools: Vec<ToolAnnotation>) -> RegistryConfig {
    RegistryConfig {
        annotators: vec![],
        trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
        tools: declared(tools),
        authorities: vec![],
        sanitizers: vec![],
        audience: crate::audience::AudienceConfig::default(),
    }
}

pub(super) fn opened(e: &Engine) -> Fact {
    opened_root(e, &traj())
}

pub(super) fn opened_root(e: &Engine, trajectory: &TrajectoryId) -> Fact {
    e.open_trajectory(trajectory, crate::profile::PolicyFileKey::of(b"policy"), None)
        .expect("the engine opens its own root")
        .into_unsealed()
        .remove(0)
}

pub(super) fn read_tool(name: &str, delta: Delta) -> ToolAnnotation {
    ToolAnnotation {
        delta,
        ..plain_tool(name)
    }
}

pub(super) fn suspicious_read() -> ToolAnnotation {
    read_tool(
        "read_suspicious",
        Delta {
            trust: Some(SUSPICIOUS),
            audience: None,
        },
    )
}

pub(super) fn internal_read() -> ToolAnnotation {
    read_tool(
        "read_internal",
        Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                "insider",
            )]))),
        },
    )
}

pub(super) fn stray_admission(trajectory: &TrajectoryId, label: Label) -> Fact {
    Fact::ValueAdmitted {
        trajectory: trajectory.clone(),
        value: LabeledValue::new(ValueBody::new("stray"), label),
        provenance: Provenance::ToolResult {
            dispatch: DispatchId::new(trajectory.clone(), call("stray", json!({})).digest(), 0),
        },
    }
}

pub(super) fn suspicious_internal_read() -> ToolAnnotation {
    read_tool(
        "read_suspicious_internal",
        Delta {
            trust: Some(SUSPICIOUS),
            audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                "insider",
            )]))),
        },
    )
}

pub(super) fn reads(
    e: &Engine,
    log: &mut Vec<Fact>,
    trajectory: &TrajectoryId,
    tool: &str,
) -> crate::value::DispatchId {
    let call = call(tool, json!({ "who": "someone" }));
    let id = format!("read-{tool}-{}", log.len());
    let decision = e
        .handle(
            &viewing(e, log),
            batch_on(trajectory, &id, Vec::new(), vec![raw(&call)], None),
        )
        .expect("a registered read decides");
    let dispatch = match answered(&decision) {
        ([release], []) => {
            let dispatch = release.dispatch.clone();
            log.extend(appended_facts(decision));
            dispatch
        }
        ([], [block]) => {
            let accepting = block
                .block
                .plans
                .iter()
                .filter_map(crate::plan::RemedyPlan::executable)
                .find(|plan| plan.narrowing().is_some() && plan.sanitizer().is_none() && plan.required.is_empty())
                .expect("a narrowing read offers its acceptance")
                .id;
            let offer = block
                .offers
                .iter()
                .find_map(|(offer, plan)| (*plan == accepting).then_some(*offer))
                .expect("the acceptance plan is offered");
            log.extend(appended_facts(decision));
            let approved = e
                .handle(
                    &viewing(e, log),
                    EngineEvent::ExecuteOffer(OfferExecution {
                        trajectory: trajectory.clone(),
                        offer,
                        outcome: OfferOutcome::Approved(Vec::new()),
                        return_policy: None,
                        offer_nonce: nonce(),
                        audience: crate::audience::AudienceEvidence::default(),
                    }),
                )
                .expect("the acceptance approves the read");
            assert!(matches!(
                approved.follow_up,
                FollowUp::Offer(OfferFollowUp::Approved { .. })
            ));
            log.extend(appended_facts(approved));
            let released = e
                .handle(
                    &viewing(e, log),
                    batch_on(
                        trajectory,
                        &format!("{id}-approved"),
                        Vec::new(),
                        vec![raw(&call)],
                        None,
                    ),
                )
                .expect("the approved read releases");
            let dispatch = match answered(&released) {
                ([release], []) => release.dispatch.clone(),
                other => panic!("the approved read releases, got {other:?}"),
            };
            log.extend(appended_facts(released));
            dispatch
        }
        other => panic!("one read decides one way, got {other:?}"),
    };
    let admitted = e
        .handle(
            &viewing(e, log),
            EngineEvent::Outcome(ToolReport {
                dispatch: dispatch.clone(),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available(ValueBody::new("read")),
                },
                evidence: Vec::new(),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the raw result admits");
    log.extend(appended_facts(admitted));
    dispatch
}

/// Spawn `child` from the parent at the end of `log` under `policy`: the marked spawn
/// blocks with its return declarations, the parent takes the one naming the policy's
/// sanitizer and supplies the policy, the approved spawn releases and prepares the fork,
/// and the child binds. Returns the facts appended after `log`.
pub(super) fn spawn_family_at(e: &Engine, log: &[Fact], child: &TrajectoryId, policy: ReturnPolicy) -> Vec<Fact> {
    let spawn = call("spawn", json!({}));
    let batch = |id: &str| {
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(format!("{id}-{}", child.as_str())),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(&spawn)],
            spawn: Some(crate::transition::SpawnMark::at(0)),
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        })
    };
    let so_far = |facts: &[Fact]| [log, facts].concat();
    let blocked = e
        .handle(&viewing(e, log), batch("spawn"))
        .expect("a marked spawn blocks with its return declarations");
    assert!(
        matches!(answered(&blocked), ([], [_])),
        "a marked spawn blocks until the parent declares its child's return policy"
    );
    let mut facts = appended_facts(blocked);
    let offer = opened_offers(&facts)
        .into_iter()
        .find_map(|(offer, plan)| (plan.return_step() == Some(&policy.route())).then_some(offer))
        .expect("the block offers the policy's declaration");
    let approved = e
        .handle(
            &viewing(e, &so_far(&facts)),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(policy),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the declaration approves the spawn");
    facts.extend(appended_facts(approved));
    let released = e
        .handle(&viewing(e, &so_far(&facts)), batch("spawn-approved"))
        .expect("the approved spawn releases and prepares the fork");
    let fork = match answered(&released) {
        ([release], []) => release.fork.clone().expect("the marked spawn carries its fork"),
        other => panic!("the approved spawn releases alone, got {other:?}"),
    };
    facts.extend(appended_facts(released));
    let bound = e
        .handle(
            &viewing(e, &so_far(&facts)),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork,
                child: child.clone(),
            }),
        )
        .expect("the child binds to its fork");
    facts.extend(appended_facts(bound));
    facts
}

/// The parent declares the bare floor for the marked `spawn` it proposes under `id`: the
/// block's facts and the approval's, appended after `log`. The approved spawn is left for
/// the caller to propose again.
pub(super) fn declared_spawn(e: &Engine, log: &[Fact], id: &str) -> Vec<Fact> {
    let blocked = e
        .handle(
            &viewing(e, log),
            spawn_batch(id, Some(crate::transition::SpawnMark::at(0))),
        )
        .expect("a marked spawn blocks with its return declarations");
    let mut facts = appended_facts(blocked);
    let (offer, _) = opened_offers(&facts)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
        .expect("the bare declaration is offered");
    let approved = e
        .handle(
            &viewing(e, &[log, &facts].concat()),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(floor_policy(e, log)),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the declaration approves the spawn");
    facts.extend(appended_facts(approved));
    facts
}

/// The bare declaration: the parent's current label as the floor, no sanitizer.
pub(super) fn floor_policy(e: &Engine, log: &[Fact]) -> ReturnPolicy {
    ReturnPolicy::Crossing(floor_crossing(e, log))
}

pub(super) fn floor_crossing(e: &Engine, log: &[Fact]) -> crate::fact::CrossingReturn {
    crate::fact::CrossingReturn {
        floor: viewing(e, log)
            .views(&traj())
            .expect("the parent is opened")
            .current_label(),
        sanitizer: None,
    }
}

pub(super) fn forked_child(e: &Engine, log: &[Fact], child: &TrajectoryId) -> Vec<Fact> {
    spawn_family_at(e, log, child, floor_policy(e, log))
}

/// A parent opened alone, with `child` spawned under the bare declaration.
pub(super) fn spawn_family(e: &Engine, child: &TrajectoryId) -> Vec<Fact> {
    let log = vec![opened(e)];
    let forked = forked_child(e, &log, child);
    [log, forked].concat()
}

/// The child's stop crossing `body` at its fold: the facts appended after `log`.
pub(super) fn crossed_report(e: &Engine, log: &[Fact], child: &TrajectoryId, body: &str) -> Vec<Fact> {
    let report = child_report(
        log,
        child,
        ChildSubmission::Value {
            body: ValueBody::new(body),
        },
    );
    appended_facts(e.handle(&viewing(e, log), report).expect("the return crosses"))
}

pub(super) fn child_report(log: &[Fact], child: &TrajectoryId, submission: ChildSubmission) -> EngineEvent {
    EngineEvent::ChildReturn(ChildReport {
        child: child.clone(),
        fork: log
            .iter()
            .find_map(|fact| match fact {
                Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
                _ => None,
            })
            .expect("the log opened a fork for this child"),
        submission,
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    })
}

pub(super) fn open_engine(cfg: RegistryConfig) -> Engine {
    let starting = crate::profile::neutral_starting_label(&cfg.trust_chain);
    open_engine_at(cfg, starting)
}

pub(super) fn open_engine_at(mut cfg: RegistryConfig, starting: Label) -> Engine {
    if !cfg.tools.iter().any(|tool| tool.name().as_str() == "spawn") {
        cfg.tools
            .push(crate::contract::ToolDeclaration::Declared(plain_tool("spawn")));
    }
    let profile = crate::profile::ProfileDeclaration {
        starting_label: starting,
        ..crate::profile::covering_declaration(&cfg)
    };
    placed(
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            profile,
        })
        .unwrap(),
    )
}

/// `engine` deciding at the first revision of the test deployment line.
pub(super) fn placed(engine: Engine) -> Engine {
    engine.at(
        crate::profile::DeploymentName::new("default"),
        crate::profile::PolicyRevision::FIRST,
    )
}

pub(super) fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
    ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
}

pub(super) fn raw(call: &ResolvedCall) -> crate::transition::ProposedCall {
    crate::transition::ProposedCall {
        tool: call.tool().clone(),
        arguments: call.canonical_arguments().canonical_bytes().to_vec(),
        annotation: call.annotation().cloned(),
    }
}

pub(super) fn check_call(engine: &Engine, log: &[Fact], call: &ResolvedCall) -> CheckOutcome {
    let p = Projection::build(log, log.len() as u64);
    let t = traj();
    engine.check(&p.view(&t), call).unwrap()
}

pub(super) fn crm_tool() -> ToolAnnotation {
    ToolAnnotation {
        delta: Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                "insider",
            )]))),
        },
        requires: Requires {
            label: floor(TRUSTED),
            ..Requires::default()
        },
        ..plain_tool("get_ticket")
    }
}

pub(super) fn readers(names: &[&str]) -> DeclaredAudience {
    DeclaredAudience::restricted(names.iter().map(|name| ReaderId::new(*name)))
}

pub(super) fn floor(trust: Trust) -> LabelRequirements {
    LabelRequirements {
        trust_floor: Some(trust),
        audience: vec![],
    }
}

pub(super) fn open(e: &Engine, log: &mut Vec<Fact>, c: &ResolvedCall) -> crate::value::DispatchId {
    let id = format!("open-{}", log.len());
    let decision = e
        .handle(&viewing(e, log), batch_on(&traj(), &id, Vec::new(), vec![raw(c)], None))
        .expect("a registered call decides");
    let dispatch = match answered(&decision) {
        ([release], []) => release.dispatch.clone(),
        other => panic!("an allowed call releases, got {other:?}"),
    };
    log.extend(appended_facts(decision));
    dispatch
}

pub(super) fn close(
    e: &Engine,
    log: &mut Vec<Fact>,
    dispatch: &crate::value::DispatchId,
    c: &ResolvedCall,
    admission: crate::admit::ResultAdmission,
) {
    let p = Projection::build(log, log.len() as u64);
    let batch = admit::admit_result(
        &e.registry,
        &p.view(&traj()),
        dispatch,
        c,
        admission,
        &crate::label::TestContext::default().context(),
        &crate::audience::AudienceEvidence::default(),
    )
    .unwrap();
    log.extend(batch);
}

pub(super) fn substituting_engine(trust: Trust) -> Engine {
    let partner = Audience::restricted([ReaderId::new("partner")]);
    let post = post_tool;
    open_engine_at(
        RegistryConfig {
            tools: {
                let mut tools = declared(vec![
                    post("post", vec![crate::names::TagName::new("outbound")]),
                    post("post_untagged", vec![]),
                    restrictable_tool("ping"),
                ]);
                tools.push(annotated(
                    post("post_dyn", vec![crate::names::TagName::new("outbound")]),
                    "acl",
                ));
                tools
            },
            annotators: vec![annotator("acl")],
            authorities: vec![crate::authority::Authority {
                name: AuthorityName::new("officer"),
                mandate: crate::authority::Mandate {
                    trust_ceiling: Some(TRUSTED),
                    reader_ceiling: Some(DeclaredAudience::literal(partner)),
                    ..crate::authority::Mandate::default()
                },
                scope: crate::authority::Scope::default(),
                hint: None,
            }],
            sanitizers: vec![crate::authority::Sanitizer {
                name: crate::names::SanitizerName::new("redact"),
                on: crate::authority::SanitizerPoints {
                    input: true,
                    output: false,
                },
                transition: crate::authority::DeclaredTransition::Audience {
                    from_includes: DeclaredAudience::restricted([ReaderId::new("insider")]),
                    to: DeclaredAudience::restricted([ReaderId::new("insider"), ReaderId::new("partner")]),
                },
                scope: crate::authority::Scope {
                    tags: vec![crate::names::TagName::new("outbound")],
                },
                hint: None,
            }],
            ..test_config(vec![])
        },
        Label::new(trust, Audience::restricted([ReaderId::new("insider")])),
    )
}

pub(super) fn internal_log(e: &Engine) -> Vec<Fact> {
    vec![opened(e)]
}

pub(super) fn post_tool(name: &str, tags: Vec<crate::names::TagName>) -> ToolAnnotation {
    ToolAnnotation {
        tags,
        parameters: crate::params::ToolParameters::compile(&json!({
            "type": "object",
            "properties": { "body": { "type": "string" } },
            "required": ["body"],
        }))
        .unwrap(),
        emits: EffectSet::new([EffectKind::new("outbound.post")]).unwrap(),
        requires: Requires {
            label: LabelRequirements {
                trust_floor: Some(TRUSTED),
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                    DeclaredAudience::restricted([ReaderId::new("partner")]),
                ))],
            },
            ..Requires::default()
        },
        ..plain_tool(name)
    }
}

pub(super) fn substitution(call: &ResolvedCall, replacement: &str) -> OfferOutcome {
    OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
        sanitizer: crate::names::SanitizerName::new("redact"),
        source: crate::value::RawResultDigest::of(call.canonical_arguments().canonical_bytes()),
        derived: ValueBody::new(replacement),
        annotation: None,
    })
}

pub(super) const REDACTED: &str = r#"{"body":"[redacted]"}"#;

pub(super) fn quiet_subject() -> crate::basis::SubjectKey {
    crate::basis::SubjectKey::Call {
        trajectory: traj(),
        batch: crate::transition::ProposalBatchId::new("never-decided"),
        position: 0,
    }
}

pub(super) fn basis_of(e: &Engine, log: &[Fact]) -> crate::basis::PolicyBasis {
    e.view(&traj(), log.to_vec(), log.len() as u64)
        .expect("the log replays")
        .projection()
        .view(&traj())
        .basis_for(&quiet_subject())
}

pub(super) fn decide(e: &Engine, log: &[Fact], id: &str, call: &ResolvedCall) -> EngineDecision {
    let view = e
        .view(&traj(), log.to_vec(), log.len() as u64)
        .expect("the log replays");
    e.handle(
        &view,
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(id),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(call)],
            spawn: None,
            offer_nonce: nonce(),
            evidence: Vec::new(),
            audience: crate::audience::AudienceEvidence::default(),
        }),
    )
    .expect("the batch decides")
}

pub(super) fn appended_facts(decision: EngineDecision) -> Vec<Fact> {
    decision.append.expect("the decision appends").facts().to_vec()
}

pub(super) fn blocked_batch(e: &Engine, log: &[Fact], id: &str, nonce: crate::value::OfferNonce) -> EngineDecision {
    proposed(e, log, id, nonce, call("get_ticket", json!({}))).expect("the batch decides")
}

pub(super) fn proposed(
    e: &Engine,
    log: &[Fact],
    id: &str,
    nonce: crate::value::OfferNonce,
    proposal: ResolvedCall,
) -> Result<EngineDecision, TransitionError> {
    proposed_with(
        e,
        log,
        id,
        nonce,
        proposal,
        crate::audience::AudienceEvidence::default(),
    )
}

pub(super) fn proposed_with(
    e: &Engine,
    log: &[Fact],
    id: &str,
    nonce: crate::value::OfferNonce,
    proposal: ResolvedCall,
    audience: crate::audience::AudienceEvidence,
) -> Result<EngineDecision, TransitionError> {
    let view = e
        .view(&traj(), log.to_vec(), log.len() as u64)
        .expect("the log replays");
    e.handle(
        &view,
        EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new(id),
            trajectory: traj(),
            provider_results: Vec::new(),
            proposals: vec![raw(&proposal)],
            spawn: None,
            offer_nonce: nonce,
            evidence: Vec::new(),
            audience,
        }),
    )
}

pub(super) fn released_under_output_sanitizer(
    e: &Engine,
    log: Vec<Fact>,
    call: &ResolvedCall,
) -> (Vec<Fact>, DispatchId) {
    let blocked = appended_facts(proposed(e, &log, "san-block", nonce(), call.clone()).expect("the call blocks"));
    let (offer, _) = opened_offers(&blocked)
        .into_iter()
        .find(|(_, plan)| plan.sanitizer().is_some())
        .expect("a confined result point offers the sanitize settlement");
    let log = [log, blocked].concat();

    let approved =
        appended_facts(execute_offer(e, &log, offer, OfferOutcome::Approved(Vec::new())).expect("the offer executes"));
    let log = [log, approved].concat();

    let release =
        appended_facts(proposed(e, &log, "san-release", nonce(), call.clone()).expect("the approval releases"));
    let dispatch = release
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
            _ => None,
        })
        .expect("the release opens the dispatch");
    ([log, release].concat(), dispatch)
}

pub(super) fn two_officer_engine() -> Engine {
    use crate::authority::{Authority, Mandate};
    let officer = |name: &str| Authority {
        name: AuthorityName::new(name),
        mandate: Mandate {
            trust_ceiling: Some(TRUSTED),
            ..Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let cfg = RegistryConfig {
        authorities: vec![officer("officer-a"), officer("officer-b")],
        ..test_config(vec![ToolAnnotation {
            requires: Requires {
                label: floor(TRUSTED),
                ..Requires::default()
            },
            ..plain_tool("wire")
        }])
    };
    open_engine_at(cfg, Label::new(SUSPICIOUS, Audience::public()))
}

pub(super) fn opened_offers(facts: &[Fact]) -> Vec<(crate::value::OfferId, plan::ExecutableRemedyPlan)> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::OfferOpened { offer, plan, .. } => Some((*offer, plan.clone())),
            _ => None,
        })
        .collect()
}

pub(super) fn evidence_for(
    offer: crate::value::OfferId,
    plan: &plan::ExecutableRemedyPlan,
    tool: &str,
    fold: Label,
) -> Vec<crate::execute::AuthorityEvidence> {
    plan.required
        .iter()
        .map(|required| crate::execute::AuthorityEvidence {
            offer,
            authority: required.authority.clone(),
            covers: required.covers.clone(),
            reviewed: crate::execute::AuthorityReview {
                tool: ToolName::new(tool),
                trajectory_label: fold.clone(),
            },
        })
        .collect()
}

pub(super) fn execute_offer(
    e: &Engine,
    log: &[Fact],
    offer: crate::value::OfferId,
    outcome: OfferOutcome,
) -> Result<EngineDecision, TransitionError> {
    execute_offer_with(e, log, offer, outcome, crate::audience::AudienceEvidence::default())
}

pub(super) fn execute_offer_with(
    e: &Engine,
    log: &[Fact],
    offer: crate::value::OfferId,
    outcome: OfferOutcome,
    audience: crate::audience::AudienceEvidence,
) -> Result<EngineDecision, TransitionError> {
    let view = e
        .view(&traj(), log.to_vec(), log.len() as u64)
        .expect("the log replays");
    e.handle(
        &view,
        EngineEvent::ExecuteOffer(OfferExecution {
            trajectory: traj(),
            offer,
            outcome,
            return_policy: None,
            offer_nonce: crate::value::OfferNonce::new([11u8; 32]),
            audience,
        }),
    )
}

pub(super) fn offer_answer(decision: &EngineDecision) -> &OfferFollowUp {
    match &decision.follow_up {
        FollowUp::Offer(answer) => answer,
        other => panic!("an offer execution answers with an offer follow-up, not {other:?}"),
    }
}

/// A tool whose result narrows the trajectory's audience, so admitting it moves the flow
/// basis.
pub(super) fn restrictable_tool(name: &str) -> ToolAnnotation {
    ToolAnnotation {
        delta: Delta {
            trust: None,
            audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                "reviewer",
            )]))),
        },
        ..plain_tool(name)
    }
}

pub(super) fn strict_tool(name: &str) -> ToolAnnotation {
    ToolAnnotation {
        parameters: crate::params::ToolParameters::compile(&json!({
            "type": "object",
            "properties": { "to": { "type": "string" } },
            "required": ["to"],
        }))
        .unwrap(),
        ..plain_tool(name)
    }
}

pub(super) fn input_sanitizer(name: &str, from: &[&str], to: &[&str]) -> crate::authority::Sanitizer {
    let audience = |readers: &[&str]| {
        DeclaredAudience::literal(Audience::restricted(
            readers.iter().map(|reader| ReaderId::new(*reader)),
        ))
    };
    crate::authority::Sanitizer {
        name: crate::names::SanitizerName::new(name),
        on: crate::authority::SanitizerPoints {
            input: true,
            output: false,
        },
        transition: crate::authority::DeclaredTransition::Audience {
            from_includes: audience(from),
            to: audience(to),
        },
        scope: crate::authority::Scope {
            tags: vec![crate::names::TagName::new("outbound")],
        },
        hint: None,
    }
}

pub(super) fn hop_named(facts: &[Fact], name: &str) -> crate::value::OfferId {
    opened_offers(facts)
        .into_iter()
        .find(|(_, plan)| plan.hop() == Some(&crate::names::SanitizerName::new(name)))
        .map(|(offer, _)| offer)
        .unwrap_or_else(|| panic!("the {name} hop is offered"))
}

/// The call a hop that cleared the last gap approved. It opens no dispatch: the model
/// proposes it, and that proposal takes the derivation.
pub(super) fn approved_by(decision: &EngineDecision) -> ResolvedCall {
    match offer_answer(decision) {
        OfferFollowUp::Approved { call } => (**call).clone(),
        other => panic!("the rewrite clears the last gap and approves the call, got {other:?}"),
    }
}

/// The facts of the batch in which the model proposes a call a hop approved.
pub(super) fn taken(e: &Engine, log: &[Fact], id: &str, call: &ResolvedCall) -> Vec<Fact> {
    taken_with(e, log, id, call, crate::audience::AudienceEvidence::default())
}

pub(super) fn taken_with(
    e: &Engine,
    log: &[Fact],
    id: &str,
    call: &ResolvedCall,
    audience: crate::audience::AudienceEvidence,
) -> Vec<Fact> {
    let decision = proposed_with(e, log, id, nonce(), call.clone(), audience).expect("the approved call is proposed");
    match &decision.follow_up {
        FollowUp::Proposals { released, blocked, .. } => {
            assert!(blocked.is_empty(), "the proposal takes the derivation the hop staged");
            assert_eq!(released[0].call, *call);
        }
        other => panic!("a proposal decides as proposals, got {other:?}"),
    }
    let facts = appended_facts(decision);
    assert!(
        facts.iter().any(|fact| matches!(fact, Fact::CandidateConsumed { .. })),
        "the proposal spends the derivation it stood on: {facts:?}"
    );
    facts
}

pub(super) fn plain_tool(name: &str) -> ToolAnnotation {
    ToolAnnotation {
        description: Some("A test tool.".to_string()),
        name: ToolName::new(name),
        tags: vec![],
        delta: Delta::NONE,
        parameters: crate::params::ToolParameters::open(),
        forwards: Delta::NONE,
        emits: EffectSet::default(),
        requires: Requires::default(),
    }
}

pub(super) fn engine_with_provider_run(tools: Vec<ToolAnnotation>, provider_run: &[&str]) -> Engine {
    provider_run_engine(test_config(tools), provider_run)
}

pub(super) fn provider_run_engine(cfg: RegistryConfig, provider_run: &[&str]) -> Engine {
    let mut declaration = crate::profile::covering_declaration(&cfg);
    for name in provider_run {
        declaration
            .executor_exceptions
            .insert(ToolName::new(*name), crate::profile::ExecutorClass::ProviderRun);
        declaration.confined_results.remove(&ToolName::new(*name));
    }
    placed(
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            profile: declaration,
        })
        .unwrap(),
    )
}

pub(super) fn exposed(tool: &str, body: &str) -> crate::transition::ProviderResult {
    crate::transition::ProviderResult {
        tool: ToolName::new(tool),
        body: ValueBody::new(body),
    }
}

pub(super) fn viewing(e: &Engine, log: &[Fact]) -> EngineView {
    e.view(&traj(), log.to_vec(), log.len() as u64)
        .expect("the log replays")
}

pub(super) fn batch_on(
    trajectory: &TrajectoryId,
    id: &str,
    provider_results: Vec<crate::transition::ProviderResult>,
    proposals: Vec<crate::transition::ProposedCall>,
    spawn: Option<SpawnMark>,
) -> EngineEvent {
    EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new(id),
        trajectory: trajectory.clone(),
        provider_results,
        proposals,
        spawn,
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    })
}

pub(super) fn batch(
    id: &str,
    provider_results: Vec<crate::transition::ProviderResult>,
    proposals: Vec<crate::transition::ProposedCall>,
) -> EngineEvent {
    batch_on(&traj(), id, provider_results, proposals, None)
}

pub(super) fn answered(decision: &EngineDecision) -> (&[Released], &[Blocked]) {
    match &decision.follow_up {
        FollowUp::Proposals { released, blocked, .. } => (released, blocked),
        other => panic!("a proposal batch answers with proposals, not {other:?}"),
    }
}

pub(super) fn tool_names(released: &[Released]) -> Vec<&str> {
    released.iter().map(|release| release.call.tool().as_str()).collect()
}

pub(super) fn batch_engine() -> Engine {
    let mut seen = plain_tool("seen");
    seen.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
    let mut wire = plain_tool("wire");
    wire.requires = Requires {
        history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
        ..Requires::default()
    };
    let mut guard = plain_tool("guard");
    guard.requires = Requires {
        history: vec![HistoryRequirement::NoPrior(EffectKind::new("k"))],
        ..Requires::default()
    };
    let mut emit = plain_tool("emit");
    emit.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
    engine_with_provider_run(
        vec![seen, wire, guard, emit, plain_tool("quiet"), plain_tool("spawn")],
        &["seen"],
    )
}

pub(super) fn opening_log(e: &Engine) -> Vec<Fact> {
    vec![opened(e)]
}

pub(super) fn slack_groups(handles: &[&str]) -> crate::audience::AudienceConfig {
    crate::audience::AudienceConfig {
        sources: vec![crate::audience::SourceRegistration {
            provider: crate::names::ProviderName::new("slack"),
            templates: vec![
                crate::audience::DeclaredTemplate::named("user-group/<handle>").expect("a well-formed template"),
            ],
        }],
        groups: handles
            .iter()
            .map(|handle| crate::audience::NamedAudience {
                name: crate::names::GroupName::new(*handle),
                within: None,
                from: vec![crate::audience::SelectorSpec {
                    provider: "slack".to_string(),
                    selector: format!("user-group/{handle}"),
                }],
            })
            .collect(),
        ..crate::audience::AudienceConfig::default()
    }
}

/// The reader Slack reports for one account: its confirmed address, else its id.
pub(super) fn slack_member(id: &str, email: Option<&str>) -> ReaderId {
    ReaderId::new(email.unwrap_or(id))
}

pub(super) fn user_group(handle: &str, members: Vec<ReaderId>) -> crate::audience::SourceClaims {
    crate::audience::SourceClaims {
        provider: "slack".to_string(),
        selector: format!("user-group/{handle}"),
        members,
    }
}

pub(super) fn source_evidence(sources: Vec<crate::audience::SourceClaims>) -> crate::audience::AudienceEvidence {
    crate::audience::AudienceEvidence {
        sources,
        ..crate::audience::AudienceEvidence::default()
    }
}

pub(super) fn group_atom(handle: &str) -> crate::label::SymbolicAtom {
    crate::label::SymbolicAtom::Group(crate::label::GroupRef::Named(crate::names::GroupName::new(handle)))
}

pub(super) fn group_audience(handle: &str) -> DeclaredAudience {
    DeclaredAudience::Union(
        crate::label::Clause::new(
            [],
            [crate::label::GroupRef::Named(crate::names::GroupName::new(handle))],
            [],
        )
        .expect("a group clause names no reader"),
    )
}

pub(super) fn corp_reader(local: &str) -> ReaderId {
    ReaderId::new(format!("{local}@corp.com"))
}

pub(super) fn returning_registry(sanitizers: Vec<crate::authority::Sanitizer>) -> RegistryConfig {
    RegistryConfig {
        sanitizers,
        ..test_config(vec![
            plain_tool("spawn"),
            plain_tool("fetch"),
            suspicious_read(),
            internal_read(),
            suspicious_internal_read(),
        ])
    }
}

pub(super) fn fork_in(log: &[Fact], child: &TrajectoryId) -> ForkId {
    log.iter()
        .find_map(|fact| match fact {
            Fact::ForkOpened { trajectory, fork } if trajectory == child => Some(fork.clone()),
            _ => None,
        })
        .expect("the fork opened")
}

pub(super) fn spawn_batch(id: &str, spawn: Option<crate::transition::SpawnMark>) -> EngineEvent {
    EngineEvent::Proposals(ProposalBatch {
        id: crate::transition::ProposalBatchId::new(id),
        trajectory: traj(),
        provider_results: Vec::new(),
        proposals: vec![raw(&call("spawn", json!({})))],
        spawn,
        offer_nonce: nonce(),
        evidence: Vec::new(),
        audience: crate::audience::AudienceEvidence::default(),
    })
}

pub(super) fn lifting_sanitizer(name: &str) -> crate::authority::Sanitizer {
    crate::authority::Sanitizer {
        name: SanitizerName::new(name),
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
    }
}

pub(super) fn attest_shape() -> crate::shape::ReturnShape {
    crate::shape::ReturnShape::compile(&json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["allow", "deny"] },
            "confidence": { "type": "integer", "minimum": 0, "maximum": 100 },
        },
        "required": ["verdict", "confidence"],
    }))
    .expect("the fixture shape compiles")
}

/// A parent that declared `floor` for a fan-out spawn and released it: the log and the
/// fan-out fork.
pub(super) fn fan_out_family(e: &Engine, floor: Label) -> (Vec<Fact>, crate::value::ForkId) {
    let mut log = vec![opened(e)];
    let blocked = e
        .handle(
            &viewing(e, &log),
            spawn_batch("fan-out", Some(crate::transition::SpawnMark::fan_out(0))),
        )
        .expect("a fan-out spawn blocks with its return declarations");
    log.extend(appended_facts(blocked));
    let (offer, _) = opened_offers(&log)
        .into_iter()
        .find(|(_, plan)| plan.return_step() == Some(&crate::fact::ReturnRoute::Raw))
        .expect("the bare declaration is offered");
    let approved = e
        .handle(
            &viewing(e, &log),
            EngineEvent::ExecuteOffer(OfferExecution {
                trajectory: traj(),
                offer,
                outcome: OfferOutcome::Approved(Vec::new()),
                return_policy: Some(ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                    floor,
                    sanitizer: None,
                })),
                offer_nonce: nonce(),
                audience: crate::audience::AudienceEvidence::default(),
            }),
        )
        .expect("the declaration approves the fan-out spawn");
    log.extend(appended_facts(approved));
    let released = e
        .handle(
            &viewing(e, &log),
            spawn_batch("fan-out-approved", Some(crate::transition::SpawnMark::fan_out(0))),
        )
        .expect("the approved fan-out spawn releases");
    let fork = match answered(&released) {
        ([release], []) => release.fork.clone().expect("the fan-out spawn carries its fork"),
        other => panic!("the approved fan-out spawn releases alone, got {other:?}"),
    };
    log.extend(appended_facts(released));
    (log, fork)
}

pub(super) fn member_started(e: &Engine, log: &mut Vec<Fact>, fan_out: &crate::value::ForkId, child: &TrajectoryId) {
    let bound = e
        .handle(
            &viewing(e, log),
            EngineEvent::BindFork(crate::transition::ForkBinding {
                fork: fan_out.clone(),
                child: child.clone(),
            }),
        )
        .expect("a fan-out child starts its member fork");
    log.extend(appended_facts(bound));
}

pub(super) fn staged_engine() -> Engine {
    let sanitizer = |name: &str, from_floor: Trust, to: Trust| crate::authority::Sanitizer {
        name: crate::names::SanitizerName::new(name),
        on: crate::authority::SanitizerPoints {
            input: false,
            output: true,
        },
        transition: crate::authority::DeclaredTransition::Trust { from_floor, to },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    open_engine(RegistryConfig {
        trust_chain: TrustChain::new(vec!["secret".into(), "suspicious".into(), "trusted".into()]),
        sanitizers: vec![
            sanitizer("redactor", Trust::new(0), Trust::new(1)),
            sanitizer("scrubber", Trust::new(1), Trust::new(2)),
        ],
        ..test_config(vec![
            ToolAnnotation {
                delta: Delta {
                    trust: Some(Trust::new(0)),
                    audience: None,
                },
                emits: EffectSet::new([EffectKind::new("web.read")]).unwrap(),
                ..plain_tool("fetch")
            },
            restrictable_tool("ping"),
        ])
    })
}

pub(super) fn staged_candidate(e: &Engine) -> (Vec<Fact>, DispatchId, ValueBody, EngineDecision) {
    let call = call("fetch", json!({}));
    let mut log = vec![opened(e)];
    let blocked = proposed(e, &log, "b1", nonce(), call.clone()).expect("the batch decides");
    let bound = opened_offers(&appended_facts(blocked))
        .into_iter()
        .find(|(_, plan)| plan.sanitizer() == Some(&crate::names::SanitizerName::new("redactor")))
        .expect("a narrowing block offers the applicable output sanitizer's dispatch path")
        .0;
    log = [
        log.clone(),
        appended_facts(proposed(e, &log, "b1", nonce(), call.clone()).expect("the batch decides")),
    ]
    .concat();
    log = [
        log.clone(),
        appended_facts(execute_offer(e, &log, bound, OfferOutcome::Approved(Vec::new())).expect("the offer runs")),
    ]
    .concat();
    let released = proposed(e, &log, "b2", nonce(), call).expect("the approved call releases");
    let dispatch = match &released.follow_up {
        FollowUp::Proposals { released, .. } => released[0].dispatch.clone(),
        other => panic!("the approved proposal releases, got {other:?}"),
    };
    log = [log, appended_facts(released)].concat();

    let raw = ValueBody::new("page bytes");
    let derived = ValueBody::new("page bytes, redacted");
    let report = |evidence: Vec<crate::transition::Evidence>| ToolReport {
        dispatch: dispatch.clone(),
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available(raw.clone()),
        },
        evidence,
        offer_nonce: nonce(),
        audience: crate::audience::AudienceEvidence::default(),
    };
    let view = |log: &[Fact]| {
        e.view(&traj(), log.to_vec(), log.len() as u64)
            .expect("the log replays")
    };
    let asked = e
        .handle(&view(&log), EngineEvent::Outcome(report(Vec::new())))
        .expect("the confined result asks for its derivation");
    log = [log, appended_facts(asked)].concat();
    let staged = e
        .handle(
            &view(&log),
            EngineEvent::Outcome(report(vec![crate::transition::Evidence::Sanitizer {
                sanitizer: crate::names::SanitizerName::new("redactor"),
                source: crate::value::RawResultDigest::of(raw.as_str().as_bytes()),
                derived: derived.clone(),
            }])),
        )
        .expect("the derivation stages");
    (log, dispatch, derived, staged)
}

/// A batch's records after the `BasisAdvanced` declaration every decided batch leads with.
pub(super) fn declared_records(facts: &[Fact]) -> &[Fact] {
    match facts {
        [Fact::BasisAdvanced { .. }, rest @ ..] => rest,
        _ => panic!("a decided batch leads with its declaration: {facts:?}"),
    }
}
