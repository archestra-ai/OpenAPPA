//! Embedded peer messages inside one family. The host names identities and a
//! released call. The runtime snapshots the sender's label, and a later read
//! binds that stored label. The host never supplies a label.
//!
//! A direct release is valid only when the recipient's live label already covers
//! the snapshot (`current.combine(message) == current`). `combine` is the meet and
//! never widens. A live fold only narrows, so a later label still covers a message
//! the earlier label covered. The recheck on a direct retry is for a reset this
//! core does not have; it is not a lock against narrowing.
//!
//! The 24-hour TTL bounds unread inbox bodies. A completed read receipt stays so
//! the same call can replay. The TTL does not erase trajectory-log data.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use appa_engine::fact::Fact;
use appa_engine::value::{FileBasis, FileSource, Provenance};
use appa_eventlog::Log;
use appa_eventlog::embedded::{DirectTake, EmbeddedClaim, EmbeddedError, EmbeddedRow, EmbeddedStatus, ReadTake};
use appa_runtime_api::{
    Actor, HookDecision, OutcomeBody, PeerDigest, PeerValueError, ProposedCall, SpawnBinding, ToolOutcome, TrajectoryId,
};

use super::{EmbeddedHookOutcome, EventError, RemedyPresentation, Runtime, ToolCallDecision, ToolResultDecision};

const CALL_ID_LIMIT: usize = 1024;

/// The in-flight read whose log appends must still own this ticket.
#[derive(Clone)]
pub(crate) struct EmbeddedReadHold {
    pub root: String,
    pub id: String,
    pub call_id: String,
    pub generation: i64,
}

