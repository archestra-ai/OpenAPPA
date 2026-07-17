//! The append-only event substrate: scoped facts and the `EventSet`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::ToolName;
use crate::audit::{AuditEvent, AuthorityName, RaiseLabels};
use crate::contract::Violation;
use crate::dimension::Effects;
use crate::remedy::LabelRaise;
use crate::remedy::{Authorization, AuthorizationScope};
use crate::revision::{ActionId, FlowId, GrantId, TransitionId, TurnId, ValueId};
use crate::turn::Actor;
use crate::value::{TransformerRef, ValueLabel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EventId(u64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event#{}", self.0)
    }
}

/// The event frontier a batch was appended against: the number of batches
/// accepted before it. The trajectory revision becomes a digest of this
/// frontier at the projection cutover; during the shadow phase the two
/// advance in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Basis(u64);

impl Basis {
    pub fn index(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Basis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "basis#{}", self.0)
    }
}

/// How an admitted value came to exist, carrying exactly the admission-time
/// label *inputs* (never the computed fold), so the label projection is the
/// thing that computes the fold — a stored copy could disagree with the
/// algebra, an input cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ValueOrigin {
    Ingress { turn: TurnId, label: ValueLabel },
    ModelOutput {
        reads: BTreeSet<ValueId>,
        control: BTreeSet<ValueId>,
    },
    ToolOutput {
        action: ActionId,
        intrinsic: ValueLabel,
        arguments: BTreeSet<ValueId>,
        control: BTreeSet<ValueId>,
    },
    Transformed {
        source: ValueId,
        transition: TransitionId,
        transformer: TransformerRef,
        declared: ValueLabel,
    },
    /// Authority fiat relabel: `source`'s bytes under the label `delta`
    /// raises `source`'s to. The raised label itself is deliberately absent —
    /// it is derivable (`delta.raise(source_label)`), and storing it too
    /// would be a second representation that could contradict the first.
    Endorsed {
        source: ValueId,
        authority: AuthorityName,
        delta: LabelRaise,
    },
}

/// One scoped fact. The vocabulary mirrors what the legacy mutations record
/// today; the remedy-vocabulary slice retypes the control-plane entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Fact {
    ValueAdmitted {
        value: ValueId,
        origin: ValueOrigin,
    },
    TurnAppended {
        turn: TurnId,
        actor: Actor,
        value: ValueId,
    },
    ActionProposed {
        action: ActionId,
        flow: FlowId,
        request: crate::request::ToolRequest,
        effects: Effects,
    },
    ActionConstrained {
        action: ActionId,
        to_tool: ToolName,
        effects: Effects,
    },
    ArgumentSubstituted {
        action: ActionId,
        from: ValueId,
        to: ValueId,
    },
    GrowthAccepted {
        action: ActionId,
        effects: Effects,
        authority: AuthorityName,
    },
    EffectsCommitted {
        action: ActionId,
        effects: Effects,
    },
    ConfirmationSpent {
        turn: TurnId,
    },
    ActionReleased {
        action: ActionId,
    },
    ActionCompleted {
        action: ActionId,
        output: ValueId,
    },
    DispatchFailed {
        action: ActionId,
    },
    ActionAbandoned {
        action: ActionId,
    },
    CheckPerformed {
        flow: FlowId,
        action: Option<ActionId>,
    },
    EmissionProposed {
        flow: FlowId,
        request: crate::request::EmissionRequest,
    },
    EmissionBodySubstituted {
        flow: FlowId,
        from: ValueId,
        to: ValueId,
    },
    EmissionAbandoned {
        flow: FlowId,
    },
    ResponseEmitted {
        value: ValueId,
    },
    GrantIssued {
        grant: GrantId,
        authorization: Authorization,
        authority: AuthorityName,
    },
    GrantConsumed {
        grant: GrantId,
        flow: FlowId,
        action: Option<ActionId>,
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
    ControlPlane {
        event: AuditEvent,
    },
}

