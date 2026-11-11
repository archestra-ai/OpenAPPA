//! What the harness's own records say about one family right now.
//!
//! The engine reads its facts back from the log, and so does everything the runtime knows
//! about an actor: who stands behind a key, what is executing, which actor a prompt reached,
//! and which tools a host has reported. This module is the reducer over one family's host
//! records — [`HostState::fold`] and the projections beside it — so the runtime holds no
//! actor state between calls and a restart loses none of it.
//!
//! Every rule here is about *liveness*: a record is not a state, and the state is what the
//! records that follow it have not ended.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use appa_engine::value::TrajectoryId;
use appa_eventlog::{
    AheadAttempt, AheadOutcome, DeclaredArguments, EndKind, HostActor, HostObservation, HostRecord, InvocationNonce,
    Log, StoredLabel,
};
use appa_runtime_api::{Actor, Adapter, Ruling, inventory::ToolInventory};

use super::{EventError, PermitKey, Unvouched, acting_trajectory, inventory_refused};

/// One actor's standing behind one key, the ruling that rides it, and the position of the
/// vouch that stands: an unnamed declaration is the attempt that seq names.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveVouch {
    pub(crate) actor: Actor,
    pub(crate) ruling: Option<Ruling>,
    pub(crate) seq: u64,
}

/// One actor's execution of what a key names, and the moment after which a reader treats it
/// as abandoned. The bound is the hard-crash backstop only: an execution that ends releases
/// its claim.
#[derive(Debug, Clone, PartialEq)]
struct Claim {
    actor: Actor,
    until: SystemTime,
}

/// The host stream of one family, reduced.
#[derive(Debug, Default)]
pub(crate) struct HostState {
    vouches: BTreeMap<PermitKey, Vec<LiveVouch>>,
    claims: BTreeMap<PermitKey, Claim>,
}

impl HostState {
    /// Reduce one family's host records. `now` decides which claims a crash left behind.
    ///
    /// A recorded key this build cannot read is skipped rather than refused: the reducer
    /// answers who stands behind a key, and a spelling it cannot name is a spelling nothing
    /// can ask about. Skipping it leaves the take with nobody, which is the closed end.
    pub(crate) fn fold(records: &[HostRecord], now: SystemTime) -> HostState {
        let mut state = HostState::default();
        for record in records {
            match &record.observation {
                // Evidence, not standing: read where it is needed, never folded. A prompt
                // mark is the latest record about it, which [`prompted`] finds by walking
                // the records back from the end.
                HostObservation::Inventory { .. }
                | HostObservation::CallBound { .. }
                | HostObservation::CallSettled { .. }
                | HostObservation::CallDenied { .. }
                | HostObservation::PromptSeen { .. }
                | HostObservation::PromptSettled { .. }
                | HostObservation::Addressed { .. }
                | HostObservation::Titled { .. }
                | HostObservation::PeerSent { .. }
                | HostObservation::PeerAdmitted { .. }
                | HostObservation::Launched { .. }
                | HostObservation::LaunchMoved { .. }
                | HostObservation::Aliased { .. }
                | HostObservation::AheadDeclared { spends: None, .. }
                | HostObservation::AheadReserved { .. }
                | HostObservation::AheadSettled { .. } => {}
                // A daemon declaration consumes the vouch that authorized it, as a release does.
                HostObservation::AheadDeclared {
                    actor,
                    spends: Some(spent),
                    ..
                } => {
                    if let Some(key) = recorded_key(&spent.key) {
                        state.end(&actor_of(actor), &key);
                    }
                }
                HostObservation::Vouched { actor, key, ruling } => {
                    let (Some(key), actor) = (recorded_key(key), actor_of(actor)) else {
                        continue;
                    };
                    // An offer has one current pursuer: its new vouch replaces the former
                    // pursuer's. A call key may be quoted by independent sessions, so those
                    // holders are kept and the take refuses the ambiguity.
                    let offer = matches!(key, PermitKey::Offer(_));
                    let holders = state.vouches.entry(key).or_default();
                    if offer {
                        holders.clear();
                    }
                    let seq = record.seq;
                    match holders.iter_mut().find(|holder| holder.actor == actor) {
                        Some(holder) => {
                            holder.ruling = *ruling;
                            holder.seq = seq;
                        }
                        None => holders.push(LiveVouch {
                            actor,
                            ruling: *ruling,
                            seq,
                        }),
                    }
                }
                HostObservation::Released { actor, key } => {
                    if let Some(key) = recorded_key(key) {
                        state.end(&actor_of(actor), &key);
                    }
                }
                HostObservation::Claimed { actor, key, until } => {
                    let (Some(key), actor) = (recorded_key(key), actor_of(actor)) else {
                        continue;
                    };
                    // Claiming spends the claimant's standing behind the key: the act it
                    // vouched for is the act now running.
                    state.end(&actor, &key);
                    state.claims.insert(key, Claim { actor, until: *until });
                }
                HostObservation::TurnEnded { actor, .. } => {
                    let ended = actor_of(actor);
                    state.vouches.retain(|_, holders| {
                        holders.retain(|holder| holder.actor != ended);
                        !holders.is_empty()
                    });
                }
            }
        }
        state.claims.retain(|_, claim| claim.until > now);
        state
    }