tokio::task_local! {
    pub(crate) static EMBEDDED_READ_HOLD: EmbeddedReadHold;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddedPeerId(String);

impl EmbeddedPeerId {
    pub fn parse(text: &str) -> Result<Self, EmbeddedPeerError> {
        uuid_text(text)
            .map(Self)
            .map_err(|_| EmbeddedPeerError::Refused(format!("{text:?} is not a peer message id")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Binding metadata for the embedding host. Before admission, show the model only
/// the opaque id and expiry. Keep content digests and routing identities host-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPeerNotice {
    pub id: EmbeddedPeerId,
    pub sender: TrajectoryId,
    pub recipient: TrajectoryId,
    pub digest: PeerDigest,
    pub expires: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedPeerArrival {
    Direct { id: EmbeddedPeerId, body: String },
    Held(EmbeddedPeerNotice),
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedPeerError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("{0}")]
    Refused(String),
}

impl Runtime {
    pub fn send_embedded_peer(
        &self,
        family: &TrajectoryId,
        sender: &TrajectoryId,
        recipient: &TrajectoryId,
        pending_spawn: Option<&str>,
        call_id: &str,
        body: &str,
    ) -> Result<EmbeddedPeerNotice, EmbeddedPeerError> {
        if body.is_empty() {
            return Err(refused("a peer message body is empty"));
        }
        let call_id = bounded(call_id, "call id")?;
        let pending_spawn = pending_spawn.map(|spawn| bounded(spawn, "pending spawn")).transpose()?;
        self.live(family, sender).map_err(peer_error)?;
        match self.live(family, recipient) {
            Ok(()) if pending_spawn.is_some() => {
                return Err(refused("a started recipient does not take a pending spawn"));
            }
            Ok(()) => {}
            Err(EventError::UnknownTrajectory) if pending_spawn.is_none() => {
                return Err(refused("an unstarted recipient needs its pending spawn identity"));
            }
            Err(EventError::UnknownTrajectory) => {
                let log = self.inner.log(family).map_err(peer_error)?;
                let binding = SpawnBinding(pending_spawn.clone().unwrap_or_default());
                if crate::engine::parse_fork(&log, &binding).is_none() {
                    return Err(refused("the pending spawn is not a prepared fork of this family"));
                }
            }
            Err(error) => return Err(peer_error(error)),
        }
        let log = self.inner.log(family).map_err(peer_error)?;
        let dispatch = log
            .call_bindings()
            .find(|binding| binding.trajectory == sender && binding.call_id == call_id)
            .map(|binding| binding.dispatch.clone())
            .ok_or_else(|| refused("the send call is not a released dispatch of this sender"))?;
        let label = self.current_label(&log, sender).map_err(peer_error)?;
        let dispatch =
            serde_json::to_string(&dispatch).map_err(|error| EmbeddedPeerError::Storage(error.to_string()))?;
        let claimed = self.inner.store.claim_embedded_peer(
            family,
            sender,
            recipient,
            pending_spawn.as_deref(),
            &dispatch,
            &PeerDigest::of_body(body),
            &label,
            body,
            SystemTime::now(),
        );
        match claimed.map_err(store_error)? {
            EmbeddedClaim::Conflict => Err(refused("this send call already recorded a different message")),
            EmbeddedClaim::Quota => Err(refused("the recipient's unread peer inbox is full")),
            EmbeddedClaim::Stored(row) => notice(&row),
        }
    }

    pub fn receive_embedded_peer(
        &self,
        family: &TrajectoryId,
        recipient: &TrajectoryId,
        id: &EmbeddedPeerId,
        sender: &TrajectoryId,
        digest: &PeerDigest,
    ) -> Result<EmbeddedPeerArrival, EmbeddedPeerError> {
        self.inner
            .store
            .expire_embedded_inbox(family, SystemTime::now())
            .map_err(store_error)?;
        let row = self
            .inner
            .store
            .load_embedded_peer(family, id.as_str())
            .map_err(store_error)?
            .ok_or_else(|| refused("no peer message with this id is held for this family"))?;
        if row.sender != sender.as_str() || row.recipient != recipient.as_str() || row.digest != digest.to_string() {
            return Err(refused(
                "the peer message proof does not match this sender, recipient, and digest",
            ));
        }
        if row.status == EmbeddedStatus::Read {
            return Err(refused("this peer message was already read"));
        }
        if row.status == EmbeddedStatus::Direct {
            self.live(family, recipient).map_err(peer_error)?;
            let log = self.inner.log(family).map_err(peer_error)?;
            let current = self.current_label(&log, recipient).map_err(peer_error)?;
            if current.combine(&row.label) != current {
                return Err(refused("the recipient no longer covers this message"));
            }
            return direct_body(&row);
        }
        if !fresh(&row) {
            return Err(refused("this peer message has expired"));
        }
        match self.live(family, recipient) {
            Err(EventError::UnknownTrajectory) => return Ok(EmbeddedPeerArrival::Held(notice(&row)?)),
            Err(error) => return Err(peer_error(error)),
            Ok(()) => {}
        }
        let log = self.inner.log(family).map_err(peer_error)?;
        let current = self.current_label(&log, recipient).map_err(peer_error)?;
        if current.combine(&row.label) != current {
            return Ok(EmbeddedPeerArrival::Held(notice(&row)?));
        }
        match self
            .inner
            .store
            .take_embedded_direct(family, id.as_str(), sender, recipient, digest)
            .map_err(store_error)?
        {
            DirectTake::Taken(row) | DirectTake::Already(row) => direct_body(&row),
            DirectTake::Busy => Err(refused("this peer message was already read")),
            DirectTake::Missing => Err(refused(
                "the peer message proof does not match this sender, recipient, and digest",
            )),
        }
    }

    pub fn list_embedded_peer(
        &self,
        family: &TrajectoryId,
        recipient: &TrajectoryId,
    ) -> Result<Vec<EmbeddedPeerNotice>, EmbeddedPeerError> {
        self.inner
            .store
            .list_embedded_peer(family, recipient, SystemTime::now())
            .map_err(store_error)?
            .iter()
            .map(notice)
            .collect()
    }

    pub async fn read_embedded_peer(
        &self,
        actor: &Actor,
        call_id: &str,
        id: &EmbeddedPeerId,
        call: ProposedCall,
    ) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
        let call_id = bounded(call_id, "call id")?;
        let recipient = actor.child.as_ref().unwrap_or(&actor.root);
        named_id(&call, id)?;
        self.read_tool_preserves_source_label(&actor.root, &call)?;
        let arguments = call_fingerprint(&call);
        let loaded = self
            .inner
            .store
            .load_embedded_peer(&actor.root, id.as_str())
            .map_err(store_error)?
            .ok_or_else(|| refused("no peer message with this id is held for this family"))?;
        if loaded.recipient != recipient.as_str() {
            return Err(refused("no peer message with this id is held for this recipient"));
        }
        let basis = message_basis(&loaded);
        let log = self.inner.log(&actor.root).map_err(peer_error)?;
        if matches!(prior_call(&log, recipient, &call_id, &basis), PriorCall::Foreign) {
            return Err(refused("this call id is already a different dispatch"));
        }
        let claim = self
            .inner
            .store
            .claim_embedded_read(&actor.root, id.as_str(), recipient, &call_id, &arguments)
            .map_err(store_error)?;
        let claim = match claim {
            ReadTake::Done(row) => return stored_outcome(&row),
            ReadTake::Ready(claim) => claim,
            ReadTake::Busy => return Err(refused("this call id is not waiting to read this message")),
            ReadTake::Missing => return Err(refused("no peer message with this id is held for this recipient")),
        };
        let generation = claim.generation;
        if !fresh(&claim.row) || claim.row.body.is_none() {
            self.release_if_unopened(&actor.root, recipient, &call_id, id, &basis, generation)?;
            return Err(refused("this peer message has expired"));
        }
        let outcome = match self.admit_read(actor, recipient, &call_id, &call, &claim, &basis).await {
            Ok(outcome) => outcome,
            Err(error) => {
                self.release_if_unopened(&actor.root, recipient, &call_id, id, &basis, generation)
                    .ok();
                return Err(error);
            }
        };
        if matches!(
            outcome.decision,
            HookDecision::DenyCall { .. } | HookDecision::Block { .. }
        ) {
            self.release_if_unopened(&actor.root, recipient, &call_id, id, &basis, generation)?;
            return Ok(outcome);
        }
        self.store_decision(&actor.root, id, &call_id, generation, &outcome, &claim.row)?;
        Ok(outcome)
    }

    fn release_if_unopened(
        &self,
        root: &TrajectoryId,
        recipient: &TrajectoryId,
        call_id: &str,
        id: &EmbeddedPeerId,
        basis: &FileBasis,
        generation: i64,
    ) -> Result<(), EmbeddedPeerError> {
        let log = self.inner.log(root).map_err(peer_error)?;
        if matches!(prior_call(&log, recipient, call_id, basis), PriorCall::Absent) {
            self.inner
                .store
                .release_embedded_read(root, id.as_str(), call_id, generation)
                .map_err(store_error)?;
        }
        Ok(())
    }

    fn store_decision(
        &self,
        root: &TrajectoryId,
        id: &EmbeddedPeerId,
        call_id: &str,
        generation: i64,
        outcome: &EmbeddedHookOutcome,
        row: &EmbeddedRow,
    ) -> Result<(), EmbeddedPeerError> {
        let encoded = serde_json::to_string(&StoredOutcome::try_from(&outcome.decision)?)
            .map_err(|error| EmbeddedPeerError::Storage(error.to_string()))?;
        let stored = self
            .inner
            .store
            .finish_embedded_read(root, id.as_str(), call_id, generation, &encoded)
            .map_err(store_error)?;
        if stored {
            return Ok(());
        }
        let reloaded = self
            .inner
            .store
            .load_embedded_peer(root, id.as_str())
            .map_err(store_error)?;
        if reloaded
            .as_ref()
            .and_then(|row| stored_outcome(row).ok())
            .is_some_and(|done| {
                matches!(
                    (&done.decision, &outcome.decision),
                    (HookDecision::DeliverValue { value: stored }, HookDecision::DeliverValue { value })
                        if stored == value && value == row.body.as_deref().unwrap_or("")
                )
            })
        {
            return Ok(());
        }
        let _ = row;
        Err(refused("this read no longer owns its claim"))
    }

    async fn admit_read(
        &self,
        actor: &Actor,
        recipient: &TrajectoryId,
        call_id: &str,
        call: &ProposedCall,
        claim: &appa_eventlog::embedded::ReadClaim,
        basis: &FileBasis,
    ) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
        let hold = EmbeddedReadHold {
            root: actor.root.as_str().to_string(),
            id: claim.row.id.clone(),
            call_id: call_id.to_string(),
            generation: claim.generation,
        };
        EMBEDDED_READ_HOLD
            .scope(
                hold,
                self.admit_read_held(actor, recipient, call_id, call, &claim.row, basis),
            )
            .await
    }

    async fn admit_read_held(
        &self,
        actor: &Actor,
        recipient: &TrajectoryId,
        call_id: &str,
        call: &ProposedCall,
        row: &EmbeddedRow,
        basis: &FileBasis,
    ) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
        let log = self.inner.log(&actor.root).map_err(peer_error)?;
        match prior_call(&log, recipient, call_id, basis) {
            PriorCall::Foreign => return Err(refused("this call id is already a different dispatch")),
            PriorCall::Admitted(body) => return governed_delivery(row, &body),
            PriorCall::Open | PriorCall::Absent => {}
        }
        let session = self.session(&actor.root, recipient).map_err(peer_error)?;
        let open = matches!(
            prior_call(
                &self.inner.log(&actor.root).map_err(peer_error)?,
                recipient,
                call_id,
                basis
            ),
            PriorCall::Open
        );
        if !open {
            match session
                .propose_with_basis(call.clone(), Some(call_id.to_string()), basis.clone())
                .await
                .map_err(peer_error)?
            {
                ToolCallDecision::Deny {
                    feedback,
                    offers,
                    review,
                    ..
                } => {
                    return Ok(EmbeddedHookOutcome {
                        decision: HookDecision::DenyCall {
                            feedback: feedback.clone(),
                            offers: offers.clone(),
                            review: review.clone(),
                        },
                        presentation: Some(RemedyPresentation {
                            feedback,
                            offers,
                            review,
                            display: Vec::new(),
                        }),
                    });
                }
                ToolCallDecision::Allow { .. } => {}
            }
        }
        let body = row
            .body
            .clone()
            .ok_or_else(|| refused("this peer message has expired"))?;
        let decided = session
            .on_tool_result_identified(
                call.clone(),
                Some(call_id.to_string()),
                ToolOutcome::Success {
                    body: OutcomeBody::Available(body.clone()),
                },
            )
            .await
            .map_err(peer_error)?;
        let proved = prior_call(
            &self.inner.log(&actor.root).map_err(peer_error)?,
            recipient,
            call_id,
            basis,
        );
        let outcome = outcome_of(decided, proved)?;
        if let HookDecision::DeliverValue { value } = &outcome.decision
            && value != &body
        {
            return Err(refused("the admitted read is not this stored message"));
        }
        Ok(outcome)
    }

    #[cfg(test)]
    pub async fn testing_open_embedded_read(
        &self,
        actor: &Actor,
        call_id: &str,
        id: &EmbeddedPeerId,
        call: ProposedCall,
    ) -> Result<(), EmbeddedPeerError> {
        let call_id = bounded(call_id, "call id")?;
        let recipient = actor.child.as_ref().unwrap_or(&actor.root);
        named_id(&call, id)?;
        self.read_tool_preserves_source_label(&actor.root, &call)?;
        let arguments = call_fingerprint(&call);
        let loaded = self
            .inner
            .store
            .load_embedded_peer(&actor.root, id.as_str())
            .map_err(store_error)?
            .ok_or_else(|| refused("no peer message with this id is held for this family"))?;
        let basis = message_basis(&loaded);
        let claim = self
            .inner
            .store
            .claim_embedded_read(&actor.root, id.as_str(), recipient, &call_id, &arguments)
            .map_err(store_error)?;
        let ReadTake::Ready(claim) = claim else {
            return Err(refused("the test read did not claim"));
        };
        let hold = EmbeddedReadHold {
            root: actor.root.as_str().to_string(),
            id: id.as_str().to_string(),
            call_id: call_id.clone(),
            generation: claim.generation,
        };
        EMBEDDED_READ_HOLD
            .scope(hold, async {
                let session = self.session(&actor.root, recipient).map_err(peer_error)?;
                session
                    .propose_with_basis(call, Some(call_id), basis)
                    .await
                    .map_err(peer_error)?;
                Ok(())
            })
            .await
    }

    fn read_tool_preserves_source_label(
        &self,
        family: &TrajectoryId,
        call: &ProposedCall,
    ) -> Result<(), EmbeddedPeerError> {
        let log = self.inner.log(family).map_err(peer_error)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log).map_err(peer_error)?;
        let registry = policy.engine().registry();
        let name = appa_engine::value::ToolName::new(&call.tool);
        if registry.classify(&name) != Some(appa_engine::registry::ToolKind::Declared) {
            return Err(refused("a peer read needs an exact declared tool with an empty delta"));
        }
        let variants: Vec<_> = registry.variants(&name).collect();
        if variants.is_empty() || variants.iter().any(|declaration| declaration.declared().is_none()) {
            return Err(refused("a peer read cannot use an annotator"));
        }
        if variants.iter().any(|declaration| {
            declaration
                .declared()
                .is_none_or(|annotation| !annotation.delta.is_none())
        }) {
            return Err(refused("a peer read tool must declare an empty delta"));
        }
        if registry.sanitizers().any(|sanitizer| {
            sanitizer.on.output
                && variants
                    .iter()
                    .any(|declaration| sanitizer.scope.covers(declaration.tags()))
        }) {
            return Err(refused("a peer read tool must not be in an output sanitizer's scope"));
        }
        Ok(())
    }
}

