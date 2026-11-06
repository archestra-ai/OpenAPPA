//! Recorder delivery: the dispatch records of every call whose declaration names a recorder,
//! pushed to that recorder after the batch that holds them is appended.
//!
//! A recorder is never consulted. The runtime never waits for one, and a recorder can neither
//! deny nor delay a call: delivery runs on one task per family after the append, never on a
//! consult's process permits, and a failed delivery is retried on that task, with backoff, a
//! bounded number of times, and is otherwise left for the family's next append. Each recorder reads each family in log order, at least
//! once, from a cursor this process keeps per family and binding: a recorder rebound to
//! another endpoint or command reads the family from its start. A runtime that restarts
//! begins every cursor at the start of the log, so a recorder deduplicates by dispatch key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use appa_runtime_api::TrajectoryId;

use super::Inner;

/// How far one recorder has read one family: the count of facts it has been delivered.
type Cursor = Arc<tokio::sync::Mutex<usize>>;

/// The waits before each delivery attempt: the first at once, the last about two minutes later.
const ATTEMPTS: [std::time::Duration; 8] = {
    use std::time::Duration;
    [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(16),
        Duration::from_secs(32),
        Duration::from_secs(64),
    ]
};

/// Every recorder binding's cursor into every family, and the families a delivery task is
/// running for. A binding is its name and how it is reached — the transport's description,
/// which names its endpoint or argv and never a credential's value.
#[derive(Default)]
pub(super) struct Cursors {
    cursors: Mutex<HashMap<(TrajectoryId, String, String), Cursor>>,
    /// A family with a delivery task, and whether it appended since that task last read it.
    running: Mutex<HashMap<TrajectoryId, bool>>,
}

impl Cursors {
    fn of(&self, root: &TrajectoryId, recorder: &str, transport: &crate::config::Transport) -> Cursor {
        let mut cursors = self
            .cursors
            .lock()
            .expect("the recorder cursors mutex is never poisoned: no panic runs while it is held");
        let key = (root.clone(), recorder.to_string(), format!("{transport:?}"));
        Arc::clone(cursors.entry(key).or_default())
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<TrajectoryId, bool>> {
        self.running
            .lock()
            .expect("the recorder tasks mutex is never poisoned: no panic runs while it is held")
    }

    /// Whether `root` needs a task of its own; a running one is told to read the log again.
    fn start(&self, root: &TrajectoryId) -> bool {
        let mut running = self.running();
        match running.get_mut(root) {
            Some(appended) => {
                *appended = true;
                false
            }
            None => {
                running.insert(root.clone(), false);
                true
            }
        }
    }

    /// Whether `root`'s task may end: nothing was appended since it last read the log.
    fn finish(&self, root: &TrajectoryId) -> bool {
        let mut running = self.running();
        match running.get_mut(root) {
            Some(appended) if *appended => {
                *appended = false;
                false
            }
            _ => {
                running.remove(root);
                true
            }
        }
    }
}

impl Inner {
    /// Deliver `root`'s undelivered records to every recorder its deployment binds, on a task
    /// of its own. Without an async runtime to run on, nothing is delivered now and the next
    /// append delivers it.
    pub(super) fn deliver_records(self: &Arc<Self>, root: &TrajectoryId) {
        if self.deployment().config.externals.recorders.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // One task per family at a time: an append while it runs makes it read the log again,
        // so a stalled recorder holds one task, not one per append.
        if !self.shared.recorder_cursors.start(root) {
            return;
        }
        let (inner, root) = (Arc::clone(self), root.clone());
        handle.spawn(async move {
            loop {
                let mut delivered = false;
                for wait in ATTEMPTS {
                    tokio::time::sleep(wait).await;
                    if inner.deliver_family(&root).await {
                        delivered = true;
                        break;
                    }
                }
                if !delivered {
                    tracing::warn!(root = %root.0, "recorder delivery gave up; the family's next append retries it");
                }
                if inner.shared.recorder_cursors.finish(&root) {
                    return;
                }
            }
        });
    }

    /// Deliver what each recorder has not read of `root`; whether every delivery succeeded.
    async fn deliver_family(&self, root: &TrajectoryId) -> bool {
        let deployment = self.deployment();
        let mut delivered_all = true;
        for (recorder, transport) in &deployment.config.externals.recorders {
            let cursor = self.shared.recorder_cursors.of(root, recorder, transport);
            let mut delivered = cursor.lock().await;
            // Read under the cursor's lock, so two deliveries to one recorder never interleave
            // and each reads the log as far as it has grown.
            let Ok(log) = self.log(root) else { return false };
            let Ok(policy) = self.resolve_policy(&deployment, &log) else {
                return false;
            };
            let Ok(view) = policy.rebuild_view(&log) else {
                return false;
            };
            let facts = log.facts();
            let mut records = policy
                .engine()
                .recorder_records(&view, &facts[(*delivered).min(facts.len())..]);
            let Some(records) = records.remove(recorder.as_str()) else {
                *delivered = facts.len();
                continue;
            };
            let delivery = serde_json::json!({
                "version": 1,
                "kind": "recorder",
                "name": recorder,
                "root": root.as_str(),
                "records": records,
            });
            match deployment.externals.deliver(recorder, &delivery).await {
                Ok(()) => *delivered = facts.len(),
                Err(reason) => {
                    delivered_all = false;
                    tracing::warn!(
                        recorder,
                        root = %root.0,
                        reason = reason.diagnostic(),
                        "a recorder delivery failed; the records stay for the next attempt"
                    );
                }
            }
        }
        delivered_all
    }
}
