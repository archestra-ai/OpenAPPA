//! Result admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::audience::AudienceEvidence;
use crate::candidate::{ConfinedFrom, DerivedCandidate, SanitizerLineage};
use crate::check::Narrowing;
use crate::fact::{CloseOutcome, EffectSet, Fact, ObservedResult};
use crate::label::{Label, MembershipContext};
use crate::names::SanitizerName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody};

pub enum ResultAdmission {
    Failure,
    FailureWithBody {
        body: ValueBody,
    },
    FailureNoValue {
        observed: RawResultDigest,
    },
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
    #[error("the bound sanitizer's transition awaits membership answers")]
    MembershipNeeded(crate::label::MembershipNeeded),
    #[error(
        "the derivation still narrows the bound its dispatch pinned: the residual is the agent's to accept or improve at the confined stage"
    )]
    ConfinedResidual,
    #[error("this dispatch has no confined candidate standing, or its candidate owes no residual")]
    NoCandidate,
}

/// What a dispatch's outcome is judged under: the contract its release pinned, met with what the
/// judging policy declares for the call now.
///
/// A release pins the call's delta, tags and effects on its `DispatchOpened`. A result that
/// arrives after a later revision was published is labelled by the meet of the pinned output label
/// and the one the judging policy declares for the call, where it still declares it, and commits
/// the effects the release reserved. A record that pinned no terms is judged by the judging
/// policy's declaration alone, as the engine that wrote it judged it.
pub(crate) struct OutcomeContract<'a> {
    /// The contract with the release's delta, tags and effects: the judging policy's
    /// declaration of the call, or one made from the pin where it declares the call no longer.
    pub(crate) contract: std::borrow::Cow<'a, crate::contract::ToolAnnotation>,
    /// The judging policy's declared output label for a call whose terms the release pinned.
    head_output: Option<Label>,
}

impl OutcomeContract<'_> {
    /// The declared output label a result of this dispatch carries, before its file basis.
    pub(crate) fn output_label(&self) -> Label {
        let pinned = self.contract.output_label();
        match &self.head_output {
            Some(head) => pinned.combine(head),
            None => pinned,
        }
    }
}

/// The contract `call`'s outcome is judged under: see [`OutcomeContract`]. `None` where neither
/// the release pinned terms nor the judging policy declares the call.
pub(crate) fn outcome_contract<'a>(
    registry: &'a Registry,
    call: &'a ResolvedCall,
    pinned: Option<&(crate::fact::ReleaseTerms, EffectSet)>,
) -> Option<OutcomeContract<'a>> {
    let head = registry.annotation_of(call);
    let Some((terms, emits)) = pinned else {
        return head.map(|contract| OutcomeContract {
            contract,
            head_output: None,
        });
    };
    let head_output = head.as_ref().map(|head| head.output_label());
    let mut contract = head
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|| crate::contract::ToolAnnotation {
            name: call.tool().clone(),
            tags: Vec::new(),
            description: None,
            parameters: crate::params::ToolParameters::open(),
            delta: crate::contract::Delta::NONE,
            forwards: crate::contract::Delta::NONE,
            emits: EffectSet::default(),
            requires: crate::contract::Requires::default(),
        });
    contract.delta = terms.delta.clone();
    contract.tags = terms.tags.clone();
    contract.emits = emits.clone();
    Some(OutcomeContract {
        contract: std::borrow::Cow::Owned(contract),
        head_output,
    })
}

