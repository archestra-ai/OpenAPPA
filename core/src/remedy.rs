//! The two-kind remedy vocabulary: `Reduce` and `Authorize`.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::audit::AuthorityName;
use crate::contract::{Unprovable, Violation};
use crate::dimension::{Effect, KnownTrust, UserId};
use crate::plan::NonEmptyVec;
use crate::revision::{FlowId, ValueId};
use crate::value::TransformerRef;

/// A durable confidentiality raise: a trust attestation and/or an audience
/// admission, vouched by an authority's fiat. Not a check-transient lift —
/// applying it mints a new value under the raised label; the source is
/// untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LabelRaise {
    pub trust: Option<KnownTrust>,
    pub audience: Option<BTreeSet<UserId>>,
}

impl LabelRaise {
    pub fn is_empty(&self) -> bool {
        self.trust.is_none() && self.audience.is_none()
    }

    /// The label a value gets when this raise is applied: trust raised and
    /// audience admitted. Monotone — the lift helpers only raise a label,
    /// never lower it, so `combine` (the taint fold, which cannot improve a
    /// label) is deliberately not used.
    pub(crate) fn raise(&self, label: &crate::value::ValueLabel) -> crate::value::ValueLabel {
        crate::value::ValueLabel {
            trust: match self.trust {
                Some(attested) => label.trust.raised_to(attested),
                None => label.trust,
            },
            audience: match &self.audience {
                Some(vouched) => label.audience.admitting(vouched),
                None => label.audience.clone(),
            },
        }
    }
}

impl fmt::Display for LabelRaise {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.trust, &self.audience) {
            (Some(trust), Some(readers)) => write!(f, "trust>={trust}+audience+{}", readers.len()),
            (Some(trust), None) => write!(f, "trust>={trust}"),
            (None, Some(readers)) => write!(f, "audience+{}", readers.len()),
            (None, None) => write!(f, "nothing"),
        }
    }
}

/// A check-transient loosening applied to one flow's check: it lifts exactly
/// its populated dimensions for a single sink check and changes no stored
/// state. Crate-internal simulation data — the public vocabulary for the
/// same ask is an [`Authorization`] whose delta carries the corresponding
/// atomic coordinates at [`AuthorizationScope::PolicyCheck`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Lift {
    pub(crate) prior_effects: Option<BTreeSet<Effect>>,
    pub(crate) confirms: bool,
    /// Exclude exactly these control dependencies from the flow label for
    /// this check. Empty releases nothing; releasing one dep never releases
    /// another.
    pub(crate) control_release: BTreeSet<ValueId>,
}

impl Lift {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Whether a delta carries any check-transient coordinate — i.e. whether
    /// applying it constitutes a lift at all. A pure durable raise does not:
    /// treating it as one would let a raise-only grant clear
    /// acknowledge-only facts by mere presence.
    pub(crate) fn lifts(delta: &AuthorizationDelta) -> bool {
        delta
            .coordinates()
            .any(|coordinate| !matches!(coordinate, DeltaCoordinate::RaiseLabel(_)))
    }

    /// Fold a delta's check-transient coordinates into this lift — the one
    /// coordinate→lift conversion, shared by the applier and the planner's
    /// replay. `RaiseLabel` contributes nothing (a durable relabel is not a
    /// lift); `AcknowledgeUnknown` contributes no field — its effect is the
    /// caller's presence rule (any lift clears acknowledge-only facts on the
    /// recheck).
    pub(crate) fn absorb(&mut self, delta: &AuthorizationDelta) {
        for coordinate in delta.coordinates() {
            match coordinate {
                DeltaCoordinate::ExceptPriorEffects(effects) => {
                    self.prior_effects
                        .get_or_insert_with(BTreeSet::new)
                        .extend(effects.iter().copied());
                }
                DeltaCoordinate::StandInConfirmation => self.confirms = true,
                DeltaCoordinate::ReleaseControl(deps) => {
                    self.control_release.extend(deps.iter().copied());
                }
                DeltaCoordinate::RaiseLabel(_) | DeltaCoordinate::AcknowledgeUnknown(_) => {}
            }
        }
    }
}

/// One atomic coordinate of an authorization delta. Each names exactly one
/// elevation a mandate must be competent for; a product of several
/// coordinates in one [`AuthorizationDelta`] requires one authority
/// competent for them all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DeltaCoordinate {
    RaiseLabel(LabelRaise),
    ExceptPriorEffects(BTreeSet<Effect>),
    StandInConfirmation,
    ReleaseControl(BTreeSet<ValueId>),
    AcknowledgeUnknown(Vec<Unprovable>),
}

impl DeltaCoordinate {
    fn rank(&self) -> u8 {
        match self {
            Self::RaiseLabel(_) => 0,
            Self::ExceptPriorEffects(_) => 2,
            Self::StandInConfirmation => 3,
            Self::ReleaseControl(_) => 4,
            Self::AcknowledgeUnknown(_) => 5,
        }
    }
}

