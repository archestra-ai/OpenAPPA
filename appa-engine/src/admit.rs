//! Result admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::candidate::{ConfinedFrom, DerivedCandidate, SanitizerLineage};
use crate::check::Narrowing;
use crate::fact::{CloseOutcome, EffectSet, Fact, ObservedResult};
use crate::groups::Expansions;
use crate::label::Label;
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody};

pub enum ResultAdmission {
    Failure,
    Indeterminate,
    SuccessNoValue,
    SuccessRaw {
        body: ValueBody,
    },
    SuccessSanitized {
        body: ValueBody,
        sanitizer: crate::names::SanitizerName,
        raw_digest: RawResultDigest,
    },
    CandidateAccepted {
        offer: crate::value::OfferId,
    },
    CandidateAdmissible,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdmitError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error("dispatch digest does not match the call")]
    DigestMismatch,
    #[error("dispatch belongs to another trajectory")]
    ForeignDispatch,
    #[error("dispatch is not open")]
    NotOpen,
    #[error("the dispatch already recorded its success checkpoint")]
    AlreadySucceeded,
    #[error("the dispatch recorded success: a failure or indeterminate close contradicts it")]
    SuccessContradicted,
    #[error("this admission carries other bytes than the dispatch's success checkpoint observed")]
    ObservationMismatch,
    #[error("an executed plan bound this dispatch to a sanitizer: only its derivation may be admitted")]
    OutputSanitizerBound,
    #[error("the derivation names a sanitizer this dispatch is not bound to")]
    SanitizerBindingMismatch,
    #[error("the bound sanitizer is not registered for output, or the raw result does not satisfy its `from`")]
    SanitizerTransitionUnmet,
    #[error(
        "the derivation still narrows the bound its dispatch pinned: the residual is the agent's to accept or improve at the confined stage"
    )]
    ConfinedResidual,
    #[error("this dispatch has no confined candidate standing, or its candidate owes no residual")]
    NoCandidate,
}

pub(crate) fn observe_success(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    call: &ResolvedCall,
    observed: ObservedResult,
) -> Result<Vec<Fact>, AdmitError> {
    let contract = registry
        .annotation_of(call)
        .ok_or_else(|| AdmitError::UnknownTool(call.tool().as_str().to_string()))?;
    if dispatch.digest() != &call.digest() {
        return Err(AdmitError::DigestMismatch);
    }
    if dispatch.trajectory() != views.trajectory() {
        return Err(AdmitError::ForeignDispatch);
    }
    if !views.is_open(dispatch) {
        return Err(AdmitError::NotOpen);
    }
    if views.is_succeeded(dispatch) {
        return Err(AdmitError::AlreadySucceeded);
    }
    Ok(vec![Fact::DispatchSucceeded {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        effects: contract.emits.clone(),
        observed,
    }])
}

/// What a confined candidate still narrows against the bound its dispatch pinned, or `None` where
/// it narrows nothing and may admit immediately.
pub(crate) fn confined_residual(receiving: &Label, derived: &Label) -> Option<Narrowing> {
    let to = receiving.combine(derived);
    (&to != receiving).then(|| Narrowing {
        from: receiving.clone(),
        to,
    })
}

/// The candidate a bound sanitizer's first derivation makes of one confined result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bound_candidate(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    contract: &crate::contract::ToolAnnotation,
    sanitizer: &SanitizerName,
    raw_digest: RawResultDigest,
    body: ValueBody,
    expansions: &Expansions,
) -> Result<(crate::authority::Transition, DerivedCandidate, SanitizerLineage), AdmitError> {
    if views.bound_sanitizer(dispatch) != Some(sanitizer) {
        return Err(AdmitError::SanitizerBindingMismatch);
    }
    let registered = registry
        .sanitizer(sanitizer)
        .ok_or_else(|| AdmitError::UnknownTool(sanitizer.as_str().to_string()))?;
    let raw_label = contract.output_label(expansions);
    let derived = registered
        .derive_output(&raw_label, &contract.tags, expansions)
        .ok_or(AdmitError::SanitizerTransitionUnmet)?;
    let receiving = views.receiving_bound(dispatch).ok_or(AdmitError::NotOpen)?;
    let residual = confined_residual(receiving, &derived);
    Ok((
        registered.transition.resolve(expansions),
        DerivedCandidate::Result {
            dispatch: dispatch.clone(),
            source: raw_digest,
            from: ConfinedFrom::Bound,
            value: LabeledValue::new(body, derived),
            residual,
        },
        SanitizerLineage::default()
            .extend(sanitizer.clone())
            .expect("an empty lineage spends no sanitizer yet"),
    ))
}