fn governed_delivery(row: &EmbeddedRow, admitted: &str) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
    if row.body.as_deref() != Some(admitted) {
        return Err(refused("the admitted read is not this stored message"));
    }
    Ok(deliver(admitted.to_string()))
}

fn outcome_of(decision: ToolResultDecision, proved: PriorCall) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
    let proved = match proved {
        PriorCall::Admitted(body) => Some(body),
        PriorCall::Open | PriorCall::Absent | PriorCall::Foreign => None,
    };
    match decision {
        ToolResultDecision::Deliver { value } => Ok(deliver(proved.unwrap_or(value))),
        ToolResultDecision::Replace {
            placeholder,
            presentation,
        } => Ok(EmbeddedHookOutcome {
            decision: HookDecision::ReplaceOutput { output: placeholder },
            presentation,
        }),
        ToolResultDecision::Keep => {
            let body = proved.ok_or_else(|| refused("the read was acknowledged without an admitted body"))?;
            Ok(deliver(body))
        }
    }
}

fn deliver(body: String) -> EmbeddedHookOutcome {
    EmbeddedHookOutcome {
        decision: HookDecision::DeliverValue { value: body },
        presentation: None,
    }
}

pub(crate) fn message_basis(row: &EmbeddedRow) -> FileBasis {
    FileBasis::Read(FileSource {
        version: row.id.clone(),
        digest: row.digest.clone(),
        label: row.label.clone(),
    })
}

