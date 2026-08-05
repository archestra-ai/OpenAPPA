//! Result and cast admission: closing a dispatch and admitting (or withholding) its value.

use thiserror::Error;

use crate::authority::CastResolution;
use crate::check::{Narrowing, UnestablishedFact};
use crate::fact::{CloseOutcome, Fact, FactBatch};
use crate::label::{Adequacy, Dim, DimValue, Label};
use crate::names::CastName;
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody};

pub enum ResultAdmission {
    Failure,
    Indeterminate,
    SuccessNoValue,
    SuccessRaw { body: ValueBody },
    SuccessCast {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
    },
    SuccessCastAccepted {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
        accepted: Narrowing,
    },
    SuccessCastLapsed {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
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
    #[error("the contract declares no pending-cast output on the resolved dimension")]
    NotPendingCast,
    #[error("no cast registered as {0}")]
    UnknownCast(String),
    #[error("cast answer does not match the constant cast's declared target")]
    ConstantMismatch,
    #[error("cast answer exceeds the resolver's may_cast ceiling")]
    CeilingExceeded,
    #[error("the cast resolution narrows the trajectory label: admission requires the agent's acceptance")]
    NarrowingUnaccepted,
    #[error("the accepted narrowing does not match the live trajectory state")]
    AcceptanceMismatch,
    #[error("the dispatch already recorded its success checkpoint")]
    AlreadySucceeded,
    #[error("the dispatch recorded success: a failure or indeterminate close contradicts it")]
    SuccessContradicted,
    #[error("an executed plan bound this dispatch to a sanitizer: only its derivation may be admitted")]
    OutputSanitizerBound,
    #[error("the derivation names a sanitizer this dispatch is not bound to")]
    SanitizerBindingMismatch,
    #[error("the bound sanitizer is not registered for output, or the raw result does not satisfy its `from`")]
    SanitizerTransitionUnmet,
}

/// Record observed success for a **still-open** dispatch whose value finalization is deferred (a
/// pending-cast offer): the declared effects commit now — the spec's one append point at success —
/// so a later call's `no_prior(k)` sees them while the raw result stays confined awaiting the
/// agent's acceptance. The eventual close contributes no duplicate effects and must be
/// success-family ([`admit_result`] refuses a contradictory `Failure`/`Indeterminate`). A dispatch
/// checkpoints at most once — a repeat is refused, never silently absorbed.
pub(crate) fn observe_success(
    registry: &Registry,
    views: &Views,
    dispatch: &DispatchId,
    call: &ResolvedCall,
) -> Result<FactBatch, AdmitError> {
    let contract = registry
        .tool(call.tool())
        .ok_or_else(|| AdmitError::UnknownTool(call.tool().as_str().to_string()))?;
    if contract.pending_cast_dim().is_none() {
        return Err(AdmitError::NotPendingCast);
    }
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
        }],
    ))
}

pub(crate) fn pending_cast_narrowing(views: &Views, filled: &Label) -> Option<Narrowing> {
    let from = views.current_label();
    let to = from.combine(filled);
    if to == from { None } else { Some(Narrowing { from, to }) }
}

fn validate_cast_resolution(
    registry: &Registry,
    contract: &crate::contract::ToolContract,
    cast: &CastName,
    resolved: &DimValue,
) -> Result<(), AdmitError> {
    if contract.pending_cast_dim() != Some(resolved.dimension()) {
        return Err(AdmitError::NotPendingCast);
    }
    let registered = registry
        .cast(cast)
        .ok_or_else(|| AdmitError::UnknownCast(cast.as_str().to_string()))?;
    match &registered.resolution {
        CastResolution::Constant(declared) => {
            if resolved != declared {
                return Err(AdmitError::ConstantMismatch);
            }
        }
        CastResolution::Resolver { may_cast } => {
            if !may_cast.admits(resolved) {
                return Err(AdmitError::CeilingExceeded);
            }
        }
    }
    Ok(())
}

