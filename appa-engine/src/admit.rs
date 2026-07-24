//! Result and cast admission: closing a dispatch and admitting (or withholding) its value.
//!
//! On tool success, effects commit and — depending on how the runtime observed the result — a
//! value is admitted raw (at the contract's output label), sanitized (an audience-only relabel of a
//! confined raw result), or not at all (oversized). The **label folds only from an admitted value**,
//! never from the close itself. A cast resolves an existing value's Unknown dimension.
//!
//! Every label here is computed by the engine from the registry — never trusted from the runtime —
//! so a compromised caller cannot smuggle a wider label in.

use thiserror::Error;

use crate::authority::CastResolution;
use crate::check::{Narrowing, UnresolvedFact};
use crate::fact::{CloseOutcome, Fact, FactBatch};
use crate::label::{Adequacy, Dim, DimValue, Label};
use crate::names::{CastName, SanitizerName};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ValueBody};

/// How a tool dispatch resolved, as the runtime observed it.
pub enum ResultAdmission {
    /// The tool failed: no effects, no value.
    Failure,
    /// The outcome was never observed (timeout, cancelled turn): no effects, no value, but the
    /// close records that the tool may or may not have run.
    Indeterminate,
    /// The tool succeeded but produced no admissible value (e.g. an oversized body): effects commit,
    /// nothing admitted.
    SuccessNoValue,
    /// The tool succeeded; admit the raw result at the contract's output label.
    SuccessRaw { body: ValueBody },
    /// The tool succeeded; a bound output sanitizer relabeled the confined raw result.
    SuccessSanitized {
        body: ValueBody,
        sanitizer: SanitizerName,
        raw_digest: RawResultDigest,
    },
    /// The tool succeeded and a registered cast resolved its pending-cast output dimension (RP5):
    /// the confined raw result is admitted at the output label with the Unknown dimension filled.
    /// The audit digest is computed by the engine from `body` (the cast never transforms the
    /// bytes), so the recorded raw-result binding cannot disagree with the admitted value.
    /// Refused when the admission would strictly narrow the live trajectory label — the whole
    /// filled label against the live fold, established dimensions included — because a narrowing
    /// folds only through the agent's acceptance ([`ResultAdmission::SuccessCastAccepted`]).
    SuccessCast {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
    },
    /// A strict-narrowing cast resolution the agent accepted: `accepted` is the exact narrowing the
    /// offer surfaced. Admission re-derives the live narrowing under the family lock and refuses on
    /// any mismatch — a stale acceptance cannot fold a different narrowing than the one shown.
    SuccessCastAccepted {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
        accepted: Narrowing,
    },
    /// A pending-cast offer the turn ended without accepting: the dispatch closes successfully —
    /// effects stand, nothing admitted — and the unaccepted resolution is recorded for audit.
    SuccessCastLapsed {
        body: ValueBody,
        cast: CastName,
        resolved: DimValue,
    },
}

/// Why a result could not be admitted.
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
    #[error("the cast resolution narrows the trajectory label: admission requires the agent's acceptance")]
    NarrowingUnaccepted,
    #[error("the accepted narrowing does not match the live trajectory state")]
    AcceptanceMismatch,
    #[error("the dispatch already recorded its success checkpoint")]
    AlreadySucceeded,
    #[error("the dispatch recorded success: a failure or indeterminate close contradicts it")]
    SuccessContradicted,
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
    // Only a pending-cast contract defers value finalization past its success; an ordinary
    // dispatch closes in one step and a checkpoint would let a misusing embedder turn a later
    // honest Failure close into a contradiction.
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

