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

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::contract::Violation;
use crate::dimension::Effects;
use crate::revision::{ActionId, PlanId, TransitionId, ValueId};
use crate::transition::EndorseDelta;
use crate::value::{TransformerRef, ValueLabel};

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
    PreconditionMismatch,
    TransformerError { message: String },
    PostconditionMismatch,
}

impl fmt::Display for TransitionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PreconditionMismatch => write!(f, "precondition no longer holds"),
            Self::TransformerError { message } => write!(f, "transformer failed: {message}"),
            Self::PostconditionMismatch => write!(f, "predicted postcondition did not hold"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TransitionOutcome {
    Applied,
    Failed(TransitionFailure),
}

/// Which check a waiver loosened. `Acknowledgment` records an
/// acknowledge-only fact (unprovable effects, a missing contract) accepted on
/// the record without loosening anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum WaiverKind {
    Effects,
    Confirmation,
    ControlRelease,
    Acknowledgment,
}

impl fmt::Display for WaiverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Effects => write!(f, "effects"),
            Self::Confirmation => write!(f, "confirmation"),
            Self::ControlRelease => write!(f, "control-release"),
            Self::Acknowledgment => write!(f, "acknowledgment"),
        }
    }
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
    ActionConstrained {
        transition: TransitionId,
        action: ActionId,
        outcome: TransitionOutcome,
    },
    WaiverApplied {
        transition: TransitionId,
        changes: BTreeSet<WaiverKind>,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    },
    EffectsCommitted { action: ActionId, effects: Effects },
    DispatchFailed { action: ActionId },
    StepFailed {
        plan: PlanId,
        step: u64,
        failure: TransitionFailure,
    },
    ApprovalRequested {
        plan: PlanId,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    },
    WaiverDenied { authority: AuthorityName, reason: String },
    AcceptApplied {
        transition: TransitionId,
        action: ActionId,
        effects: Effects,
        authority: AuthorityName,
        resolved: Vec<Violation>,
    },
    AcceptDenied { authority: AuthorityName, reason: String },
    EndorseApplied {
        transition: TransitionId,
        source: ValueId,
        derived: ValueId,
        authority: AuthorityName,
        delta: EndorseDelta,
        input: ValueLabel,
        raised: ValueLabel,
    },
    EndorseDenied { authority: AuthorityName, reason: String },
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
            Self::ActionConstrained { action, outcome, .. } => match outcome {
                TransitionOutcome::Applied => write!(f, "{action} constrained"),
                TransitionOutcome::Failed(failure) => write!(f, "constraining {action} failed: {failure}"),
            },
            Self::WaiverApplied { changes, authority, .. } => {
                write!(f, "waiver by {authority}:")?;
                for change in changes {
                    write!(f, " {change}")?;
                }
                Ok(())
            }
            Self::EffectsCommitted { action, effects } => {
                write!(f, "{action} dispatching, effects committed: {effects}")
            }
            Self::DispatchFailed { action } => {
                write!(f, "{action} dispatch failed; committed effects stay")
            }
            Self::StepFailed { plan, step, failure } => {
                write!(f, "{plan} step {step} refused: {failure}")
            }
            Self::ApprovalRequested { plan, authority, .. } => {
                write!(f, "{plan}: approval requested from {authority}")
            }
            Self::WaiverDenied { authority, reason } => {
                write!(f, "waiver denied by {authority}: {reason}")
            }
            Self::AcceptApplied {
                action,
                effects,
                authority,
                ..
            } => {
                write!(f, "{action}: growth {effects} acquired by {authority}")
            }
            Self::AcceptDenied { authority, reason } => {
                write!(f, "accept denied by {authority}: {reason}")
            }
            Self::EndorseApplied {
                source,
                derived,
                authority,
                delta,
                ..
            } => {
                write!(f, "{source} -> {derived} endorsed by {authority} ({delta})")
            }
            Self::EndorseDenied { authority, reason } => {
                write!(f, "endorse denied by {authority}: {reason}")
            }
        }
    }
}

/// The monotone, append-only control-plane state of one trajectory:
/// may-effects that were committed at dispatch time, and the audit log.
/// Nothing here is ever removed or loosened.
#[derive(Debug, Serialize)]
pub struct TrajectoryState {
    past_effects: Effects,
    audit: Vec<AuditEvent>,
}

impl Default for TrajectoryState {
    fn default() -> Self {
        Self {
            past_effects: Effects::none(),
            audit: Vec::new(),
        }
    }
}

impl TrajectoryState {
    pub fn past_effects(&self) -> &Effects {
        &self.past_effects
    }

    pub fn audit(&self) -> &[AuditEvent] {
        &self.audit
    }

    /// Append one audit event. Append-only by construction.
    pub fn record(&mut self, event: AuditEvent) {
        self.audit.push(event);
    }

    /// Fold newly committed effects into the monotone past. Combine is a
    /// union, so effects can only accumulate; failure of a later dispatch
    /// never removes them.
    pub fn commit_effects(&mut self, effects: Effects) {
        self.past_effects = self.past_effects.clone().combine(effects);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::Effect;

    #[test]
    fn effects_only_accumulate() {
        let mut state = TrajectoryState::default();
        state.commit_effects(Effects::declared([Effect::Egress]));
        state.commit_effects(Effects::none());
        assert_eq!(state.past_effects(), &Effects::declared([Effect::Egress]));

        state.commit_effects(Effects::declared([Effect::Mutation]));
        assert_eq!(
            state.past_effects(),
            &Effects::declared([Effect::Egress, Effect::Mutation])
        );
    }

    #[test]
    fn unknown_effects_absorb_permanently() {
        let mut state = TrajectoryState::default();
        state.commit_effects(Effects::UNKNOWN);
        state.commit_effects(Effects::none());
        assert_eq!(state.past_effects(), &Effects::UNKNOWN);
    }
}