pub(crate) fn observe_success(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    call: &ResolvedCall,
    observed: ObservedResult,
) -> Result<Vec<Fact>, AdmitError> {
    let contract = outcome_contract(registry, call, views.release_terms(dispatch))
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
        effects: contract.contract.emits.clone(),
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
    call: &ResolvedCall,
    contract: &OutcomeContract<'_>,
    sanitizer: &SanitizerName,
    raw_digest: RawResultDigest,
    body: ValueBody,
    context: &MembershipContext<'_>,
) -> Result<(DerivedCandidate, SanitizerLineage), AdmitError> {
    if views.bound_sanitizer(dispatch) != Some(sanitizer) {
        return Err(AdmitError::SanitizerBindingMismatch);
    }
    let registered = registry
        .sanitizer(sanitizer)
        .ok_or_else(|| AdmitError::UnknownTool(sanitizer.as_str().to_string()))?;
    let receiving = views.receiving_bound(dispatch).ok_or(AdmitError::NotOpen)?;
    let derived = registered
        .derive_output(
            &call.output_label_from(&contract.output_label(), receiving),
            &contract.contract.tags,
            context,
        )
        .map_err(AdmitError::MembershipNeeded)?
        .ok_or(AdmitError::SanitizerTransitionUnmet)?;
    let residual = confined_residual(receiving, &derived);
    Ok((
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
    context: &MembershipContext<'_>,
    evidence: &AudienceEvidence,
) -> Result<Vec<Fact>, AdmitError> {
    let contract = outcome_contract(registry, call, views.release_terms(dispatch))
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
        ResultAdmission::FailureNoValue { observed } => Some(*observed),
        ResultAdmission::CandidateAccepted { .. }
        | ResultAdmission::CandidateAdmissible
        | ResultAdmission::SuccessNoValue
        | ResultAdmission::Failure
        | ResultAdmission::FailureWithBody { .. }
        | ResultAdmission::Indeterminate => None,
    };
    if let (Some(reported), Some(observed)) = (reported, views.observed_result(dispatch))
        && observed != &ObservedResult::Available(reported)
    {
        return Err(AdmitError::ObservationMismatch);
    }

    let trajectory = views.trajectory().clone();
    let receiving = views.receiving_bound(dispatch).ok_or(AdmitError::NotOpen)?;
    let output_label = || call.output_label_from(&contract.output_label(), receiving);
    let close_success = || Fact::DispatchClosed {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        outcome: CloseOutcome::Success {
            effects: if checkpointed {
                EffectSet::default()
            } else {
                contract.contract.emits.clone()
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
    if bound.is_some()
        && matches!(
            admission,
            ResultAdmission::SuccessRaw { .. } | ResultAdmission::FailureWithBody { .. }
        )
    {
        return Err(AdmitError::OutputSanitizerBound);
    }

    let facts = match admission {
        ResultAdmission::Failure => vec![Fact::DispatchClosed {
            trajectory: trajectory.clone(),
            dispatch: dispatch.clone(),
            outcome: CloseOutcome::Failure,
        }],
        ResultAdmission::FailureWithBody { body } => {
            if call.file_basis().is_none() {
                return Err(AdmitError::ObservationMismatch);
            }
            let observed = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                Fact::DispatchClosed {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    outcome: CloseOutcome::FailureWithBody { observed },
                },
                admit_value(output_label(), body),
            ]
        }
        ResultAdmission::FailureNoValue { observed } => {
            if call.file_basis().is_none() {
                return Err(AdmitError::ObservationMismatch);
            }
            vec![Fact::DispatchClosed {
                trajectory: trajectory.clone(),
                dispatch: dispatch.clone(),
                outcome: CloseOutcome::FailureWithBody { observed },
            }]
        }
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
            let (derived, lineage) = bound_candidate(
                registry, views, dispatch, call, &contract, &sanitizer, raw_digest, body, context,
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
                    sanitizer,
                    derived,
                    lineage,
                    evidence: evidence.clone(),
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
    use crate::contract::{Delta, DeltaAudience, ToolAnnotation, ToolDeclaration};
    use crate::fact::EffectKind;
    use crate::label::DeclaredAudience;
    use crate::label::{Audience, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{ToolName, TrajectoryId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);

    fn admit(
        registry: &Registry,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<Vec<Fact>, AdmitError> {
        let parts = crate::label::TestContext::default();
        admit_result(
            registry,
            views,
            dispatch,
            call,
            admission,
            &parts.context(),
            &AudienceEvidence::default(),
        )
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("insider")])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn opened() -> Fact {
        crate::profile::opening_at(traj(), Label::new(Trust::new(1), Audience::public()))
    }

    fn registry() -> Registry {
        let get = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeltaAudience::Static(DeclaredAudience::literal(internal()))),
            },
            parameters: crate::params::ToolParameters::open(),
            forwards: Delta::NONE,
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
                to: DeclaredAudience::literal(Audience::public()),
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
                from_includes: DeclaredAudience::restricted([ReaderId::new("finance")]),
                to: DeclaredAudience::literal(Audience::public()),
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
                audience: Some(DeltaAudience::Static(DeclaredAudience::literal(internal()))),
            },
            parameters: crate::params::ToolParameters::open(),
            forwards: Delta::NONE,
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let poll = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("poll_room"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeltaAudience::Static(DeclaredAudience::literal(internal()))),
            },
            parameters: crate::params::ToolParameters::open(),
            forwards: Delta::NONE,
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
            audience: crate::audience::AudienceConfig::default(),
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
            terms: None,
            trajectory: traj(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            declaration: call.declaration_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: Label::top(),
            receiving: Label::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            annotation: call.annotation().cloned(),
            file_basis: call.file_basis().cloned(),
            subject: crate::basis::fixture_subject(&traj()),
            evidence: crate::audience::AudienceEvidence::default(),
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

    /// The label `call` returns at the bound its open dispatch received.
    fn released_output(reg: &Registry, log: &[Fact], dispatch: &DispatchId, call: &ResolvedCall) -> Label {
        let contract = reg.annotation_of(call).expect("the fixture registers the tool");
        let views = views_of(log);
        let receiving = views
            .view(&traj())
            .receiving_bound(dispatch)
            .expect("the fixture opened the dispatch")
            .clone();
        call.output_label(&contract, &receiving)
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
        let batch = admit(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new("ticket #7"),
            },
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
        let batch = admit(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure).unwrap();
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
    fn file_failure_body_is_admitted_at_the_result_label_without_success_effects() {
        let reg = registry();
        let predecessor = crate::value::FileSource {
            version: "v1".into(),
            digest: "old".into(),
            label: Label::new(SUSPICIOUS, internal()),
        };
        let call = get_call().with_file_basis(Some(crate::value::FileBasis::Replace(Some(predecessor))));
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let batch = admit(
            &reg,
            &p.view(&traj()),
            &dispatch,
            &call,
            ResultAdmission::FailureWithBody {
                body: ValueBody::new("native error containing old content"),
            },
        )
        .unwrap();

        assert!(matches!(
            &batch[0],
            Fact::DispatchClosed {
                outcome: CloseOutcome::FailureWithBody { .. },
                ..
            }
        ));
        assert!(matches!(
            &batch[1],
            Fact::ValueAdmitted { value, .. }
                if value.label == Label::new(SUSPICIOUS, internal())
        ));
        let projection = views_of(&[log, batch].concat());
        assert_eq!(
            projection.view(&traj()).dispatch_call(&dispatch).unwrap().file_basis(),
            call.file_basis(),
            "the persisted opening rebuilds the pinned file basis"
        );
        assert!(!projection.view(&traj()).has_effect(&EffectKind::new("read")));
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
            admit(&reg, &p.view(&t), &dispatch, &other, ResultAdmission::SuccessNoValue,),
            Err(AdmitError::DigestMismatch)
        );
        let empty = views_of(&[opened()]);
        assert_eq!(
            admit(&reg, &empty.view(&t), &dispatch, &call, ResultAdmission::SuccessNoValue,),
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
            admit(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::CandidateAccepted { offer: offer() },
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
            admit(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure,),
            Err(AdmitError::SuccessContradicted)
        );
        assert_eq!(
            admit(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Indeterminate,),
            Err(AdmitError::SuccessContradicted)
        );

        let batch = admit(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new(BODY),
            },
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
            admit(
                &reg,
                &p.view(&sibling),
                &dispatch,
                &call,
                ResultAdmission::SuccessNoValue,
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
            admit(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure,),
            Err(AdmitError::SuccessContradicted)
        );

        let batch = admit(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessRaw {
                body: ValueBody::new(BODY),
            },
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
        let parts = crate::label::TestContext::default();
        log.push(Fact::OutputSanitizerBound {
            trajectory: t.clone(),
            dispatch: dispatch.clone(),
            plan: crate::plan::PlanId::new(1),
            sanitizer: crate::names::SanitizerName::new("declassify"),
            contribution: crate::plan::bound_contribution(
                &reg,
                &reg.annotation_of(&call).expect("the fixture registers the tool"),
                &released_output(&reg, &log, &dispatch, &call),
                &crate::names::SanitizerName::new("declassify"),
                &parts.context(),
            )
            .expect("the fixture's audiences are literal")
            .expect("declassify applies to this output"),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        let p = views_of(&log);

        assert_eq!(
            admit(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("ticket"),
                },
            ),
            Err(AdmitError::OutputSanitizerBound)
        );

        assert_eq!(
            admit(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("finance-only"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );

        assert_eq!(
            admit(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
            ),
            Err(AdmitError::ConfinedResidual)
        );
    }

    #[test]
    fn a_bound_dispatch_admits_no_raw_failure_body() {
        let reg = registry();
        let call = get_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();
        let parts = crate::label::TestContext::default();
        log.push(Fact::OutputSanitizerBound {
            trajectory: t.clone(),
            dispatch: dispatch.clone(),
            plan: crate::plan::PlanId::new(1),
            sanitizer: crate::names::SanitizerName::new("declassify"),
            contribution: crate::plan::bound_contribution(
                &reg,
                &reg.annotation_of(&call).expect("the fixture registers the tool"),
                &released_output(&reg, &log, &dispatch, &call),
                &crate::names::SanitizerName::new("declassify"),
                &parts.context(),
            )
            .expect("the fixture's audiences are literal")
            .expect("declassify applies to this output"),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        let p = views_of(&log);

        assert_eq!(
            admit(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::FailureWithBody {
                    body: ValueBody::new("ENOENT"),
                },
            ),
            Err(AdmitError::OutputSanitizerBound)
        );
    }

    /// A bound sanitizer whose `from` names a group derives only under that group's answer:
    /// an admission whose context lacks it asks for the atom instead of reporting the
    /// transition unmet.
    #[test]
    fn a_bound_derivation_missing_a_membership_answer_asks_for_it() {
        let team = crate::label::GroupRef::Named(crate::names::GroupName::new("team"));
        let team_atom = crate::label::SymbolicAtom::Group(team.clone());
        let declassify = crate::names::SanitizerName::new("declassify");
        let get = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeltaAudience::Static(DeclaredAudience::literal(internal()))),
            },
            parameters: crate::params::ToolParameters::open(),
            forwards: Delta::NONE,
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let reg = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![ToolDeclaration::Declared(get)],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![Sanitizer {
                name: declassify.clone(),
                on: SanitizerPoints {
                    input: false,
                    output: true,
                },
                transition: DeclaredTransition::Audience {
                    from_includes: DeclaredAudience::Union(
                        crate::label::Clause::new([], [team], []).expect("a group clause"),
                    ),
                    to: DeclaredAudience::literal(internal()),
                },
                scope: Scope::default(),
                hint: None,
            }],
            audience: crate::audience::AudienceConfig {
                sources: vec![crate::audience::SourceRegistration {
                    provider: crate::names::ProviderName::new("slack"),
                    templates: vec![
                        crate::audience::DeclaredTemplate::named("user-group/<handle>")
                            .expect("a well-formed template"),
                    ],
                }],
                groups: vec![crate::audience::NamedAudience {
                    name: crate::names::GroupName::new("team"),
                    within: None,
                    from: vec![crate::audience::SelectorSpec {
                        provider: "slack".to_string(),
                        selector: "user-group/team".to_string(),
                    }],
                }],
                ..crate::audience::AudienceConfig::default()
            },
        })
        .unwrap();
        let call = get_call();
        let (mut log, dispatch) = open_log(&call);
        let answered = crate::label::TestContext {
            expansions: crate::label::Expansions::new(
                [(
                    team_atom.clone(),
                    std::collections::BTreeSet::from([ReaderId::new("insider")]),
                )],
                [],
            ),
            ..crate::label::TestContext::default()
        };
        log.push(Fact::OutputSanitizerBound {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            plan: crate::plan::PlanId::new(1),
            sanitizer: declassify.clone(),
            contribution: crate::plan::bound_contribution(
                &reg,
                &reg.annotation_of(&call).expect("the fixture registers the tool"),
                &released_output(&reg, &log, &dispatch, &call),
                &declassify,
                &answered.context(),
            )
            .expect("the binding answered the group")
            .expect("declassify applies to this output"),
            evidence: crate::audience::AudienceEvidence::default(),
        });
        let p = views_of(&log);

        assert_eq!(
            admit(
                &reg,
                &p.view(&traj()),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: declassify,
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
            ),
            Err(AdmitError::MembershipNeeded(crate::label::MembershipNeeded {
                needed: vec![team_atom]
            }))
        );
    }

    #[test]
    fn an_unbound_dispatch_refuses_a_derivation() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        assert_eq!(
            admit(
                &reg,
                &p.view(&traj()),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: crate::names::SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"ticket"),
                },
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );
    }
}
