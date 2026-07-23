//! Result and cast admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::authority::CastResolution;
use crate::check::UnresolvedFact;
use crate::fact::{CloseOutcome, Fact, FactBatch};
use crate::label::{Adequacy, Dim, DimValue, Label};
use crate::names::{CastName, SanitizerName};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody};

pub enum ResultAdmission {
    Failure,
    Indeterminate,
    SuccessNoValue,
    SuccessRaw { body: ValueBody },
    SuccessSanitized {
        body: ValueBody,
        sanitizer: SanitizerName,
        raw_digest: RawResultDigest,
    },
    SuccessCast {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
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
    #[error("no sanitizer registered as {0}")]
    UnknownSanitizer(String),
    #[error("sanitizer {0} is not registered for tool output")]
    SanitizerNotOutput(String),
    #[error("raw result does not satisfy the sanitizer's `from` precondition")]
    TransitionSourceUnmet,
    #[error("the contract declares a pending-cast output: only a cast-resolved admission may carry a value")]
    OutputPendingCast,
    #[error("the contract binds an output sanitizer: a raw value may not enter")]
    OutputSanitizerBound,
    #[error("the sanitizer is not the contract's bound output sanitizer")]
    NotBoundSanitizer,
    #[error("the contract declares no pending-cast output on the resolved dimension")]
    NotPendingCast,
    #[error("no cast registered as {0}")]
    UnknownCast(String),
    #[error("cast answer does not match the constant cast's declared target")]
    ConstantMismatch,
    #[error("cast answer exceeds the resolver's may_cast ceiling")]
    CeilingExceeded,
}

pub struct CastAnswer {
    pub cast: CastName,
    pub resolved: DimValue,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CastError {
    #[error("no cast registered as {0}")]
    UnknownCast(String),
    #[error("cast answer resolves a different dimension than the unresolved fact")]
    DimensionMismatch,
    #[error("cast answer exceeds the resolver's may_cast ceiling")]
    CeilingExceeded,
    #[error("cast answer does not match the constant cast's declared target")]
    ConstantMismatch,
    #[error("target value is unknown or out of range")]
    UnknownValue,
    #[error("target value belongs to another trajectory")]
    ForeignValue,
    #[error("target value's dimension is already established")]
    NotUnknown,
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

    let trajectory = views.trajectory().clone();
    let close_success = || Fact::DispatchClosed {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        outcome: CloseOutcome::Success {
            effects: contract.emits.clone(),
        },
    };
    let admit_value = |label: Label, body: ValueBody| Fact::ValueAdmitted {
        trajectory: trajectory.clone(),
        value: LabeledValue::new(body, label),
        provenance: Provenance::ToolResult {
            dispatch: dispatch.clone(),
        },
    };

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
            if contract.delta.pending_cast_dim().is_some() {
                return Err(AdmitError::OutputPendingCast);
            }
            if contract.output_sanitizer.is_some() {
                return Err(AdmitError::OutputSanitizerBound);
            }
            vec![close_success(), admit_value(contract.delta.output_label(), body)]
        }
        ResultAdmission::SuccessCast { body, cast, resolved } => {
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            if contract.delta.pending_cast_dim() != Some(resolved.dimension()) {
                return Err(AdmitError::NotPendingCast);
            }
            let registered = registry
                .cast(&cast)
                .ok_or_else(|| AdmitError::UnknownCast(cast.as_str().to_string()))?;
            match &registered.resolution {
                CastResolution::Constant(declared) => {
                    if &resolved != declared {
                        return Err(AdmitError::ConstantMismatch);
                    }
                }
                CastResolution::Resolver { may_cast } => {
                    if !may_cast.admits(&resolved) {
                        return Err(AdmitError::CeilingExceeded);
                    }
                }
            }
            let output = contract.delta.output_label();
            // Fill exactly the pending dimension; the established one is preserved untouched.
            let label = match &resolved {
                DimValue::Trust(t) => Label::new(Dim::Known(*t), output.audience),
                DimValue::Audience(a) => Label::new(output.trust, Dim::Known(a.clone())),
            };
            vec![
                close_success(),
                Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    dimension: resolved.dimension(),
                    resolved,
                    raw_digest,
                },
                admit_value(label, body),
            ]
        }
        ResultAdmission::SuccessSanitized {
            body,
            sanitizer,
            raw_digest,
        } => {
            if contract.delta.pending_cast_dim().is_some() {
                return Err(AdmitError::OutputPendingCast);
            }
            if contract.output_sanitizer.as_ref() != Some(&sanitizer) {
                return Err(AdmitError::NotBoundSanitizer);
            }
            let san = registry
                .sanitizer(&sanitizer)
                .ok_or_else(|| AdmitError::UnknownSanitizer(sanitizer.as_str().to_string()))?;
            if !san.on.output {
                return Err(AdmitError::SanitizerNotOutput(sanitizer.as_str().to_string()));
            }
            let raw = contract.delta.output_label();
            if raw.audience.covers(&san.can_reduce.from_includes) != Adequacy::Holds {
                return Err(AdmitError::TransitionSourceUnmet);
            }
            // Audience-only: trust is preserved from the raw, audience becomes the declared `to`.
            let sanitized = Label::new(raw.trust.clone(), Dim::Known(san.can_reduce.to.clone()));
            vec![
                close_success(),
                Fact::SanitizerApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    sanitizer,
                    raw_digest,
                    from: san.can_reduce.from_includes.clone(),
                    to: san.can_reduce.to.clone(),
                },
                admit_value(sanitized, body),
            ]
        }
    };

    Ok(FactBatch::new(views.revision(), facts))
}

