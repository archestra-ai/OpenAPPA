//! Recorder delivery: the dispatch records of every call whose declaration names a recorder,
//! pushed to that recorder after the batch that holds them is appended.
//!
//! A recorder is never consulted. The runtime never waits for one, and a recorder can neither
//! deny nor delay a call: delivery runs on its own task after the append, and a failed
//! delivery leaves the records for the next one. Each recorder reads each family in log
//! order, at least once, from a cursor this process keeps per family and recorder. A runtime
//! that restarts begins every cursor at the start of the log, so a recorder deduplicates by
//! dispatch key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use appa_runtime_api::TrajectoryId;

use super::Inner;

/// How far one recorder has read one family: the count of facts it has been delivered.
type Cursor = Arc<tokio::sync::Mutex<usize>>;

/// Every recorder's cursor into every family.
#[derive(Default)]
pub(super) struct Cursors(Mutex<HashMap<(TrajectoryId, String), Cursor>>);

impl Cursors {
    fn of(&self, root: &TrajectoryId, recorder: &str) -> Cursor {
        let mut cursors = self
            .0
            .lock()
            .expect("the recorder cursors mutex is never poisoned: no panic runs while it is held");
        Arc::clone(cursors.entry((root.clone(), recorder.to_string())).or_default())
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
        let (inner, root) = (Arc::clone(self), root.clone());
        handle.spawn(async move { inner.deliver_family(&root).await });
    }

    async fn deliver_family(&self, root: &TrajectoryId) {
        let deployment = self.deployment();
        for recorder in deployment.config.externals.recorders.keys() {
            let cursor = self.shared.recorder_cursors.of(root, recorder);
            let mut delivered = cursor.lock().await;
            // Read under the cursor's lock, so two deliveries to one recorder never interleave
            // and each reads the log as far as it has grown.
            let Ok(log) = self.log(root) else { return };
            let Ok(policy) = self.resolve_policy(&deployment, &log) else {
                return;
            };
            let Ok(view) = policy.rebuild_view(&log) else { return };
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
                Err(reason) => tracing::warn!(
                    recorder,
                    root = %root.0,
                    reason = reason.diagnostic(),
                    "a recorder delivery failed; the records stay for the next delivery"
                ),
            }
        }
    }
}
