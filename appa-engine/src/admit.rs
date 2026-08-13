//! Result and cast admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::authority::CastRefusal;
use crate::check::Narrowing;
use crate::fact::{CloseOutcome, EffectSet, Fact, FactBatch, ObservedResult};
use crate::label::{EstablishedLabel, Label};
use crate::names::CastName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody, ValueId};

pub enum ResultAdmission {
    Failure,
    Indeterminate,
    SuccessNoValue,
    SuccessRaw { body: ValueBody },
    SuccessCast {
        body: ValueBody,
        cast: CastName,
        resolved: EstablishedLabel,
    },
    SuccessCastAccepted {
        body: ValueBody,
        cast: CastName,
        resolved: EstablishedLabel,
        accepted: Narrowing,
    },
    SuccessCastLapsed {
        body: ValueBody,
        cast: CastName,
        resolved: EstablishedLabel,
    },
    SuccessSanitized {
        body: ValueBody,
        sanitizer: crate::names::SanitizerName,
        raw_digest: RawResultDigest,
    },
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
    #[error("the accepted narrowing does not match the live trajectory state")]
    AcceptanceMismatch,
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
}

pub(crate) fn observe_success(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    call: &ResolvedCall,
    observed: ObservedResult,
) -> Result<FactBatch, AdmitError> {
    let contract = registry
        .tool(call.tool())
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
    Ok(FactBatch::new(
        views.revision(),
        vec![Fact::DispatchSucceeded {
            trajectory: views.trajectory().clone(),
            dispatch: dispatch.clone(),
            effects: contract.emits.clone(),
            observed,
        }],
    ))
}

pub(crate) fn pending_cast_narrowing(views: &Views, resolved: &EstablishedLabel) -> Option<Narrowing> {
    let from = views.current_label().bound().clone();
    let to = from.combine(resolved);
    if to == from { None } else { Some(Narrowing { from, to }) }
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
        .validate(output_label, resolved)
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
) -> Result<FactBatch, AdmitError> {
    let contract = registry
        .tool(call.tool())
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
        ResultAdmission::SuccessRaw { body }
        | ResultAdmission::SuccessCast { body, .. }
        | ResultAdmission::SuccessCastAccepted { body, .. }
        | ResultAdmission::SuccessCastLapsed { body, .. } => Some(RawResultDigest::of(body.as_str().as_bytes())),
        ResultAdmission::SuccessSanitized { raw_digest, .. } => Some(*raw_digest),
        ResultAdmission::SuccessNoValue | ResultAdmission::Failure | ResultAdmission::Indeterminate => None,
    };
    if let (Some(reported), Some(observed)) = (reported, views.observed_result(dispatch))
        && observed != &ObservedResult::Available(reported)
    {
        return Err(AdmitError::ObservationMismatch);
    }

    let trajectory = views.trajectory().clone();
    let output_label =
        || contract.output_label_for_resolutions(views.dynamic_resolutions(dispatch).unwrap_or_default());
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
    let admit_value = |label: Label, body: ValueBody| Fact::ValueAdmitted {
        trajectory: trajectory.clone(),
        value: LabeledValue::new(body, label),
        provenance: Provenance::ToolResult {
            dispatch: dispatch.clone(),
        },
    };

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
            validate_pending_cast(registry, contract, &output_label(), &cast, &resolved)?;
            if pending_cast_narrowing(views, &resolved).is_some() {
                return Err(AdmitError::NarrowingUnaccepted);
            }
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                close_success(),
                Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    resolved: resolved.clone(),
                    raw_digest,
                },
                admit_value(resolved.into_label(), body),
            ]
        }
        ResultAdmission::SuccessCastAccepted {
            body,
            cast,
            resolved,
            accepted,
        } => {
            validate_pending_cast(registry, contract, &output_label(), &cast, &resolved)?;
            if pending_cast_narrowing(views, &resolved) != Some(accepted.clone()) {
                return Err(AdmitError::AcceptanceMismatch);
            }
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                close_success(),
                Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    resolved: resolved.clone(),
                    raw_digest,
                },
                Fact::OutputCastAccepted {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    narrowing: accepted,
                },
                admit_value(resolved.into_label(), body),
            ]
        }
        ResultAdmission::SuccessSanitized {
            body,
            sanitizer,
            raw_digest,
        } => {
            match bound {
                Some(name) if name == &sanitizer => {}
                Some(_) => return Err(AdmitError::SanitizerBindingMismatch),
                None => return Err(AdmitError::SanitizerBindingMismatch),
            }
            let registered = registry
                .sanitizer(&sanitizer)
                .ok_or_else(|| AdmitError::UnknownTool(sanitizer.as_str().to_string()))?;
            let raw_label = output_label();
            let derived = registered
                .derive_output(&raw_label)
                .ok_or(AdmitError::SanitizerTransitionUnmet)?;
            vec![
                close_success(),
                Fact::OutputSanitizerApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    sanitizer,
                    transition: registered.transition.clone(),
                    raw_digest,
                },
                admit_value(derived, body),
            ]
        }
        ResultAdmission::SuccessCastLapsed { body, cast, resolved } => {
            validate_pending_cast(registry, contract, &output_label(), &cast, &resolved)?;
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                close_success(),
                Fact::OutputCastLapsed {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    resolved,
                    raw_digest,
                },
            ]
        }
    };

    Ok(FactBatch::new(views.revision(), facts))
}