pub(crate) fn admit_result(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    call: &ResolvedCall,
    admission: ResultAdmission,
    expansions: &Expansions,
) -> Result<Vec<Fact>, AdmitError> {
    let contract = registry
        .annotation_of(call)
        .ok_or_else(|| AdmitError::UnknownTool(call.tool().as_str().to_string()))?;
    if dispatch.digest() != &call.digest() {
        return Err(AdmitError::DigestMismatch);
    }
    if dispatch.trajectory() != views.trajectory() {
        return Err(AdmitError::ForeignDispatch);
    }
    if !views.is_open(dispatch) {
        return Err(AdmitError::NotOpen);
    }
    let checkpointed = views.is_succeeded(dispatch);
    if checkpointed && matches!(admission, ResultAdmission::Failure | ResultAdmission::Indeterminate) {
        return Err(AdmitError::SuccessContradicted);
    }
    let reported = match &admission {
        ResultAdmission::SuccessRaw { body } => Some(RawResultDigest::of(body.as_str().as_bytes())),
        ResultAdmission::SuccessSanitized { raw_digest, .. } => Some(*raw_digest),
        ResultAdmission::CandidateAccepted { .. }
        | ResultAdmission::CandidateAdmissible
        | ResultAdmission::SuccessNoValue
        | ResultAdmission::Failure
        | ResultAdmission::Indeterminate => None,
    };
    if let (Some(reported), Some(observed)) = (reported, views.observed_result(dispatch))
        && observed != &ObservedResult::Available(reported)
    {
        return Err(AdmitError::ObservationMismatch);
    }

    let trajectory = views.trajectory().clone();
    let output_label = || contract.output_label(expansions);
    let close_success = || Fact::DispatchClosed {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        outcome: CloseOutcome::Success {
            effects: if checkpointed {
                EffectSet::default()
            } else {
                contract.emits.clone()
            },
        },
    };
    let admit_derived = |value: LabeledValue| Fact::ValueAdmitted {
        trajectory: trajectory.clone(),
        value,
        provenance: Provenance::ToolResult {
            dispatch: dispatch.clone(),
        },
    };
    let admit_value = |label: Label, body: ValueBody| admit_derived(LabeledValue::new(body, label));

    let bound = views.bound_sanitizer(dispatch);
    if bound.is_some() && matches!(admission, ResultAdmission::SuccessRaw { .. }) {
        return Err(AdmitError::OutputSanitizerBound);
    }

    let facts = match admission {
        ResultAdmission::Failure => vec![Fact::DispatchClosed {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            outcome: CloseOutcome::Failure,
        }],
        ResultAdmission::Indeterminate => vec![Fact::DispatchClosed {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            outcome: CloseOutcome::Indeterminate,
        }],
        ResultAdmission::SuccessNoValue => vec![close_success()],
        ResultAdmission::SuccessRaw { body } => {
            // A raw success admits at the tool's declared output label, unsanitized.
            vec![close_success(), admit_value(output_label(), body)]
        }
        ResultAdmission::SuccessSanitized {
            body,
            sanitizer,
            raw_digest,
        } => {
            let (transition, derived, lineage) = bound_candidate(
                registry, views, dispatch, contract, &sanitizer, raw_digest, body, expansions,
            )?;
            let DerivedCandidate::Result { value, residual, .. } = &derived else {
                unreachable!("a bound output sanitizer derives a confined result")
            };
            if residual.is_some() {
                return Err(AdmitError::ConfinedResidual);
            }
            let value = value.clone();
            vec![
                close_success(),
                Fact::CandidateDerived {
                    trajectory: trajectory.clone(),
                    subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
                    via: crate::candidate::DerivedVia {
                        name: sanitizer,
                        transition,
                    },
                    derived,
                    lineage,
                    resolutions: registry.resolutions(expansions),
                },
                admit_derived(value),
            ]
        }
        ResultAdmission::CandidateAccepted { offer } => {
            let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
            let Some(DerivedCandidate::Result {
                value,
                residual: Some(narrowing),
                ..
            }) = views.candidate(&subject)
            else {
                return Err(AdmitError::NoCandidate);
            };
            let mut facts = vec![
                Fact::CandidateAccepted {
                    trajectory: trajectory.clone(),
                    subject: subject.clone(),
                    offer,
                    narrowing: narrowing.clone(),
                },
                close_success(),
            ];
            facts.push(admit_derived(value.clone()));
            facts
        }
        ResultAdmission::CandidateAdmissible => {
            let subject = crate::basis::SubjectKey::ConfinedResult(dispatch.clone());
            let Some(DerivedCandidate::Result {
                value, residual: None, ..
            }) = views.candidate(&subject)
            else {
                return Err(AdmitError::NoCandidate);
            };
            vec![close_success(), admit_derived(value.clone())]
        }
    };

    Ok(facts)
}

