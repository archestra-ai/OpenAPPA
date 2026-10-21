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

#[derive(Default)]
pub(super) struct SpawnHolds {
    /// Every engine append wakes the held spawns: only an append moves a fork out of flight.
    appended: Notify,
    /// One turn per family, taken in arrival order, so held spawns release first come first.
    turns: std::sync::Mutex<HashMap<TrajectoryId, Arc<Semaphore>>>,
}

impl SpawnHolds {
    pub(super) fn appended(&self) {
        self.appended.notify_waiters();
    }

    fn turn(&self, root: &TrajectoryId) -> Arc<Semaphore> {
        let mut turns = self
            .turns
            .lock()
            .expect("the spawn-turn mutex is never poisoned: no panic runs while it is held");
        Arc::clone(turns.entry(root.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))))
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
        let holds = &self.inner.shared.spawn_holds;
        let turn = holds.turn(root);
        let Ok(Ok(_turn)) = tokio::time::timeout_at(until, turn.acquire()).await else {
            return Err(EventError::SpawnOutstanding);
        };
        loop {
            // Registered before the check, so an append between the check and the wait wakes it.
            let mut appended = std::pin::pin!(holds.appended.notified());
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
