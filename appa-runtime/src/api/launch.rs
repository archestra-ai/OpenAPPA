//! One protected launch, one family. A host may move a running conversation to a new
//! session id (a cleared or branched conversation keeps its process and its subagents), so
//! the host id alone does not say which family an event belongs to. A launched start
//! records which family its launch belongs to, and every later host id the launch
//! continues under is recorded as an alias of that family, durably, so a resume by any of
//! those ids reopens it.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use appa_eventlog::{HostObservation, Log, host_alias_key, launch_key};
use appa_runtime_api::{LaunchStart, LaunchToken, StartKind, TrajectoryId};

use super::{EventError, Runtime};

/// The launches a start is deciding now: a second start of one launch is refused as
/// contended until the first is done, so none reads the family between another's handoff
/// and arrival. Per runtime: another runtime on the same store is not excluded by it.
#[derive(Default)]
pub(crate) struct Launches {
    deciding: Mutex<HashSet<LaunchToken>>,
}

impl Launches {
    /// `None` while another start of the launch is deciding.
    fn decide(&self, launch: LaunchStart) -> Option<Deciding<'_>> {
        // The set only gains and loses whole tokens, so a panic elsewhere leaves it consistent.
        self.deciding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(launch.launch.clone())
            .then(|| Deciding { launches: self, launch })
    }
}

/// One launched start deciding its family. Until it drops, every other start of its launch
/// on this runtime is refused as contended. A start holds it from its first read of the
/// family, the alias lookup, through the last thing it records, and nothing awaits under it.
pub(crate) struct Deciding<'a> {
    launches: &'a Launches,
    launch: LaunchStart,
}

impl Drop for Deciding<'_> {
    fn drop(&mut self) {
        self.launches
            .deciding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.launch.launch);
    }
}

/// Which family a launched start opened or continued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchedStart {
    /// The start's own root: new, reopened, or forked from the session it copied.
    Opened(TrajectoryId),
    /// The host continues this family under the start's id.
    Continued(TrajectoryId),
    /// A clear that found the family still owing something: it continues, label and all.
    ClearKept(TrajectoryId),
}

impl LaunchedStart {
    pub(crate) fn root(&self) -> &TrajectoryId {
        match self {
            Self::Opened(root) | Self::Continued(root) | Self::ClearKept(root) => root,
        }
    }
}

/// What a resume did with the launch it asked a family to hand over.
enum Handoff {
    Moved,
    /// The family no longer ran the launch: a concurrent start moved it first.
    Gone,
}

/// What a clear did with the launch.
enum Cleared {
    Moved,
    /// The family still owes something: it keeps the launch, and the clear's id aliases it.
    Kept,
    /// The family no longer ran the launch: a concurrent start moved it first.
    Gone,
}

