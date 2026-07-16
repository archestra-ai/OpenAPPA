//! Authorities and process-local external approval.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::audit::AuthorityName;
use crate::contract::Violation;
use crate::engine::EngineId;
use crate::projection::TrajectoryProjection;
use crate::remedy::Authorization;
use crate::revision::{FlowId, PlanId, Revision, ValueId};
use crate::transition::AuthorityMandate;
use crate::turn::TrajectoryId;
use crate::value::{Provenance, ValueLabel};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Ruling {
    Approve { reason: String },
    Deny { reason: String },
}

/// A deterministic inline decision function: registered policy over the authorization
/// it is asked to grant, the violations that grant targets, and a
/// read-only view of the trajectory (labels and provenance of the values in
/// scope). `None` abstains — routing falls through to the next competent
/// authority, so abstention keeps the contract total.
pub type AuthorityFn = fn(&Authorization, &[Violation], &TrajectoryView<'_>) -> Option<Ruling>;

/// A read-only slice of the trajectory handed to an inline authority: the
/// label and provenance of any value it needs to judge a grant. Borrowed and
/// taken before any mutation, so an inline ruling cannot observe its own
/// effects.
pub struct TrajectoryView<'a> {
    projection: &'a TrajectoryProjection,
}

impl<'a> TrajectoryView<'a> {
    pub(crate) fn new(projection: &'a TrajectoryProjection) -> Self {
        Self { projection }
    }

    pub fn label(&self, value: ValueId) -> Option<&ValueLabel> {
        self.projection.label(value)
    }

    /// The transitive provenance ancestry of `value` — the value and every
    /// value it derives from — as (id, label, provenance) triples, so an inline
    /// authority can refuse to endorse a value with suspicious ancestry even
    /// when the value's own label is clean (D3). A value laundered below the
    /// fold does not name a suspicious ancestor in its own label; only walking
    /// the closure reveals it.
    pub fn ancestry(&self, value: ValueId) -> impl Iterator<Item = (ValueId, &ValueLabel, &Provenance)> {
        self.projection
            .provenance_closure([value])
            .into_iter()
            .filter_map(|id| Some((id, self.projection.label(id)?, self.projection.provenance_of(id)?)))
    }
}

/// One value's ruling-relevant projection: its label and provenance, never its
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValueView {
    pub label: ValueLabel,
    pub provenance: Provenance,
}

/// An owned, serializable snapshot of the values relevant to a grant, embedded
/// in a [`PendingApproval`] so an out-of-process authority can judge without a
/// live trajectory — a borrow cannot cross the approval boundary. Never bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AncestrySnapshot {
    values: BTreeMap<ValueId, ValueView>,
}

impl AncestrySnapshot {
    /// Snapshot the label and provenance of the transitive provenance closure
    /// of `ids`, taken before any mutation. Unknown ids are skipped — the
    /// snapshot is context for a ruling, not a check.
    pub(crate) fn of(projection: &TrajectoryProjection, ids: impl IntoIterator<Item = ValueId>) -> Self {
        let values = projection
            .provenance_closure(ids)
            .into_iter()
            .filter_map(|id| {
                Some((
                    id,
                    ValueView {
                        label: projection.label(id)?.clone(),
                        provenance: projection.provenance_of(id)?.clone(),
                    },
                ))
            })
            .collect();
        Self { values }
    }