pub(crate) fn admit_cast(
    registry: &Registry,
    views: &Views,
    target: &UnresolvedFact,
    answer: CastAnswer,
) -> Result<FactBatch, CastError> {
    let cast = registry
        .cast(&answer.cast)
        .ok_or_else(|| CastError::UnknownCast(answer.cast.as_str().to_string()))?;
    if answer.resolved.dimension() != target.dimension {
        return Err(CastError::DimensionMismatch);
    }
    match &cast.resolution {
        CastResolution::Constant(declared) => {
            if &answer.resolved != declared {
                return Err(CastError::ConstantMismatch);
            }
        }
        CastResolution::Resolver { may_cast } => {
            if !may_cast.admits(&answer.resolved) {
                return Err(CastError::CeilingExceeded);
            }
        }
    }
    // A cast fills an Unknown of the caller's own branch-local value, never a sibling's.
    if !views.owns_value(target.value) {
        return Err(CastError::ForeignValue);
    }
    let label = views.value_label(target.value).ok_or(CastError::UnknownValue)?;
    let is_unknown = match target.dimension {
        crate::label::Dimension::Trust => matches!(label.trust, Dim::Unknown),
        crate::label::Dimension::Audience => matches!(label.audience, Dim::Unknown),
    };
    if !is_unknown {
        return Err(CastError::NotUnknown);
    }

    let fact = Fact::CastApplied {
        trajectory: views.trajectory().clone(),
        value: target.value,
        dimension: target.dimension,
        resolved: answer.resolved,
        cast: answer.cast,
    };
    Ok(FactBatch::new(views.revision(), vec![fact]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AudienceTransition, Cast, CastCeiling, Sanitizer, SanitizerPoints};
    use crate::contract::{Delta, ToolContract};
    use crate::fact::{EffectKind, Revision};
    use crate::label::{Audience, Dim, Dimension, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, ToolName, TrajectoryId, ValueId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn registry() -> Registry {
        let get = ToolContract {
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            },
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: None,
        };
        let out_san = Sanitizer {
            name: crate::names::SanitizerName::new("declassify"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            can_reduce: AudienceTransition {
                from_includes: internal(),
                to: Audience::Public,
            },
        };
        let finance_san = Sanitizer {
            name: crate::names::SanitizerName::new("finance-only"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            can_reduce: AudienceTransition {
                from_includes: Audience::restricted([ReaderId::new("finance")]),
                to: Audience::Public,
            },
        };
        let const_cast = Cast {
            name: CastName::new("paranoid"),
            resolution: CastResolution::Constant(DimValue::Trust(SUSPICIOUS)),
        };
        let resolver_cast = Cast {
            name: CastName::new("classifier"),
            resolution: CastResolution::Resolver {
                may_cast: CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: vec![Audience::Public],
                },
            },
        };
        let scan = ToolContract {
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal())),
            },
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: None,
        };
        let export = ToolContract {
            name: ToolName::new("export_ticket"),
            tags: vec![],
            delta: Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            },
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: Some(crate::names::SanitizerName::new("declassify")),
        };
        Registry::build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![get, scan, export],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            casts: vec![const_cast, resolver_cast],
        })
        .unwrap()
    }

    fn export_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("export_ticket"), json!({}), vec![])
    }

    fn scan_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("scan_inbox"), json!({}), vec![])
    }

    fn get_call() -> ResolvedCall {
        ResolvedCall::new(ToolName::new("get_ticket"), json!({}), vec![])
    }

    fn open_log(call: &ResolvedCall) -> (Vec<Fact>, DispatchId) {
        let dispatch = DispatchId::new(traj(), call.digest(), 0);
        let log = vec![Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: dispatch.clone(),
            proposed_label: Label::top(),
            proposed_effects: vec![EffectKind::new("read")],
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
                &UnresolvedFact {
                    value: ValueId::new(0),
                    dimension: Dimension::Trust,
                },
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: DimValue::Trust(SUSPICIOUS),
                },
            ),
            Err(CastError::ForeignValue)
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
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &[EffectKind::new("read")]
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
    fn sanitized_preserves_trust_relabels_audience() {
        let reg = registry();
        let call = export_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessSanitized {
                body: ValueBody::new("redacted"),
                sanitizer: SanitizerName::new("declassify"),
                raw_digest: RawResultDigest::of(b"ticket #7"),
            },
        )
        .unwrap();
        match batch.facts.last().unwrap() {
            Fact::ValueAdmitted { value, .. } => {
                assert_eq!(value.label.trust, Dim::Known(SUSPICIOUS));
                assert_eq!(value.label.audience, Dim::Known(Audience::Public));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn a_bound_tool_confines_raw_and_refuses_an_unbound_transformer() {
        let reg = registry();
        let t = traj();
        let call = export_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("ticket #7"),
                },
            ),
            Err(AdmitError::OutputSanitizerBound)
        );
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: SanitizerName::new("finance-only"),
                    raw_digest: RawResultDigest::of(b"ticket #7"),
                },
            ),
            Err(AdmitError::NotBoundSanitizer)
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
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"ticket #7"),
                },
            ),
            Err(AdmitError::NotBoundSanitizer)
        );
    }

    #[test]
    fn swapped_call_and_unopened_rejected() {
        let reg = registry();
        let call = get_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        let other = ResolvedCall::new(ToolName::new("get_ticket"), json!({ "x": 1 }), vec![]);
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
    fn cast_within_ceiling_admits_and_resolves_fold() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        let target = UnresolvedFact {
            value: ValueId::new(0),
            dimension: Dimension::Trust,
        };
        let batch = admit_cast(
            &reg,
            &p.view(&t),
            &target,
            CastAnswer {
                cast: CastName::new("classifier"),
                resolved: DimValue::Trust(SUSPICIOUS),
            },
        )
        .unwrap();
        let mut next = log.clone();
        next.extend(batch.facts);
        let p2 = views_of(&next);
        assert_eq!(p2.view(&t).current_label().trust, Dim::Known(SUSPICIOUS));
    }

    #[test]
    fn cast_exceeding_ceiling_rejected() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        let target = UnresolvedFact {
            value: ValueId::new(0),
            dimension: Dimension::Trust,
        };
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                &target,
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: DimValue::Trust(Trust::new(1)),
                }
            ),
            Err(CastError::CeilingExceeded)
        );
    }

    #[test]
    fn cast_dimension_mismatch_and_already_known_rejected() {
        let reg = registry();
        let log = unknown_value_log();
        let p = views_of(&log);
        let t = traj();
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                &UnresolvedFact {
                    value: ValueId::new(0),
                    dimension: Dimension::Trust,
                },
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: DimValue::Audience(Audience::Public),
                }
            ),
            Err(CastError::DimensionMismatch)
        );
        assert_eq!(
            admit_cast(
                &reg,
                &p.view(&t),
                &UnresolvedFact {
                    value: ValueId::new(0),
                    dimension: Dimension::Audience,
                },
                CastAnswer {
                    cast: CastName::new("classifier"),
                    resolved: DimValue::Audience(Audience::Public),
                }
            ),
            Err(CastError::NotUnknown)
        );
    }

    #[test]
    fn pending_cast_confines_raw_and_sanitized_admission() {
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
        let p = views_of(&log);
        assert_eq!(
            admit_result(
                &reg,
                &p.view(&t),
                &dispatch,
                &call,
                ResultAdmission::SuccessSanitized {
                    body: ValueBody::new("redacted"),
                    sanitizer: SanitizerName::new("declassify"),
                    raw_digest: RawResultDigest::of(b"raw bytes"),
                },
            ),
            Err(AdmitError::OutputPendingCast)
        );
    }

    #[test]
    fn pending_cast_admits_at_the_resolved_label() {
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
            ResultAdmission::SuccessCast {
                body: ValueBody::new("inbox contents"),
                cast: CastName::new("paranoid"),
                resolved: DimValue::Trust(SUSPICIOUS),
            },
        )
        .unwrap();
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &[EffectKind::new("read")]
        ));
        assert!(matches!(
            &batch.facts[1],
            Fact::OutputCastApplied { dimension: Dimension::Trust, resolved: DimValue::Trust(t), .. } if *t == SUSPICIOUS
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
    fn pending_cast_admission_validates_the_resolution() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let t = traj();
        let admission = |cast: &str, resolved: DimValue| ResultAdmission::SuccessCast {
            body: ValueBody::new("inbox contents"),
            cast: CastName::new(cast),
            resolved,
        };
        let attempt = |adm: ResultAdmission| {
            let p = views_of(&log);
            admit_result(&reg, &p.view(&t), &dispatch, &call, adm)
        };
        assert_eq!(
            attempt(admission("classifier", DimValue::Trust(Trust::new(1)))),
            Err(AdmitError::CeilingExceeded)
        );
        assert_eq!(
            attempt(admission("paranoid", DimValue::Trust(Trust::new(1)))),
            Err(AdmitError::ConstantMismatch)
        );
        assert_eq!(
            attempt(admission("classifier", DimValue::Audience(Audience::Public))),
            Err(AdmitError::NotPendingCast)
        );
        assert_eq!(
            attempt(admission("bogus", DimValue::Trust(SUSPICIOUS))),
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
                    resolved: DimValue::Trust(SUSPICIOUS),
                },
            ),
            Err(AdmitError::NotPendingCast)
        );
    }
}