    /// The trajectory vouched for this key, with the ruling its harness attached, or why no
    /// one answer names a caller.
    pub(crate) fn vouched(&self, key: &PermitKey) -> Result<(Actor, Option<Ruling>), Unvouched> {
        self.standing(key).map(|only| (only.actor, only.ruling))
    }

    /// The one live vouch behind this key, or why no one answer names a caller.
    pub(crate) fn standing(&self, key: &PermitKey) -> Result<LiveVouch, Unvouched> {
        match self.vouches.get(key).map(Vec::as_slice) {
            Some([only]) => Ok(only.clone()),
            Some([]) | None => Err(Unvouched::Nobody),
            Some(_) => Err(Unvouched::Ambiguous),
        }
    }

    /// Until when what this key names is being executed, if it is right now. The bound also
    /// names the claim: an execution renewing or releasing its own finds it here unchanged.
    pub(crate) fn claimed(&self, key: &PermitKey) -> Option<SystemTime> {
        self.claims.get(key).map(|claim| claim.until)
    }

    fn end(&mut self, actor: &Actor, key: &PermitKey) {
        if let Some(holders) = self.vouches.get_mut(key) {
            holders.retain(|holder| holder.actor != *actor);
            if holders.is_empty() {
                self.vouches.remove(key);
            }
        }
        if self.claims.get(key).is_some_and(|claim| claim.actor == *actor) {
            self.claims.remove(key);
        }
    }
}

/// One declaration made ahead of the spawn, as its record holds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Declared {
    pub(crate) actor: Actor,
    pub(crate) arguments: DeclaredArguments,
    pub(crate) label: StoredLabel,
    pub(crate) seq: u64,
}

/// What one family's records say about returns declared ahead of a spawn. Keys are the acting
/// trajectory, the child where the harness named one.
#[derive(Debug, Default)]
pub(crate) struct AheadState {
    declared: BTreeMap<(TrajectoryId, AheadAttempt), Declared>,
    /// The latest declaration of each trajectory: a newer one supersedes the one before it.
    latest: BTreeMap<TrajectoryId, AheadAttempt>,
    /// What each call's first record took: a declaration, or none.
    assigned: BTreeMap<(TrajectoryId, String), Option<AheadAttempt>>,
    /// The declarations a spawn took: a call's first record, or an invocation's reservation.
    taken: BTreeSet<(TrajectoryId, AheadAttempt)>,
    reserved: BTreeMap<(TrajectoryId, InvocationNonce), AheadAttempt>,
    settled: BTreeMap<(TrajectoryId, AheadAttempt), AheadOutcome>,
    /// The seq of each trajectory's latest turn boundary.
    last_boundary: BTreeMap<TrajectoryId, u64>,
}

