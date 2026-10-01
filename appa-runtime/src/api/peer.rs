//! Peer messages: one protected Claude Code session's `SendMessage` to another.
//!
//! A family records where it receives peer messages (`Addressed`) and the title its host
//! shows (`Titled`). A send to a socket address is released only when that address is the
//! current one of exactly one other live family pinned to the same policy and principal;
//! every released send records its body's digest and the label it left at (`PeerSent`).
//!
//! A delivered message takes the `combine` of the labels of every send record of its digest
//! among families pinned alike. It enters directly when that label does not narrow the
//! receiver. Otherwise, or when no record matches, it is held: the body waits in the held
//! store, the model is told at its next prompt or tool result, and it reads the body with
//! `read_peer_message`, whose result carries the held label.

use std::time::{Duration, SystemTime};

use appa_engine::fact::{Fact, TrajectoryOpening};
use appa_engine::label::{Audience, Label, Trust};
use appa_engine::value::{DispatchId, FileBasis, FileSource};
use appa_eventlog::{HeldNotice, HeldPeerId, HostObservation, Log, LogStore};
use appa_runtime_api::{Actor, PeerAddress, PeerDigest, PeerFrame, ProposedCall, SessionTitle, TrajectoryId};

use super::{EventError, Runtime, acting_trajectory};

/// The host tool one session messages another with.
pub(crate) const SEND_MESSAGE: &str = "host/claude-code/SendMessage";

/// The runtime tool that reads a held peer message.
pub(crate) const READ_PEER_MESSAGE: &str = "read_peer_message";

const HELD_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How many held messages one family keeps; a newer one drops the oldest.
const HELD_QUOTA: usize = 32;

/// The largest peer message a session sends or takes in.
const PEER_MESSAGE_LIMIT: usize = 64 * 1024;

fn within_limit(body: &str) -> Result<(), EventError> {
    match body.len() {
        bytes if bytes > PEER_MESSAGE_LIMIT => Err(EventError::PeerMessageTooLarge {
            bytes,
            limit: PEER_MESSAGE_LIMIT,
        }),
        _ => Ok(()),
    }
}

/// What the send gate decided before the engine judges the call.
pub(crate) enum PeerSend {
    /// The engine judges the call; a release records the digest.
    Judge { digest: PeerDigest },
    /// The call is refused with this feedback.
    Refused { feedback: String },
}

/// How a delivered message was taken in.
pub(crate) enum Received {
    Direct,
    Held(HeldNotice),
}

#[derive(serde::Deserialize)]
struct SendArguments {
    to: String,
    message: String,
    recipient: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadPeerArgs {
    /// The id the held message's notice gave.
    pub(crate) id: String,
}

fn opening(log: &Log) -> Option<&TrajectoryOpening> {
    match log.facts().first() {
        Some(Fact::TrajectoryOpened(opening)) => Some(opening),
        _ => None,
    }
}

/// Two families opened under one policy for one principal: a label one records means the
/// same to the other.
fn pinned_alike(one: &Log, other: &Log) -> bool {
    match (opening(one), opening(other)) {
        (Some(one), Some(other)) => one.policy_digest == other.policy_digest && one.principal == other.principal,
        _ => false,
    }
}

/// The label a message nobody stands behind is read at: no trust, and no audience claim.
fn unattributed() -> Label {
    Label::new(Trust::new(0), Audience::public())
}

/// The family's current address and title, the latest of each it recorded.
fn identity(log: &Log) -> (Option<&PeerAddress>, Option<&SessionTitle>) {
    log.host_records()
        .iter()
        .fold((None, None), |(address, title), record| match &record.observation {
            HostObservation::Addressed { address } => (Some(address), title),
            HostObservation::Titled { title } => (address, Some(title)),
            _ => (address, title),
        })
}

/// The held message a `read_peer_message` call names, or `None` when the call is not one.
pub(crate) fn held_read(call: &ProposedCall) -> Option<HeldPeerId> {
    if super::bare_runtime_tool(&call.tool) != READ_PEER_MESSAGE {
        return None;
    }
    let args = serde_json::from_str::<ReadPeerArgs>(call.arguments.get()).ok()?;
    HeldPeerId::parse(&args.id).ok()
}

/// The basis a `read_peer_message` call reads under: the held message's digest and label.
pub(super) fn held_basis(store: &LogStore, root: &TrajectoryId, id: &HeldPeerId) -> Result<FileBasis, EventError> {
    let notice = store
        .peek_peer_message(root, id, SystemTime::now())
        .map_err(|error| EventError::Storage(error.to_string()))?
        .ok_or_else(|| EventError::RemedyArguments {
            detail: format!("no peer message {} is held for this session", id.as_str()),
        })?;
    Ok(FileBasis::Read(FileSource {
        version: notice.id.as_str().to_string(),
        digest: notice.digest.to_string(),
        label: notice.label,
    }))
}

impl Runtime {
    /// Record the family's address and title where either changed. A resumed session with
    /// the same identity records nothing.
    pub(crate) fn record_peer_identity(
        &self,
        root: &TrajectoryId,
        address: Option<&PeerAddress>,
        title: Option<&SessionTitle>,
    ) -> Result<(), EventError> {
        if let Some(address) = address {
            self.inner.append_host_with::<EventError, _>(root, |log| {
                let changed = identity(log).0 != Some(address);
                Ok((
                    changed.then(|| HostObservation::Addressed {
                        address: address.clone(),
                    }),
                    (),
                ))
            })?;
        }
        if let Some(title) = title {
            self.inner.append_host_with::<EventError, _>(root, |log| {
                let changed = identity(log).1 != Some(title);
                Ok((changed.then(|| HostObservation::Titled { title: title.clone() }), ()))
            })?;
        }
        Ok(())
    }