impl fmt::Display for DeltaCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RaiseLabel(raise) => write!(f, "raise {raise}"),
            Self::ExceptPriorEffects(effects) => write!(f, "except {} prior effect(s)", effects.len()),
            Self::StandInConfirmation => write!(f, "confirmation"),
            Self::ReleaseControl(deps) => write!(f, "release {} control dep(s)", deps.len()),
            Self::AcknowledgeUnknown(_) => write!(f, "acknowledgment"),
        }
    }
}

/// An exact, non-empty metadata delta: one atomic coordinate or a canonical
/// product of several. "Authorize nothing" is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AuthorizationDelta(NonEmptyVec<DeltaCoordinate>);

impl AuthorizationDelta {
    pub fn single(coordinate: DeltaCoordinate) -> Self {
        Self(NonEmptyVec::new(coordinate, Vec::new()))
    }

    /// A product delta in canonical (kind-ranked, admission-stable) order.
    /// `None` iff `coordinates` is empty.
    pub fn product(coordinates: Vec<DeltaCoordinate>) -> Option<Self> {
        let mut coordinates = coordinates;
        coordinates.sort_by_key(DeltaCoordinate::rank);
        NonEmptyVec::from_vec(coordinates).map(Self)
    }

    pub fn coordinates(&self) -> impl Iterator<Item = &DeltaCoordinate> {
        self.0.iter()
    }
}

impl fmt::Display for AuthorizationDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, coordinate) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, "+")?;
            }
            write!(f, "{coordinate}")?;
        }
        Ok(())
    }
}

/// Where a grant applies. Durable and one-off authorization are the same
/// remedy kind with different scopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AuthorizationScope {
    DerivedValue { source: ValueId },
    PolicyCheck { flow: FlowId },
}

impl fmt::Display for AuthorizationScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DerivedValue { source } => write!(f, "derived value of {source}"),
            Self::PolicyCheck { flow } => write!(f, "one check of {flow}"),
        }
    }
}

/// The typed elevation an authority rules on: an exact delta at an exact
/// scope. Constructed only through [`Authorization::new`], which refuses
/// coordinates outside their scope and no-op coordinates — a malformed
/// authorization is unrepresentable, not merely unrouted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Authorization {
    delta: AuthorizationDelta,
    scope: AuthorizationScope,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MalformedAuthorization {
    #[error("{coordinate} does not apply at {scope}")]
    CoordinateOutsideScope {
        coordinate: DeltaCoordinate,
        scope: AuthorizationScope,
    },
    #[error("{coordinate} authorizes nothing")]
    EmptyCoordinate { coordinate: DeltaCoordinate },
    /// A product carries two coordinates of the same kind; application is
    /// defined over at most one coordinate per kind, so a duplicate would be
    /// silently dropped rather than ruled on.
    #[error("duplicate {coordinate} coordinate kind in one product")]
    DuplicateCoordinateKind { coordinate: DeltaCoordinate },
}

impl Authorization {
    /// The only constructor. Refuses (a) coordinates incompatible with the
    /// scope and (b) no-op coordinates. An empty [`DeltaCoordinate::AcknowledgeUnknown`]
    /// stays valid: it demands the explicit acknowledge competence rather
    /// than authorizing nothing.
    pub fn new(delta: AuthorizationDelta, scope: AuthorizationScope) -> Result<Self, MalformedAuthorization> {
        for coordinate in delta.coordinates() {
            let fits = matches!(
                (coordinate, &scope),
                (DeltaCoordinate::RaiseLabel(_), AuthorizationScope::DerivedValue { .. })
                    | (
                        DeltaCoordinate::ExceptPriorEffects(_)
                            | DeltaCoordinate::StandInConfirmation
                            | DeltaCoordinate::ReleaseControl(_)
                            | DeltaCoordinate::AcknowledgeUnknown(_),
                        AuthorizationScope::PolicyCheck { .. },
                    )
            );
            if !fits {
                return Err(MalformedAuthorization::CoordinateOutsideScope {
                    coordinate: coordinate.clone(),
                    scope,
                });
            }
            let noop = match coordinate {
                DeltaCoordinate::RaiseLabel(raise) => raise.is_empty(),
                DeltaCoordinate::ExceptPriorEffects(effects) => effects.is_empty(),
                DeltaCoordinate::ReleaseControl(deps) => deps.is_empty(),
                DeltaCoordinate::StandInConfirmation | DeltaCoordinate::AcknowledgeUnknown(_) => false,
            };
            if noop {
                return Err(MalformedAuthorization::EmptyCoordinate {
                    coordinate: coordinate.clone(),
                });
            }
        }
        let coordinates: Vec<_> = delta.coordinates().collect();
        for pair in coordinates.windows(2) {
            if pair[0].rank() == pair[1].rank() {
                return Err(MalformedAuthorization::DuplicateCoordinateKind {
                    coordinate: pair[1].clone(),
                });
            }
        }
        Ok(Self { delta, scope })
    }