impl AheadState {
    pub(crate) fn fold(records: &[HostRecord]) -> AheadState {
        let mut state = AheadState::default();
        for record in records {
            match &record.observation {
                HostObservation::AheadDeclared {
                    actor,
                    attempt,
                    arguments,
                    label,
                    ..
                } => {
                    let trajectory = acting_trajectory(&actor_of(actor)).clone();
                    state.latest.insert(trajectory.clone(), attempt.clone());
                    state
                        .declared
                        .entry((trajectory, attempt.clone()))
                        .or_insert_with(|| Declared {
                            actor: actor_of(actor),
                            arguments: arguments.clone(),
                            label: label.clone(),
                            seq: record.seq,
                        });
                }
                HostObservation::CallBound {
                    trajectory,
                    call_id,
                    ahead,
                    ..
                }
                | HostObservation::CallDenied {
                    trajectory,
                    call_id,
                    ahead,
                    ..
                } => state.assign(trajectory, call_id, ahead.as_ref()),
                HostObservation::AheadReserved {
                    trajectory,
                    attempt,
                    invocation,
                } => {
                    state
                        .reserved
                        .entry((trajectory.clone(), *invocation))
                        .or_insert_with(|| attempt.clone());
                    state.taken.insert((trajectory.clone(), attempt.clone()));
                }
                HostObservation::AheadSettled {
                    trajectory,
                    attempt,
                    outcome,
                } => {
                    state
                        .settled
                        .entry((trajectory.clone(), attempt.clone()))
                        .or_insert(*outcome);
                }
                HostObservation::TurnEnded {
                    actor,
                    end_kind: EndKind::Boundary,
                } => {
                    let trajectory = acting_trajectory(&actor_of(actor)).clone();
                    state.last_boundary.insert(trajectory, record.seq);
                }
                _ => {}
            }
        }
        state
    }

    /// Only a call's first record assigns: a retry reuses what it took, and a call first
    /// recorded with none never takes a declaration later.
    fn assign(&mut self, trajectory: &TrajectoryId, call_id: &str, ahead: Option<&AheadAttempt>) {
        let call = (trajectory.clone(), call_id.to_string());
        if self.assigned.contains_key(&call) {
            return;
        }
        self.assigned.insert(call, ahead.cloned());
        if let Some(attempt) = ahead {
            self.taken.insert((trajectory.clone(), attempt.clone()));
        }
    }

    /// Whether a turn boundary of `trajectory` landed after this declaration.
    pub(crate) fn cleared(&self, trajectory: &TrajectoryId, declared: &Declared) -> bool {
        self.last_boundary
            .get(trajectory)
            .is_some_and(|boundary| *boundary > declared.seq)
    }

    /// The declaration the trajectory's next spawn takes: its latest, unless a spawn already
    /// took it or a turn boundary cleared it.
    pub(crate) fn live(&self, trajectory: &TrajectoryId) -> Option<&AheadAttempt> {
        let attempt = self.latest.get(trajectory)?;
        let key = (trajectory.clone(), attempt.clone());
        let declared = self.declared.get(&key)?;
        (!self.taken.contains(&key) && !self.cleared(trajectory, declared)).then_some(attempt)
    }

    pub(crate) fn declared(&self, trajectory: &TrajectoryId, attempt: &AheadAttempt) -> Option<&Declared> {
        self.declared.get(&(trajectory.clone(), attempt.clone()))
    }

    /// What this call's first record took: `None` before any record, `Some(None)` when it took
    /// no declaration.
    pub(crate) fn assignment(&self, trajectory: &TrajectoryId, call_id: &str) -> Option<Option<&AheadAttempt>> {
        self.assigned
            .get(&(trajectory.clone(), call_id.to_string()))
            .map(Option::as_ref)
    }

    pub(crate) fn reservation_of(
        &self,
        trajectory: &TrajectoryId,
        invocation: InvocationNonce,
    ) -> Option<&AheadAttempt> {
        self.reserved.get(&(trajectory.clone(), invocation))
    }

    pub(crate) fn settlement(&self, trajectory: &TrajectoryId, attempt: &AheadAttempt) -> Option<AheadOutcome> {
        self.settled.get(&(trajectory.clone(), attempt.clone())).copied()
    }

    /// Every declaration this family made under `attempt`, whichever actor made it.
    pub(crate) fn declared_by_attempt(&self, attempt: &AheadAttempt) -> Vec<&Declared> {
        self.declared
            .iter()
            .filter(|((_, made), _)| made == attempt)
            .map(|(_, declared)| declared)
            .collect()
    }
}