    /// The families that recorded `key`, with their logs.
    fn mentioning(&self, key: &str) -> Result<Vec<(TrajectoryId, Log)>, EventError> {
        let roots = self.inner.store.roots_mentioning(key).map_err(|error| {
            self.inner
                .note_store_error(None, crate::events::StoreOperation::Read, &error);
            EventError::Storage(error.to_string())
        })?;
        roots
            .into_iter()
            .map(|root| self.inner.log(&root).map(|log| (root, log)))
            .collect()
    }

    /// The one other live family pinned like `sender` whose current identity `matches`.
    fn sole_peer(
        &self,
        sender: &TrajectoryId,
        sent: &Log,
        key: &str,
        matches: impl Fn(&Log) -> bool,
    ) -> Result<Option<(TrajectoryId, Log)>, EventError> {
        let found: Vec<_> = self
            .mentioning(key)?
            .into_iter()
            .filter(|(root, log)| {
                root != sender && matches(log) && pinned_alike(sent, log) && self.live(root, root).is_ok()
            })
            .collect();
        Ok(match found.len() {
            1 => found.into_iter().next(),
            _ => None,
        })
    }

    /// The send gate. `to` as a socket address must be a verified peer's; any other `to` is
    /// a name the policy judges. `recipient` is the same field under another name, and a
    /// call that spells the two apart is refused.
    pub(crate) fn peer_send(&self, sender: &TrajectoryId, call: &ProposedCall) -> Result<PeerSend, EventError> {
        let Ok(arguments) = serde_json::from_str::<SendArguments>(call.arguments.get()) else {
            return Ok(PeerSend::Refused {
                feedback: "SendMessage takes `to` and `message` as strings".to_string(),
            });
        };
        if arguments
            .recipient
            .as_ref()
            .is_some_and(|recipient| recipient != &arguments.to)
        {
            return Ok(PeerSend::Refused {
                feedback: "`recipient` must equal `to`".to_string(),
            });
        }
        if let Err(error) = within_limit(&arguments.message) {
            return Ok(PeerSend::Refused {
                feedback: error.to_string(),
            });
        }
        let digest = PeerDigest::of_body(&arguments.message);
        let Ok(address) = PeerAddress::parse(&arguments.to) else {
            return Ok(PeerSend::Judge { digest });
        };
        let sent = self.inner.log(sender)?;
        let verified = self.sole_peer(sender, &sent, &appa_eventlog::peer_address_key(&address), |log| {
            identity(log).0 == Some(&address)
        })?;
        Ok(match verified {
            Some(_) => PeerSend::Judge { digest },
            None => PeerSend::Refused {
                feedback: format!(
                    "{} is not the address of a live protected session under this policy",
                    arguments.to
                ),
            },
        })
    }

    /// The address of the one live peer pinned like `sender` whose title is the name a
    /// refused send used, for the feedback to point at.
    pub(crate) fn peer_hint(
        &self,
        sender: &TrajectoryId,
        call: &ProposedCall,
    ) -> Result<Option<PeerAddress>, EventError> {
        let Ok(arguments) = serde_json::from_str::<SendArguments>(call.arguments.get()) else {
            return Ok(None);
        };
        let Ok(title) = SessionTitle::parse(&arguments.to) else {
            return Ok(None);
        };
        let sent = self.inner.log(sender)?;
        let peer = self.sole_peer(sender, &sent, &appa_eventlog::peer_title_key(&title), |log| {
            identity(log).1 == Some(&title)
        })?;
        Ok(peer.and_then(|(_, log)| identity(&log).0.cloned()))
    }