#[cfg(test)]
mod tests {
    const BODY: &str = "the result";

    use super::*;
    use crate::authority::{DeclaredTransition, Sanitizer, SanitizerPoints, Scope};
    use crate::contract::{Delta, ToolAnnotation, ToolDeclaration};
    use crate::fact::EffectKind;
    use crate::groups::DeclaredAudience;
    use crate::label::{Audience, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{ToolName, TrajectoryId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn opened() -> Fact {
        crate::profile::opening_at(traj(), Label::new(Trust::new(1), Audience::Public))
    }

    fn registry() -> Registry {
        let get = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeclaredAudience::literal(internal())),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let out_san = Sanitizer {
            name: crate::names::SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(internal()),
                to: DeclaredAudience::literal(Audience::Public),
            },
            scope: Scope::default(),
            hint: None,
        };
        let finance_san = Sanitizer {
            name: crate::names::SanitizerName::new("finance-only"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(Audience::restricted([ReaderId::new("finance")])),
                to: DeclaredAudience::literal(Audience::Public),
            },
            scope: Scope::default(),
            hint: None,
        };
        let scan = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeclaredAudience::literal(internal())),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let poll = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("poll_room"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeclaredAudience::literal(internal())),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let dynamic_scan = ToolDeclaration::Annotated {
            name: ToolName::new("dynamic_scan"),
            tags: vec![],
            description: Some("A test tool.".to_string()),
            parameters: crate::params::test_string_argument_schema("room"),
            annotator: crate::names::AnnotatorName::new("directory"),
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![
                ToolDeclaration::Declared(get),
                ToolDeclaration::Declared(scan),
                ToolDeclaration::Declared(poll),
                dynamic_scan,
            ],
            annotators: vec![crate::registry::AnnotatorDeclaration {
                name: crate::names::AnnotatorName::new("directory"),
                trust: None,
                audiences: None,
                marks: None,
                effects: None,
            }],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            membership: None,
        })
        .unwrap()
    }

    fn scan_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("scan_inbox"), crate::params::test_arguments(&json!({})))
    }

    fn get_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("get_ticket"), crate::params::test_arguments(&json!({})))
    }

    fn dispatch_opened(call: &ResolvedCall) -> (Fact, DispatchId) {
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let record = Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::top(),
            receiving: Label::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            annotation: call.annotation().cloned(),
            memberships: Vec::new(),
            subject: crate::basis::fixture_subject(&traj()),
            resolutions: vec![],
        };
        (record, dispatch)
    }

    fn open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let (dispatch_record, dispatch) = dispatch_opened(call);
        (vec![opened(), dispatch_record], dispatch)
    }

    fn views_of(log: &[Fact]) -> Projection {
        Projection::build(log, log.len() as u64)
    }

    fn offer() -> crate::value::OfferId {
        crate::value::OfferId::of_plan(
            &crate::value::BlockId::of_proposal(
                &crate::value::OfferNonce::new([7u8; 32]),
                &traj(),
                &crate::transition::ProposalBatchId::new("b"),
                0,
                &scan_call().digest(),
            ),
            0,
            b"acceptance",
        )
    }

    #[test]
    fn raw_admits_contract_output_label_and_effects() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new("ticket #7"),
            },
            &Expansions::default(),
        )
        .unwrap();
        assert!(matches!(
            &batch[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &EffectSet::new([EffectKind::new("read")]).unwrap()
        ));
        match &batch[1] {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, SUSPICIOUS);
                assert_eq!(value.label.audience, internal());
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn failure_admits_no_value_no_effects() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::Failure,
            &Expansions::default(),
        )
        .unwrap();
        assert_eq!(batch.len(), 1);
        assert!(matches!(
            &batch[0],
            Fact::DispatchClosed {
                outcome: CloseOutcome::Failure,
                ..
            }
        ));
    }

    #[test]
    fn swapped_call_and_unopened_rejected() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let other = ResolvedCall::new(
            ToolName::new("get_ticket"),
            crate::params::test_arguments(&json!({ "x": 1 })),
        );
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &other,
                ResultAdmission::SuccessNoValue,
                &Expansions::default()
            ),
            Err(AdmitError::DigestMismatch)
        );
        let empty = views_of(&[opened()]);
        assert_eq!(
            admit_result(
                &reg,
                &empty.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessNoValue,
                &Expansions::default()
            ),
            Err(AdmitError::NotOpen)
        );
    }

    fn narrowed_open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let (mut log, dispatch) = open_log(call);
        log[0] = crate::profile::opening_at(traj(), Label::new(SUSPICIOUS, internal()));
        let Fact::DispatchOpened { receiving, .. } = &mut log[1] else {
            unreachable!("open_log holds exactly one DispatchOpened")
        };
        *receiving = Label::new(SUSPICIOUS, internal());
        (log, dispatch)
    }

    #[test]
    fn an_acceptance_without_a_matching_candidate_is_refused() {
        let reg = registry();
        let call = scan_call();
        let t = traj();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::CandidateAccepted { offer: offer() },
                &Expansions::default()
            ),
            Err(AdmitError::NoCandidate)
        );
    }

    #[test]
    fn a_success_checkpoint_commits_effects_once_and_pins_the_close_family() {
        let reg = registry();
        let call = scan_call();
        let (mut log, dispatch) = narrowed_open_log(&call);
        let t = traj();

        let p = views_of(&log);
        let observed = ObservedResult::Available(RawResultDigest::of(BODY.as_bytes()));
        let batch = observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()).unwrap();
        log.extend(batch);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()),
            Err(AdmitError::AlreadySucceeded)
        );

        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::Failure,
                &Expansions::default()
            ),
            Err(AdmitError::SuccessContradicted)
        );
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::Indeterminate,
                &Expansions::default()
            ),
            Err(AdmitError::SuccessContradicted)
        );

        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new(BODY),
            },
            &Expansions::default(),
        )
        .unwrap();
        assert!(batch.iter().any(|fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { effects },
                ..
            } if effects.is_empty()
        )));
        log.extend(batch);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(!p.view(&t).is_open(&dispatch));
    }

    #[test]
    fn a_foreign_trajectory_cannot_close_a_dispatch() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let sibling = TrajectoryId::new("sibling");
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&sibling),
                &dispatch,
                &call,
                ResultAdmission::SuccessNoValue,
                &Expansions::default()
            ),
            Err(AdmitError::ForeignDispatch)
        );
    }

    #[test]
    fn an_ordinary_dispatch_checkpoints_at_typed_success() {
        let reg = registry();
        let call = get_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();

        let p = views_of(&log);
        let observed = ObservedResult::Available(RawResultDigest::of(BODY.as_bytes()));
        let batch = observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()).unwrap();
        log.extend(batch);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()),
            Err(AdmitError::AlreadySucceeded)
        );
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::Failure,
                &Expansions::default()
            ),
            Err(AdmitError::SuccessContradicted)
        );

        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new(BODY),
            },
            &Expansions::default(),
        )
        .unwrap();
        assert!(batch.iter().any(|fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { effects },
                ..
            } if effects.is_empty()
        )));
        assert!(batch.iter().any(|fact| matches!(fact, Fact::ValueAdmitted { .. })));
        log.extend(batch);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(!p.view(&t).is_open(&dispatch));
    }

    #[test]
    fn a_bound_dispatch_admits_only_its_sanitizers_derivation() {
        let reg = registry();
        let call = get_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();
        log.push(Fact::OutputSanitizerBound {
            trajectory: t.clone(),
            dispatch: dispatch.clone(),
            plan: crate::plan::PlanId::new(1),
            sanitizer: crate::names::SanitizerName::new("declassify"),
            contribution: crate::plan::bound_contribution(
                &reg,
                reg.annotation_of(&call).expect("the fixture registers the tool"),
                &crate::names::SanitizerName::new("declassify"),
                &Expansions::default(),
            )
            .expect("declassify applies to this output"),
            resolutions: vec![],
        });
        let p = views_of(&log);

        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("ticket"),
                },
                &Expansions::default()
            ),
            Err(AdmitError::OutputSanitizerBound)
        );

        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("finance-only"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
                &Expansions::default()
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );

        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
                &Expansions::default()
            ),
            Err(AdmitError::ConfinedResidual)
        );
    }

    #[test]
    fn an_unbound_dispatch_refuses_a_derivation() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&traj()),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
                &Expansions::default()
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );
    }
}
