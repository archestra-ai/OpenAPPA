//! Audit as control-plane history, and the monotone trajectory state.
//!
//! Audit lives outside labels: at value granularity, referencing a
//! value twice would duplicate its history, and a *failed* transition has no
//! output label to record its failure on. Instead every transition attempt —
//! applied or failed — appends one [`AuditEvent`] to append-only trajectory
//! state.
//!
//! Raw bytes and content digests deliberately do not appear here: the audit
//! record names identities, labels, and outcomes only.

use std::fmt;

use serde::Serialize;

use crate::contract::Violation;
use crate::dimension::Effects;
use crate::remedy::Authorization;
use crate::revision::{ActionId, PlanId, TransitionId, ValueId};
use crate::value::{TransformerRef, ValueLabel};

/// The exact before/after labels of a durable raise, carried on its audit
/// record so the record is self-contained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RaiseLabels {
    pub input: ValueLabel,
    pub raised: ValueLabel,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AuthorityName(String);

impl AuthorityName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthorityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TransitionFailure {
    ReductionRefused,
    TransformerError { message: String },
}

impl fmt::Display for TransitionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReductionRefused => write!(f, "the registered reduction relation does not hold"),
            Self::TransformerError { message } => write!(f, "transformer failed: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TransitionOutcome {
    Applied,
    Failed(TransitionFailure),
}

/// One control-plane audit record. Failures append an event but create no
/// derived value or action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AuditEvent {
    ValueTransition {
        transition: TransitionId,
        transformer: TransformerRef,
        source: ValueId,
        derived: Option<ValueId>,
        input: ValueLabel,
        declared_output: ValueLabel,
        outcome: TransitionOutcome,
    },
    AuthorizationApplied {
        transition: TransitionId,
        authorization: Authorization,
        authority: AuthorityName,
        resolved: Vec<Violation>,
        derived: Option<ValueId>,
        labels: Option<RaiseLabels>,
    },
    AuthorizationDenied {
        authorization: Authorization,
        authority: AuthorityName,
        reason: String,
    },
    EffectsCommitted { action: ActionId, effects: Effects },
    DispatchFailed { action: ActionId },
    ApprovalRequested {
        plan: PlanId,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    },
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueTransition {
                transformer,
                source,
                derived,
                outcome,
                ..
            } => match (derived, outcome) {
                (Some(derived), _) => {
                    write!(f, "{source} -> {derived} admitted under transition by {transformer}")
                }
                (None, TransitionOutcome::Failed(failure)) => {
                    write!(f, "transition of {source} by {transformer} failed: {failure}")
                }
                (None, TransitionOutcome::Applied) => {
                    write!(f, "transition of {source} by {transformer} applied")
                }
            },
            Self::AuthorizationApplied {
                authorization,
                authority,
                derived,
                labels,
                ..
            } => match (derived, labels) {
                (Some(derived), Some(labels)) => write!(
                    f,
                    "{authorization} granted by {authority}, minted {derived}: {} -> {}",
                    labels.input, labels.raised
                ),
                (Some(derived), None) => write!(f, "{authorization} granted by {authority}, minted {derived}"),
                (None, _) => write!(f, "{authorization} granted by {authority}"),
            },
            Self::AuthorizationDenied {
                authorization,
                authority,
                reason,
            } => {
                write!(f, "{authorization} denied by {authority}: {reason}")
            }
            Self::EffectsCommitted { action, effects } => {
                write!(f, "{action} dispatching, effects committed: {effects}")
            }
            Self::DispatchFailed { action } => {
                write!(f, "{action} dispatch failed; committed effects stay")
            }
            Self::ApprovalRequested { plan, authority, .. } => {
                write!(f, "{plan}: approval requested from {authority}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::Effect;
    use crate::event::EventSet;
    use crate::projection::committed_effects;

    fn commit(events: &mut EventSet, action: u64, effects: Effects) {
        let action = crate::revision::ActionId::new(action);
        events
            .append_batch(vec![
                crate::event::Fact::ActionProposed {
                    action,
                    flow: crate::revision::FlowId::new(action.index()),
                    request: crate::request::ToolRequest::new(
                        crate::ToolName::new("seed.dispatch"),
                        crate::request::ArgumentTree::empty(),
                        std::collections::BTreeSet::new(),
                    ),
                    effects: effects.clone(),
                },
                crate::event::Fact::EffectsCommitted { action, effects },
                crate::event::Fact::ActionReleased { action },
                crate::event::Fact::DispatchFailed { action },
            ])
            .expect("the synthetic dispatch is a well-formed lifecycle");
    }

    #[test]
    fn effects_only_accumulate() {
        let mut events = EventSet::default();
        commit(&mut events, 0, Effects::declared([Effect::Egress]));
        commit(&mut events, 1, Effects::none());
        assert_eq!(committed_effects(&events), Effects::declared([Effect::Egress]));

        commit(&mut events, 2, Effects::declared([Effect::Mutation]));
        assert_eq!(
            committed_effects(&events),
            Effects::declared([Effect::Egress, Effect::Mutation])
        );
    }

    #[test]
    fn unknown_effects_absorb_permanently() {
        let mut events = EventSet::default();
        commit(&mut events, 0, Effects::UNKNOWN);
        commit(&mut events, 1, Effects::none());
        assert_eq!(committed_effects(&events), Effects::UNKNOWN);
    }
}