enum PriorCall {
    Absent,
    Foreign,
    Open,
    Admitted(String),
}

pub(crate) fn basis_of(
    log: &Log,
    trajectory: &TrajectoryId,
    dispatch: &appa_engine::value::DispatchId,
) -> Option<FileBasis> {
    let mut basis = None;
    let mut closed = false;
    for fact in log.facts() {
        match fact {
            Fact::DispatchOpened {
                trajectory: opened,
                dispatch: id,
                file_basis,
                ..
            } if opened == trajectory && id == dispatch => {
                basis = file_basis.clone();
                closed = false;
            }
            Fact::DispatchClosed { dispatch: id, .. } if id == dispatch => closed = true,
            _ => {}
        }
    }
    (!closed).then_some(basis).flatten()
}

fn prior_call(log: &Log, recipient: &TrajectoryId, call_id: &str, basis: &FileBasis) -> PriorCall {
    let Some(dispatch) = log
        .call_bindings()
        .find(|binding| binding.trajectory == recipient && binding.call_id == call_id)
        .map(|binding| binding.dispatch.clone())
    else {
        return PriorCall::Absent;
    };
    let mut opened = false;
    let mut admitted = None;
    for fact in log.facts() {
        match fact {
            Fact::DispatchOpened {
                trajectory,
                dispatch: id,
                file_basis,
                ..
            } if trajectory == recipient && id == &dispatch => {
                if file_basis.as_ref() != Some(basis) {
                    return PriorCall::Foreign;
                }
                opened = true;
            }
            Fact::ValueAdmitted {
                trajectory,
                value,
                provenance: Provenance::ToolResult { dispatch: id },
            } if trajectory == recipient && id == &dispatch => {
                admitted = Some(value.body.as_str().to_string());
            }
            _ => {}
        }
    }
    if !opened {
        return PriorCall::Foreign;
    }
    match admitted {
        Some(body) => PriorCall::Admitted(body),
        None => PriorCall::Open,
    }
}