/// The narrowing admitting a cast-resolved value would fold into the **live** trajectory label, or
/// `None` when the admission does not move it. The whole filled label is considered, established
/// dimensions included: the pre-dispatch check accepted the established contribution against the
/// fold *at check time*, but the fold may have moved since (a child return merging under an
/// accepted return plan), making that same contribution newly restrictive — so admission re-derives
/// against live state, like every other offer in this codebase. The cost is an occasional second
/// acceptance for a tool whose established dimension is restrictive; the alternative folds a
/// composed narrowing nobody accepted. An `Unknown` live dimension absorbs and yields `None`.
pub(crate) fn pending_cast_narrowing(views: &Views, filled: &Label) -> Option<Narrowing> {
    let from = views.current_label();
    let to = from.combine(filled);
    if to == from { None } else { Some(Narrowing { from, to }) }
}

/// Validate a pending-cast resolution against the contract and the registered cast: the resolved
/// dimension must be the contract's pending one, and the answer must sit inside the registered
/// cast's declaration — a misbehaving resolver (or runtime) cannot widen a label past the ceiling.
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

/// An authority/resolver's answer to an Unknown dimension.
pub struct CastAnswer {
    pub cast: CastName,
    pub resolved: DimValue,
}

/// Why a cast could not be admitted.
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

