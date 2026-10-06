//! One protected launch, one family. A host may move a running conversation to a new
//! session id (a cleared or branched conversation keeps its process and its subagents), so
//! the host id alone does not say which family an event belongs to. A launched start
//! records which family its launch belongs to, and every later host id the launch
//! continues under is recorded as an alias of that family, durably, so a resume by any of
//! those ids reopens it.

use appa_eventlog::{HostObservation, Log, host_alias_key, launch_key};
use appa_runtime_api::{LaunchStart, LaunchToken, StartKind, TrajectoryId};

use super::{EventError, Runtime};

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

impl Runtime {
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
    /// already resolved through [`Runtime::alias_of`].
    pub(crate) fn launched_start(
        &self,
        root: &TrajectoryId,
        start: Option<StartKind>,
        launch: &LaunchStart,
    ) -> Result<LaunchedStart, EventError> {
        let token = &launch.launch;
        let family = self.launch_family(token)?;
        let known = self.has_root(root)?;
        let opened = match (known, family) {
            (true, Some(family)) if &family == root => LaunchedStart::Opened(family),
            // The host resumed another conversation APPA already holds: the launch now runs that one.
            (true, Some(family)) => {
                self.move_launch(&family, token, root)?;
                LaunchedStart::Opened(root.clone())
            }
            (true, None) => LaunchedStart::Opened(root.clone()),
            (false, Some(family)) => match start {
                Some(StartKind::Clear) => match self.clear_launch(&family, token, root)? {
                    true => {
                        self.open_fresh(root)?;
                        LaunchedStart::Opened(root.clone())
                    }
                    false => LaunchedStart::ClearKept(family),
                },
                // A conversation APPA never held replaces this one, as a resume of it would.
                Some(StartKind::Resume) => {
                    self.move_launch(&family, token, root)?;
                    self.open_fresh(root)?;
                    LaunchedStart::Opened(root.clone())
                }
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
            self.record_once(root, HostObservation::Launched { launch: token.clone() })?;
        }
        Ok(opened)
    }

    /// The family `launch` currently runs, if any start recorded one.
    fn launch_family(&self, launch: &LaunchToken) -> Result<Option<TrajectoryId>, EventError> {
        let mut current = Vec::new();
        for root in self.roots_named(&launch_key(launch))? {
            if !moved_away(&self.inner.log(&root)?, launch) {
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
    fn clear_launch(&self, family: &TrajectoryId, launch: &LaunchToken, to: &TrajectoryId) -> Result<bool, EventError> {
        let deployment = self.inner.deployment();
        self.inner.append_host_with(family, |log| {
            let policy = self.inner.resolve_policy(&deployment, log)?;
            let view = policy.engine().rebuild_view(log).map_err(EventError::from)?;
            Ok(match policy.engine().is_quiescent(&view) {
                true => (
                    Some(HostObservation::LaunchMoved {
                        launch: launch.clone(),
                        to: to.clone(),
                    }),
                    true,
                ),
                false => (Some(HostObservation::Aliased { host: to.clone() }), false),
            })
        })
    }

    fn move_launch(&self, family: &TrajectoryId, launch: &LaunchToken, to: &TrajectoryId) -> Result<(), EventError> {
        self.inner.append_host_with(family, |log| {
            Ok((
                (!moved_away(log, launch)).then(|| HostObservation::LaunchMoved {
                    launch: launch.clone(),
                    to: to.clone(),
                }),
                (),
            ))
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
            .map_err(|refusal| EventError::Storage(refusal.to_string()))?;
        match &self.inner.shared.files {
            Some(files) => files
                .bind_as(&self.inner.store, root, &parent)
                .map_err(|error| EventError::Storage(error.to_string())),
            None => Ok(()),
        }
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

/// Whether this family gave `launch` up.
fn moved_away(log: &Log, launch: &LaunchToken) -> bool {
    log.host_records().iter().any(
        |record| matches!(&record.observation, HostObservation::LaunchMoved { launch: moved, .. } if moved == launch),
    )
}