fn call_fingerprint(call: &ProposedCall) -> String {
    format!("{}\n{}", call.tool, call.arguments.get())
}

fn notice(row: &EmbeddedRow) -> Result<EmbeddedPeerNotice, EmbeddedPeerError> {
    Ok(EmbeddedPeerNotice {
        id: EmbeddedPeerId::parse(&row.id)?,
        sender: TrajectoryId(row.sender.clone()),
        recipient: TrajectoryId(row.recipient.clone()),
        digest: PeerDigest::parse(&row.digest).map_err(digest_error)?,
        expires: UNIX_EPOCH + Duration::from_millis(u64::try_from(row.expires_at).unwrap_or(0)),
    })
}

fn direct_body(row: &EmbeddedRow) -> Result<EmbeddedPeerArrival, EmbeddedPeerError> {
    let body = row
        .body
        .clone()
        .filter(|_| fresh(row))
        .ok_or_else(|| refused("this peer message has expired"))?;
    Ok(EmbeddedPeerArrival::Direct {
        id: EmbeddedPeerId::parse(&row.id)?,
        body,
    })
}

fn fresh(row: &EmbeddedRow) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    row.expires_at > now
}

fn named_id(call: &ProposedCall, id: &EmbeddedPeerId) -> Result<(), EmbeddedPeerError> {
    let arguments: serde_json::Value =
        serde_json::from_str(call.arguments.get()).map_err(|_| refused("a peer read's arguments are not an object"))?;
    let object = arguments
        .as_object()
        .ok_or_else(|| refused("a peer read's arguments are not an object"))?;
    if object.contains_key("label") {
        return Err(refused("a peer read cannot carry a label"));
    }
    let named = object
        .get("message_id")
        .or_else(|| object.get("id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| refused("a peer read must name its message id"))?;
    if named != id.as_str() {
        return Err(refused("the peer read names a different message"));
    }
    Ok(())
}

fn stored_outcome(row: &EmbeddedRow) -> Result<EmbeddedHookOutcome, EmbeddedPeerError> {
    let raw = row
        .decision
        .as_deref()
        .ok_or_else(|| refused("this peer read has no stored decision"))?;
    let stored: StoredOutcome =
        serde_json::from_str(raw).map_err(|error| EmbeddedPeerError::Storage(error.to_string()))?;
    Ok(EmbeddedHookOutcome {
        decision: stored.into(),
        presentation: None,
    })
}

#[derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredOutcome {
    Deliver { value: String },
    Replace { output: String },
}

impl TryFrom<&HookDecision> for StoredOutcome {
    type Error = EmbeddedPeerError;

    fn try_from(decision: &HookDecision) -> Result<Self, Self::Error> {
        match decision {
            HookDecision::DeliverValue { value } => Ok(Self::Deliver { value: value.clone() }),
            HookDecision::ReplaceOutput { output } => Ok(Self::Replace { output: output.clone() }),
            HookDecision::Ack => Err(refused("the read was acknowledged without an admitted body")),
            HookDecision::AllowCall { .. }
            | HookDecision::PassControl
            | HookDecision::Context { .. }
            | HookDecision::ChildReturn { .. }
            | HookDecision::DenyCall { .. }
            | HookDecision::Block { .. }
            | HookDecision::Refuse { .. } => Err(refused("this read did not admit the stored message")),
        }
    }
}

impl From<StoredOutcome> for HookDecision {
    fn from(stored: StoredOutcome) -> Self {
        match stored {
            StoredOutcome::Deliver { value } => HookDecision::DeliverValue { value },
            StoredOutcome::Replace { output } => HookDecision::ReplaceOutput { output },
        }
    }
}

fn bounded(text: &str, what: &str) -> Result<String, EmbeddedPeerError> {
    if text.is_empty() || text.len() > CALL_ID_LIMIT || text.chars().any(char::is_control) {
        return Err(refused(format!(
            "the {what} is empty, too long, or contains a control character"
        )));
    }
    Ok(text.to_string())
}

fn uuid_text(text: &str) -> Result<String, ()> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return Err(());
    }
    let widths = [8, 4, 4, 4, 12];
    let mut index = 0;
    for (group, width) in widths.into_iter().enumerate() {
        if group > 0 {
            if bytes.get(index) != Some(&b'-') {
                return Err(());
            }
            index += 1;
        }
        for _ in 0..width {
            if !bytes.get(index).is_some_and(u8::is_ascii_hexdigit) {
                return Err(());
            }
            index += 1;
        }
    }
    Ok(text.to_ascii_lowercase())
}