    pub fn get(&self, value: ValueId) -> Option<&ValueView> {
        self.values.get(&value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (ValueId, &ValueView)> {
        self.values.iter().map(|(id, view)| (*id, view))
    }
}

/// A registered decision-maker: a name, the competence it may exercise, and
/// how it decides. Inline authorities decide synchronously; external ones
/// defer to an out-of-process ruling through [`PendingApproval`].
#[derive(Debug, Clone)]
pub struct Authority {
    pub name: AuthorityName,
    pub mandate: AuthorityMandate,
    pub mode: AuthorityMode,
}

impl Authority {
    pub fn inline(name: impl Into<String>, mandate: AuthorityMandate, rule: AuthorityFn) -> Self {
        Self {
            name: AuthorityName::new(name),
            mandate,
            mode: AuthorityMode::Inline(rule),
        }
    }

    /// An out-of-process authority whose rulings re-enter through
    /// [`crate::engine::PolicyEngine::apply_approval`].
    pub fn external(name: impl Into<String>, mandate: AuthorityMandate) -> Self {
        Self {
            name: AuthorityName::new(name),
            mandate,
            mode: AuthorityMode::External,
        }
    }
}

/// How an [`Authority`] rules. Inline authorities are consulted before
/// external ones during routing (a deterministic answer beats a round-trip to
/// a human).
#[derive(Debug, Clone)]
pub enum AuthorityMode {
    Inline(AuthorityFn),
    External,
}

/// A grant step awaiting an external authority's ruling. Issued by the
/// engine when an `Authorize` step names an external authority; consumed by
/// [`crate::engine::PolicyEngine::apply_approval`], which dispatches on the
/// granted authorization's scope.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct PendingApproval {
    plan: PlanId,
    flow: FlowId,
    grant: Authorization,
    authority: AuthorityName,
    resolved: Vec<Violation>,
    ancestry: AncestrySnapshot,
    trajectory: TrajectoryId,
    revision: Revision,
    engine: EngineId,
}

/// The consumed contents of a [`PendingApproval`]. The plan id stays behind
/// on the serialized approval only — validation binds through the revision
/// and the pending flow.
pub(crate) struct ApprovalParts {
    pub(crate) flow: FlowId,
    pub(crate) grant: Authorization,
    pub(crate) authority: AuthorityName,
    pub(crate) resolved: Vec<Violation>,
    pub(crate) trajectory: TrajectoryId,
    pub(crate) revision: Revision,
    pub(crate) engine: EngineId,
}

impl PendingApproval {
    #[expect(
        clippy::too_many_arguments,
        reason = "crate-internal constructor mirroring the binding fields"
    )]
    pub(crate) fn new(
        plan: PlanId,
        flow: FlowId,
        grant: Authorization,
        authority: AuthorityName,
        resolved: Vec<Violation>,
        ancestry: AncestrySnapshot,
        trajectory: TrajectoryId,
        revision: Revision,
        engine: EngineId,
    ) -> Self {
        Self {
            plan,
            flow,
            grant,
            authority,
            resolved,
            ancestry,
            trajectory,
            revision,
            engine,
        }
    }

    pub fn authority(&self) -> &AuthorityName {
        &self.authority
    }

    pub fn ancestry(&self) -> &AncestrySnapshot {
        &self.ancestry
    }

    pub fn grant(&self) -> &Authorization {
        &self.grant
    }

    pub fn resolves(&self) -> &[Violation] {
        &self.resolved
    }

    pub(crate) fn into_parts(self) -> ApprovalParts {
        ApprovalParts {
            flow: self.flow,
            grant: self.grant,
            authority: self.authority,
            resolved: self.resolved,
            trajectory: self.trajectory,
            revision: self.revision,
            engine: self.engine,
        }
    }
}

impl fmt::Display for PendingApproval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "approval of {} by {} pending on {} at {}",
            self.grant, self.authority, self.trajectory, self.revision
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::dimension::{Audience, Trust, UserId};
    use crate::turn::{Speaker, Trajectory};
    use crate::value::OpaqueValue;

    fn suspicious_for(reader: &str) -> ValueLabel {
        ValueLabel {
            audience: Audience::readers([UserId::new(reader)]),
            trust: Trust::SUSPICIOUS,
        }
    }

    fn ingress(trajectory: &mut Trajectory, label: ValueLabel, body: &str) -> ValueId {
        trajectory.ingress(Speaker::user(UserId::new("alice")), label, OpaqueValue::new(body))
    }

    #[test]
    fn ancestry_snapshot_walks_the_transitive_closure_including_the_seed() {
        let mut trajectory = Trajectory::new();
        let root = ingress(&mut trajectory, suspicious_for("alice"), "raw");
        let mid = trajectory.seed_transformed(root, ValueLabel::identity());
        let leaf = trajectory.seed_transformed(mid, ValueLabel::identity());

        let snapshot = AncestrySnapshot::of(trajectory.view(), [leaf]);

        let ids: BTreeSet<ValueId> = snapshot.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, BTreeSet::from([root, mid, leaf]));
    }

    #[test]
    fn ancestry_snapshot_deduplicates_a_diamond_ancestor() {
        let mut trajectory = Trajectory::new();
        let root = ingress(&mut trajectory, suspicious_for("alice"), "raw");
        let left = trajectory.seed_transformed(root, ValueLabel::identity());
        let right = trajectory.seed_transformed(root, ValueLabel::identity());
        let joined = trajectory
            .admit_model_output(
                OpaqueValue::new("merged"),
                BTreeSet::from([left, right]),
                BTreeSet::new(),
            )
            .unwrap();

        let snapshot = AncestrySnapshot::of(trajectory.view(), [joined]);

        assert_eq!(snapshot.iter().filter(|(id, _)| *id == root).count(), 1);
        assert_eq!(snapshot.iter().count(), 4);
    }

    #[test]
    fn ancestry_snapshot_skips_ids_the_store_never_admitted() {
        let mut trajectory = Trajectory::new();
        let known = ingress(&mut trajectory, ValueLabel::identity(), "hi");
        let missing = ValueId::new(u64::MAX);

        let snapshot = AncestrySnapshot::of(trajectory.view(), [known, missing]);

        assert!(snapshot.get(known).is_some());
        assert_eq!(snapshot.get(missing), None);
        assert_eq!(snapshot.iter().count(), 1);
    }

    #[test]
    fn value_view_carries_the_stored_label_and_provenance() {
        let mut trajectory = Trajectory::new();
        let value = ingress(&mut trajectory, suspicious_for("bob"), "secret");

        let snapshot = AncestrySnapshot::of(trajectory.view(), [value]);
        let view = snapshot.get(value).unwrap();
        let stored = trajectory.value(value).unwrap();

        assert_eq!(&view.label, stored.label());
        assert_eq!(&view.provenance, stored.provenance());
    }
}
