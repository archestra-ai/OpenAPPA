//! Result and cast admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::authority::CastRefusal;
use crate::candidate::{ConfinedFrom, DerivedCandidate, SanitizerLineage};
use crate::check::Narrowing;
use crate::fact::{CloseOutcome, EffectSet, Fact, ObservedResult};
use crate::groups::Expansions;
use crate::label::{EstablishedLabel, Label};
use crate::names::{CastName, SanitizerName};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody, ValueId};

pub enum ResultAdmission {
    Failure,
    Indeterminate,
    SuccessNoValue,
    SuccessRaw {
        body: ValueBody,
    },
    SuccessCast {
        body: ValueBody,
        cast: CastName,
        resolved: EstablishedLabel,
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
    #[error("the contract declares a pending-cast output: only a cast-resolved admission may carry a value")]
    OutputPendingCast,
    #[error("the contract declares no pending-cast output")]
    NotPendingCast,
    #[error("no cast registered as {0}")]
    UnknownCast(String),
    #[error("cast answer does not match the constant cast's declared label")]
    ConstantMismatch,
    #[error("cast answer exceeds the resolver's may_cast ceiling")]
    CeilingExceeded,
    #[error("cast answer holds a non-literal reader id")]
    NonLiteralAnswer,
    #[error("cast answer changes a dimension the output label already establishes")]
    EstablishedMismatch,
    #[error("the dispatched tool is outside the cast's scope")]
    OutOfScopeCast,
    #[error("the cast resolution narrows the trajectory label: admission requires the agent's acceptance")]
    NarrowingUnaccepted,
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
        .contract(call)
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
pub(crate) fn confined_residual(receiving: &EstablishedLabel, derived: &Label) -> Option<Narrowing> {
    let to = receiving.combine(&derived.established_part());
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
    contract: &crate::contract::ToolContract,
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
    let raw_label =
        contract.output_label_for_resolutions(views.tool_resolutions(dispatch).unwrap_or_default(), expansions);
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

/// The bound a pending-cast resolution on `dispatch` is measured from: the receiving bound the
/// dispatch pinned, narrowed by the acceptance its release recorded, if one did. The static
/// part of the tool's delta was accepted at the check, so admission owes no second acceptance
/// for it; anything the resolution narrows beyond that accepted bound stays owed. Only the
/// dispatch's own facts move the baseline — never the live fold.
pub(crate) fn cast_baseline(views: &Views, dispatch: &DispatchId) -> Option<EstablishedLabel> {
    let receiving = views.receiving_bound(dispatch)?;
    Some(match views.accepted_narrowing(dispatch) {
        Some(accepted) => receiving.combine(&accepted.to),
        None => receiving.clone(),
    })
}

/// The candidate a validated pending-cast resolution makes of one confined result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cast_candidate(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    contract: &crate::contract::ToolContract,
    cast: &CastName,
    body: ValueBody,
    resolved: &EstablishedLabel,
    expansions: &Expansions,
) -> Result<DerivedCandidate, AdmitError> {
    let output_label =
        contract.output_label_for_resolutions(views.tool_resolutions(dispatch).unwrap_or_default(), expansions);
    validate_pending_cast(registry, contract, &output_label, cast, resolved, expansions)?;
    let baseline = cast_baseline(views, dispatch).ok_or(AdmitError::NotOpen)?;
    let label = resolved.clone().into_label();
    let residual = confined_residual(&baseline, &label);
    Ok(DerivedCandidate::Result {
        dispatch: dispatch.clone(),
        source: RawResultDigest::of(body.as_str().as_bytes()),
        from: ConfinedFrom::Bound,
        value: LabeledValue::new(body, label),
        residual,
    })
}

fn refusal_error(refusal: CastRefusal) -> AdmitError {
    match refusal {
        CastRefusal::NonLiteralReader => AdmitError::NonLiteralAnswer,
        CastRefusal::ConstantMismatch => AdmitError::ConstantMismatch,
        CastRefusal::EstablishedMismatch(_) => AdmitError::EstablishedMismatch,
        CastRefusal::CeilingExceeded(_) => AdmitError::CeilingExceeded,
    }
}

/// Validate a pending-cast resolution against the contract and the registered cast: the contract
/// must declare a pending-cast output, and the answer must be the complete whole-source
/// resolution of that output label — established dimensions preserved exactly, the pending one
/// inside the registered declaration — so a misbehaving resolver (or runtime) cannot widen a
/// label past the ceiling or move a dimension the contract settled.
pub(crate) fn validate_pending_cast(
    registry: &Registry,
    contract: &crate::contract::ToolContract,
    output_label: &Label,
    cast: &CastName,
    resolved: &EstablishedLabel,
    expansions: &Expansions,
) -> Result<(), AdmitError> {
    if contract.pending_cast_dim().is_none() {
        return Err(AdmitError::NotPendingCast);
    }
    let registered = registry
        .cast(cast)
        .ok_or_else(|| AdmitError::UnknownCast(cast.as_str().to_string()))?;
    if !registered.scope.covers(&contract.tags) {
        return Err(AdmitError::OutOfScopeCast);
    }
    registered
        .resolution
        .validate(output_label, resolved, expansions)
        .map_err(refusal_error)
}

/// A registered cast's complete answer for one source: the whole label, never a
/// single dimension.
pub struct CastAnswer {
    pub cast: CastName,
    pub resolved: EstablishedLabel,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CastError {
    #[error("no cast registered as {0}")]
    UnknownCast(String),
    #[error("cast answer exceeds the resolver's may_cast ceiling")]
    CeilingExceeded,
    #[error("cast answer does not match the constant cast's declared label")]
    ConstantMismatch,
    #[error("cast answer holds a non-literal reader id")]
    NonLiteralAnswer,
    #[error("cast answer changes a dimension the source already establishes")]
    EstablishedMismatch,
    #[error("target value is unknown or out of range")]
    UnknownValue,
    #[error("target value is neither admitted by nor inherited into this branch")]
    ForeignValue,
    #[error("target value's originating tool is outside the cast's scope")]
    OutOfScope,
    #[error("target value's label is already fully established")]
    AlreadyEstablished,
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
        .contract(call)
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
        ResultAdmission::SuccessRaw { body } | ResultAdmission::SuccessCast { body, .. } => {
            Some(RawResultDigest::of(body.as_str().as_bytes()))
        }
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
    let output_label =
        || contract.output_label_for_resolutions(views.tool_resolutions(dispatch).unwrap_or_default(), expansions);
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
            if contract.pending_cast_dim().is_some() {
                return Err(AdmitError::OutputPendingCast);
            }
            vec![close_success(), admit_value(output_label(), body)]
        }
        ResultAdmission::SuccessCast { body, cast, resolved } => {
            let candidate = cast_candidate(registry, views, dispatch, contract, &cast, body, &resolved, expansions)?;
            let DerivedCandidate::Result {
                source,
                value,
                residual,
                ..
            } = candidate
            else {
                unreachable!("a pending-cast resolution derives a confined result")
            };
            if residual.is_some() {
                return Err(AdmitError::NarrowingUnaccepted);
            }
            vec![
                close_success(),
                Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    resolved,
                    raw_digest: source,
                    resolutions: registry.resolutions(expansions),
                },
                admit_derived(value),
            ]
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
                    via: crate::candidate::DerivedVia::Sanitizer {
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
                source,
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
            if let Some(crate::candidate::DerivedVia::Cast { name }) = views.candidate_via(&subject) {
                let resolved = EstablishedLabel::from_label(&value.label)
                    .expect("a cast candidate carries the complete resolved label");
                facts.push(Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast: name.clone(),
                    resolved,
                    raw_digest: *source,
                    resolutions: registry.resolutions(expansions),
                });
            }
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

/// Validate a whole-source cast answer against the registered cast and the target value, then
/// emit the one `CastApplied` fact: the complete resolution or nothing.
pub(crate) fn admit_cast(
    registry: &Registry,
    views: &Views,
    value: ValueId,
    answer: CastAnswer,
    expansions: &Expansions,
) -> Result<Vec<Fact>, CastError> {
    let cast = registry
        .cast(&answer.cast)
        .ok_or_else(|| CastError::UnknownCast(answer.cast.as_str().to_string()))?;
    let prior = views.value_label(value).ok_or(CastError::UnknownValue)?;
    if !views.may_resolve(value) {
        return Err(CastError::ForeignValue);
    }
    // The scope gate, the one routing predicate planning and validation also run.
    let applicable = cast
        .scope
        .reaches(registry, views, value)
        .expect("admission resolves casts only for values whose routing records the log retains");
    if !applicable {
        return Err(CastError::OutOfScope);
    }
    if EstablishedLabel::from_label(prior).is_some() {
        return Err(CastError::AlreadyEstablished);
    }
    cast.resolution
        .validate(prior, &answer.resolved, expansions)
        .map_err(|refusal| match refusal {
            CastRefusal::NonLiteralReader => CastError::NonLiteralAnswer,
            CastRefusal::ConstantMismatch => CastError::ConstantMismatch,
            CastRefusal::EstablishedMismatch(_) => CastError::EstablishedMismatch,
            CastRefusal::CeilingExceeded(_) => CastError::CeilingExceeded,
        })?;

    let fact = Fact::CastApplied {
        trajectory: views.trajectory().clone(),
        value,
        resolved: answer.resolved,
        cast: answer.cast,
        resolutions: registry.resolutions(expansions),
    };
    Ok(vec![fact])
}

#[cfg(test)]
mod tests {
    const BODY: &str = "the result";

    use super::*;
    use crate::authority::{
        Cast, CastCeiling, CastResolution, DeclaredLabel, DeclaredTransition, Sanitizer, SanitizerPoints, Scope,
    };
    use crate::contract::{Delta, PinnedToolResolution, ResolverReturn, ToolContract, ToolResolverUse};
    use crate::fact::EffectKind;
    use crate::groups::DeclaredAudience;
    use crate::label::{Audience, Dim, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, ToolName, TrajectoryId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn dynamic_binding() -> ToolResolverUse {
        ToolResolverUse {
            resolver: crate::names::DynamicResolverName::new("directory"),
            inputs: std::collections::BTreeMap::from([(
                "room".to_string(),
                crate::contract::ToolCallSource::argument("room").expect("a plain name is a source"),
            )]),
            returns: [ResolverReturn::Audience].into_iter().collect(),
        }
    }

    fn audience_pin(audience: Audience) -> PinnedToolResolution {
        PinnedToolResolution::from_answer(
            dynamic_binding(),
            crate::contract::ResolverArgsDigest::of(b""),
            None,
            Some(audience),
            None,
            None,
            None,
        )
        .expect("a literal reader set pins")
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn opened() -> Fact {
        crate::profile::opening_at(
            traj(),
            Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public)),
        )
    }

    fn registry() -> Registry {
        let get = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal()).into()),
            }),
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
        let const_cast = Cast {
            name: CastName::new("paranoid"),
            resolution: CastResolution::Constant(DeclaredLabel::literal(EstablishedLabel::new(SUSPICIOUS, internal()))),
            scope: Scope::default(),
        };
        let resolver_cast = Cast {
            name: CastName::new("classifier"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: DeclaredAudience::literal(Audience::restricted([
                        ReaderId::new("finance"),
                        ReaderId::new("audit"),
                    ])),
                },
            },
            scope: Scope::default(),
        };
        let scan = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal()).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let poll = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("poll_room"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Unknown.into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        let dynamic_scan = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![dynamic_binding()],
            name: ToolName::new("dynamic_scan"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            parameters: crate::params::test_string_argument_schema("room"),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![get, scan, poll, dynamic_scan],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            casts: vec![resolver_cast, const_cast],
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
            contract: call.contract_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            receiving: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            tool_resolutions: Vec::new(),
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

    fn staged_cast_candidate(
        dispatch: &DispatchId,
        body: &str,
        resolved: EstablishedLabel,
        residual: Narrowing,
    ) -> Fact {
        Fact::CandidateDerived {
            trajectory: traj(),
            subject: crate::basis::SubjectKey::ConfinedResult(dispatch.clone()),
            via: crate::candidate::DerivedVia::Cast {
                name: CastName::new("paranoid"),
            },
            derived: DerivedCandidate::Result {
                dispatch: dispatch.clone(),
                source: RawResultDigest::of(body.as_bytes()),
                from: ConfinedFrom::Bound,
                value: LabeledValue::new(ValueBody::new(body), resolved.into_label()),
                residual: Some(residual),
            },
            lineage: SanitizerLineage::default(),
            resolutions: vec![],
        }
    }

    #[test]
    fn foreign_trajectory_cannot_close_or_cast() {
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
        let value_log = unknown_value_log();
        let p2 = views_of(&value_log);
        assert_eq!(
            admit_cast(
                &reg,
                &p2.view(&sibling),
                ValueId::new(0),
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
                },
                &Expansions::default()
            ),
            Err(CastError::ForeignValue)
        );
        assert_eq!(
            admit_cast(
                &reg,
                &p2.view(&sibling),
                ValueId::new(99),
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
                },
                &Expansions::default()
            ),
            Err(CastError::UnknownValue)
        );
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
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(internal()));
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

    fn unknown_value_log() -> Vec<Fact> {
        let (mut log, dispatch) = open_log(&scan_call());
        log.push(Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::ToolResult { dispatch },
        });
        log
    }

    #[test]
    fn a_scoped_cast_applies_only_to_covered_tool_results() {
        let fetch = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("fetch"),
            tags: vec![crate::names::TagName::new("web")],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(Audience::Public).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Default::default(),
        };
        let note = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("note"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(Audience::Public).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Default::default(),
        };
        let webby = Cast {
            name: CastName::new("webby"),
            resolution: CastResolution::Constant(DeclaredLabel::literal(EstablishedLabel::new(
                SUSPICIOUS,
                Audience::Public,
            ))),
            scope: Scope {
                tags: vec![crate::names::TagName::new("web")],
            },
        };
        let fallback = Cast {
            name: CastName::new("fallback"),
            resolution: CastResolution::Constant(DeclaredLabel::literal(EstablishedLabel::new(
                SUSPICIOUS,
                Audience::Public,
            ))),
            scope: Scope::default(),
        };
        let reg = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch, note],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![webby, fallback],
            membership: None,
        })
        .unwrap();
        let fetch_call = ResolvedCall::new(ToolName::new("fetch"), crate::params::test_arguments(&json!({})));
        let note_call = ResolvedCall::new(ToolName::new("note"), crate::params::test_arguments(&json!({})));
        let (fetch_opened, fetch_dispatch) = dispatch_opened(&fetch_call);
        let (note_opened, note_dispatch) = dispatch_opened(&note_call);
        let log = vec![
            opened(),
            fetch_opened,
            note_opened,
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("page"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: Provenance::ToolResult {
                    dispatch: fetch_dispatch,
                },
            },
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("note"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: Provenance::ToolResult {
                    dispatch: note_dispatch,
                },
            },
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("digest"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: Provenance::ChildReturn {
                    child: TrajectoryId::new("child"),
                    id: crate::value::ChildReturnId::new(TrajectoryId::new("child"), 0),
                },
            },
        ];
        let p = views_of(&log);
        let answer = |cast: &str| CastAnswer {
            cast: CastName::new(cast),
            resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
        };
        assert!(
            admit_cast(
                &reg,
                &p.view(&traj()),
                ValueId::new(0),
                answer("webby"),
                &Expansions::default()
            )
            .is_ok()
        );
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&traj()),
                ValueId::new(1),
                answer("webby"),
                &Expansions::default()
            ),
            Err(CastError::OutOfScope)
        );
        assert!(
            admit_cast(
                &reg,
                &p.view(&traj()),
                ValueId::new(1),
                answer("fallback"),
                &Expansions::default()
            )
            .is_ok()
        );
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&traj()),
                ValueId::new(2),
                answer("webby"),
                &Expansions::default()
            ),
            Err(CastError::OutOfScope)
        );
        assert!(
            admit_cast(
                &reg,
                &p.view(&traj()),
                ValueId::new(2),
                answer("fallback"),
                &Expansions::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn cast_within_ceiling_admits_and_resolves_fold() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        let batch = admit_cast(
            &reg,
            &p.view(&t),
            ValueId::new(0),
            CastAnswer {
                cast: CastName::new("classifier"),
                resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
            },
            &Expansions::default(),
        )
        .unwrap();
        let mut next = log.clone();
        next.extend(batch);
        let p2 = views_of(&next);
        let current = p2.view(&t).current_label();
        assert!(current.is_fully_established());
        assert_eq!(current.bound().trust, SUSPICIOUS);
    }

    #[test]
    fn cast_exceeding_ceiling_rejected() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                ValueId::new(0),
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(Trust::new(1), Audience::Public),
                },
                &Expansions::default()
            ),
            Err(CastError::CeilingExceeded)
        );
    }

    #[test]
    fn a_mismatched_established_dimension_is_refused_whole() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                ValueId::new(0),
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")])),
                },
                &Expansions::default()
            ),
            Err(CastError::EstablishedMismatch)
        );
    }

    #[test]
    fn a_second_resolution_after_the_first_admitted_answer_is_refused() {
        let reg = registry();
        let mut log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        let first = admit_cast(
            &reg,
            &p.view(&t),
            ValueId::new(0),
            CastAnswer {
                cast: CastName::new("classifier"),
                resolved: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
            },
            &Expansions::default(),
        )
        .unwrap();
        log.extend(first);
        let p = views_of(&log);
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                ValueId::new(0),
                CastAnswer {
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
                &Expansions::default()
            ),
            Err(CastError::AlreadyEstablished)
        );
        assert_eq!(
            p.view(&t).value_label(ValueId::new(0)),
            Some(&EstablishedLabel::new(SUSPICIOUS, Audience::Public).into_label())
        );
    }

    #[test]
    fn a_moved_established_dimension_demands_acceptance() {
        let reg = registry();
        let call = scan_call();
        let (mut log, dispatch) = open_log(&call);
        log.push(Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("merged from a child"),
                Label::new(
                    Dim::Known(SUSPICIOUS),
                    Dim::Known(Audience::restricted([ReaderId::new("finance")])),
                ),
            ),
            provenance: Provenance::ChildReturn {
                child: TrajectoryId::new("child"),
                id: crate::value::ChildReturnId::new(TrajectoryId::new("child"), 0),
            },
        });
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("inbox contents"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
                &Expansions::default()
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
    }

    #[test]
    fn an_audience_resolution_is_bounded_at_the_engine_not_the_wire() {
        let reg = registry();
        let call = ResolvedCall::new(ToolName::new("poll_room"), crate::params::test_arguments(&json!({})));
        let (log, dispatch) = open_log(&call);
        let t = traj();
        let attempt = |audience: Audience| {
            let p = views_of(&log);
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("room roster"),
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, audience),
                },
                &Expansions::default(),
            )
        };
        for malformed in [
            Audience::restricted([ReaderId::new("@hr")]),
            Audience::restricted([ReaderId::new("public")]),
        ] {
            assert_eq!(attempt(malformed), Err(AdmitError::NonLiteralAnswer));
        }
        for out_of_cap in [Audience::Public, Audience::restricted([ReaderId::new("stranger")])] {
            assert_eq!(attempt(out_of_cap), Err(AdmitError::CeilingExceeded));
        }
    }

    #[test]
    fn a_public_resolution_is_admitted_under_a_public_cap() {
        let fetch = ToolContract {
            description: Some("A test tool.".to_string()),
            uses: vec![],
            name: ToolName::new("fetch_page"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Unknown.into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Default::default(),
        };
        let librarian = Cast {
            name: CastName::new("librarian"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: DeclaredAudience::literal(Audience::Public),
                },
            },
            scope: Scope::default(),
        };
        let reg = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![librarian],
            membership: None,
        })
        .unwrap();
        let call = ResolvedCall::new(ToolName::new("fetch_page"), crate::params::test_arguments(&json!({})));
        let (log, dispatch) = open_log(&call);
        let t = traj();
        let attempt = |resolved: Audience| {
            let p = views_of(&log);
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("wiki article"),
                    cast: CastName::new("librarian"),
                    resolved: EstablishedLabel::new(Trust::new(u8::MAX), resolved),
                },
                &Expansions::default(),
            )
        };
        assert_eq!(
            attempt(Audience::restricted([ReaderId::new("public")])),
            Err(AdmitError::NonLiteralAnswer)
        );
        let batch = attempt(Audience::Public).unwrap();
        match batch.last().unwrap() {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.audience, Dim::Known(Audience::Public));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn an_audience_pending_cast_follows_the_same_acceptance_discipline() {
        let reg = registry();
        let call = ResolvedCall::new(ToolName::new("poll_room"), crate::params::test_arguments(&json!({})));
        let (log, dispatch) = open_log(&call);
        let t = traj();
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("room roster"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
                &Expansions::default()
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
    }

    #[test]
    fn a_dynamic_audience_survives_trust_cast_acceptance_and_admission() {
        let reg = registry();
        let call = ResolvedCall::new(
            ToolName::new("dynamic_scan"),
            crate::params::test_arguments(&json!({ "room": "internal" })),
        )
        .with_tool_resolutions(vec![audience_pin(Audience::restricted([ReaderId::new("finance")]))]);
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let log = vec![Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            contract: call.contract_id(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            receiving: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            tool_resolutions: vec![audience_pin(internal())],
            memberships: Vec::new(),
            subject: crate::basis::fixture_subject(&traj()),
            resolutions: vec![],
        }];
        let projection = views_of(&log);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let expected = Narrowing {
            from: EstablishedLabel::top(),
            to: EstablishedLabel::new(SUSPICIOUS, internal()),
        };
        let resolved = EstablishedLabel::new(SUSPICIOUS, internal());
        assert_eq!(
            confined_residual(&EstablishedLabel::top(), &resolved.clone().into_label()),
            Some(expected.clone())
        );
        assert_eq!(
            admit_result(
                &reg,
                &views,
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("scan"),
                    cast: CastName::new("classifier"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")])),
                },
                &Expansions::default()
            ),
            Err(AdmitError::EstablishedMismatch)
        );
        assert_eq!(
            admit_result(
                &reg,
                &views,
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("scan"),
                    cast: CastName::new("paranoid"),
                    resolved: resolved.clone(),
                },
                &Expansions::default()
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );

        let mut log = log;
        log.push(staged_cast_candidate(&dispatch, "scan", resolved, expected));
        let projection = views_of(&log);
        let batch = admit_result(
            &reg,
            &projection.view(&trajectory),
            &dispatch,
            &call,
            ResultAdmission::CandidateAccepted { offer: offer() },
            &Expansions::default(),
        )
        .unwrap();
        match batch.last().unwrap() {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label, Label::new(Dim::Known(SUSPICIOUS), Dim::Known(internal())));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn pending_cast_confines_raw_admission() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("raw bytes"),
                },
                &Expansions::default()
            ),
            Err(AdmitError::OutputPendingCast)
        );
    }

    fn narrowed_open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let (mut log, dispatch) = open_log(call);
        log[0] = crate::profile::opening_at(traj(), Label::new(Dim::Known(SUSPICIOUS), Dim::Known(internal())));
        let Fact::DispatchOpened { receiving, .. } = &mut log[1] else {
            unreachable!("open_log holds exactly one DispatchOpened")
        };
        *receiving = EstablishedLabel::new(SUSPICIOUS, internal());
        (log, dispatch)
    }

    #[test]
    fn a_non_narrowing_cast_admits_at_the_resolved_label() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = narrowed_open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCast {
                body: ValueBody::new("inbox contents"),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
            },
            &Expansions::default(),
        )
        .unwrap();
        assert!(matches!(
            &batch[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &EffectSet::new([EffectKind::new("read")]).unwrap()
        ));
        assert!(matches!(
            &batch[1],
            Fact::OutputCastApplied { resolved, .. } if resolved == &EstablishedLabel::new(SUSPICIOUS, internal())
        ));
        match &batch[2] {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(internal()));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn a_narrowing_cast_resolution_requires_acceptance() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("inbox contents"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
                &Expansions::default()
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
    }

    #[test]
    fn an_accepted_cast_narrowing_admits_in_one_batch() {
        let reg = registry();
        let call = scan_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();
        let accepted = Narrowing {
            from: EstablishedLabel::top(),
            to: EstablishedLabel::new(SUSPICIOUS, internal()),
        };
        let resolved = EstablishedLabel::new(SUSPICIOUS, internal());
        log.push(staged_cast_candidate(
            &dispatch,
            "inbox contents",
            resolved.clone(),
            accepted.clone(),
        ));
        let p = views_of(&log);
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::CandidateAccepted { offer: offer() },
            &Expansions::default(),
        )
        .unwrap();
        assert!(matches!(
            &batch[0],
            Fact::CandidateAccepted { narrowing, .. } if narrowing == &accepted
        ));
        assert!(matches!(
            &batch[1],
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { .. },
                ..
            }
        ));
        assert!(matches!(
            &batch[2],
            Fact::OutputCastApplied { cast, resolved: restated, raw_digest, .. }
                if cast.as_str() == "paranoid"
                    && restated == &resolved
                    && raw_digest == &RawResultDigest::of(b"inbox contents")
        ));
        match &batch[3] {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(internal()));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
        let mut next = log.clone();
        next.extend(batch);
        let p2 = views_of(&next);
        assert_eq!(p2.view(&t).current_label().bound().trust, SUSPICIOUS);
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
        let mut log = log;
        log.push(staged_cast_candidate(
            &dispatch,
            "inbox contents",
            EstablishedLabel::new(SUSPICIOUS, internal()),
            Narrowing {
                from: EstablishedLabel::top(),
                to: EstablishedLabel::new(SUSPICIOUS, internal()),
            },
        ));
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::CandidateAdmissible,
                &Expansions::default()
            ),
            Err(AdmitError::NoCandidate)
        );
    }

    #[test]
    fn pending_cast_admission_validates_the_resolution() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let t = traj();
        let admission = |cast: &str, resolved: EstablishedLabel| ResultAdmission::SuccessCast {
            body: ValueBody::new("inbox contents"),
            cast: CastName::new(cast),
            resolved,
        };
        let attempt = |adm: ResultAdmission| {
            let p = views_of(&log);
            admit_result(&reg, &p.view(&t), &dispatch, &call, adm, &Expansions::default())
        };
        assert_eq!(
            attempt(admission(
                "classifier",
                EstablishedLabel::new(Trust::new(1), internal())
            )),
            Err(AdmitError::CeilingExceeded)
        );
        assert_eq!(
            attempt(admission("paranoid", EstablishedLabel::new(Trust::new(1), internal()))),
            Err(AdmitError::ConstantMismatch)
        );
        assert_eq!(
            attempt(admission(
                "classifier",
                EstablishedLabel::new(SUSPICIOUS, Audience::restricted([ReaderId::new("finance")]))
            )),
            Err(AdmitError::EstablishedMismatch)
        );
        assert_eq!(
            attempt(admission("bogus", EstablishedLabel::new(SUSPICIOUS, internal()))),
            Err(AdmitError::UnknownCast("bogus".to_string()))
        );
        let plain = get_call();
        let (plain_log, plain_dispatch) = open_log(&plain);
        let p = views_of(&plain_log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &plain_dispatch,
                &plain,
                ResultAdmission::SuccessCast {
                    body: ValueBody::new("x"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
                &Expansions::default()
            ),
            Err(AdmitError::NotPendingCast)
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
            ResultAdmission::SuccessCast {
                body: ValueBody::new(BODY),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
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
                reg.tool(call.tool()).expect("the fixture registers the tool"),
                &call,
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