/// Close the dispatch and admit (or withhold) its value. See [`ResultAdmission`].
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
    // The dispatch is closed and its result admitted into its own branch — never a sibling's, even
    // though the open-dispatch view is family-wide.
    if dispatch.trajectory() != views.trajectory() {
        return Err(AdmitError::ForeignDispatch);
    }
    if !views.is_open(dispatch) {
        return Err(AdmitError::NotOpen);
    }
    // A checkpointed dispatch observed success already: only a success-family close may follow,
    // and the close contributes no duplicate effects — the checkpoint committed them.
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
            // A pending-cast output confines the raw result: no value may carry an unestablished
            // label into the trajectory (the model would see the body before its label exists).
            if contract.pending_cast_dim().is_some() {
                return Err(AdmitError::OutputPendingCast);
            }
            // A sanitizer-bound tool's raw result is likewise confined: only the bound derivation
            // may enter (RP4) — the binding is enforced here, not left to the runtime.
            if contract.output_sanitizer.is_some() {
                return Err(AdmitError::OutputSanitizerBound);
            }
            vec![close_success(), admit_value(contract.output_label(), body)]
        }
        ResultAdmission::SuccessCast { body, cast, resolved } => {
            validate_cast_resolution(registry, contract, &cast, &resolved)?;
            let label = cast_filled_label(contract, &resolved);
            // An admission that strictly narrows the live label folds only through the agent's
            // acceptance — refused bare, at this single admission choke point (D2).
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
            // The acceptance binds the exact narrowing the offer surfaced; the live narrowing is
            // re-derived here, under the family lock — a stale acceptance (the label moved, or
            // nothing narrows any more) mismatches and is refused.
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
        ResultAdmission::SuccessCastLapsed { body, cast, resolved } => {
            validate_cast_resolution(registry, contract, &cast, &resolved)?;
            let raw_digest = RawResultDigest::of(body.as_str().as_bytes());
            // The turn ended without the agent accepting: effects stand, nothing admitted, and the
            // unaccepted resolution is durable audit — never only feedback.
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
        ResultAdmission::SuccessSanitized {
            body,
            sanitizer,
            raw_digest,
        } => {
            if contract.pending_cast_dim().is_some() {
                return Err(AdmitError::OutputPendingCast);
            }
            // Only the contract's own bound sanitizer may relabel this tool's output — a sanitized
            // admission through any other transformer (or for an unbound tool) is refused, so the
            // caller cannot choose a more permissive transition than the policy declared.
            if contract.output_sanitizer.as_ref() != Some(&sanitizer) {
                return Err(AdmitError::NotBoundSanitizer);
            }
            let san = registry
                .sanitizer(&sanitizer)
                .ok_or_else(|| AdmitError::UnknownSanitizer(sanitizer.as_str().to_string()))?;
            if !san.on.output {
                return Err(AdmitError::SanitizerNotOutput(sanitizer.as_str().to_string()));
            }
            let raw = contract.output_label();
            // The raw source must satisfy the transition's `from` before the `to` may apply.
            // (Load validation already refuses an inapplicable binding, so this cannot fire for a
            // built registry; kept so the function stays total over its inputs.)
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

/// Validate a cast answer against the registered cast and the target value, then emit the override.
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
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            }),
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
        // A tool whose output trust is pending-cast: the raw result stays confined until a
        // registered cast establishes it (RP5).
        let scan = ToolContract {
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(internal())),
            }),
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: None,
        };
        // A tool whose output audience is pending-cast — the other dimension's variant.
        let poll = ToolContract {
            name: ToolName::new("poll_room"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Unknown),
            }),
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: None,
        };
        // A tool bound to the declassify output sanitizer (RP4): raw is confined, only the bound
        // derivation admits.
        let export = ToolContract {
            name: ToolName::new("export_ticket"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            }),
            emits: vec![EffectKind::new("read")],
            requires: Default::default(),
            output_sanitizer: Some(crate::names::SanitizerName::new("declassify")),
        };
        Registry::build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![get, scan, poll, export],
            authorities: vec![],
            sanitizers: vec![out_san, finance_san],
            casts: vec![const_cast, audience_cast, resolver_cast],
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

    /// A log holding one open dispatch of `get_ticket`.
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
        // A sibling branch's views cannot admit a dispatch opened on trajectory `t`.
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
        // Nor can it cast a value that belongs to trajectory `t`.
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
        // DispatchClosed{Success, effects=[read]} then ValueAdmitted at {suspicious, internal}.
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
        // Trust stays suspicious (never rises through a sanitizer); audience becomes public.
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
        // A bound tool's raw result may not enter …
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
        // … nor a derivation through any sanitizer but the bound one …
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
        // … and an unbound tool admits no sanitized value at all (the policy declared none).
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
        // A different call → digest mismatch against the dispatch.
        let other = ResolvedCall::new(ToolName::new("get_ticket"), json!({ "x": 1 }), vec![]);
        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &other, ResultAdmission::SuccessNoValue),
            Err(AdmitError::DigestMismatch)
        );
        // A dispatch not in the open set → NotOpen.
        let empty = views_of(&[]);
        assert_eq!(
            admit_result(&reg, &empty.view(&t), &dispatch, &call, ResultAdmission::SuccessNoValue),
            Err(AdmitError::NotOpen)
        );
    }

    /// A branch holding one Unknown-trust value to cast.
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
        // Applying the CastApplied fact resolves the branch fold's trust.
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
        // classifier may_cast only suspicious; trusted exceeds the ceiling.
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
        // The unresolved fact is Trust, but the answer resolves Audience.
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
        // The audience dimension is already Known → NotUnknown.
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
    fn a_moved_established_dimension_demands_acceptance() {
        // The blocker scenario: between dispatch and admission the live audience moved (a child
        // return merging under an accepted return plan) to a set disjoint from the contract's
        // established `internal`. The resolved trust itself moves nothing — but the established
        // audience contribution is newly restrictive against the live fold, and it may not fold
        // silently: the full-label rule demands acceptance.
        let reg = registry();
        let call = scan_call();
        // The moving value lands AFTER the dispatch opened — the state at open time would have
        // passed; the movement is what admission must catch.
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
        // From the top label the filled {suspicious, internal} strictly narrows both dimensions:
        // refused bare, admitted with the exact full-label acceptance.
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
    fn pending_cast_confines_raw_and_sanitized_admission() {
        let reg = registry();
        let call = scan_call();
        let (log, dispatch) = open_log(&call);
        let p = views_of(&log);
        let t = traj();
        // No value may enter carrying an unestablished label — raw and sanitized both refused.
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

    /// A log already folded to suspicious/internal — `scan_inbox`'s whole filled label — holding
    /// one open dispatch: admitting the cast-resolved result moves nothing.
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
        // close-success(effects) → OutputCastApplied(audit) → ValueAdmitted at the filled label.
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
                // The established audience dimension is preserved untouched.
                assert_eq!(value.label.audience, Dim::Known(internal()));
            }
            other => panic!("expected ValueAdmitted, got {other:?}"),
        }
    }

    #[test]
    fn a_narrowing_cast_resolution_requires_acceptance() {
        let reg = registry();
        let call = scan_call();
        // The fold sits at top trust: resolving to suspicious strictly narrows it.
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
        // The narrowing is the whole admission's fold move — the resolved trust AND the
        // established internal audience, both against the live label.
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
        // One atomic batch: close-success → OutputCastApplied → OutputCastAccepted → ValueAdmitted.
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
        // Folding the batch lands the accepted narrowing in the trajectory label.
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
        // An acceptance whose narrowing does not match the live one — here missing the established
        // audience contribution the full-label rule includes — is refused.
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
        // A live state where nothing narrows any more refuses the acceptance too — the runtime
        // retries the plain admission instead.
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
        // Effects stand, the unaccepted resolution is durable audit, nothing is admitted.
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
        // The fold is untouched by the lapse.
        let mut next = log.clone();
        next.extend(batch.facts);
        let p2 = views_of(&next);
        assert_eq!(p2.view(&t).current_label(), Label::top());
        // A lapse still validates the resolution — an unregistered cast records nothing.
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
        // A resolver answer above its may_cast ceiling cannot widen the label.
        assert_eq!(
            attempt(admission("classifier", DimValue::Trust(Trust::new(1)))),
            Err(AdmitError::CeilingExceeded)
        );
        // A constant cast admits exactly its declared target.
        assert_eq!(
            attempt(admission("paranoid", DimValue::Trust(Trust::new(1)))),
            Err(AdmitError::ConstantMismatch)
        );
        // The resolved dimension must be the contract's pending one.
        assert_eq!(
            attempt(admission("classifier", DimValue::Audience(Audience::Public))),
            Err(AdmitError::NotPendingCast)
        );
        // An unregistered cast never admits.
        assert_eq!(
            attempt(admission("bogus", DimValue::Trust(SUSPICIOUS))),
            Err(AdmitError::UnknownCast("bogus".to_string()))
        );
        // A cast admission for a contract with no pending dimension is refused.
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

        // Only a pending-cast contract defers finalization: an ordinary dispatch cannot
        // checkpoint (it would turn a later honest Failure close into a contradiction).
        let plain = get_call();
        let (plain_log, plain_dispatch) = open_log(&plain);
        let p = views_of(&plain_log);
        assert_eq!(
            observe_success(&reg, &p.view(&t), &plain_dispatch, &plain),
            Err(AdmitError::NotPendingCast)
        );

        // The checkpoint commits the declared effects while the dispatch stays open and confined.
        let p = views_of(&log);
        let batch = observe_success(&reg, &p.view(&t), &dispatch, &call).unwrap();
        log.extend(batch.facts);
        let p = views_of(&log);
        assert!(p.view(&t).has_effect(&EffectKind::new("read")));
        assert!(p.view(&t).is_open(&dispatch));

        // At most once — a repeat checkpoint is refused, never silently absorbed.
        assert_eq!(
            observe_success(&reg, &p.view(&t), &dispatch, &call),
            Err(AdmitError::AlreadySucceeded)
        );

        // Success was observed: a contradictory close is refused.
        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Failure),
            Err(AdmitError::SuccessContradicted)
        );
        assert_eq!(
            admit_result(&reg, &p.view(&t), &dispatch, &call, ResultAdmission::Indeterminate),
            Err(AdmitError::SuccessContradicted)
        );

        // The success-family close lands with no duplicate effects: the checkpoint stays the one
        // carrier, and the effect view still holds exactly what it held.
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
}
