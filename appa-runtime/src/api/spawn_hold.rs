//! Single spawns held behind the family's spawn in flight.
//!
//! A single spawn binds whichever child starts next, so the engine releases one only while no
//! other waits for its child. A harness that proposes several spawns in one turn starts each
//! child as soon as its own spawn is released, so a refused sibling would only have to be
//! proposed again a moment later. The hook holds it instead, until the spawn in flight binds
//! its child or stops waiting, and then decides it again.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;

use super::{EventError, Runtime, TrajectoryId};

/// How often a held spawn reads the log without being woken: another runtime sharing the
/// store binds children without waking this one.
const RECHECK: Duration = Duration::from_secs(1);

/// The families with a spawn held, each entry kept only while a hold of it lives.
#[derive(Default)]
pub(super) struct SpawnHolds {
    families: std::sync::Mutex<HashMap<TrajectoryId, Entry>>,
}

struct Entry {
    family: Arc<FamilyHolds>,
    /// The live holds of the family, counted under the map's lock.
    holds: usize,
}

/// What one family's held spawns share.
struct FamilyHolds {
    /// Every engine append to the family wakes its held spawns: only an append moves a fork
    /// out of flight.
    appended: Notify,
    /// One turn at a time, taken in arrival order, so held spawns release first come first.
    turn: Semaphore,
}

/// One held spawn's share of its family's entry; the last one dropped removes the entry.
struct Hold<'a> {
    holds: &'a SpawnHolds,
    root: &'a TrajectoryId,
    family: Arc<FamilyHolds>,
}

impl SpawnHolds {
    fn families(&self) -> std::sync::MutexGuard<'_, HashMap<TrajectoryId, Entry>> {
        self.families
            .lock()
            .expect("the spawn-hold mutex is never poisoned: no panic runs while it is held")
    }

    /// Wake the spawns `root`'s family holds, if it holds any.
    pub(super) fn appended(&self, root: &TrajectoryId) {
        // Woken outside the lock: a waker that drops a hold takes it again.
        let family = self.families().get(root).map(|entry| Arc::clone(&entry.family));
        if let Some(family) = family {
            family.appended.notify_waiters();
        }
    }

    fn hold<'a>(&'a self, root: &'a TrajectoryId) -> Hold<'a> {
        let mut families = self.families();
        let entry = families.entry(root.clone()).or_insert_with(|| Entry {
            family: Arc::new(FamilyHolds {
                appended: Notify::new(),
                turn: Semaphore::new(1),
            }),
            holds: 0,
        });
        entry.holds += 1;
        Hold {
            holds: self,
            root,
            family: Arc::clone(&entry.family),
        }
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mut families = self.holds.families();
        let last = families.get_mut(self.root).is_some_and(|entry| {
            entry.holds -= 1;
            entry.holds == 0
        });
        if last {
            families.remove(self.root);
        }
    }
}

impl Runtime {
    /// Decide a single spawn the engine refused as outstanding again, each time no single
    /// spawn of the family waits for its child, until it is not refused as outstanding. The
    /// last check runs at `until`; a spawn still waiting then is refused as before.
    pub(crate) async fn hold_spawn<T, Run>(
        &self,
        root: &TrajectoryId,
        until: Instant,
        propose: impl Fn() -> Run,
    ) -> Result<T, EventError>
    where
        Run: Future<Output = Result<T, EventError>>,
    {
        let hold = self.inner.shared.spawn_holds.hold(root);
        let Ok(Ok(_turn)) = tokio::time::timeout_at(until, hold.family.turn.acquire()).await else {
            return Err(EventError::SpawnOutstanding);
        };
        loop {
            // Registered before the check, so an append between the check and the wait wakes it.
            let mut appended = std::pin::pin!(hold.family.appended.notified());
            appended.as_mut().enable();
            if !self.spawn_in_flight(root)? {
                match propose().await {
                    Err(EventError::SpawnOutstanding) => {}
                    decided => return decided,
                }
            }
            if Instant::now() >= until {
                return Err(EventError::SpawnOutstanding);
            }
            tracing::debug!(root = %root.0, "spawn held: an earlier spawn has not bound its child");
            let _ = tokio::time::timeout_at(until.min(Instant::now() + RECHECK), appended).await;
        }
    }

    fn spawn_in_flight(&self, root: &TrajectoryId) -> Result<bool, EventError> {
        let log = self.inner.log(root)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log)?;
        Ok(!policy.engine().forks_in_flight(&view).is_empty())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{SpawnHolds, TrajectoryId};

    fn root(name: &str) -> TrajectoryId {
        TrajectoryId(name.to_string())
    }

    #[test]
    fn a_family_s_entry_lives_only_while_a_hold_of_it_does() {
        let holds = SpawnHolds::default();
        let family = root("family");
        let first = holds.hold(&family);
        let second = holds.hold(&family);
        let turn = first.family.turn.try_acquire().expect("the family's turn is free");
        assert!(second.family.turn.try_acquire().is_err(), "both holds take one turn");
        drop(turn);
        // A share that outlives its hold, as one being dropped on another thread does.
        let _lingering = std::sync::Arc::clone(&first.family);
        drop(first);
        assert!(
            holds.families().contains_key(&family),
            "the second hold keeps the entry"
        );
        drop(second);
        assert!(holds.families().is_empty());
    }

    #[tokio::test]
    async fn an_append_wakes_only_its_own_family_s_held_spawns() {
        let holds = SpawnHolds::default();
        let (family, other) = (root("family"), root("other"));
        let hold = holds.hold(&family);
        let _other_hold = holds.hold(&other);
        let mut appended = std::pin::pin!(hold.family.appended.notified());
        appended.as_mut().enable();

        holds.appended(&other);
        assert!(
            tokio::time::timeout(Duration::ZERO, appended.as_mut()).await.is_err(),
            "another family's append leaves the hold asleep"
        );
        holds.appended(&family);
        assert!(tokio::time::timeout(Duration::ZERO, appended).await.is_ok());
    }
}
