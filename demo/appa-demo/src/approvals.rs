//! The playground's human-in-the-loop channel: the visitor is the authority.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::events::WireEvent;

/// How long a parked ruling waits for the visitor before abstaining. As long
/// as the runtime allows: kept just under the resolver ceiling (300 s) the
/// service configures, so the answer is always ours and never a transport
/// fault. The runtime bounds every resolver round-trip, so a truly
/// non-expiring approval would need durable rulings — out of this demo's
/// scope.
pub const DECISION_WINDOW: Duration = Duration::from_secs(290);

#[derive(Default)]
pub struct Approvals {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    events: Mutex<Vec<WireEvent>>,
}

#[derive(Debug, thiserror::Error)]
#[error("no pending approval {0:?} (it may have expired)")]
pub struct UnknownApproval(pub String);

impl Approvals {
    /// Park a ruling request: surface it to the chat and wait for the
    /// visitor's answer. `None` means the window closed without one.
    pub async fn request(&self, tool: &str, detail: serde_json::Value) -> Option<bool> {
        let id = approval_id();
        let (sender, receiver) = oneshot::channel();
        self.pending.expect_lock().insert(id.clone(), sender);
        self.events.expect_lock().push(WireEvent::ApprovalRequested {
            id: id.clone(),
            tool: tool.to_string(),
            detail,
        });

        let answer = match tokio::time::timeout(DECISION_WINDOW, receiver).await {
            Ok(Ok(approve)) => Some(approve),
            // Expired or the session dropped: no ruling either way.
            Ok(Err(_)) | Err(_) => None,
        };
        self.pending.expect_lock().remove(&id);
        self.events.expect_lock().push(WireEvent::ApprovalResolved {
            id,
            approved: answer.unwrap_or(false),
            expired: answer.is_none(),
        });
        answer
    }

    pub fn resolve(&self, id: &str, approve: bool) -> Result<(), UnknownApproval> {
        let sender = self
            .pending
            .expect_lock()
            .remove(id)
            .ok_or_else(|| UnknownApproval(id.to_string()))?;
        let _ = sender.send(approve);
        Ok(())
    }

    pub fn drain_events(&self) -> Vec<WireEvent> {
        std::mem::take(&mut *self.events.expect_lock())
    }
}

fn approval_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

trait ExpectLock<T> {
    fn expect_lock(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> ExpectLock<T> for Mutex<T> {
    fn expect_lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().expect("no panic while holding an approvals lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_click_resolves_a_parked_request() {
        let approvals = std::sync::Arc::new(Approvals::default());
        let desk = approvals.clone();
        let request = tokio::spawn(async move { desk.request("make_transfer", serde_json::json!({})).await });

        let id = loop {
            let events = approvals.drain_events();
            if let Some(WireEvent::ApprovalRequested { id, .. }) = events.into_iter().next() {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        approvals.resolve(&id, true).unwrap();
        assert_eq!(request.await.unwrap(), Some(true));
        assert!(matches!(
            approvals.drain_events().as_slice(),
            [WireEvent::ApprovalResolved {
                approved: true,
                expired: false,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn an_unknown_id_is_refused() {
        let approvals = Approvals::default();
        assert!(approvals.resolve("nope", true).is_err());
    }
}