/// The output label with exactly the pending dimension filled by the resolution; the established
/// one is preserved untouched.
pub(crate) fn cast_filled_label(contract: &crate::contract::ToolContract, resolved: &DimValue) -> Label {
    let output = contract.output_label();
    match resolved {
        DimValue::Trust(t) => Label::new(Dim::Known(*t), output.audience),
        DimValue::Audience(a) => Label::new(output.trust, Dim::Known(a.clone())),
    }
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
    let checkpointed = views.is_succeeded(dispatch);
    if checkpointed && matches!(admission, ResultAdmission::Failure | ResultAdmission::Indeterminate) {
        return Err(AdmitError::SuccessContradicted);
    }

    let trajectory = views.trajectory().clone();
    let close_success = || Fact::DispatchClosed {
        trajectory: trajectory.clone(),
        dispatch: dispatch.clone(),
        outcome: CloseOutcome::Success {
            effects: if checkpointed {
                Vec::new()
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
            vec![close_success(), admit_value(contract.output_label(), body)]
        }
        ResultAdmission::SuccessCast { body, cast, resolved } => {
            validate_cast_resolution(registry, contract, &cast, &resolved)?;
            let label = cast_filled_label(contract, &resolved);
            if pending_cast_narrowing(views, &label).is_some() {
                return Err(AdmitError::NarrowingUnaccepted);
            }
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
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
        ResultAdmission::SuccessCastAccepted {
            body,
            cast,
            resolved,
            accepted,
        } => {
            validate_cast_resolution(registry, contract, &cast, &resolved)?;
            let label = cast_filled_label(contract, &resolved);
            if pending_cast_narrowing(views, &label) != Some(accepted.clone()) {
                return Err(AdmitError::AcceptanceMismatch);
            }
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                close_success(),
                Fact::OutputCastApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    dimension: resolved.dimension(),
                    resolved: resolved.clone(),
                    raw_digest,
                },
                Fact::OutputCastAccepted {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    narrowing: accepted,
                },
                admit_value(label, body),
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
            let raw_label = contract.output_label();
            if !registered.on.output || registered.transition.admits(&raw_label) != Adequacy::Holds {
                return Err(AdmitError::SanitizerTransitionUnmet);
            }
            vec![
                close_success(),
                Fact::OutputSanitizerApplied {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    sanitizer,
                    transition: registered.transition.clone(),
                    raw_digest,
                },
                admit_value(registered.transition.derive(&raw_label), body),
            ]
        }
        ResultAdmission::SuccessCastLapsed { body, cast, resolved } => {
            validate_cast_resolution(registry, contract, &cast, &resolved)?;
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            vec![
                close_success(),
                Fact::OutputCastLapsed {
                    trajectory: trajectory.clone(),
                    dispatch: dispatch.clone(),
                    cast,
                    dimension: resolved.dimension(),
                    resolved,
                    raw_digest,
                },
            ]
        }
    };

    Ok(FactBatch::new(views.revision(), facts))
}

pub(crate) fn admit_cast(
    registry: &Registry,
    views: &Views,
    target: &UnestablishedFact,
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
    use crate::authority::{Cast, CastCeiling, Sanitizer, SanitizerPoints, Transition};
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
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            }),
            emits: vec![EffectKind::new("read")],
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
            resolution: CastResolution::Constant(DimValue::Trust(SUSPICIOUS)),
        };
        let audience_cast = Cast {
            name: CastName::new("roomer"),
            resolution: CastResolution::Constant(DimValue::Audience(internal())),
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
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal())),
            }),
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
        };
        let poll = ToolContract {
            name: ToolName::new("poll_room"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Unknown),
            }),
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
        };
        Registry::build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![get, scan, poll],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            casts: vec![const_cast, audience_cast, resolver_cast],
        })
        .unwrap()
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
                &UnestablishedFact {
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
        let target = UnestablishedFact {
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
        let target = UnestablishedFact {
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
                &UnestablishedFact {
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
                &UnestablishedFact {
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
                    resolved: DimValue::Trust(SUSPICIOUS),
                },
            ),
            Err(AdmitError::NarrowingUnaccepted)
        );
    }

    #[test]
    fn an_audience_pending_cast_follows_the_same_acceptance_discipline() {
        let reg = registry();
        let call = ResolvedCall::new(ToolName::new("poll_room"), json!({}), vec![]);
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
                    cast: CastName::new("roomer"),
                    resolved: DimValue::Audience(internal()),
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
                cast: CastName::new("roomer"),
                resolved: DimValue::Audience(internal()),
                accepted: Narrowing {
                    from: Label::top(),
                    to: Label::new(Dim::Known(SUSPICIOUS), Dim::Known(internal())),
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
                    resolved: DimValue::Trust(SUSPICIOUS),
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
            from: Label::top(),
            to: Label::new(Dim::Known(SUSPICIOUS), Dim::Known(internal())),
        };
        let batch = admit_result(
            &reg,
            &p.view(&t),
            &dispatch,
            &call,
            ResultAdmission::SuccessCastAccepted {
                body: ValueBody::new("inbox contents"),
                cast: CastName::new("paranoid"),
                resolved: DimValue::Trust(SUSPICIOUS),
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
        assert_eq!(p2.view(&t).current_label().trust, Dim::Known(SUSPICIOUS));
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
                    resolved: DimValue::Trust(SUSPICIOUS),
                    accepted: Narrowing {
                        from: Label::top(),
                        to: Label::new(Dim::Known(SUSPICIOUS), Dim::Known(Audience::Public)),
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
                    resolved: DimValue::Trust(SUSPICIOUS),
                    accepted: Narrowing {
                        from: Label::top(),
                        to: Label::new(Dim::Known(SUSPICIOUS), Dim::Known(Audience::Public)),
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
                resolved: DimValue::Trust(SUSPICIOUS),
            },
        )
        .unwrap();
        assert_eq!(batch.facts.len(), 2);
        assert!(matches!(
            &batch.facts[0],
            Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. } if effects == &[EffectKind::new("read")]
        ));
        assert!(matches!(
            &batch.facts[1],
            Fact::OutputCastLapsed {
                dimension: Dimension::Trust,
                resolved: DimValue::Trust(tr),
                raw_digest,
                ..
            } if *tr == SUSPICIOUS && raw_digest == &RawResultDigest::of(b"inbox contents")
        ));
        let mut next = log.clone();
        next.extend(batch.facts);
        let p2 = views_of(&next);
        assert_eq!(p2.view(&t).current_label(), Label::top());
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
                    resolved: DimValue::Trust(SUSPICIOUS),
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

    #[test]
    fn a_success_checkpoint_commits_effects_once_and_pins_the_close_family() {
        let reg = registry();
        let call = scan_call();
        let (mut log, dispatch) = open_log(&call);
        let t = traj();

        let plain = get_call();
        let (plain_log, plain_dispatch) = open_log(&plain);
        let p = views_of(&plain_log);
        assert_eq!(
            observe_success(&reg, &p.view(&t), &plain_dispatch, &plain),
            Err(AdmitError::NotPendingCast)
        );

        let p = views_of(&log);
        let batch = observe_success(&reg, &p.view(&t), &dispatch, &call).unwrap();
        log.extend(batch.facts);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call),
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
                body: ValueBody::new("mail"),
                cast: CastName::new("paranoid"),
                resolved: DimValue::Trust(SUSPICIOUS),
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