impl Runtime {
    /// Begin deciding a start of `launch`, or refuse it as contended while another start of
    /// that launch decides.
    pub(crate) fn decide_launch(&self, launch: LaunchStart) -> Result<Deciding<'_>, EventError> {
        self.inner.shared.launches.decide(launch).ok_or_else(raced)
    }

    /// The family the host continues under `host`, where it continues one there. `None` for
    /// a host id that names its own root or names nothing.
    pub(crate) fn alias_of(&self, host: &TrajectoryId) -> Result<Option<TrajectoryId>, EventError> {
        if self.has_root(host)? {
            return Ok(None);
        }
        match self.roots_named(&host_alias_key(host))?.as_slice() {
            [] => Ok(None),
            [family] => Ok(Some(family.clone())),
            _ => Err(EventError::EngineInvariant(format!(
                "host id {} continues more than one family",
                host.0
            ))),
        }
    }

    /// Open the family a launched start belongs to. `root` is the start's own host id,
    /// resolved through [`Runtime::alias_of`] under the same `deciding`. A start whose
    /// handoff finds the launch already moved is refused as contended: it claims nothing.
    pub(crate) fn launched_start(
        &self,
        root: &TrajectoryId,
        start: Option<StartKind>,
        deciding: &Deciding<'_>,
    ) -> Result<LaunchedStart, EventError> {
        let launch = &deciding.launch;
        let token = &launch.launch;
        let family = self.launch_family(token)?;
        let known = self.has_root(root)?;
        let opened = match (known, family) {
            // Already the launch's family: nothing moves, and nothing is claimed again over a
            // move a concurrent start made since.
            (true, Some(family)) if &family == root => return Ok(LaunchedStart::Opened(family)),
            // The host resumed another conversation APPA already holds: the launch now runs that one.
            (true, Some(family)) => match self.move_launch(&family, token, root)? {
                Handoff::Moved => LaunchedStart::Opened(root.clone()),
                Handoff::Gone => return Err(raced()),
            },
            (true, None) => LaunchedStart::Opened(root.clone()),
            (false, Some(family)) => match start {
                Some(StartKind::Clear) => match self.clear_launch(&family, token, root)? {
                    Cleared::Moved => {
                        self.open_fresh(root)?;
                        LaunchedStart::Opened(root.clone())
                    }
                    Cleared::Kept => LaunchedStart::ClearKept(family),
                    Cleared::Gone => return Err(raced()),
                },
                // A conversation APPA never held replaces this one, as a resume of it would.
                Some(StartKind::Resume) => match self.move_launch(&family, token, root)? {
                    Handoff::Moved => {
                        self.open_fresh(root)?;
                        LaunchedStart::Opened(root.clone())
                    }
                    Handoff::Gone => return Err(raced()),
                },
                Some(StartKind::Startup | StartKind::Compact | StartKind::Fork) | None => {
                    self.record_once(&family, HostObservation::Aliased { host: root.clone() })?;
                    LaunchedStart::Continued(family)
                }
            },
            (false, None) => {
                match (start, &launch.forked_from) {
                    (Some(StartKind::Fork), Some(parent)) => self.fork_launch(parent, root)?,
                    _ => self.open_fresh(root)?,
                }
                LaunchedStart::Opened(root.clone())
            }
        };
        if let LaunchedStart::Opened(root) = &opened {
            self.inner.append_host_with::<EventError, _>(root, |log| {
                Ok((
                    (!runs(log, token)).then(|| HostObservation::Launched { launch: token.clone() }),
                    (),
                ))
            })?;
        }
        Ok(opened)
    }

    /// The family `launch` currently runs, if any start recorded one.
    fn launch_family(&self, launch: &LaunchToken) -> Result<Option<TrajectoryId>, EventError> {
        let mut current = Vec::new();
        for root in self.roots_named(&launch_key(launch))? {
            if runs(&self.inner.log(&root)?, launch) {
                current.push(root);
            }
        }
        match current.as_slice() {
            [] => Ok(None),
            [family] => Ok(Some(family.clone())),
            _ => Err(EventError::EngineInvariant(format!(
                "launch {launch} runs more than one family"
            ))),
        }
    }

    /// Move `launch` from `family` to `to`, the clear's new root, only when the family has
    /// nothing in flight or owed; otherwise record `to` as the family's alias. One
    /// compare-and-swap decides both, so nothing the family starts can slip between the
    /// check and the move.
    fn clear_launch(
        &self,
        family: &TrajectoryId,
        launch: &LaunchToken,
        to: &TrajectoryId,
    ) -> Result<Cleared, EventError> {
        let deployment = self.inner.deployment();
        self.inner.append_host_with(family, |log| {
            if !runs(log, launch) {
                return Ok((None, Cleared::Gone));
            }
            let policy = self.inner.resolve_policy(&deployment, log)?;
            let view = policy.rebuild_view(log).map_err(EventError::from)?;
            Ok(match policy.engine().is_quiescent(&view) {
                true => (
                    Some(HostObservation::LaunchMoved {
                        launch: launch.clone(),
                        to: to.clone(),
                    }),
                    Cleared::Moved,
                ),
                false => (Some(HostObservation::Aliased { host: to.clone() }), Cleared::Kept),
            })
        })
    }

    fn move_launch(
        &self,
        family: &TrajectoryId,
        launch: &LaunchToken,
        to: &TrajectoryId,
    ) -> Result<Handoff, EventError> {
        self.inner.append_host_with(family, |log| {
            Ok(match runs(log, launch) {
                true => (
                    Some(HostObservation::LaunchMoved {
                        launch: launch.clone(),
                        to: to.clone(),
                    }),
                    Handoff::Moved,
                ),
                false => (None, Handoff::Gone),
            })
        })
    }

    /// Open `root` as a root fork of the family the host copied. A parent APPA never held
    /// gives nothing to carry, so the fork opens fresh, as a resume of that conversation
    /// would.
    fn fork_launch(&self, parent: &TrajectoryId, root: &TrajectoryId) -> Result<(), EventError> {
        let parent = match self.alias_of(parent)? {
            Some(family) => family,
            None if self.has_root(parent)? => parent.clone(),
            None => return self.open_fresh(root),
        };
        self.open_root_fork(&parent, &parent, root)
            .map(|_| ())
            .map_err(|refusal| EventError::Storage(refusal.to_string()))
    }

    fn open_fresh(&self, root: &TrajectoryId) -> Result<(), EventError> {
        match self.create_session(root.clone(), None) {
            Ok(_) | Err(EventError::TrajectoryExists) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn record_once(&self, root: &TrajectoryId, observation: HostObservation) -> Result<(), EventError> {
        self.inner.append_host_with(root, |log| {
            let recorded = log
                .host_records()
                .iter()
                .any(|record| record.observation == observation);
            Ok(((!recorded).then(|| observation.clone()), ()))
        })
    }

    fn has_root(&self, root: &TrajectoryId) -> Result<bool, EventError> {
        self.inner
            .store
            .has_root(root)
            .map_err(|error| EventError::Storage(error.to_string()))
    }

    fn roots_named(&self, key: &str) -> Result<Vec<TrajectoryId>, EventError> {
        self.inner
            .store
            .roots_mentioning(key)
            .map_err(|error| EventError::Storage(error.to_string()))
    }
}

/// A concurrent start of the launch is deciding, or moved it between this start's read and
/// its handoff.
fn raced() -> EventError {
    EventError::Contended { attempts: 1 }
}

/// Whether this family runs `launch` now: the latest record about it says the launch
/// arrived, not that it left. A launch can leave a family and come back.
fn runs(log: &Log, launch: &LaunchToken) -> bool {
    log.host_records()
        .iter()
        .rev()
        .find_map(|record| match &record.observation {
            HostObservation::Launched { launch: arrived } if arrived == launch => Some(true),
            HostObservation::LaunchMoved { launch: moved, .. } if moved == launch => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}