/// Validate a whole-source cast answer against the registered cast and the target value, then
/// emit the one `CastApplied` fact: the complete resolution or nothing.
pub(crate) fn admit_cast(
    registry: &Registry,
    views: &Views,
    value: ValueId,
    answer: CastAnswer,
) -> Result<FactBatch, CastError> {
    let cast = registry
        .cast(&answer.cast)
        .ok_or_else(|| CastError::UnknownCast(answer.cast.as_str().to_string()))?;
    let prior = views.value_label(value).ok_or(CastError::UnknownValue)?;
    if !views.may_resolve(value) {
        return Err(CastError::ForeignValue);
    }
    let applicable = match views
        .value_provenance(value)
        .expect("the value_label lookup proved the record exists")
    {
        crate::value::Provenance::ToolResult { dispatch } => {
            let tool = views
                .dispatch_tool(dispatch)
                .expect("a result value's dispatch is opened in the log");
            let contract = registry.tool(tool).expect("dispatches open only for registered tools");
            cast.scope.covers(&contract.tags)
        }
        crate::value::Provenance::UserInput | crate::value::Provenance::ChildReturn { .. } => cast.scope.is_unscoped(),
    };
    if !applicable {
        return Err(CastError::OutOfScope);
    }
    if EstablishedLabel::from_label(prior).is_some() {
        return Err(CastError::AlreadyEstablished);
    }
    cast.resolution
        .validate(prior, &answer.resolved)
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
    };
    Ok(FactBatch::new(views.revision(), vec![fact]))
}

#[cfg(test)]
mod tests {
    const BODY: &str = "the result";