/// The key a record names, or nothing where this build cannot read the spelling.
fn recorded_key(recorded: &str) -> Option<PermitKey> {
    PermitKey::parse(recorded).or_else(|| {
        tracing::warn!(key = %recorded, "a recorded key is spelled in a way this build cannot read");
        None
    })
}

/// The actor a record names, in the vocabulary the harness speaks.
fn actor_of(actor: &HostActor) -> Actor {
    Actor {
        root: actor.root.clone(),
        child: actor.child.clone(),
    }
}

/// The actor of a record, as a record carries it.
pub(crate) fn host_actor(actor: &Actor) -> HostActor {
    HostActor {
        root: actor.root.clone(),
        child: actor.child.clone(),
    }
}

/// Whether a prompt reached `acting` — the spelling of the acting trajectory, the child where
/// the harness named one — and nothing has settled what it left behind.
///
/// The mark is the latest of three records and nothing else: a prompt raises it, settling it
/// and the turn's end lower it, and no other record says anything about it. So the answer is
/// that one record, found from the end, and not a reduction of the family.
pub(crate) fn prompted(records: &[HostRecord], acting: &appa_engine::value::TrajectoryId) -> bool {
    let marks = |observation: &HostObservation| match observation {
        HostObservation::PromptSeen { actor }
        | HostObservation::PromptSettled { actor }
        | HostObservation::TurnEnded { actor, .. } => {
            actor.child.as_ref().unwrap_or(&actor.root).as_str() == acting.as_str()
        }
        _ => false,
    };
    matches!(
        records.iter().rev().find(|record| marks(&record.observation)),
        Some(HostRecord {
            observation: HostObservation::PromptSeen { .. },
            ..
        })
    )
}

/// Every tool this actor's host has reported, under the opening's own snapshot.
///
/// A projection over the log rather than a field of [`HostState`]: an inventory is large and
/// every tool call folds, so it is read where it is needed and nowhere else.
pub(crate) fn inventory_at(log: &Log, actor: &Actor, adapter: Adapter) -> Result<ToolInventory, EventError> {
    let mut previous = ToolInventory::default();
    if actor.child.is_none() {
        #[derive(serde::Deserialize)]
        struct OpeningInventory {
            #[serde(default)]
            appa_inventory: ToolInventory,
        }
        let source = std::str::from_utf8(log.policy_file())
            .map_err(|_| EventError::PolicyUnavailable("stored policy is not UTF-8".into()))?;
        let opening: OpeningInventory = toml::from_str(source)
            .map_err(|_| EventError::PolicyUnavailable("stored inventory does not decode".into()))?;
        previous = opening.appa_inventory;
    }
    previous.validate(adapter).map_err(inventory_refused)?;
    let mut tools: BTreeMap<_, _> = previous
        .tools
        .into_iter()
        .map(|tool| (tool.name.clone(), tool))
        .collect();
    let mut sources: BTreeMap<_, _> = previous
        .sources
        .into_iter()
        .map(|source| (source.server.clone(), source))
        .collect();
    let scope = acting_trajectory(actor);
    for record in log.host_records() {
        let HostObservation::Inventory {
            actor: observed,
            adapter: observed_adapter,
            inventory,
        } = &record.observation
        else {
            continue;
        };
        if observed != scope {
            continue;
        }
        if *observed_adapter != adapter.name {
            return Err(EventError::InventoryRefused(
                "an actor cannot change its plugin adapter".into(),
            ));
        }
        inventory.validate(adapter).map_err(inventory_refused)?;
        for tool in &inventory.tools {
            if let Some(old) = tools.get(&tool.name)
                && old.tool != tool.tool
            {
                return Err(EventError::InventoryRefused(
                    "a recorded tool changed identity within this actor".into(),
                ));
            }
            tools.insert(tool.name.clone(), tool.clone());
        }
        for source in &inventory.sources {
            sources.insert(source.server.clone(), source.clone());
        }
    }
    let previous = ToolInventory {
        tools: tools.into_values().collect(),
        sources: sources.into_values().collect(),
    };
    previous.validate(adapter).map_err(inventory_refused)?;
    Ok(previous)
}
