//! The session store: server-minted trajectories, their families, and the one append-only log per
//! family behind a compare-and-swap `Revision`.
//!
//! This is the runtime's state, kept deliberately narrow (one in-memory implementation; a durable
//! backend is a follow-up behind this same surface). Three things live here:
//!
//! - **Sessions.** Each trajectory id is *server-minted* and bound to an authenticated caller (the
//!   trusted host / tenant, RP1). A foreign session or parent id is rejected, not namespaced. A
//!   child's parent link is fixed at fork and never reparented — there is no setter.
//! - **Families.** A root session and everything forked under it share one append-only log and one
//!   monotone [`Revision`] (RP6): effects and history are family-wide, so a child's egress is
//!   visible to a parent's `no_prior`. A fork joins the parent's family; it never starts a new log.
//! - **Append.** [`SessionStore::conditional_append`] is the serialization point — a batch lands
//!   only if the family is still at the batch's `basis` (CAS), so two branches cannot both consume
//!   the same revision. [`SessionStore::finalize_append`] is the shielded close path (CC5/RP2): it
//!   appends at whatever the current revision is, in **one** lock acquisition, so a dispatch close
//!   lands in bounded steps even under continuous competing appends — it never CAS-loops, so it
//!   cannot livelock.
//!
//! The store holds `std::sync::Mutex`es and never awaits inside a critical section — every method is
//! synchronous and non-blocking (in-memory), safe to call from the async request path.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use appa_engine::branch::BranchError;
use appa_engine::fact::{Fact, FactBatch, Revision};
use appa_engine::value::TrajectoryId;

/// An authenticated caller — the trusted host / tenant a session is bound to (RP1). Sessions are
/// isolated per tenant: a request may only touch a session its caller minted.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TenantId(String);

impl TenantId {
    pub fn new(id: impl Into<String>) -> Self {
        TenantId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("unknown session {0}")]
    UnknownSession(String),
    #[error("session {session} is not owned by caller {tenant}")]
    ForeignSession { session: String, tenant: String },
    #[error("stale basis: the family is at revision {} (a concurrent branch advanced it)", current.value())]
    Stale { current: Revision },
    #[error("seeding the child failed: {0}")]
    Seed(BranchError),
}

#[derive(Debug)]
struct FamilyLog {
    facts: Vec<Fact>,
    revision: Revision,
}

#[derive(Debug)]
struct Family {
    log: Mutex<FamilyLog>,
}