    use super::*;
    use crate::authority::{Cast, CastCeiling, CastResolution, Sanitizer, SanitizerPoints, Scope, Transition};
    use crate::contract::{AudienceDelta, Delta, DynamicAudienceBinding, PinnedDynamicResolution, ToolContract};
    use crate::fact::{EffectKind, Revision};
    use crate::label::{Audience, Dim, PartialLabel, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, ToolName, TrajectoryId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn dynamic_binding() -> DynamicAudienceBinding {
        DynamicAudienceBinding {
            resolver: crate::names::DynamicResolverName::new("directory"),
            argument: "room".into(),
        }
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn registry() -> Registry {
        let get = ToolContract {
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
            transition: Transition::Audience {
                from_includes: internal(),
                to: Audience::Public,
            },
            hint: None,
        };
        let finance_san = Sanitizer {
            name: crate::names::SanitizerName::new("finance-only"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: Transition::Audience {
                from_includes: Audience::restricted([ReaderId::new("finance")]),
                to: Audience::Public,
            },
            hint: None,
        };
        let const_cast = Cast {
            name: CastName::new("paranoid"),
            resolution: CastResolution::Constant(EstablishedLabel::new(SUSPICIOUS, internal())),
            scope: Scope::default(),
        };
        let resolver_cast = Cast {
            name: CastName::new("classifier"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::restricted([ReaderId::new("finance"), ReaderId::new("audit")]),
                },
            },
            scope: Scope::default(),
        };
        let scan = ToolContract {
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
            name: ToolName::new("dynamic_scan"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(AudienceDelta::Dynamic(dynamic_binding())),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Default::default(),
        };
        Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![get, scan, poll, dynamic_scan],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            casts: vec![resolver_cast, const_cast],
        })
        .unwrap()
    }

    fn scan_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("scan_inbox"), crate::params::test_arguments(&json!({})))
    }

    fn get_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("get_ticket"), crate::params::test_arguments(&json!({})))
    }

    fn open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let log = vec![Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            dynamic_resolutions: Vec::new(),
        }];
        (log, dispatch)
    }

    fn views_of(log: &[Fact]) -> Projection {
        Projection::build(log, Revision::new(log.len() as u64))
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
        )
        .unwrap();
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &EffectSet::new([EffectKind::new("read")]).unwrap()
        ));
        match &batch.facts[1] {
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
        let batch = admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure).unwrap();
        assert_eq!(batch.facts.len(), 1);
        assert!(matches!(
            &batch.facts[0],
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
            admit_result(&reg, &p.view(&t), &dispatch, &other, ResultAdmission::SuccessNoValue),
            Err(AdmitError::DigestMismatch)
        );
        let empty = views_of(&[]);
        assert_eq!(
            admit_result(&reg, &empty.view(&t), &dispatch, &call, ResultAdmission::SuccessNoValue),
            Err(AdmitError::NotOpen)
        );
    }

    fn unknown_value_log() -> Vec<Fact> {
        vec![Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::UserInput,
        }]
    }

    #[test]
    fn a_scoped_cast_applies_only_to_covered_tool_results() {
        let fetch = ToolContract {
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
        let webby = Cast {
            name: CastName::new("webby"),
            resolution: CastResolution::Constant(EstablishedLabel::new(SUSPICIOUS, Audience::Public)),
            scope: Scope {
                tags: vec![crate::names::TagName::new("web")],
            },
        };
        let fallback = Cast {
            name: CastName::new("fallback"),
            resolution: CastResolution::Constant(EstablishedLabel::new(SUSPICIOUS, Audience::Public)),
            scope: Scope::default(),
        };
        let reg = Registry::build_covered(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![webby, fallback],
        })
        .unwrap();
        let call = ResolvedCall::new(ToolName::new("fetch"), crate::params::test_arguments(&json!({})));
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let log = vec![
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch: dispatch.clone(),
                tool: call.tool().clone(),
                arguments: call.canonical_arguments().clone(),
                proposed_label: EstablishedLabel::top(),
                proposed_effects: EffectSet::default(),
                dynamic_resolutions: Vec::new(),
            },
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("page"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: Provenance::ToolResult { dispatch },
            },
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("note"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: Provenance::UserInput,
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
        assert!(admit_cast(&reg, &p.view(&traj()), ValueId::new(0), answer("webby")).is_ok());
        assert_eq!(
            admit_cast(&reg, &p.view(&traj()), ValueId::new(1), answer("webby")),
            Err(CastError::OutOfScope)
        );
        assert!(admit_cast(&reg, &p.view(&traj()), ValueId::new(1), answer("fallback")).is_ok());
        assert_eq!(
            admit_cast(&reg, &p.view(&traj()), ValueId::new(2), answer("webby")),
            Err(CastError::OutOfScope)
        );
        assert!(admit_cast(&reg, &p.view(&traj()), ValueId::new(2), answer("fallback")).is_ok());
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
        )
        .unwrap();
        let mut next = log.clone();
        next.extend(batch.facts);
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
                }
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
                }
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
        )
        .unwrap();
        log.extend(first.facts);
        let p = views_of(&log);
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                ValueId::new(0),
                CastAnswer {
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                }
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
            provenance: Provenance::UserInput,
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
                    audience: Audience::Public,
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
            )
        };
        assert_eq!(
            attempt(Audience::restricted([ReaderId::new("public")])),
            Err(AdmitError::NonLiteralAnswer)
        );
        let batch = attempt(Audience::Public).unwrap();
        match batch.facts.last().unwrap() {
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
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
        let p = views_of(&log);
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCastAccepted {
                body: ValueBody::new("room roster"),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                accepted: Narrowing {
                    from: EstablishedLabel::top(),
                    to: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
            },
        )
        .unwrap();
        match batch.facts.last().unwrap() {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(internal()));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn a_dynamic_audience_survives_trust_cast_acceptance_and_admission() {
        let reg = registry();
        let binding = dynamic_binding();
        let call = ResolvedCall::new(
            ToolName::new("dynamic_scan"),
            crate::params::test_arguments(&json!({ "room": "internal" })),
        )
        .with_dynamic_resolutions(vec![PinnedDynamicResolution::from_answer(
            binding.clone(),
            Some(Audience::restricted([ReaderId::new("finance")])),
        )]);
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let log = vec![Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            tool: call.tool().clone(),
            arguments: call.canonical_arguments().clone(),
            proposed_label: EstablishedLabel::top(),
            proposed_effects: EffectSet::new([EffectKind::new("read")]).unwrap(),
            dynamic_resolutions: vec![PinnedDynamicResolution::from_answer(binding, Some(internal()))],
        }];
        let projection = views_of(&log);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let expected = Narrowing {
            from: EstablishedLabel::top(),
            to: EstablishedLabel::new(SUSPICIOUS, internal()),
        };
        let resolved = EstablishedLabel::new(SUSPICIOUS, internal());
        assert_eq!(pending_cast_narrowing(&views, &resolved), Some(expected.clone()));
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
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );

        let batch = admit_result(
            &reg,
            &views,
            &dispatch,
            &call,
            ResultAdmission::SuccessCastAccepted {
                body: ValueBody::new("scan"),
                cast: CastName::new("paranoid"),
                resolved,
                accepted: expected,
            },
        )
        .unwrap();
        match batch.facts.last().unwrap() {
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
            ),
            Err(AdmitError::OutputPendingCast)
        );
    }

    fn narrowed_open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let (mut log, dispatch) = open_log(call);
        log.insert(
            0,
            Fact::ValueAdmitted {
                trajectory: traj(),
                value: LabeledValue::new(
                    ValueBody::new("prior suspicious internal read"),
                    Label::new(Dim::Known(SUSPICIOUS), Dim::Known(internal())),
                ),
                provenance: Provenance::UserInput,
            },
        );
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
        )
        .unwrap();
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &EffectSet::new([EffectKind::new("read")]).unwrap()
        ));
        assert!(matches!(
            &batch.facts[1],
            Fact::OutputCastApplied { resolved, .. } if resolved == &EstablishedLabel::new(SUSPICIOUS, internal())
        ));
        match &batch.facts[2] {
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
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
    }

    #[test]
    fn an_accepted_cast_narrowing_admits_in_one_batch() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let accepted = Narrowing {
            from: EstablishedLabel::top(),
            to: EstablishedLabel::new(SUSPICIOUS, internal()),
        };
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCastAccepted {
                body: ValueBody::new("inbox contents"),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                accepted: accepted.clone(),
            },
        )
        .unwrap();
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { .. },
                ..
            }
        ));
        assert!(matches!(&batch.facts[1], Fact::OutputCastApplied { .. }));
        assert!(matches!(
            &batch.facts[2],
            Fact::OutputCastAccepted { narrowing, .. } if narrowing == &accepted
        ));
        match &batch.facts[3] {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(internal()));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
        let mut next = log.clone();
        next.extend(batch.facts);
        let p2 = views_of(&next);
        assert_eq!(p2.view(&t).current_label().bound().trust, SUSPICIOUS);
    }

    #[test]
    fn a_stale_cast_acceptance_is_refused() {
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
                ResultAdmission::SuccessCastAccepted {
                    body: ValueBody::new("inbox contents"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                    accepted: Narrowing {
                        from: EstablishedLabel::top(),
                        to: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
                    },
                },
            ),
            Err(AdmitError::AcceptanceMismatch)
        );
        let (narrowed, dispatch) = narrowed_open_log(&call);
        let p = views_of(&narrowed);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCastAccepted {
                    body: ValueBody::new("inbox contents"),
                    cast: CastName::new("paranoid"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                    accepted: Narrowing {
                        from: EstablishedLabel::top(),
                        to: EstablishedLabel::new(SUSPICIOUS, Audience::Public),
                    },
                },
            ),
            Err(AdmitError::AcceptanceMismatch)
        );
    }

    #[test]
    fn a_lapsed_cast_closes_with_audit_and_no_value() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCastLapsed {
                body: ValueBody::new("inbox contents"),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
            },
        )
        .unwrap();
        assert_eq!(batch.facts.len(), 2);
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &EffectSet::new([EffectKind::new("read")]).unwrap()
        ));
        assert!(matches!(
            &batch.facts[1],
            Fact::OutputCastLapsed {
                resolved,
                raw_digest,
                ..
            } if resolved == &EstablishedLabel::new(SUSPICIOUS, internal())
                && raw_digest == &RawResultDigest::of(b"inbox contents")
        ));
        let mut next = log.clone();
        next.extend(batch.facts);
        let p2 = views_of(&next);
        assert_eq!(
            p2.view(&t).current_label(),
            PartialLabel::established(EstablishedLabel::top())
        );
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessCastLapsed {
                    body: ValueBody::new("inbox contents"),
                    cast: CastName::new("bogus"),
                    resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
                },
            ),
            Err(AdmitError::UnknownCast("bogus".to_string()))
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
            admit_result(&reg, &p.view(&t), &dispatch, &call, adm)
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
            ),
            Err(AdmitError::NotPendingCast)
        );
    }

    #[test]
    fn a_success_checkpoint_commits_effects_once_and_pins_the_close_family() {
        let reg = registry();
        let call = scan_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();

        let p = views_of(&log);
        let observed = ObservedResult::Available(RawResultDigest::of(BODY.as_bytes()));
        let batch = observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()).unwrap();
        log.extend(batch.facts);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()),
            Err(AdmitError::AlreadySucceeded)
        );

        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure),
            Err(AdmitError::SuccessContradicted)
        );
        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Indeterminate),
            Err(AdmitError::SuccessContradicted)
        );

        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCastLapsed {
                body: ValueBody::new(BODY),
                cast: CastName::new("paranoid"),
                resolved: EstablishedLabel::new(SUSPICIOUS, internal()),
            },
        )
        .unwrap();
        assert!(batch.facts.iter().any(|fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { effects },
                ..
            } if effects.is_empty()
        )));
        log.extend(batch.facts);
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
        log.extend(batch.facts);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call, observed.clone()),
            Err(AdmitError::AlreadySucceeded)
        );
        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure),
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
        )
        .unwrap();
        assert!(batch.facts.iter().any(|fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { effects },
                ..
            } if effects.is_empty()
        )));
        assert!(
            batch
                .facts
                .iter()
                .any(|fact| matches!(fact, Fact::ValueAdmitted { .. }))
        );
        log.extend(batch.facts);
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
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );

        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessSanitized {
                body: ValueBody::new("redacted"),
                sanitizer: crate::names::SanitizerName::new("declassify"),
                raw_digest: RawResultDigest::of(b"ticket"),
            },
        )
        .unwrap();
        assert!(batch.facts.iter().any(|fact| matches!(
            fact,
            Fact::OutputSanitizerApplied { sanitizer, raw_digest, .. }
                if sanitizer.as_str() == "declassify" && raw_digest == &RawResultDigest::of(b"ticket")
        )));
        let admitted = batch
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::ValueAdmitted { value, .. } => Some(value),
                _ => None,
            })
            .expect("the derivation is admitted");
        assert_eq!(admitted.body.as_str(), "redacted");
        assert_eq!(
            admitted.label,
            Label::new(Dim::Known(SUSPICIOUS), Dim::Known(Audience::Public))
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
            ),
            Err(AdmitError::SanitizerBindingMismatch)
        );
    }
}