    /// Record a released send in the sender family's log, at the label the sending
    /// trajectory holds as it is released.
    pub(crate) fn record_peer_sent(
        &self,
        actor: &Actor,
        digest: PeerDigest,
        dispatch: DispatchId,
    ) -> Result<(), EventError> {
        let log = self.inner.log(&actor.root)?;
        let label = self.current_label(&log, acting_trajectory(actor))?;
        self.inner.append_host_with::<EventError, _>(&actor.root, |_| {
            Ok((
                Some(HostObservation::PeerSent {
                    digest,
                    label: label.clone(),
                    dispatch: dispatch.clone(),
                }),
                (),
            ))
        })
    }

    fn current_label(&self, log: &Log, trajectory: &TrajectoryId) -> Result<Label, EventError> {
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, log)?;
        let view = policy.engine().rebuild_view(log).map_err(EventError::from)?;
        view.views(trajectory)
            .map(|views| views.current_label())
            .ok_or(EventError::UnknownTrajectory)
    }

    /// The label every send record of `digest` stands behind, among families pinned like
    /// `receiver`; `None` when none does.
    fn attributed(&self, receiver: &Log, digest: &PeerDigest) -> Result<Option<Label>, EventError> {
        let labels = self
            .mentioning(&appa_eventlog::peer_sent_key(digest))?
            .into_iter()
            .filter(|(_, log)| pinned_alike(receiver, log))
            .flat_map(|(_, log)| {
                log.host_records()
                    .iter()
                    .filter_map(|record| match &record.observation {
                        HostObservation::PeerSent {
                            digest: sent, label, ..
                        } if sent == digest => Some(label.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
        Ok(labels.reduce(|label, next| label.combine(&next)))
    }

    /// Take in one delivered message: directly when the label its senders stand behind does
    /// not narrow the receiving root, held otherwise. `text` is the whole prompt, held as is
    /// when the frame could not be read. A message over the limit is refused, never held.
    pub(crate) fn receive_peer(
        &self,
        root: &TrajectoryId,
        frame: &PeerFrame,
        text: &str,
    ) -> Result<Received, EventError> {
        let body = match frame {
            PeerFrame::Parsed { body } => body.as_str(),
            PeerFrame::Malformed => text,
        };
        within_limit(body)?;
        let log = self.inner.log(root)?;
        let digest = PeerDigest::of_body(body);
        let attributed = match frame {
            PeerFrame::Parsed { .. } => self.attributed(&log, &digest)?,
            PeerFrame::Malformed => None,
        };
        let current = self.current_label(&log, root)?;
        if let Some(label) = &attributed
            && current.combine(label) == current
        {
            self.inner.append_host_with::<EventError, _>(root, |_| {
                Ok((Some(HostObservation::PeerAdmitted { digest }), ()))
            })?;
            return Ok(Received::Direct);
        }
        let label = attributed.unwrap_or_else(unattributed);
        let notice = self
            .inner
            .store
            .hold_peer_message(root, digest, &label, body, SystemTime::now(), HELD_TTL, HELD_QUOTA)
            .map_err(|error| EventError::Storage(error.to_string()))?;
        Ok(Received::Held(notice))
    }

    /// The notice of every message held for `root` that the model has not been told of, once.
    pub(crate) fn peer_notices(&self, root: &TrajectoryId) -> Result<Option<String>, EventError> {
        let notices = self
            .inner
            .store
            .peer_notices(root, SystemTime::now())
            .map_err(|error| EventError::Storage(error.to_string()))?;
        if notices.is_empty() {
            return Ok(None);
        }
        let log = self.inner.log(root)?;
        let deployment = self.inner.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let lines: Vec<String> = notices
            .iter()
            .map(|notice| {
                let label = policy.engine().render_label(&notice.label);
                let (trust, audience) = label.map_or(("?".into(), "?".into()), |label| (label.trust, label.audience));
                format!(
                    "- id {}: reading it brings trust {trust}, audience {audience}",
                    notice.id.as_str()
                )
            })
            .collect();
        Ok(Some(format!(
            "[appa] Another session sent messages that are held because taking them in would narrow \
             this session or no protected sender stands behind them. Read one with \
             {READ_PEER_MESSAGE}(id); its content then carries the label below. Reading it inside a \
             subagent keeps this session's label. Held messages expire after a day.\n{}",
            lines.join("\n")
        )))
    }

    /// Take a held message's body for the session that holds it; `None` when it is gone.
    #[cfg(feature = "daemon")]
    pub(crate) fn take_held(&self, root: &TrajectoryId, id: &HeldPeerId) -> Result<Option<String>, EventError> {
        self.inner
            .store
            .take_peer_message(root, id, SystemTime::now())
            .map(|held| held.map(|held| held.body))
            .map_err(|error| EventError::Storage(error.to_string()))
    }
}