impl Family {
    fn new() -> Arc<Family> {
        Arc::new(Family {
            log: Mutex::new(FamilyLog {
                facts: Vec::new(),
                revision: Revision::ZERO,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct SessionRecord {
    tenant: TenantId,
    family: Arc<Family>,
    parent: Option<TrajectoryId>,
    turn_lock: Arc<AsyncMutex<()>>,
}

#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: Mutex<BTreeMap<TrajectoryId, SessionRecord>>,
    next_id: AtomicU64,
}

impl SessionStore {
    pub fn new() -> Self {
        SessionStore::default()
    }

    fn mint(&self) -> TrajectoryId {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        TrajectoryId::new(format!("appa-s-{n}"))
    }

    pub fn create_session(&self, tenant: TenantId) -> TrajectoryId {
        let id = self.mint();
        let record = SessionRecord {
            tenant,
            family: Family::new(),
            parent: None,
            turn_lock: Arc::new(AsyncMutex::new(())),
        };
        self.sessions.lock().expect("store lock").insert(id.clone(), record);
        id
    }

    /// Fork a child of `parent` into the parent's family (RP6), **atomically** with its seed. Under
    /// the family lock this mints the child id, lets `make_seed` build the `Fork` seed batch at the
    /// current revision, appends it, and only then registers the child — so a child never exists
    /// without its seed as its first fact (no fresh-slate `Label::top()` laundering branch if seeding
    /// is rejected, loses, or is cancelled). `make_seed` runs the engine's seed derivation; a failure
    /// leaves no child. The caller must own `parent`.
    pub fn fork<F>(
        &self,
        tenant: &TenantId,
        parent: &TrajectoryId,
        make_seed: F,
    ) -> Result<(TrajectoryId, Revision), StoreError>
    where
        F: FnOnce(&TrajectoryId, &[Fact], Revision) -> Result<FactBatch, BranchError>,
    {
        let family = self.family_of(tenant, parent)?;
        let child = self.mint();
        let revision = {
            let mut log = family.log.lock().expect("family lock");
            let batch = make_seed(&child, &log.facts, log.revision).map_err(StoreError::Seed)?;
            if batch.basis != log.revision {
                return Err(StoreError::Stale { current: log.revision });
            }
            log.facts.extend(batch.facts);
            log.revision = log.revision.next();
            log.revision
        };
        self.sessions.lock().expect("store lock").insert(
            child.clone(),
            SessionRecord {
                tenant: tenant.clone(),
                family,
                parent: Some(parent.clone()),
                turn_lock: Arc::new(AsyncMutex::new(())),
            },
        );
        Ok((child, revision))
    }

    pub fn parent_of(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<Option<TrajectoryId>, StoreError> {
        let sessions = self.sessions.lock().expect("store lock");
        Ok(require_owned(&sessions, tenant, session)?.parent.clone())
    }

    /// The session's turn lease. A driver acquires it (async) for the whole turn so only one turn runs
    /// per trajectory at a time (see [`SessionRecord`]). The caller must own `session`.
    pub fn turn_lock(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<Arc<AsyncMutex<()>>, StoreError> {
        let sessions = self.sessions.lock().expect("store lock");
        Ok(require_owned(&sessions, tenant, session)?.turn_lock.clone())
    }

    pub fn snapshot(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<(Vec<Fact>, Revision), StoreError> {
        let family = self.family_of(tenant, session)?;
        let log = family.log.lock().expect("family lock");
        Ok((log.facts.clone(), log.revision))
    }

    /// Append a batch iff the family is still at `batch.basis` — the serialization point. A loser
    /// (a concurrent branch advanced the revision) gets [`StoreError::Stale`] and must re-project.
    pub fn conditional_append(
        &self,
        tenant: &TenantId,
        session: &TrajectoryId,
        batch: FactBatch,
    ) -> Result<Revision, StoreError> {
        let family = self.family_of(tenant, session)?;
        let mut log = family.log.lock().expect("family lock");
        if batch.basis != log.revision {
            return Err(StoreError::Stale { current: log.revision });
        }
        log.facts.extend(batch.facts);
        log.revision = log.revision.next();
        Ok(log.revision)
    }

    /// The shielded finalization path (CC5/RP2). Under the family lock, `decide` re-projects the log
    /// as it *now* stands and returns the close batch **only if** the work is still pending (e.g. the
    /// dispatch is still open) — returning `None` if it was already finalized. So a normal completion
    /// and a concurrent cancellation cannot both append the same close (no double-commit of effects),
    /// and because it is one lock acquisition (never a CAS-loop) it lands in bounded steps despite
    /// continuous competing appends — it cannot livelock.
    pub fn finalize<F>(&self, tenant: &TenantId, session: &TrajectoryId, decide: F) -> Result<Revision, StoreError>
    where
        F: FnOnce(&[Fact], Revision) -> Option<FactBatch>,
    {
        let family = self.family_of(tenant, session)?;
        let mut log = family.log.lock().expect("family lock");
        match decide(&log.facts, log.revision) {
            Some(batch) => {
                if batch.basis != log.revision {
                    return Err(StoreError::Stale { current: log.revision });
                }
                log.facts.extend(batch.facts);
                log.revision = log.revision.next();
                Ok(log.revision)
            }
            // Already finalized — idempotent no-op, the revision stands.
            None => Ok(log.revision),
        }
    }

    fn family_of(&self, tenant: &TenantId, session: &TrajectoryId) -> Result<Arc<Family>, StoreError> {
        let sessions = self.sessions.lock().expect("store lock");
        Ok(require_owned(&sessions, tenant, session)?.family.clone())
    }
}

fn require_owned<'a>(
    sessions: &'a BTreeMap<TrajectoryId, SessionRecord>,
    tenant: &TenantId,
    session: &TrajectoryId,
) -> Result<&'a SessionRecord, StoreError> {
    let record = sessions
        .get(session)
        .ok_or_else(|| StoreError::UnknownSession(session.as_str().to_string()))?;
    if &record.tenant != tenant {
        return Err(StoreError::ForeignSession {
            session: session.as_str().to_string(),
            tenant: tenant.as_str().to_string(),
        });
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::fact::{BoundaryKind, Fact};
    use appa_engine::label::Label;
    use appa_engine::projection::Projection;

    fn boundary(trajectory: &TrajectoryId) -> Fact {
        Fact::Boundary {
            trajectory: trajectory.clone(),
            kind: BoundaryKind::TurnEnd,
        }
    }

    fn batch(basis: Revision, facts: Vec<Fact>) -> FactBatch {
        FactBatch::new(basis, facts)
    }

    fn seed(parent: TrajectoryId) -> impl FnOnce(&TrajectoryId, &[Fact], Revision) -> Result<FactBatch, BranchError> {
        move |child, _facts, revision| {
            Ok(FactBatch::new(
                revision,
                vec![Fact::Boundary {
                    trajectory: child.clone(),
                    kind: BoundaryKind::Fork {
                        parent,
                        seed: Label::top(),
                    },
                }],
            ))
        }
    }

    #[test]
    fn append_then_replay_reconstructs_state() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let s = store.create_session(tenant.clone());

        store
            .conditional_append(&tenant, &s, batch(Revision::ZERO, vec![boundary(&s)]))
            .unwrap();
        let rev = store
            .conditional_append(&tenant, &s, batch(Revision::new(1), vec![boundary(&s)]))
            .unwrap();
        assert_eq!(rev, Revision::new(2));

        let (facts, revision) = store.snapshot(&tenant, &s).unwrap();
        let projection = Projection::build(&facts, revision);
        assert_eq!(projection.revision(), Revision::new(2));
        assert_eq!(projection.view(&s).boundary_count(), 2);
    }

    #[test]
    fn concurrent_double_consume_is_rejected() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let s = store.create_session(tenant.clone());

        store
            .conditional_append(&tenant, &s, batch(Revision::ZERO, vec![boundary(&s)]))
            .unwrap();
        let stale = store.conditional_append(&tenant, &s, batch(Revision::ZERO, vec![boundary(&s)]));
        assert_eq!(
            stale,
            Err(StoreError::Stale {
                current: Revision::new(1)
            })
        );
    }

    #[test]
    fn finalize_lands_in_bounded_steps_under_contention() {
        use std::sync::atomic::AtomicBool;
        use std::thread;

        let store = Arc::new(SessionStore::new());
        let tenant = TenantId::new("host-a");
        let s = store.create_session(tenant.clone());
        let stop = Arc::new(AtomicBool::new(false));

        let competitor = {
            let store = store.clone();
            let tenant = tenant.clone();
            let s = s.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                let mut appended = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let (_, rev) = store.snapshot(&tenant, &s).unwrap();
                    if store
                        .conditional_append(&tenant, &s, batch(rev, vec![boundary(&s)]))
                        .is_ok()
                    {
                        appended += 1;
                    }
                }
                appended
            })
        };

        let final_rev = store
            .finalize(&tenant, &s, |_facts, revision| {
                Some(batch(revision, vec![boundary(&s)]))
            })
            .unwrap();
        assert!(final_rev.value() >= 1);
        stop.store(true, Ordering::Relaxed);

        let appended = competitor.join().unwrap();
        let (_, rev) = store.snapshot(&tenant, &s).unwrap();
        assert_eq!(rev, Revision::new(appended + 1));
    }

    #[test]
    fn finalize_is_idempotent_when_the_close_is_already_in_the_log() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let s = store.create_session(tenant.clone());

        let close = |facts: &[Fact], revision: Revision| {
            let already_closed = facts.iter().any(|f| {
                matches!(
                    f,
                    Fact::Boundary {
                        kind: BoundaryKind::TurnEnd,
                        ..
                    }
                )
            });
            (!already_closed).then(|| batch(revision, vec![boundary(&s)]))
        };
        let first = store.finalize(&tenant, &s, close).unwrap();
        let second = store.finalize(&tenant, &s, close).unwrap();

        assert_eq!(first, Revision::new(1));
        assert_eq!(second, Revision::new(1)); // saw the close in the log → no-op
        assert_eq!(store.snapshot(&tenant, &s).unwrap().0.len(), 1);
    }

    #[test]
    fn foreign_session_and_parent_are_rejected() {
        let store = SessionStore::new();
        let owner = TenantId::new("host-a");
        let intruder = TenantId::new("host-b");
        let s = store.create_session(owner.clone());

        assert!(matches!(
            store.snapshot(&intruder, &s),
            Err(StoreError::ForeignSession { .. })
        ));
        assert!(matches!(
            store.fork(&intruder, &s, seed(s.clone())),
            Err(StoreError::ForeignSession { .. })
        ));
        let missing = TrajectoryId::new("appa-s-999");
        assert!(matches!(
            store.snapshot(&owner, &missing),
            Err(StoreError::UnknownSession(_))
        ));
    }

    #[test]
    fn fork_commits_the_seed_atomically_into_the_shared_family_log() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let parent = store.create_session(tenant.clone());
        let (child, revision) = store.fork(&tenant, &parent, seed(parent.clone())).unwrap();

        assert_eq!(revision, Revision::new(1));
        let (facts, rev) = store.snapshot(&tenant, &parent).unwrap();
        let projection = Projection::build(&facts, rev);
        assert_eq!(projection.view(&parent).parent_of(&child), Some(&parent));
        assert_eq!(store.parent_of(&tenant, &child).unwrap(), Some(parent));
    }

    #[test]
    fn a_rejected_seed_registers_no_child() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let parent = store.create_session(tenant.clone());

        let result = store.fork(&tenant, &parent, |_child, _facts, _rev| {
            Err(BranchError::ParentUnresolved)
        });
        assert!(matches!(result, Err(StoreError::Seed(BranchError::ParentUnresolved))));
        assert_eq!(store.snapshot(&tenant, &parent).unwrap(), (vec![], Revision::ZERO));
    }

    #[test]
    fn a_root_session_has_no_parent() {
        let store = SessionStore::new();
        let tenant = TenantId::new("host-a");
        let root = store.create_session(tenant.clone());
        assert_eq!(store.parent_of(&tenant, &root).unwrap(), None);
    }
}