fn refused(detail: impl Into<String>) -> EmbeddedPeerError {
    EmbeddedPeerError::Refused(detail.into())
}

fn store_error(error: EmbeddedError) -> EmbeddedPeerError {
    match error {
        EmbeddedError::TooLarge => refused("a peer message exceeds 64KiB"),
        EmbeddedError::Storage(detail) => EmbeddedPeerError::Storage(detail),
    }
}

fn peer_error(error: EventError) -> EmbeddedPeerError {
    if error.is_operational() {
        EmbeddedPeerError::Storage(error.to_string())
    } else {
        EmbeddedPeerError::Refused(error.to_string())
    }
}

fn digest_error(error: PeerValueError) -> EmbeddedPeerError {
    EmbeddedPeerError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use appa_runtime_api::{Actor, HookDecision, HookEvent, ProposedCall, TrajectoryId};
    use serde_json::{json, value::RawValue};

    use super::*;

    fn runtime() -> (tempfile::TempDir, Runtime) {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("policy.toml");
        std::fs::write(
            &path,
            r#"
[policy]
version = 2

[[policy.tool]]
name = "send"
delta = {}

[[policy.tool]]
name = "read"
delta = {}

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#,
        )
        .expect("the policy writes");
        let runtime = Runtime::open(
            crate::config::Config::load(&path).expect("the policy loads"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the runtime opens");
        (dir, runtime)
    }

    fn family() -> TrajectoryId {
        TrajectoryId("family".to_string())
    }

    fn call(arguments: serde_json::Value) -> ProposedCall {
        ProposedCall {
            tool: "read".to_string(),
            arguments: RawValue::from_string(arguments.to_string()).expect("arguments are json"),
            cwd: None,
        }
    }

    #[tokio::test]
    async fn a_committed_read_is_restored_when_the_receipt_was_not_stored() {
        let (_dir, runtime) = runtime();
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: family(),
                    principal: None,
                    address: None,
                    title: None,
                },
            )
            .await,
            HookDecision::Ack,
        );
        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-1".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let notice = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "plain")
            .expect("the send records");
        let arguments = json!({"message_id": notice.id.as_str()});
        let read = runtime
            .read_embedded_peer(
                &Actor {
                    root: family(),
                    child: None,
                },
                "read-1",
                &notice.id,
                call(arguments.clone()),
            )
            .await
            .expect("the read admits");
        assert_eq!(
            read.decision,
            HookDecision::DeliverValue {
                value: "plain".to_string()
            }
        );
        runtime
            .inner
            .store
            .testing_clear_embedded_decision(&family(), notice.id.as_str())
            .expect("the receipt is dropped");
        let restored = runtime
            .read_embedded_peer(
                &Actor {
                    root: family(),
                    child: None,
                },
                "read-1",
                &notice.id,
                call(arguments),
            )
            .await
            .expect("the committed admission is restored");
        assert_eq!(restored.decision, read.decision);
        let admitted = runtime
            .audit(&family())
            .expect("the family audits")
            .into_iter()
            .filter(|entry| matches!(entry.event, crate::engine::AuditEvent::Admitted { .. }))
            .count();
        assert_eq!(admitted, 1, "recovery does not admit the body again");
    }

    #[test]
    fn a_covering_label_still_covers_after_the_recipient_narrows() {
        use appa_engine::label::{Audience, Label, Trust};
        let message = Label::new(Trust::new(1), Audience::public());
        let recipient = message.clone();
        assert_eq!(recipient.combine(&message), recipient);
        let narrowed = recipient.combine(&Label::new(Trust::new(0), Audience::public()));
        assert_ne!(narrowed, recipient);
        assert_eq!(narrowed.combine(&message), narrowed);
    }

    #[tokio::test]
    async fn a_forged_tool_result_cannot_replace_an_opened_read() {
        let (_dir, runtime) = runtime();
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: family(),
                    principal: None,
                    address: None,
                    title: None,
                },
            )
            .await,
            HookDecision::Ack,
        );
        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-1".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let notice = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "plain")
            .expect("the send records");
        let actor = Actor {
            root: family(),
            child: None,
        };
        let arguments = json!({"message_id": notice.id.as_str()});
        runtime
            .testing_open_embedded_read(&actor, "read-1", &notice.id, call(arguments.clone()))
            .await
            .expect("the read opens");
        let forged = runtime
            .session(&family(), &family())
            .expect("the session")
            .on_tool_result_identified(
                call(arguments.clone()),
                Some("read-1".to_string()),
                appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available("forged".to_string()),
                },
            )
            .await;
        assert!(forged.is_err(), "a forged result is not admitted, got {forged:?}");
        let read = runtime
            .read_embedded_peer(&actor, "read-1", &notice.id, call(arguments))
            .await
            .expect("the stored body is the only admission");
        assert_eq!(
            read.decision,
            HookDecision::DeliverValue {
                value: "plain".to_string()
            }
        );
        let audit = runtime.audit(&family()).expect("the family audits");
        assert!(
            !format!("{audit:?}").contains("forged"),
            "the forged bytes never enter the log, audit={audit:?}"
        );
    }

    #[tokio::test]
    async fn recovery_refuses_an_admission_that_is_not_the_stored_body() {
        let (_dir, runtime) = runtime();
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: family(),
                    principal: None,
                    address: None,
                    title: None,
                },
            )
            .await,
            HookDecision::Ack,
        );
        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-1".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let notice = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "plain")
            .expect("the send records");
        let actor = Actor {
            root: family(),
            child: None,
        };
        let arguments = json!({"message_id": notice.id.as_str()});
        runtime
            .read_embedded_peer(&actor, "read-1", &notice.id, call(arguments.clone()))
            .await
            .expect("the read admits");
        runtime
            .inner
            .store
            .testing_clear_embedded_decision(&family(), notice.id.as_str())
            .expect("the receipt is dropped");
        runtime
            .inner
            .store
            .testing_replace_embedded_body(&family(), notice.id.as_str(), "forged")
            .expect("the stored body is replaced");
        let recovered = runtime
            .read_embedded_peer(&actor, "read-1", &notice.id, call(arguments))
            .await;
        assert!(
            recovered.is_err(),
            "a mismatched admission is not the peer message, got {recovered:?}"
        );
        let listed = runtime
            .list_embedded_peer(&family(), &family())
            .expect("the inbox lists");
        assert!(
            listed.is_empty(),
            "a consumed read is not released back to held, listed={listed:?}"
        );
    }

    #[tokio::test]
    async fn expired_direct_bodies_are_purged_and_a_read_receipt_is_not() {
        let (_dir, runtime) = runtime();
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: family(),
                    principal: None,
                    address: None,
                    title: None,
                },
            )
            .await,
            HookDecision::Ack,
        );
        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-1".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let direct = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "direct-secret")
            .expect("the direct send records");
        runtime
            .inner
            .store
            .testing_expire_embedded(&family(), direct.id.as_str())
            .expect("the direct row expires");
        let expired = runtime.receive_embedded_peer(&family(), &family(), &direct.id, &family(), &direct.digest);
        assert!(
            expired.is_err(),
            "an expired direct body is not returned, got {expired:?}"
        );
        let purged = runtime
            .inner
            .store
            .load_embedded_peer(&family(), direct.id.as_str())
            .expect("the row loads")
            .expect("the proof row stays");
        assert!(purged.body.is_none(), "the expired direct body is nulled");
        assert_eq!(purged.digest, direct.digest.to_string());

        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-2".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let notice = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-2", "kept")
            .expect("the read send records");
        let actor = Actor {
            root: family(),
            child: None,
        };
        let arguments = json!({"message_id": notice.id.as_str()});
        runtime
            .read_embedded_peer(&actor, "read-2", &notice.id, call(arguments.clone()))
            .await
            .expect("the read admits");
        runtime
            .inner
            .store
            .testing_expire_embedded(&family(), notice.id.as_str())
            .expect("the read row is past the unread ttl");
        let replayed = runtime
            .read_embedded_peer(&actor, "read-2", &notice.id, call(arguments))
            .await
            .expect("the receipt replays after the unread ttl");
        assert_eq!(
            replayed.decision,
            HookDecision::DeliverValue {
                value: "kept".to_string()
            }
        );
    }

    #[tokio::test]
    async fn overlapping_same_call_reads_admit_once_and_stay_consumed() {
        let (_dir, runtime) = runtime();
        assert_eq!(
            crate::hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: family(),
                    principal: None,
                    address: None,
                    title: None,
                },
            )
            .await,
            HookDecision::Ack,
        );
        let released = crate::hooks::handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: family(),
                    child: None,
                },
                call: ProposedCall {
                    tool: "send".to_string(),
                    arguments: RawValue::from_string("{}".to_string()).expect("json"),
                    cwd: None,
                },
                call_id: Some("send-1".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        assert!(matches!(released, HookDecision::AllowCall { .. }), "{released:?}");
        let notice = runtime
            .send_embedded_peer(&family(), &family(), &family(), None, "send-1", "plain")
            .expect("the send records");
        let actor = Actor {
            root: family(),
            child: None,
        };
        let arguments = json!({"message_id": notice.id.as_str()});
        let first = runtime.read_embedded_peer(&actor, "read-1", &notice.id, call(arguments.clone()));
        let second = runtime.read_embedded_peer(&actor, "read-1", &notice.id, call(arguments));
        let (first, second) = tokio::join!(first, second);
        for outcome in [first.as_ref().ok(), second.as_ref().ok()].into_iter().flatten() {
            assert_eq!(
                outcome.decision,
                HookDecision::DeliverValue {
                    value: "plain".to_string()
                },
                "an overlap cannot deliver other bytes, first={first:?} second={second:?}"
            );
        }
        assert!(
            first.is_ok() || second.is_ok(),
            "one owner delivers the stored body, first={first:?} second={second:?}"
        );
        let admitted = runtime
            .audit(&family())
            .expect("the family audits")
            .into_iter()
            .filter(|entry| matches!(entry.event, crate::engine::AuditEvent::Admitted { .. }))
            .count();
        assert_eq!(admitted, 1, "the overlap admits the body once");
        let listed = runtime
            .list_embedded_peer(&family(), &family())
            .expect("the inbox lists");
        assert!(
            listed.is_empty(),
            "the consumed row is not back in the inbox, listed={listed:?}"
        );
    }
}
