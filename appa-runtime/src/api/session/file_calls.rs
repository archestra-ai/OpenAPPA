use super::*;

impl Session {
    /// Execute one workspace-event projection operation on the blocking pool, acting as this
    /// trajectory. These operations can hash entire files and replay the full workspace stream.
    /// The executor also serves the harness's hooks and MCP requests. `bind`, `cancel` and
    /// `abandon` stay inline when they do not read files.
    pub(super) async fn ledger<T: Send + 'static>(
        &self,
        work: impl FnOnce(&appa_eventlog::files::FileStore, &str) -> Result<T, appa_eventlog::files::FileStoreError>
        + Send
        + 'static,
    ) -> Result<T, EventError> {
        let (inner, root, actor) = (self.inner.clone(), self.root.clone(), self.trajectory.0.clone());
        let joined = tokio::task::spawn_blocking(move || {
            let files =
                inner.shared.files.as_ref().ok_or_else(|| {
                    appa_eventlog::files::FileStoreError::Corrupt("file tools are not enabled".into())
                })?;
            let store = files.store(&inner.store, &root)?;
            work(&store, &actor)
        })
        .await
        .map_err(|error| crate::api::files::refused(format!("the file ledger task failed: {error}")))?;
        joined.map_err(crate::api::files::refused)
    }

    /// Repair the ledger reservation of a released file call after its writer has stopped.
    ///
    /// The durable write request fixes the Label of any attributable publication before the
    /// call runs. Recovery can therefore publish its file metadata without manufacturing a
    /// successful tool outcome. An incompatible physical state keeps the reservation.
    ///
    /// A ledger failure never turns a turn end into a refusal: the session would then be
    /// blocked by bookkeeping rather than by a policy decision, and the reservation it could
    /// not read stays exactly as it was.
    pub(super) async fn repair_file_reservation(&self, dispatch: &appa_engine::value::DispatchId) {
        if self.inner.shared.files.is_none() {
            return;
        }
        let key = match crate::api::files::key(dispatch) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(%error, "a file reservation key could not be rendered");
                return;
            }
        };
        let repaired = self.ledger(move |store, actor| store.repair(actor, &key)).await;
        match repaired {
            Ok(appa_eventlog::files::RepairOutcome::Absent) => {}
            Ok(appa_eventlog::files::RepairOutcome::Released) => tracing::info!(
                trajectory = %self.trajectory.0,
                "released the reservation of a file call the harness never ran"
            ),
            Ok(appa_eventlog::files::RepairOutcome::Repaired) => tracing::info!(
                trajectory = %self.trajectory.0,
                "repaired the file version of a call whose outcome was not reported"
            ),
            Ok(appa_eventlog::files::RepairOutcome::Quarantined) => tracing::warn!(
                trajectory = %self.trajectory.0,
                "a released file call left the workspace inconsistent; the reservation stands and file calls stay refused"
            ),
            Err(error) => tracing::warn!(%error, "a file reservation could not be repaired"),
        }
    }

    /// Execute only the exact released runtime-owned call. Content-dependent checks happen
    /// here, never during proposal parsing or in Claude Code's native validation.
    ///
    /// The file work runs on the blocking pool: it reads and hashes files, and a Process call
    /// may run a sandboxed command for two minutes. The runtime serves hooks and MCP on this
    /// executor, so none of that belongs on an async worker.
    #[cfg(feature = "daemon")]
    pub(in crate::api) async fn execute_file(
        &self,
        call: ProposedCall,
    ) -> Result<crate::api::files::FileReply, EventError> {
        if self.inner.shared.files.is_none() {
            return Err(crate::api::files::refused("file tools are not enabled"));
        }
        let open = self.report_calls(None)?;
        let [open] = open.as_slice() else {
            return Err(EventError::UnknownDispatch);
        };
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        if !is_open_call(&call, || policy.engine().canonical_bytes(&call), open) {
            return Err(EventError::OutcomeMismatch);
        }
        let key = crate::api::files::key(&open.id)?;
        let pin = self
            .ledger({
                let key = key.clone();
                move |store, actor| store.pin_for(actor, &key)
            })
            .await?
            .ok_or_else(|| crate::api::files::refused("the released call holds no file reservation"))?;
        let result = {
            let inner = self.inner.clone();
            let (root, call, pin) = (self.root.clone(), call.clone(), pin.clone());
            tokio::task::spawn_blocking(move || match inner.shared.files.as_ref() {
                Some(files) => files
                    .store(&inner.store, &root)
                    .map_err(|error| error.to_string())
                    .and_then(|store| crate::api::files::perform(files, store.workspace(), &call, &pin)),
                None => Err("file tools are not enabled".to_string()),
            })
            .await
            .map_err(|error| crate::api::files::refused(format!("the file operation did not complete: {error}")))?
        };
        let outcome = match &result {
            Ok(value) => ToolOutcome::Success {
                body: OutcomeBody::Available(value.clone()),
            },
            Err(message) => ToolOutcome::Failure {
                message: message.clone(),
            },
        };
        // No bytes reach the MCP caller until the engine admits the observation and the
        // ledger reconciles the mutation. A failed commit returns no file-derived detail.
        let decision = self.on_tool_result_identified(call, None, outcome).await?;
        let succeeded = result.is_ok();
        let value = match decision {
            ToolResultDecision::Keep => result.unwrap_or_else(|error| error),
            ToolResultDecision::Deliver { value } => value,
            ToolResultDecision::Replace { placeholder, .. } => {
                return Ok(crate::api::files::FileReply::Failure(placeholder));
            }
        };
        Ok(if succeeded {
            crate::api::files::FileReply::Value(value)
        } else {
            crate::api::files::FileReply::Failure(value)
        })
    }
}
