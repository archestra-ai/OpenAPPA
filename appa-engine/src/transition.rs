//! The engine's one mutation boundary: the sealed batch and the view it was computed against.
//!
//! Two types carry the structural half of `IMP-4` here. A [`ValidatedFactBatch`] is engine
//! output no caller can forge: its constructor is crate-private, so the only batches a runtime
//! can append are ones the engine validated. An [`EngineView`] is the derived working picture
//! the engine decides against — the runtime holds it and hands it back, but only the engine
//! builds, rebuilds or advances it, so a caller cannot decide against a picture the engine never
//! validated.
//!
//! Advancing is deliberately not an incremental fold. [`Projection`] is rebuilt whole from the
//! record list on every advance, because a second incremental fold beside it would be a second
//! semantics to police (see `projection`'s module docs). `IMP-2` asks who may mutate the cache,
//! not that the cache be updated in place.

use serde::{Deserialize, Serialize};

use crate::fact::{Fact, FactBatch, Revision};
use crate::plan::PlannedBlock;
use crate::projection::Projection;
use crate::value::{DispatchId, ResolvedCall, TrajectoryId};

/// The identity of one complete policy-content payload for one trajectory. Runtime
/// supplies it as a nonce — it is the party that knows a delivery retry from a fresh response —
/// and the engine binds it to the payload, so repeating it returns the recorded decision and
/// reusing it for different content is refused.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProposalBatchId(String);

impl ProposalBatchId {
    pub fn new(id: impl Into<String>) -> ProposalBatchId {
        ProposalBatchId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One model response's policy content: the ordered proposals it made for one
/// trajectory, under the identity that makes the act repeatable. Structurally plural from the
/// start — a deployment's hook gates one call at a time today, and the atomic
/// multi-proposal composition is `T01`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalBatch {
    pub id: ProposalBatchId,
    pub trajectory: TrajectoryId,
    pub proposals: Vec<ResolvedCall>,
}

/// One act the engine decides. A closed enum: an external operational failure is not an
/// event, and neither is a request, user turn, transcript, or host run.
///
/// Tool outcome, offer execution and child return join it as they move off the composed
/// operations; forking joins as `T39`'s `BindFork`, in its final shape rather than an interim
/// child-start variant this boundary would have to unpublish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Proposals(ProposalBatch),
}

/// One released call: the dispatch the engine opened for it, and the canonical call to invoke.
/// The runtime never re-derives the identity — deriving it twice is what `T31` removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Released {
    pub dispatch: DispatchId,
    pub call: ResolvedCall,
}

/// One refused call and the remedies that would lift it. The engine owns the block and its plans;
/// rendering them for the model is runtime feedback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub call: ResolvedCall,
    pub block: PlannedBlock,
}

/// What the runtime does after appending the decision's batch. Delivery vocabulary —
/// transports, placeholders, transcript shape — stays outside the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FollowUp {
    Proposals {
        released: Vec<Released>,
        blocked: Vec<Blocked>,
        spent: Vec<ResolvedCall>,
    },
}

/// Why the boundary refused an event outright. A policy block is a decision, not an error: this
/// means the event cannot be processed at all — a malformed call, a fork the branch rules refuse.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error(transparent)]
    Call(#[from] crate::engine::EngineError),
    /// The batch identity is already bound to other policy content, or to another trajectory.
    /// Two different acts under one identity would make the log's decision boundaries
    /// unreadable, so neither is decided.
    #[error("this proposal batch id is already bound to different policy content")]
    BatchIdentityConflict,
    #[error("a proposal batch carries at least one proposal")]
    EmptyBatch,
    #[error("more than one proposal in a batch awaits ordered in-batch composition (T01)")]
    UncomposedBatch,
}

/// One engine interaction's outcome: the sealed batch to append against its basis
/// revision, and the one typed follow-up package. The runtime appends the whole batch
/// before it performs any follow-up item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineDecision {
    pub append: Option<ValidatedFactBatch>,
    pub follow_up: FollowUp,
}