/// One admitted event: an identified fact bound to the frontier it was
/// appended against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub id: EventId,
    pub basis: Basis,
    pub fact: Fact,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventConflict {
    #[error("event {id} was already admitted with different content")]
    IdCollision { id: EventId },
    #[error("event {id} skips ahead of the frontier")]
    NonContiguous { id: EventId },
    #[error("event {id} carries basis {basis:?} outside the canonical batch order")]
    NonCanonicalBasis { id: EventId, basis: Basis },
    #[error("value {value} was already admitted")]
    DuplicateValue { value: ValueId },
    #[error("turn {turn} was already appended")]
    DuplicateTurn { turn: TurnId },
    #[error("{action}: fact contradicts its admitted lifecycle")]
    ActionLifecycle { action: ActionId },
    #[error("another action is live; {action} cannot be proposed")]
    ActionSlotOccupied { action: ActionId },
    #[error("confirmation of {turn} was already spent")]
    ConfirmationAlreadySpent { turn: TurnId },
    #[error("emission {flow}: fact contradicts its admitted lifecycle")]
    EmissionLifecycle { flow: FlowId },
    #[error("grant {grant} was already issued")]
    GrantAlreadyIssued { grant: GrantId },
    #[error("grant {grant} was never issued")]
    UnknownGrant { grant: GrantId },
    #[error("grant {grant} was already consumed")]
    GrantAlreadyConsumed { grant: GrantId },
    #[error("another emission is live; {flow} cannot be proposed")]
    EmissionSlotOccupied { flow: FlowId },
    #[error("an empty batch records no fact and cannot advance the frontier")]
    EmptyBatch,
    #[error("grant {grant} is not check-scoped; only one-off grants have an availability")]
    GrantNotCheckScoped { grant: GrantId },
    #[error("grant {grant} was issued for {issued}, not {consumed}")]
    GrantScopeMismatch {
        grant: GrantId,
        issued: FlowId,
        consumed: FlowId,
    },
    #[error("{turn} is not an admitted confirming user turn")]
    UnknownConfirmation { turn: TurnId },
    #[error("turn {turn} contributes value {value}, which was never admitted")]
    UnknownTurnValue { turn: TurnId, value: ValueId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum ActionPhase {
    Open,
    Released,
}

/// The append-only, totally ordered event set of one trajectory.
#[derive(Debug, Default, Serialize)]
pub struct EventSet {
    events: Vec<Event>,
    batches: u64,
    #[serde(skip)]
    state: ProbeState,
}

impl EventSet {
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn frontier(&self) -> Basis {
        Basis(self.batches)
    }

    fn next_id(&self) -> EventId {
        EventId(self.events.len() as u64)
    }

    /// Admit one event. Idempotent on exact replay: an already-admitted
    /// event (same id, same content) is a no-op; the same id with different
    /// content, a gap past the frontier, a non-canonical basis, or a fact
    /// contradicting the admitted lifecycle is refused, and refusal changes
    /// nothing.
    ///
    /// Crate-internal on purpose: there is no public write surface into an
    /// event set — engine-owned batches are the only admission path, so a
    /// forged event (wrong basis, unknown endorse source) is unrepresentable
    /// outside the crate rather than merely refused. Test-only today — replay
    /// exists for the event-algebra property tests; a future rehydration API
    /// must validate a foreign log through this same path.
    #[cfg(test)]
    pub(crate) fn admit(&mut self, event: Event) -> Result<(), EventConflict> {
        match event.id.0.cmp(&(self.events.len() as u64)) {
            std::cmp::Ordering::Less => {
                let admitted = &self.events[event.id.0 as usize];
                if *admitted == event {
                    Ok(())
                } else {
                    Err(EventConflict::IdCollision { id: event.id })
                }
            }
            std::cmp::Ordering::Greater => Err(EventConflict::NonContiguous { id: event.id }),
            std::cmp::Ordering::Equal => {
                let expected_next = self.batches;
                let expected_same = self.events.last().map(|last| last.basis.0);
                if event.basis.0 != expected_next && Some(event.basis.0) != expected_same {
                    return Err(EventConflict::NonCanonicalBasis {
                        id: event.id,
                        basis: event.basis,
                    });
                }
                self.check_fact(&event.fact)?;
                self.index_fact(&event.fact);
                let after = event.basis.0.checked_add(1).expect("frontier overflow: refuse to wrap");
                self.batches = self.batches.max(after);
                self.events.push(event);
                Ok(())
            }
        }
    }

    /// Append one mutation's facts as one atomic batch and advance the
    /// frontier once. All facts are validated against the admitted state
    /// (plus the earlier facts of the same batch) before any is admitted, so
    /// a refused batch changes nothing. Crate-internal like [`Self::admit`]:
    /// batches enter only through engine-owned mutations.
    pub(crate) fn append_batch(&mut self, facts: Vec<Fact>) -> Result<(), EventConflict> {
        if facts.is_empty() {
            return Err(EventConflict::EmptyBatch);
        }
        self.check_batch(&facts)?;
        let basis = self.frontier();
        for fact in facts {
            let event = Event {
                id: self.next_id(),
                basis,
                fact,
            };
            self.index_fact(&event.fact);
            self.events.push(event);
        }
        self.batches = self.batches.checked_add(1).expect("frontier overflow: refuse to wrap");
        Ok(())
    }

    fn check_batch(&self, facts: &[Fact]) -> Result<(), EventConflict> {
        let mut probe = self.state.clone();
        for fact in facts {
            probe.check(fact)?;
            probe.index(fact);
        }
        Ok(())
    }

    #[cfg(test)]
    fn check_fact(&self, fact: &Fact) -> Result<(), EventConflict> {
        self.state.check(fact)
    }

    fn index_fact(&mut self, fact: &Fact) {
        self.state.index(fact);
    }
}

#[derive(Debug, Clone, Default)]
struct ProbeState {
    admitted_values: BTreeSet<ValueId>,
    admitted_turns: BTreeSet<TurnId>,
    confirming_turns: BTreeSet<TurnId>,
    spent_confirmations: BTreeSet<TurnId>,
    live_action: Option<(ActionId, ActionPhase)>,
    live_emission: Option<FlowId>,
    issued_grants: BTreeMap<GrantId, FlowId>,
    consumed_grants: BTreeSet<GrantId>,
}

impl ProbeState {
    fn check(&self, fact: &Fact) -> Result<(), EventConflict> {
        match fact {
            Fact::ValueAdmitted { value, .. } => match self.admitted_values.contains(value) {
                true => Err(EventConflict::DuplicateValue { value: *value }),
                false => Ok(()),
            },
            Fact::TurnAppended { turn, value, .. } => {
                match (self.admitted_turns.contains(turn), self.admitted_values.contains(value)) {
                    (true, _) => Err(EventConflict::DuplicateTurn { turn: *turn }),
                    (false, false) => Err(EventConflict::UnknownTurnValue {
                        turn: *turn,
                        value: *value,
                    }),
                    (false, true) => Ok(()),
                }
            }
            Fact::ActionProposed { action, .. } => match self.live_action {
                Some(_) => Err(EventConflict::ActionSlotOccupied { action: *action }),
                None => Ok(()),
            },
            Fact::ActionConstrained { action, .. }
            | Fact::ArgumentSubstituted { action, .. }
            | Fact::GrowthAccepted { action, .. }
            | Fact::EffectsCommitted { action, .. } => self.requires_live(*action, ActionPhase::Open),
            Fact::CheckPerformed { action, flow } => match action {
                Some(action) => self.requires_live(*action, ActionPhase::Open),
                None => self.requires_live_emission(*flow),
            },
            Fact::EmissionProposed { flow, .. } => match self.live_emission {
                Some(_) => Err(EventConflict::EmissionSlotOccupied { flow: *flow }),
                None => Ok(()),
            },
            Fact::EmissionBodySubstituted { flow, .. } | Fact::EmissionAbandoned { flow } => {
                self.requires_live_emission(*flow)
            }
            Fact::ActionReleased { action } => self.requires_live(*action, ActionPhase::Open),
            Fact::ActionCompleted { action, .. } | Fact::DispatchFailed { action } => {
                self.requires_live(*action, ActionPhase::Released)
            }
            Fact::ActionAbandoned { action } => self.requires_live(*action, ActionPhase::Open),
            Fact::ConfirmationSpent { turn } => match (
                self.confirming_turns.contains(turn),
                self.spent_confirmations.contains(turn),
            ) {
                (false, _) => Err(EventConflict::UnknownConfirmation { turn: *turn }),
                (true, true) => Err(EventConflict::ConfirmationAlreadySpent { turn: *turn }),
                (true, false) => Ok(()),
            },
            Fact::GrantIssued {
                grant, authorization, ..
            } => {
                if self.issued_grants.contains_key(grant) {
                    return Err(EventConflict::GrantAlreadyIssued { grant: *grant });
                }
                match authorization.scope() {
                    AuthorizationScope::PolicyCheck { .. } => Ok(()),
                    _ => Err(EventConflict::GrantNotCheckScoped { grant: *grant }),
                }
            }
            Fact::GrantConsumed { grant, flow, action } => {
                let issued = match self.issued_grants.get(grant) {
                    None => return Err(EventConflict::UnknownGrant { grant: *grant }),
                    Some(issued) => *issued,
                };
                if self.consumed_grants.contains(grant) {
                    return Err(EventConflict::GrantAlreadyConsumed { grant: *grant });
                }
                if issued != *flow {
                    return Err(EventConflict::GrantScopeMismatch {
                        grant: *grant,
                        issued,
                        consumed: *flow,
                    });
                }
                match action {
                    Some(action) => self.requires_live(*action, ActionPhase::Open),
                    None => Ok(()),
                }
            }
            Fact::ResponseEmitted { .. }
            | Fact::AuthorizationApplied { .. }
            | Fact::AuthorizationDenied { .. }
            | Fact::ControlPlane { .. } => Ok(()),
        }
    }

    fn requires_live(&self, action: ActionId, phase: ActionPhase) -> Result<(), EventConflict> {
        match self.live_action {
            Some((live, live_phase)) if live == action && live_phase == phase => Ok(()),
            _ => Err(EventConflict::ActionLifecycle { action }),
        }
    }

    fn requires_live_emission(&self, flow: FlowId) -> Result<(), EventConflict> {
        match self.live_emission {
            Some(live) if live == flow => Ok(()),
            _ => Err(EventConflict::EmissionLifecycle { flow }),
        }
    }

    fn index(&mut self, fact: &Fact) {
        match fact {
            Fact::ValueAdmitted { value, .. } => {
                self.admitted_values.insert(*value);
            }
            Fact::TurnAppended { turn, actor, .. } => {
                self.admitted_turns.insert(*turn);
                if matches!(actor, Actor::User(crate::turn::UserTurn { confirms: Some(_), .. })) {
                    self.confirming_turns.insert(*turn);
                }
            }
            Fact::ActionProposed { action, .. } => {
                self.live_action = Some((*action, ActionPhase::Open));
            }
            Fact::ActionReleased { action } => {
                self.live_action = Some((*action, ActionPhase::Released));
            }
            Fact::ActionCompleted { .. } | Fact::DispatchFailed { .. } | Fact::ActionAbandoned { .. } => {
                self.live_action = None;
            }
            Fact::ConfirmationSpent { turn } => {
                self.spent_confirmations.insert(*turn);
            }
            Fact::EmissionProposed { flow, .. } => {
                self.live_emission = Some(*flow);
            }
            Fact::EmissionAbandoned { .. } | Fact::ResponseEmitted { .. } => {
                self.live_emission = None;
            }
            Fact::GrantIssued {
                grant, authorization, ..
            } => {
                if let AuthorizationScope::PolicyCheck { flow } = authorization.scope() {
                    self.issued_grants.insert(*grant, *flow);
                }
            }
            Fact::GrantConsumed { grant, .. } => {
                self.consumed_grants.insert(*grant);
            }
            Fact::ActionConstrained { .. }
            | Fact::ArgumentSubstituted { .. }
            | Fact::GrowthAccepted { .. }
            | Fact::EffectsCommitted { .. }
            | Fact::CheckPerformed { .. }
            | Fact::EmissionBodySubstituted { .. }
            | Fact::AuthorizationApplied { .. }
            | Fact::AuthorizationDenied { .. }
            | Fact::ControlPlane { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::Effects;
    use crate::revision::{ActionId, TurnId, ValueId};
    use crate::turn::{Actor, UserTurn};
    use crate::value::ValueLabel;

    fn ingress_fact(index: u64, label: ValueLabel) -> Fact {
        Fact::ValueAdmitted {
            value: ValueId::new(index),
            origin: ValueOrigin::Ingress {
                turn: TurnId::new(index),
                label,
            },
        }
    }

    fn turn_fact(index: u64) -> Fact {
        Fact::TurnAppended {
            turn: TurnId::new(index),
            actor: Actor::User(UserTurn {
                id: crate::dimension::UserId::new("alice"),
                confirms: None,
            }),
            value: ValueId::new(index),
        }
    }

    fn confirming_turn_fact(index: u64) -> Fact {
        Fact::TurnAppended {
            turn: TurnId::new(index),
            actor: Actor::User(UserTurn {
                id: crate::dimension::UserId::new("alice"),
                confirms: Some(crate::ToolName::new("db.drop")),
            }),
            value: ValueId::new(index),
        }
    }

    fn proposal(action: u64) -> Fact {
        Fact::ActionProposed {
            action: ActionId::new(action),
            flow: crate::revision::FlowId::new(action),
            request: crate::request::ToolRequest::new(
                crate::ToolName::new("email.send"),
                crate::request::ArgumentTree::empty(),
                std::collections::BTreeSet::new(),
            ),
            effects: Effects::none(),
        }
    }

    #[test]
    fn replaying_an_admitted_event_is_a_noop() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity()), turn_fact(0)])
            .unwrap();
        let snapshot: Vec<Event> = set.events().to_vec();
        for event in &snapshot {
            set.admit(event.clone()).unwrap();
        }
        assert_eq!(set.events(), snapshot.as_slice());
        assert_eq!(set.frontier(), Basis(1));
    }

    #[test]
    fn same_id_with_different_content_is_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        let mut forged = set.events()[0].clone();
        forged.fact = ingress_fact(0, ValueLabel::unknown());
        assert_eq!(set.admit(forged), Err(EventConflict::IdCollision { id: EventId(0) }));
        assert_eq!(set.events().len(), 1);
    }

    #[test]
    fn skipping_ahead_of_the_frontier_is_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        let mut ahead = set.events()[0].clone();
        ahead.id = EventId(5);
        assert_eq!(set.admit(ahead), Err(EventConflict::NonContiguous { id: EventId(5) }));
    }

    #[test]
    fn a_forged_basis_is_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        let mut inflated = EventSet::default();
        let mut forged = set.events()[0].clone();
        forged.basis = Basis(2);
        assert_eq!(
            inflated.admit(forged),
            Err(EventConflict::NonCanonicalBasis {
                id: EventId(0),
                basis: Basis(2),
            })
        );
        assert_eq!(inflated.frontier(), Basis(0));
        set.append_batch(vec![turn_fact(0)]).unwrap();
        let mut regressed = set.events()[1].clone();
        regressed.id = EventId(2);
        regressed.basis = Basis(0);
        assert_eq!(
            set.admit(regressed),
            Err(EventConflict::NonCanonicalBasis {
                id: EventId(2),
                basis: Basis(0),
            })
        );
    }

    #[test]
    fn lifecycle_conflicts_are_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![proposal(0)]).unwrap();

        assert!(matches!(
            set.append_batch(vec![Fact::ActionCompleted {
                action: ActionId::new(0),
                output: ValueId::new(0),
            }]),
            Err(EventConflict::ActionLifecycle { .. })
        ));
        assert!(matches!(
            set.append_batch(vec![proposal(1)]),
            Err(EventConflict::ActionSlotOccupied { .. })
        ));

        set.append_batch(vec![Fact::ActionReleased {
            action: ActionId::new(0),
        }])
        .unwrap();
        assert!(matches!(
            set.append_batch(vec![Fact::ActionReleased {
                action: ActionId::new(0)
            }]),
            Err(EventConflict::ActionLifecycle { .. })
        ));
    }

    #[test]
    fn double_confirmation_spend_is_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity()), confirming_turn_fact(0)])
            .unwrap();
        set.append_batch(vec![Fact::ConfirmationSpent { turn: TurnId::new(0) }])
            .unwrap();
        assert!(matches!(
            set.append_batch(vec![Fact::ConfirmationSpent { turn: TurnId::new(0) }]),
            Err(EventConflict::ConfirmationAlreadySpent { .. })
        ));
    }

    #[test]
    fn confirmation_spend_requires_an_admitted_confirming_turn() {
        let mut set = EventSet::default();
        assert!(matches!(
            set.append_batch(vec![Fact::ConfirmationSpent { turn: TurnId::new(0) }]),
            Err(EventConflict::UnknownConfirmation { .. })
        ));
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity()), turn_fact(0)])
            .unwrap();
        assert!(matches!(
            set.append_batch(vec![Fact::ConfirmationSpent { turn: TurnId::new(0) }]),
            Err(EventConflict::UnknownConfirmation { .. })
        ));
    }

    #[test]
    fn a_turn_naming_an_unadmitted_value_is_refused() {
        let mut set = EventSet::default();
        assert_eq!(
            set.append_batch(vec![turn_fact(0)]),
            Err(EventConflict::UnknownTurnValue {
                turn: TurnId::new(0),
                value: ValueId::new(0),
            })
        );
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity()), turn_fact(0)])
            .unwrap();
    }

    #[test]
    fn duplicate_value_admission_is_refused() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        assert!(matches!(
            set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]),
            Err(EventConflict::DuplicateValue { .. })
        ));
    }

    #[test]
    fn a_refused_batch_changes_nothing() {
        let mut set = EventSet::default();
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        let before: Vec<Event> = set.events().to_vec();
        let frontier = set.frontier();

        assert!(
            set.append_batch(vec![
                ingress_fact(1, ValueLabel::identity()),
                ingress_fact(0, ValueLabel::identity()),
            ])
            .is_err()
        );
        assert_eq!(set.events(), before.as_slice());
        assert_eq!(set.frontier(), frontier);
    }

    #[test]
    fn grant_consumption_is_linear_at_admission() {
        use crate::remedy::{Authorization, AuthorizationDelta, AuthorizationScope, DeltaCoordinate};
        let authorization = Authorization::new(
            AuthorizationDelta::single(DeltaCoordinate::StandInConfirmation),
            AuthorizationScope::PolicyCheck {
                flow: crate::revision::FlowId::new(0),
            },
        )
        .unwrap();
        let grant = crate::revision::GrantId::new(0);
        let issued = Fact::GrantIssued {
            grant,
            authorization,
            authority: AuthorityName::new("human"),
        };
        let consumed = Fact::GrantConsumed {
            grant,
            flow: crate::revision::FlowId::new(0),
            action: None,
        };

        let mut set = EventSet::default();
        assert!(matches!(
            set.append_batch(vec![consumed.clone()]),
            Err(EventConflict::UnknownGrant { .. })
        ));

        set.append_batch(vec![issued.clone(), consumed.clone()]).unwrap();
        assert!(crate::projection::grant_availability(&set).is_empty());

        assert!(matches!(
            set.append_batch(vec![consumed.clone()]),
            Err(EventConflict::GrantAlreadyConsumed { .. })
        ));
        set.append_batch(vec![ingress_fact(0, ValueLabel::identity())]).unwrap();
        assert!(matches!(
            set.append_batch(vec![Fact::GrantConsumed {
                grant,
                flow: crate::revision::FlowId::new(7),
                action: Some(ActionId::new(3)),
            }]),
            Err(EventConflict::GrantAlreadyConsumed { .. })
        ));
        assert!(matches!(
            set.append_batch(vec![issued]),
            Err(EventConflict::GrantAlreadyIssued { .. })
        ));
    }

    #[test]
    fn grant_admission_enforces_the_issued_scope() {
        use crate::remedy::{Authorization, AuthorizationDelta, AuthorizationScope, DeltaCoordinate};

        let durable = Authorization::new(
            AuthorizationDelta::single(DeltaCoordinate::RaiseLabel(crate::remedy::LabelRaise {
                trust: Some(crate::dimension::KnownTrust::Trusted),
                audience: None,
            })),
            AuthorizationScope::DerivedValue {
                source: ValueId::new(0),
            },
        )
        .unwrap();
        let mut set = EventSet::default();
        assert!(matches!(
            set.append_batch(vec![Fact::GrantIssued {
                grant: crate::revision::GrantId::new(0),
                authorization: durable,
                authority: AuthorityName::new("human"),
            }]),
            Err(EventConflict::GrantNotCheckScoped { .. })
        ));

        let checked = Authorization::new(
            AuthorizationDelta::single(DeltaCoordinate::StandInConfirmation),
            AuthorizationScope::PolicyCheck {
                flow: crate::revision::FlowId::new(7),
            },
        )
        .unwrap();
        let grant = crate::revision::GrantId::new(1);
        set.append_batch(vec![Fact::GrantIssued {
            grant,
            authorization: checked,
            authority: AuthorityName::new("human"),
        }])
        .unwrap();
        assert!(matches!(
            set.append_batch(vec![Fact::GrantConsumed {
                grant,
                flow: crate::revision::FlowId::new(8),
                action: None,
            }]),
            Err(EventConflict::GrantScopeMismatch { .. })
        ));
        assert!(matches!(
            set.append_batch(vec![Fact::GrantConsumed {
                grant,
                flow: crate::revision::FlowId::new(7),
                action: Some(ActionId::new(4)),
            }]),
            Err(EventConflict::ActionLifecycle { .. })
        ));
        assert!(!crate::projection::grant_availability(&set).is_empty());
        set.append_batch(vec![Fact::GrantConsumed {
            grant,
            flow: crate::revision::FlowId::new(7),
            action: None,
        }])
        .unwrap();
        assert!(crate::projection::grant_availability(&set).is_empty());
    }

    #[test]
    fn an_empty_batch_is_refused() {
        let mut set = EventSet::default();
        let frontier = set.frontier();
        assert!(matches!(set.append_batch(Vec::new()), Err(EventConflict::EmptyBatch)));
        assert_eq!(set.frontier(), frontier);
    }

    mod laws {
        use proptest::prelude::*;

        use super::*;
        use crate::test_strategies::arb_value_label;

        fn arb_simple_batches() -> impl Strategy<Value = Vec<Vec<Fact>>> {
            prop::collection::vec((arb_value_label(), any::<bool>(), any::<bool>()), 0..12).prop_map(|entries| {
                entries
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, with_turn, with_history))| {
                        let index = index as u64;
                        let mut batch = vec![ingress_fact(index, label)];
                        if with_turn {
                            batch.push(turn_fact(index));
                        }
                        if with_history {
                            batch.push(Fact::ControlPlane {
                                event: crate::audit::AuditEvent::DispatchFailed {
                                    action: ActionId::new(index),
                                },
                            });
                        }
                        batch
                    })
                    .collect()
            })
        }

        proptest! {
            #[test]
            fn replay_is_idempotent_and_rebuilds_equal_sets(batches in arb_simple_batches()) {
                let mut set = EventSet::default();
                for batch in &batches {
                    set.append_batch(batch.clone()).unwrap();
                }
                let canonical: Vec<Event> = set.events().to_vec();

                for event in &canonical {
                    set.admit(event.clone()).unwrap();
                }
                prop_assert_eq!(set.events(), canonical.as_slice());

                let mut rebuilt = EventSet::default();
                for event in &canonical {
                    rebuilt.admit(event.clone()).unwrap();
                }
                prop_assert_eq!(rebuilt.events(), canonical.as_slice());
                prop_assert_eq!(rebuilt.frontier(), set.frontier());
            }

            #[test]
            fn projections_are_deterministic_over_replay(batches in arb_simple_batches()) {
                let mut set = EventSet::default();
                for batch in &batches {
                    set.append_batch(batch.clone()).unwrap();
                }
                let mut rebuilt = EventSet::default();
                for event in set.events() {
                    rebuilt.admit(event.clone()).unwrap();
                }
                prop_assert_eq!(
                    crate::projection::value_labels(&set),
                    crate::projection::value_labels(&rebuilt)
                );
                prop_assert_eq!(
                    crate::projection::provenance(&set),
                    crate::projection::provenance(&rebuilt)
                );
                prop_assert_eq!(
                    crate::projection::committed_effects(&set),
                    crate::projection::committed_effects(&rebuilt)
                );
                prop_assert_eq!(
                    crate::projection::confirmation_available(&set),
                    crate::projection::confirmation_available(&rebuilt)
                );
            }
        }
    }
}
