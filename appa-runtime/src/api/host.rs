//! What the harness's own records say about one family right now.
//!
//! The engine reads its facts back from the log, and so does everything the runtime knows
//! about an actor: who stands behind a key, what is executing, which actor a prompt reached,
//! and which tools a host has reported. This module is the reducer over the host stream —
//! [`HostState::fold`] and the projections beside it — so the runtime holds no actor state
//! between calls and a restart loses none of it.
//!
//! Every rule here is about *liveness*: a record is not a state, and the state is what the
//! records that follow it have not ended.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use appa_eventlog::{HostActor, HostObservation, HostRecord, Log};
use appa_runtime_api::{Actor, Adapter, Ruling, TrajectoryId, inventory::ToolInventory};

use super::{EventError, PermitKey, Unvouched, acting_trajectory, inventory_refused};

/// One actor's standing behind one key, and the ruling that rides it.
#[derive(Debug, Clone, PartialEq)]
struct LiveVouch {
    actor: Actor,
    ruling: Option<Ruling>,
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
    /// The acting trajectory's spelling, which is what a prompt mark is about.
    prompted: BTreeSet<String>,
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
                // Evidence, not standing: read where it is needed, never folded.
                HostObservation::Inventory { .. } | HostObservation::CallBound { .. } => {}
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
                    match holders.iter_mut().find(|holder| holder.actor == actor) {
                        Some(holder) => holder.ruling = *ruling,
                        None => holders.push(LiveVouch { actor, ruling: *ruling }),
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
                HostObservation::PromptSeen { actor } => {
                    state.prompted.insert(marked(actor));
                }
                HostObservation::PromptSettled { actor } => {
                    state.prompted.remove(&marked(actor));
                }
                HostObservation::TurnEnded { actor } => {
                    let ended = actor_of(actor);
                    state.prompted.remove(&marked(actor));
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
        match self.vouches.get(key).map(Vec::as_slice) {
            Some([only]) => Ok((only.actor.clone(), only.ruling)),
            Some([]) | None => Err(Unvouched::Nobody),
            Some(_) => Err(Unvouched::Ambiguous),
        }
    }

    /// Whether what this key names is being executed right now.
    pub(crate) fn claimed(&self, key: &PermitKey) -> bool {
        self.claims.contains_key(key)
    }

    /// Whether a prompt reached this actor and nothing has settled what it left behind.
    pub(crate) fn prompted(&self, actor: &Actor) -> bool {
        self.prompted.contains(&acting_trajectory(actor).0)
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
        root: TrajectoryId(actor.root.as_str().to_string()),
        child: actor
            .child
            .as_ref()
            .map(|child| TrajectoryId(child.as_str().to_string())),
    }
}

/// The actor of a record, as a record carries it.
pub(crate) fn host_actor(actor: &Actor) -> HostActor {
    HostActor {
        root: crate::engine::engine_id(&actor.root),
        child: actor.child.as_ref().map(crate::engine::engine_id),
    }
}

/// What a prompt mark is about: the acting trajectory, which is the child where the harness
/// named one.
fn marked(actor: &HostActor) -> String {
    actor.child.as_ref().unwrap_or(&actor.root).as_str().to_string()
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
    let scope = crate::engine::engine_id(acting_trajectory(actor));
    for record in log.host_records() {
        let HostObservation::Inventory {
            actor: observed,
            adapter: observed_adapter,
            inventory,
        } = &record.observation
        else {
            continue;
        };
        if *observed != scope {
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