/// A batch the engine validated and sealed. The runtime treats it as opaque: it performs the
/// whole-batch revision append and advances its [`EngineView`] with it, and never
/// reconstructs engine work by reading the facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedFactBatch(FactBatch);

impl ValidatedFactBatch {
    /// Seal a validated batch. Crate-private on purpose: every call site is an engine transition
    /// that has already run the batch through validation.
    pub(crate) fn seal(batch: FactBatch) -> ValidatedFactBatch {
        ValidatedFactBatch(batch)
    }

    pub fn basis(&self) -> Revision {
        self.0.basis
    }

    /// The facts to append, for the store that persists them. Reading them to reconstruct
    /// released work or feedback is forbidden; the decision's follow-up carries that.
    pub fn facts(&self) -> &[Fact] {
        &self.0.facts
    }

    /// Serialization removes the seal: what crosses to storage is the plain batch, and
    /// what comes back is untrusted until it passes the validator again.
    pub fn into_unsealed(self) -> FactBatch {
        self.0
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[error("the batch was computed against revision {batch:?} but the view stands at {view:?}")]
pub struct StaleBatch {
    pub view: Revision,
    pub batch: Revision,
}

/// The engine's derived working picture of one family log: the validated records and
/// the projection built from them. Opaque and disposable — the runtime stores it for the next
/// event, but every constructor and mutator here belongs to the engine.
#[derive(Clone, Debug)]
pub struct EngineView {
    records: Vec<Fact>,
    projection: Projection,
}

impl EngineView {
    /// Build the view from a record stream the caller has already validated. Crate-private: the
    /// public entry is `Engine::view`, which validates first.
    pub(crate) fn over(records: Vec<Fact>, revision: Revision) -> EngineView {
        let projection = Projection::build(&records, revision);
        EngineView { records, projection }
    }

    pub(crate) fn projection(&self) -> &Projection {
        &self.projection
    }

    pub fn revision(&self) -> Revision {
        self.projection.revision()
    }

    /// Advance the cache by a batch the store accepted: the runtime may advance only
    /// from a sealed batch, and only through the engine. The projection is rebuilt whole rather
    /// than folded forward — see the module docs.
    ///
    /// The batch's basis must be exactly where this view stands, which is what the store's
    /// conditional append already proved before accepting it. A batch computed against
    /// any other revision belongs to another view, and applying it would leave records and
    /// revision describing different logs.
    pub fn advance(&mut self, batch: &ValidatedFactBatch) -> Result<(), StaleBatch> {
        if batch.basis() != self.revision() {
            return Err(StaleBatch {
                view: self.revision(),
                batch: batch.basis(),
            });
        }
        self.records.extend_from_slice(batch.facts());
        self.projection = Projection::build(&self.records, batch.basis().next());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{Audience, Dim, EstablishedLabel, Label, PartialLabel, Trust};
    use crate::value::{LabeledValue, Provenance, TrajectoryId, ValueBody};

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn admit(trust: u8) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(
                ValueBody::new("body"),
                Label::new(Dim::Known(Trust::new(trust)), Dim::Known(Audience::Public)),
            ),
            provenance: Provenance::UserInput,
        }
    }

    #[test]
    fn advancing_a_view_matches_rebuilding_it_from_the_records() {
        let first = vec![admit(3)];
        let mut held = EngineView::over(first.clone(), Revision::new(1));
        let batch = ValidatedFactBatch::seal(FactBatch::new(Revision::new(1), vec![admit(1)]));
        held.advance(&batch).unwrap();

        let whole = [first, batch.facts().to_vec()].concat();
        let rebuilt = EngineView::over(whole, Revision::new(2));

        assert_eq!(held.revision(), rebuilt.revision());
        assert_eq!(
            held.projection().view(&traj()).current_label(),
            rebuilt.projection().view(&traj()).current_label()
        );
        assert_eq!(
            held.projection().view(&traj()).current_label(),
            PartialLabel::established(EstablishedLabel::new(Trust::new(1), Audience::Public))
        );

        assert_eq!(
            held.advance(&batch),
            Err(StaleBatch {
                view: Revision::new(2),
                batch: Revision::new(1),
            })
        );
    }
}
