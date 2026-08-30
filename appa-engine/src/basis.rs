//! `PolicyBasis` — what makes an offer or a call approval current.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::transition::ProposalBatchId;
use crate::value::{ChildReturnId, DispatchId, ForkId, OfferId, TrajectoryId};

/// How many decisions have moved the **family's** shared policy state: effects reserved by a
/// release, or a reservation settled or effects recorded by an outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FamilyVersion(u64);

/// How many decisions have moved **one trajectory's** flow state: its label, its unresolved
/// sources or its denials, or a release whose result can restrict any of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FlowVersion(u64);

/// How many times **one durable subject** has advanced, been consumed, or been abandoned. A
/// candidate is exactly a subject at a generation, so a successor is the same subject one
/// generation on and a predecessor needs no stored back-reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubjectGeneration(u64);

macro_rules! counter {
    ($name:ident) => {
        impl $name {
            pub const ZERO: $name = $name(0);

            pub(crate) const fn next(self) -> $name {
                $name(self.0 + 1)
            }

            pub const fn value(self) -> u64 {
                self.0
            }
        }
    };
}

counter!(FamilyVersion);
counter!(FlowVersion);
counter!(SubjectGeneration);

/// The three counters an offer or a call approval binds itself to. Compared whole and
/// only for equality: a mismatch on any component makes the record stale, and staleness is
/// terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyBasis {
    pub family: FamilyVersion,
    pub flow: FlowVersion,
    pub subject: SubjectGeneration,
}

impl PolicyBasis {
    pub(crate) const fn new(family: FamilyVersion, flow: FlowVersion, subject: SubjectGeneration) -> PolicyBasis {
        PolicyBasis { family, flow, subject }
    }

    /// Where this basis lands once `advance` applies — the post-decision value a decision stamps
    /// onto the offers and approvals it opens.
    pub(crate) fn advanced_by(
        self,
        advance: &BasisAdvance,
        trajectory: &TrajectoryId,
        subject: &SubjectKey,
    ) -> PolicyBasis {
        PolicyBasis {
            family: if advance.family {
                self.family.next()
            } else {
                self.family
            },
            flow: if advance.flows.contains(trajectory) {
                self.flow.next()
            } else {
                self.flow
            },
            subject: advance
                .subjects
                .iter()
                .filter(|moved| *moved == subject)
                .fold(self.subject, |generation, _| generation.next()),
        }
    }
}

/// The durable thing an offer or an approval is **about**.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SubjectKey {
    Call {
        trajectory: TrajectoryId,
        batch: ProposalBatchId,
        position: u32,
    },
    Approval(OfferId),
    ConfinedResult(DispatchId),
    Return(ChildReturnId),
}

/// The subject a hand-built opening stands on, for a fixture whose point is not which decision
/// released it. Test-only: every opening a decision produces names the proposal position it was
/// released for.
#[cfg(test)]
pub(crate) fn fixture_subject(trajectory: &TrajectoryId) -> SubjectKey {
    SubjectKey::Call {
        trajectory: trajectory.clone(),
        batch: ProposalBatchId::new("fixture"),
        position: 0,
    }
}

/// Which engine act a declaration belongs to, named by the identity that act already has. No new
/// identity concept: a decision is recognised by the thing it decides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecidedAct {
    Proposals(ProposalBatchId),
    Outcome(DispatchId),
    ChildReturn(ChildReturnId),
    Binding(ForkId),
    Offer(OfferId),
}

/// What one decision declares it moves. `family` and `flow` move at most once in a decision;
/// `subjects` is a sequence rather than a set, because that cap covers only the first
/// two and one decision may advance several subjects — or the same subject twice, when it both
/// consumes a candidate and opens its successor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisAdvance {
    pub family: bool,
    /// Every trajectory whose flow moved — a set, because one decision can move the flows of
    /// several branches.
    pub flows: BTreeSet<TrajectoryId>,
    pub subjects: Vec<SubjectKey>,
}

impl BasisAdvance {
    pub(crate) fn is_empty(&self) -> bool {
        !self.family && self.flows.is_empty() && self.subjects.is_empty()
    }

    /// Fold one implied advance into this one. Used to build a decision's declaration from the
    /// records it is about to append, and to accumulate what a record stream actually implies.
    pub(crate) fn absorb(&mut self, other: &BasisAdvance) {
        self.family |= other.family;
        self.flows.extend(other.flows.iter().cloned());
        self.subjects.extend(other.subjects.iter().cloned());
    }

    pub(crate) fn flow(trajectory: &TrajectoryId) -> BasisAdvance {
        BasisAdvance {
            flows: BTreeSet::from([trajectory.clone()]),
            ..BasisAdvance::default()
        }
    }

    pub(crate) fn family() -> BasisAdvance {
        BasisAdvance {
            family: true,
            ..BasisAdvance::default()
        }
    }
}

/// The counters as a replay-derived read model. Only [`Versions::advance`] writes, and it takes a
/// whole [`BasisAdvance`], so a component cannot move without something declaring or implying it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Versions {
    family: FamilyVersion,
    flows: BTreeMap<TrajectoryId, FlowVersion>,
    generations: BTreeMap<SubjectKey, SubjectGeneration>,
}

impl Versions {
    pub(crate) fn advance(&mut self, advance: &BasisAdvance) {
        if advance.family {
            self.family = self.family.next();
        }
        for trajectory in &advance.flows {
            let flow = self.flows.entry(trajectory.clone()).or_default();
            *flow = flow.next();
        }
        for subject in &advance.subjects {
            let generation = self.generations.entry(subject.clone()).or_default();
            *generation = generation.next();
        }
    }

    pub(crate) fn flow(&self, trajectory: &TrajectoryId) -> FlowVersion {
        self.flows.get(trajectory).copied().unwrap_or_default()
    }

    pub(crate) fn generation(&self, subject: &SubjectKey) -> SubjectGeneration {
        self.generations.get(subject).copied().unwrap_or_default()
    }

    /// The basis one subject stands at: the family and its trajectory's flow, plus its own
    /// generation. An approval takes the flow of the call it approves, which is the trajectory its
    /// offer's subject named.
    pub(crate) fn basis_for(&self, trajectory: &TrajectoryId, subject: &SubjectKey) -> PolicyBasis {
        PolicyBasis::new(self.family, self.flow(trajectory), self.generation(subject))
    }
}