    pub fn delta(&self) -> &AuthorizationDelta {
        &self.delta
    }

    pub fn scope(&self) -> &AuthorizationScope {
        &self.scope
    }
}

impl fmt::Display for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} for {}", self.delta, self.scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_scope() -> AuthorizationScope {
        AuthorizationScope::PolicyCheck { flow: FlowId::new(0) }
    }

    #[test]
    fn construction_refuses_duplicate_coordinate_kinds() {
        let release_a = DeltaCoordinate::ReleaseControl(std::collections::BTreeSet::from([ValueId::new(0)]));
        let release_b = DeltaCoordinate::ReleaseControl(std::collections::BTreeSet::from([ValueId::new(1)]));
        let delta = AuthorizationDelta::product(vec![release_a, release_b]).unwrap();
        assert!(matches!(
            Authorization::new(delta, check_scope()),
            Err(MalformedAuthorization::DuplicateCoordinateKind { .. })
        ));

        let mixed = AuthorizationDelta::product(vec![
            DeltaCoordinate::StandInConfirmation,
            DeltaCoordinate::ReleaseControl(std::collections::BTreeSet::from([ValueId::new(0)])),
        ])
        .unwrap();
        assert!(Authorization::new(mixed, check_scope()).is_ok());
    }

    #[test]
    fn construction_refuses_coordinates_outside_their_scope() {
        let raise = DeltaCoordinate::RaiseLabel(LabelRaise {
            trust: Some(KnownTrust::Trusted),
            audience: None,
        });
        let lift = DeltaCoordinate::StandInConfirmation;
        let derived = AuthorizationScope::DerivedValue {
            source: ValueId::new(0),
        };

        assert!(Authorization::new(AuthorizationDelta::single(raise.clone()), derived.clone()).is_ok());
        assert!(Authorization::new(AuthorizationDelta::single(lift.clone()), check_scope()).is_ok());

        for (coordinate, wrong_scope) in [(raise.clone(), check_scope()), (lift, derived.clone())] {
            assert!(matches!(
                Authorization::new(AuthorizationDelta::single(coordinate), wrong_scope),
                Err(MalformedAuthorization::CoordinateOutsideScope { .. })
            ));
        }

        let cross = AuthorizationDelta::product(vec![
            DeltaCoordinate::RaiseLabel(LabelRaise {
                trust: Some(KnownTrust::Trusted),
                audience: None,
            }),
            DeltaCoordinate::StandInConfirmation,
        ])
        .expect("two coordinates");
        for scope in [
            AuthorizationScope::DerivedValue {
                source: ValueId::new(0),
            },
            check_scope(),
        ] {
            assert!(matches!(
                Authorization::new(cross.clone(), scope),
                Err(MalformedAuthorization::CoordinateOutsideScope { .. })
            ));
        }
    }

    #[test]
    fn construction_refuses_noop_coordinates() {
        for (coordinate, scope) in [
            (
                DeltaCoordinate::RaiseLabel(LabelRaise::default()),
                AuthorizationScope::DerivedValue {
                    source: ValueId::new(0),
                },
            ),
            (DeltaCoordinate::ExceptPriorEffects(BTreeSet::new()), check_scope()),
            (DeltaCoordinate::ReleaseControl(BTreeSet::new()), check_scope()),
        ] {
            assert!(matches!(
                Authorization::new(AuthorizationDelta::single(coordinate), scope),
                Err(MalformedAuthorization::EmptyCoordinate { .. })
            ));
        }

        assert!(
            Authorization::new(
                AuthorizationDelta::single(DeltaCoordinate::AcknowledgeUnknown(Vec::new())),
                check_scope(),
            )
            .is_ok()
        );
    }
}

/// A typed reduction target: what a `Reduce` remedy changes, always through
/// a registered relation (a transformer's declared output) — fewer
/// arguments or changed bytes are not inherently safer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ReductionTarget {
    DeriveValue {
        source: ValueId,
        transformer: TransformerRef,
    },
}

impl fmt::Display for ReductionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeriveValue { source, transformer } => write!(f, "derive {source} via {transformer}"),
        }
    }
}

/// One planned remedy step: the two-kind vocabulary plus the planner's
/// prediction metadata. `routes` are the authorities competent for the
/// authorization at planning time — identification, not a pin: application
/// still resolves the ruling authority live against the current registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PlannedRemedy {
    Reduce(ReductionTarget),
    Authorize {
        authorization: Authorization,
        routes: NonEmptyVec<AuthorityName>,
        targets: Vec<Violation>,
    },
}

impl fmt::Display for PlannedRemedy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reduce(target) => write!(f, "reduce: {target}"),
            Self::Authorize { authorization, .. } => write!(f, "authorize: {authorization}"),
        }
    }
}
