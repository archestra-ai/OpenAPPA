//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::elicit::Elicitation;

use crate::consult::{
    AnnotationAnswer, AnnotationArtifact, Consult, ConsultBody, LookupAnswer, MembersAnswer, SanitizerAnswer,
};
use crate::engine::{
    Abstention, AuthorityVerdict, EngineDecision, EngineEvent, EngineView, ExternalEvidence, ExternalRequest, Feedback,
    ForkStatus, Liveness, Next, OfferNonce, OpenDispatch, PendingReview, Presentation, RemedyArguments,
};
use crate::external::ConsultOutcome;
use appa_engine::contract::{AnnotationContext, ContextEntry};
use appa_engine::label::ReaderId;
use appa_engine::names::ContextProviderName;
use appa_engine::transition::ProposalBatchId;

use super::{
    ChildReturnDecision, ConsultContext, ConsultRecord, Deployment, EmbeddedPresentationOptions, EventError, ExactCall,
    Inner, OfferId, OutcomeBody, PromptKey, ProposedCall, RemedyDecision, RemedyPresentation, SpawnKind, SpawnRef,
    SpawnResultDecision, ToolCallDecision, ToolOutcome, ToolResultDecision, TrajectoryId,
};

/// The runtime's own control tool, recognized by its one canonical
/// identity, `appa/execute_remedy_plan`: the served adapter identifies it
/// from the host's registered spelling of the runtime's MCP server, and
/// nothing else is identified as it. Selecting an offer is not a checked flow. A
/// lookalike on another server — say `mcp/evil/execute_remedy_plan` —
/// is an ordinary checked call.
pub(crate) fn is_control_tool(tool: &str) -> bool {
    tool == appa_runtime_api::CONTROL_TOOL
}

/// Why a reported outcome named no reportable dispatch. The threat model puts
/// operator diagnostics for these reports on the runtime: an
/// uncontrolled host makes integration mistakes the engine cannot
/// diagnose for it. The model-facing refusal is unchanged — the case
/// goes to the operator, not into feedback.
///
/// These are the contexts the runtime can observe, which are not
/// the threat model's three cases. An already-consumed report is
/// indistinguishable from an unknown one here: a closed dispatch leaves
/// the open set, and an outcome is attributable solely through its open
/// dispatch, so a byte match against a closed one would name
/// a call, never the occurrence that report belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnreportableOutcome {
    NoOpenDispatch,
    ByteMismatch,
}

impl UnreportableOutcome {
    fn case(self) -> &'static str {
        match self {
            UnreportableOutcome::NoOpenDispatch => "no_open_dispatch",
            UnreportableOutcome::ByteMismatch => "byte_mismatch",
        }
    }

    fn refusal(self) -> EventError {
        match self {
            UnreportableOutcome::NoOpenDispatch => EventError::UnknownDispatch,
            UnreportableOutcome::ByteMismatch => EventError::OutcomeMismatch,
        }
    }
}

fn is_open_call(call: &ProposedCall, canonical: impl FnOnce() -> Option<Vec<u8>>, open: &OpenDispatch) -> bool {
    call.tool == open.tool && canonical().as_deref() == Some(open.bytes.as_slice())
}

/// Execute one workspace-event projection operation on the blocking pool. These operations can
/// hash entire files and replay the full workspace stream. The executor also serves the harness's
/// hooks and MCP requests. `bind`, `cancel` and `abandon` stay inline when they do not read files.
async fn ledger<T: Send + 'static>(
    inner: std::sync::Arc<super::Inner>,
    root: super::TrajectoryId,
    work: impl FnOnce(&appa_eventlog::files::FileStore) -> Result<T, appa_eventlog::files::FileStoreError> + Send + 'static,
) -> Result<T, EventError> {
    let joined = tokio::task::spawn_blocking(move || {
        let files = inner
            .shared
            .files
            .as_ref()
            .ok_or_else(|| appa_eventlog::files::FileStoreError::Corrupt("file tools are not enabled".into()))?;
        let store = files.store(&inner.store, &root)?;
        work(&store)
    })
    .await
    .map_err(|error| super::files::refused(format!("the file ledger task failed: {error}")))?;
    joined.map_err(super::files::refused)
}

/// Which dispatch a reported outcome belongs to, or why none can take
/// it. Total over everything the log shows: the reported call in the
/// engine's canonical domain and the dispatches this
/// trajectory has open. `canonical` is deferred because a report with
/// no open dispatch — the duplicate every crash recovery produces —
/// settles without canonicalizing anything.
///
/// A host call id selects one occurrence among parallel dispatches. A
/// legacy event without an id can report only when exactly one dispatch
/// is open; a byte match among several occurrences would be a guess.
fn classify_report_identified<'a>(
    call: &ProposedCall,
    call_id: Option<&str>,
    canonical: impl FnOnce() -> Option<Vec<u8>>,
    open: &[OpenDispatch],
    bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
) -> Result<appa_engine::value::DispatchId, UnreportableOutcome> {
    let open = match call_id {
        Some(call_id) => {
            let Some(binding) = bindings
                .into_iter()
                .find(|binding| binding.trajectory == trajectory && binding.call_id == call_id)
            else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            let Some(open) = open.iter().find(|open| open.id == *binding.dispatch) else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            open
        }
        None => {
            let [open] = open else {
                return Err(UnreportableOutcome::NoOpenDispatch);
            };
            open
        }
    };
    if !is_open_call(call, canonical, open) {
        return Err(UnreportableOutcome::ByteMismatch);
    }
    Ok(open.id.clone())
}

/// Whether an open dispatch is an identified spawn: a spawn the harness named with a call id.
/// The harness reports its result under that id, so a report or a proposal that carries none
/// never concerns it, for as long as it is open.
fn identified_spawn<'a>(
    engine: &crate::engine::RuntimeEngine,
    view: &EngineView,
    mut bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
    open: &OpenDispatch,
) -> bool {
    bindings.any(|binding| binding.trajectory == trajectory && *binding.dispatch == open.id)
        && !matches!(
            engine.fork_status(view, &appa_engine::value::ForkId::of(&open.id)),
            ForkStatus::Unprepared
        )
}

/// Whether an open dispatch is an identified spawn whose child runs on past its parent's
/// turn: its fork is bound to a live child.
fn child_runs_on<'a>(
    engine: &crate::engine::RuntimeEngine,
    view: &EngineView,
    bindings: impl Iterator<Item = appa_eventlog::CallBinding<'a>>,
    trajectory: &appa_engine::value::TrajectoryId,
    open: &OpenDispatch,
) -> bool {
    identified_spawn(engine, view, bindings, trajectory, open)
        && matches!(
            engine.fork_status(view, &appa_engine::value::ForkId::of(&open.id)),
            ForkStatus::Bound(child) if engine.liveness(view, &child) == Liveness::Live
        )
}

/// What a late child open did: bound the child now, or found the
/// same pair already bound — the engine's own idempotent answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateOpen {
    Opened,
    AlreadyOpen,
}

enum SpawnPlan {
    Outcome,
    Bind {
        fork: appa_engine::value::ForkId,
        child: TrajectoryId,
    },
    Replay,
    Withheld,
    Close(EventError),
}

/// Delivered when a child terminated without completing its return check.
const UNCHECKED_RETURN: &str = "[appa] the subagent ended outside the return check and its message is withheld; \
                                nothing from it crossed into this session";

/// Blocks subsequent output from a child that already ended without a return.
const ENDED_CHILD: &str = "[appa] this subagent ended without a return; nothing it says now can cross. Stop with an \
                           empty final message (send no text or explanation).";

fn outcome_decision(
    decision: EngineDecision,
    externals: &crate::external::ExternalServices,
) -> Result<ToolResultDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::KeepOutput) => Ok(ToolResultDecision::Keep),
        Next::PresentToModel(Presentation::ReplaceOutput { placeholder, .. }) => Ok(ToolResultDecision::Replace {
            placeholder,
            presentation: None,
        }),
        // An admitted value delivered in place of the raw output, as it crossed.
        Next::PresentToModel(Presentation::Value { value }) => Ok(ToolResultDecision::Deliver { value }),
        // The runtime's own words: the narrowing this result causes and the control call
        // that accepts it.
        Next::PresentToModel(Presentation::Blocked {
            feedback,
            offers,
            review,
            display,
        }) => {
            let presentation = remedy_presentation(
                feedback.clone(),
                offers,
                review,
                display.into_iter().collect(),
                externals,
            );
            Ok(ToolResultDecision::Replace {
                placeholder: feedback,
                presentation: Some(presentation),
            })
        }
        _ => Err(EventError::UnexpectedDecision),
    }
}

fn return_decision(decision: EngineDecision) -> Result<ChildReturnDecision, EventError> {
    match decision.then {
        Next::PresentToModel(Presentation::Value { value }) => Ok(ChildReturnDecision::Returned { value }),
        Next::PresentToModel(Presentation::Staged { value }) => Ok(ChildReturnDecision::Staged { value }),
        Next::PresentToModel(Presentation::NoValue) => Ok(ChildReturnDecision::NoValue),
        Next::PresentToModel(Presentation::Blocked { feedback, .. }) => Ok(ChildReturnDecision::Blocked { feedback }),
        _ => Err(EventError::UnexpectedDecision),
    }
}

const REPLAY_LIMIT: u32 = 8;

/// What a proposal's append binds beside its facts: the host's call id, and the prompt a
/// fan-out spawn it releases is keyed by.
#[derive(Debug, Clone, Copy, Default)]
struct Opening<'a> {
    call_id: Option<&'a str>,
    prompt: Option<&'a PromptKey>,
}

/// The most external-resolution rounds one invocation runs before refusing operationally.
/// Gathering is designed to close at least one ask per round, so this cap never fires on a
/// healthy deployment; it bounds the blast radius of a gathering bug or a hostile external.
pub(super) const RESOLUTION_ROUNDS: u32 = 8;

/// How many publishes one event follows before it refuses. Each one means the head moved
/// while the event was decided; a run this long is a publisher looping, not a policy change.
/// Exceeding it surfaces [`EventError::PolicyMoved`], an operational refusal the harness
/// retries: the next attempt decides at the head.
const PUBLISHES_FOLLOWED: u32 = 4;

/// The event a drive runs for, as far as its consults' records join it: the host's call
/// id, or the offer being executed. A proposal's call id also binds the dispatch it opens,
/// and its prompt keys a fan-out spawn it releases.
#[derive(Debug, Clone, Copy)]
enum Occasion<'a> {
    Proposal {
        call_id: Option<&'a str>,
        prompt: Option<&'a PromptKey>,
    },
    Report {
        call_id: Option<&'a str>,
    },
    Remedy {
        offer: &'a OfferId,
    },
    Other,
}

impl<'a> Occasion<'a> {
    fn call_id(self) -> Option<&'a str> {
        match self {
            Occasion::Proposal { call_id, .. } | Occasion::Report { call_id } => call_id,
            Occasion::Remedy { .. } | Occasion::Other => None,
        }
    }
}

fn fresh_entropy() -> OfferNonce {
    OfferNonce(rand::random::<[u8; 32]>())
}

/// One per trajectory (root or child). The adapter drives it; it never
/// renders, and the adapter never stores.
///
/// The dispatcher builds one per hook event and drops it when the event
/// ends; nothing caches a session across events. That is what makes the
/// deployment snapshot below an event-scoped read rather than a
/// trajectory-scoped one, and a dispatcher that started caching sessions
/// would have to take the snapshot per event instead. The snapshot moves only
/// between attempts of one decision, when a publish moved the head under it
/// ([`Session::follow_head`]); the engine that frames a consult and the externals
/// that answer it are always one snapshot's.
pub struct Session {
    deployment: std::sync::RwLock<Arc<Deployment>>,
    inner: Arc<Inner>,
    trajectory: TrajectoryId,
    root: TrajectoryId,
    presentation: EmbeddedPresentationOptions,
}

/// What a spawn's block says when the return its trajectory declared ahead has lapsed. The
/// block's reason already says to declare right before the subagent call.
const LAPSED_AHEAD: &str = "[appa] The return you declared ahead lapsed, so it was not applied: a call between it \
    and this subagent call changed this session's label.";

/// What a spawn's block says when no declaration it offers fits the return declared ahead.
const UNFIT_AHEAD: &str = "[appa] The return you declared ahead does not fit this call, so it was not applied.";

/// What a fan-out spawn's block says about a return declared ahead, which it does not apply.
const FAN_OUT_AHEAD: &str = "[appa] A return declared ahead covers a single subagent call, so it was not applied. \
    A call that starts several subagents declares at its block.";

/// Follows a notice on a block that offers plans.
const APPLY_BELOW: &str = " Apply a plan below.";

impl Session {
    pub(super) fn attach(
        inner: Arc<Inner>,
        deployment: Arc<Deployment>,
        trajectory: TrajectoryId,
        root: TrajectoryId,
        presentation: EmbeddedPresentationOptions,
    ) -> Session {
        Session {
            deployment: std::sync::RwLock::new(deployment),
            inner,
            trajectory,
            root,
            presentation,
        }
    }

    /// The deployment snapshot this event decides under.
    pub(crate) fn deployment(&self) -> Arc<Deployment> {
        Arc::clone(
            &self
                .deployment
                .read()
                .expect("the session's deployment lock is never poisoned: no panic runs while it is held"),
        )
    }

    /// Take the deployment this view now serves after a publish moved the head under `stale`.
    /// `false` when it serves no later revision than `stale`: a view pinned to one revision
    /// cannot follow. The operational refusal reaches its host, which reads and pins the head.
    fn follow_head(&self, stale: &Deployment) -> bool {
        let serving = self.inner.deployment();
        if serving.revision() <= stale.revision() {
            return false;
        }
        *self
            .deployment
            .write()
            .expect("the session's deployment lock is never poisoned: no panic runs while it is held") = serving;
        true
    }

    /// The actor's turn is over. Calls still open here got no outcome
    /// hook and will never get one: Claude Code reports none for a call
    /// refused at its permission prompt, and none for a turn the user
    /// interrupted. Left open they keep effect reservations and remain
    /// reportable for the life of the trajectory. Two
    /// hooks reach here: the turn end, and the first tool call after a
    /// prompt that no turn end preceded.
    ///
    /// The close is `Indeterminate`, not a failure. What the runtime
    /// observed is the absence of a report, which does not say whether
    /// the call ran: an outcome hook that never reached the runtime
    /// looks the same as a call the harness refused. Indeterminate is
    /// the outcome for exactly that — the dispatch closes and the
    /// effect reservation stands, so a real emission is never dropped
    /// from the ledger on the strength of a missing hook.
    ///
    /// An identified spawn whose child is still live stays open: the child
    /// works past its parent's turn, as a background subagent does, and
    /// the spawn closes when the harness reports its result.
    ///
    /// Not an engine event when nothing is carried, which is every
    /// ordinary turn: the view is read, no fact is appended.
    pub async fn on_turn_end(&self) -> Result<(), EventError> {
        let open = self.turn_calls()?;
        if open.is_empty() {
            tracing::debug!(trajectory = %self.trajectory.0, "no call outstanding");
            return Ok(());
        }
        for dispatch in open {
            match self
                .abandon_dispatch(dispatch.id.clone(), &ToolOutcome::Indeterminate)
                .await
            {
                Ok(_) => {
                    self.repair_file_reservation(&dispatch.id).await;
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        dispatch = ?dispatch.id,
                        tool = %dispatch.tool,
                        "call closed as unreported",
                    );
                }
                Err(EventError::UnknownDispatch) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
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
    async fn repair_file_reservation(&self, dispatch: &appa_engine::value::DispatchId) {
        if self.inner.shared.files.is_none() {
            return;
        }
        let key = match super::files::key(dispatch) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(%error, "a file reservation key could not be rendered");
                return;
            }
        };
        let repaired = ledger(self.inner.clone(), self.root.clone(), {
            let (actor, key) = (self.trajectory.0.clone(), key);
            move |store| store.repair(&actor, &key)
        })
        .await;
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

    /// The reader this session's family acts for, as its opening pinned it.
    pub(crate) fn principal(&self) -> Result<Option<ReaderId>, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        Ok(view.principal().cloned())
    }

    /// The calls this trajectory carries: its open dispatches. A trajectory
    /// that has ended or never opened carries none, so a turn end that
    /// names one is a no-op rather than a refusal — a turn ends for reasons
    /// the engine does not model.
    ///
    fn carried_calls(&self) -> Result<Vec<OpenDispatch>, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        match policy.engine().liveness(&view, &self.trajectory) {
            Liveness::Ended | Liveness::Unopened => Ok(Vec::new()),
            Liveness::Live => Ok(policy.engine().open_dispatches(&view, &self.trajectory)),
        }
    }

    /// The calls a turn end closes: every carried call but a spawn whose child runs on past
    /// the turn. A spawn the harness did not identify closes: such a harness runs one call at
    /// a time, so the open spawn would refuse its next call.
    fn turn_calls(&self) -> Result<Vec<OpenDispatch>, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        let engine = policy.engine();
        if engine.liveness(&view, &self.trajectory) != Liveness::Live {
            return Ok(Vec::new());
        }
        Ok(engine
            .open_dispatches(&view, &self.trajectory)
            .into_iter()
            .filter(|open| !child_runs_on(engine, &view, log.call_bindings(), &self.trajectory, open))
            .collect())
    }

    /// The carried calls a report can name: all of them under a call id, and without one every
    /// call but an identified spawn.
    fn report_calls(&self, call_id: Option<&str>) -> Result<Vec<OpenDispatch>, EventError> {
        let open = self.carried_calls()?;
        if call_id.is_some() || open.is_empty() {
            return Ok(open);
        }
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        let engine = policy.engine();
        Ok(open
            .into_iter()
            .filter(|open| !identified_spawn(engine, &view, log.call_bindings(), &self.trajectory, open))
            .collect())
    }

    #[tracing::instrument(target = "appa_telemetry", name = "appa.policy.check", skip_all, fields(
        appa.trajectory.root = crate::telemetry::name(&self.root.0),
        appa.trajectory.id = crate::telemetry::name(&self.trajectory.0),
        appa.tool.name = crate::telemetry::name(&call.tool),
        appa.tool.call.id = call_id.as_deref().map(crate::telemetry::name).unwrap_or_default(),
        appa.outcome = tracing::field::Empty,
        appa.policy.id = tracing::field::Empty,
        appa.policy.gaps = tracing::field::Empty,
        appa.policy.narrowing = tracing::field::Empty,
    ))]
    pub async fn on_tool_call_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
    ) -> Result<ToolCallDecision, EventError> {
        let started = std::time::Instant::now();
        let tool = call.tool.clone();
        let result = self.check_tool_call(call, call_id, spawn, prompt).await;
        crate::telemetry::policy(&result, &tool, started.elapsed().as_secs_f64());
        result
    }

    async fn check_tool_call(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
    ) -> Result<ToolCallDecision, EventError> {
        if let Some(id) = super::peer::held_read(&call) {
            let log = self.inner.log(&self.root)?;
            let chain = self.inner.trust_chain(&self.deployment(), &log)?;
            let basis = super::peer::held_basis(&self.inner.store, &self.root, &id, &chain)?;
            return self.propose_tool_call(call, call_id, spawn, prompt, Some(basis)).await;
        }
        let Some(files) = &self.inner.shared.files else {
            return self.propose_tool_call(call, call_id, spawn, prompt, None).await;
        };
        if !super::files::owns(&call) {
            let bound = files
                .root_is_bound(&self.inner.store, &self.root)
                .map_err(super::files::refused)?;
            if bound && super::files::bypasses_tracking(&call) {
                return Err(super::files::refused(
                    "the bound workspace refuses native filesystem and shell tools",
                ));
            }
            return self.propose_tool_call(call, call_id, spawn, prompt, None).await;
        }
        let ledger_policy = match call.cwd.as_deref() {
            Some(cwd) => files.bind(&self.inner.store, &self.root, cwd),
            None => files.store(&self.inner.store, &self.root),
        }
        .and_then(|store| store.policy())
        .map_err(super::files::refused)?;
        let (operation, path) = super::files::operation(&call)?;
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        // A family shares the ledger with every family of its deployment while the ledger opened
        // under a policy file the deployment published with the trust chain the family is judged
        // under: the ledger's labels rank against that chain.
        let followed = log.deployment();
        let shares_ledger = followed.as_str() == files.deployment
            && match self
                .inner
                .store
                .published_as(followed, &ledger_policy)
                .map_err(|error| EventError::Storage(error.to_string()))?
            {
                Some(revision) => {
                    let ledger_chain = self.inner.replay_engine(revision.value(), None)?;
                    let judging = self.inner.trust_chain(&deployment, &log)?;
                    ledger_chain.trust_chain().names().eq(judging.names())
                }
                None => false,
            };
        if !shares_ledger {
            return Err(super::files::refused(
                "the workspace ledger must belong to the trajectory's policy and rank its labels against \
                 the same trust chain",
            ));
        }
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log)?;
        let expected = policy.engine().file_dispatch(&view, &self.trajectory, &call)?;
        let key = super::files::key(&expected)?;
        let pin = match operation {
            appa_eventlog::files::FileOperation::Process => {
                if files.process_backend.is_none() {
                    return Err(super::files::refused("isolated processing is not enabled"));
                }
                let args: super::files::ProcessArgs =
                    serde_json::from_str(call.arguments.get()).map_err(super::files::refused)?;
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path, inputs) =
                        (self.trajectory.0.clone(), key.clone(), path.clone(), args.input_paths);
                    move |store| store.prepare_process(&actor, &key, &inputs, &path)
                })
                .await?
            }
            appa_eventlog::files::FileOperation::Copy | appa_eventlog::files::FileOperation::Move => {
                let args: super::files::FileTransferArgs =
                    serde_json::from_str(call.arguments.get()).map_err(super::files::refused)?;
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path, source) =
                        (self.trajectory.0.clone(), key.clone(), path.clone(), args.source_path);
                    move |store| store.prepare_transfer(&actor, &key, operation, &source, &path)
                })
                .await?
            }
            _ => {
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path) = (self.trajectory.0.clone(), key.clone(), path.clone());
                    move |store| store.prepare(&actor, &key, operation, &path)
                })
                .await?
            }
        };
        // Managed writes must not reconfigure Claude Code, Git hooks, or MCP execution.
        // Claude loads instruction files implicitly, outside the file-tool observation path.
        let moved_from = match &pin.basis {
            appa_eventlog::files::PinnedBasis::Move { source, .. } => Some(source.path.as_str()),
            _ => None,
        };
        if operation != appa_eventlog::files::FileOperation::Read
            && std::iter::once(pin.path.as_str())
                .chain(moved_from)
                .flat_map(|path| path.split('/'))
                .any(|part| {
                    matches!(
                        part,
                        ".claude" | ".git" | ".mcp.json" | ".appa" | "CLAUDE.md" | "CLAUDE.local.md"
                    )
                })
        {
            files
                .store(&self.inner.store, &self.root)
                .map_err(super::files::refused)?
                .cancel(&self.trajectory.0, &key)
                .map_err(super::files::refused)?;
            return Err(super::files::refused(
                "execution-control files are not writable in file-tracking mode",
            ));
        }
        let basis = pin.file_basis();
        let decision = self.propose_tool_call(call, call_id, spawn, prompt, Some(basis)).await;
        match &decision {
            Ok(ToolCallDecision::Allow { dispatch, .. }) => {
                if dispatch != &expected {
                    return Err(super::files::refused("dispatch changed while reserving the file"));
                }
                let log = self.inner.log(&self.root)?;
                let view = policy.rebuild_view(&log)?;
                let label = policy.engine().file_output_label(&view, dispatch)?;
                files
                    .store(&self.inner.store, &self.root)
                    .map_err(super::files::refused)?
                    .bind(&self.trajectory.0, &key, dispatch, &label)
                    .map_err(super::files::refused)?;
            }
            _ => {
                files
                    .store(&self.inner.store, &self.root)
                    .map_err(super::files::refused)?
                    .cancel(&self.trajectory.0, &key)
                    .map_err(super::files::refused)?;
            }
        }
        decision
    }

    pub(crate) async fn propose_with_basis(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        basis: appa_engine::value::FileBasis,
    ) -> Result<ToolCallDecision, EventError> {
        self.propose_tool_call(call, call_id, None, None, Some(basis)).await
    }

    /// Check a return declaration made ahead of the spawn at this trajectory's label, and answer
    /// that label: the declaration stands only there.
    pub(crate) fn declare_return_ahead(
        &self,
        arguments: &RemedyArguments,
    ) -> Result<appa_engine::label::Label, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        policy
            .engine()
            .declare_ahead(&view, &self.trajectory, arguments)
            .map_err(|detail| EventError::RemedyArguments { detail })
    }

    /// A return this trajectory declared ahead covers its next spawn, which takes it whatever
    /// its decision. A blocked single spawn applies it through the block's own declaration
    /// offer, one that asks no authority, and is proposed again, so it meets only what else
    /// blocks it. A declaration held at another label has lapsed; one no offer fits, or one a
    /// fan-out spawn meets, is not applied. The block says which.
    async fn propose_tool_call(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
        file_basis: Option<appa_engine::value::FileBasis>,
    ) -> Result<ToolCallDecision, EventError> {
        let decision = self
            .propose_once(call.clone(), call_id.clone(), spawn, prompt.clone(), file_basis.clone())
            .await?;
        let Some(held) = spawn.and_then(|_| self.inner.take_held_return(&self.root, &self.trajectory)) else {
            return Ok(decision);
        };
        let ToolCallDecision::Deny {
            feedback,
            offers,
            display,
            review,
        } = decision
        else {
            return Ok(decision);
        };
        let noted = |notice: &str, apply: &str| ToolCallDecision::Deny {
            feedback: format!("{notice}{apply}\n\n{feedback}"),
            offers: offers.clone(),
            display: display.clone(),
            review: review.clone(),
        };
        let apply = if offers.is_empty() { "" } else { APPLY_BELOW };
        if spawn == Some(SpawnKind::FanOut) {
            return Ok(noted(FAN_OUT_AHEAD, apply));
        }
        let route = match held.arguments.return_schema {
            Some(_) => appa_runtime_api::OfferedReturn::Sanitized {
                sanitizer: appa_engine::names::SanitizerName::ATTEST_SCHEMA.to_string(),
            },
            None => appa_runtime_api::OfferedReturn::AsSpoken,
        };
        let (offer, label) = {
            let log = self.inner.log(&self.root)?;
            let deployment = self.deployment();
            let policy = self.inner.resolve_policy(&deployment, &log)?;
            let view = policy.rebuild_view(&log).map_err(EventError::from)?;
            let offer = offers
                .iter()
                .filter(|offer| offer.returns.as_ref() == Some(&route))
                .filter_map(|offer| crate::engine::resolve_rendered(&log, &OfferId(offer.id.clone())))
                .find(|offer| policy.engine().offer_declares_only(&view, &self.trajectory, offer));
            (offer, policy.engine().label_of(&view, &self.trajectory))
        };
        // A lapse is told first: at a changed label no offer may fit, and the cause is the change.
        if label != Some(held.label) {
            return Ok(noted(LAPSED_AHEAD, apply));
        }
        let Some(offer) = offer else {
            return Ok(noted(UNFIT_AHEAD, apply));
        };
        // A declaration that cannot be applied leaves the spawn the block it already met.
        match self.on_remedy(offer, held.arguments, None, None).await {
            Ok(RemedyDecision::Authorized { .. }) => self.propose_once(call, call_id, spawn, prompt, file_basis).await,
            Ok(_) => Ok(noted(UNFIT_AHEAD, apply)),
            Err(error) => {
                tracing::debug!(trajectory = %self.trajectory.0, %error, "the declaration made ahead was not applied");
                Ok(noted(UNFIT_AHEAD, apply))
            }
        }
    }

    async fn propose_once(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
        file_basis: Option<appa_engine::value::FileBasis>,
    ) -> Result<ToolCallDecision, EventError> {
        self.inner.note_working_directory(&self.root, call.cwd.as_deref());
        if spawn.is_some()
            && self.inner.shared.naming.spawn_coverage() == super::SpawnCoverage::Declared
            && !self.names_tool(&call.tool)?
        {
            tracing::debug!(trajectory = %self.trajectory.0, tool = %call.tool, "spawn denied: the policy names no such agent");
            return Err(EventError::UndeclaredSpawn {
                tool: call.tool.clone(),
            });
        }
        let decision = self
            .drive_with_evidence(
                |context, mut evidence| {
                    if let Some(basis) = &file_basis {
                        evidence.push(ExternalEvidence::File { basis: basis.clone() });
                    }
                    Ok(EngineEvent::ModelResponse {
                        call: call.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        spawn,
                        replay: context.replay_batch(&call, call_id.as_deref())?,
                    })
                },
                None,
                None,
                Occasion::Proposal {
                    call_id: call_id.as_deref(),
                    prompt: prompt.as_ref(),
                },
            )
            .await?;

        match decision.then {
            Next::ModelResponse { invocations, feedback } => match (invocations.as_slice(), feedback.as_slice()) {
                ([released], []) => {
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        tool = %released.tool,
                        spawn = released.fork.is_some(),
                        "call released"
                    );
                    Ok(ToolCallDecision::Allow {
                        spawn: released.fork.clone(),
                        dispatch: released.dispatch.clone(),
                    })
                }
                ([], feedback) if !feedback.is_empty() => {
                    tracing::debug!(trajectory = %self.trajectory.0, "call blocked");
                    Ok(ToolCallDecision::Deny {
                        feedback: join_feedback(feedback),
                        offers: feedback.iter().flat_map(|entry| entry.offers.clone()).collect(),
                        display: feedback.iter().filter_map(|entry| entry.display.clone()).collect(),
                        review: join_review(feedback, &self.deployment().externals),
                    })
                }
                _ => Err(EventError::UnexpectedDecision),
            },
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// Whether the serving policy writes a contract for this tool's exact name — the
    /// wildcard does not count. What a spawn needs under [`super::SpawnCoverage::Declared`].
    fn names_tool(&self, tool: &str) -> Result<bool, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        Ok(policy.engine().names_tool(tool))
    }

    /// Close one call this trajectory has open as one that did not run.
    /// The dispatch is re-read from the view on every replay, so a
    /// contended append never closes an occurrence the winning writer
    /// already closed. Both callers reach here the same way: a released
    /// call that got no outcome before its turn or its child ended. A
    /// staged derivation opens nothing and so has nothing to close.
    async fn abandon_dispatch(
        &self,
        dispatch: appa_engine::value::DispatchId,
        outcome: &ToolOutcome,
    ) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            move |context, evidence| {
                if !context.open_dispatches().iter().any(|open| open.id == dispatch) {
                    return Err(EventError::UnknownDispatch);
                }
                Ok(EngineEvent::ToolOutcome {
                    dispatch: dispatch.clone(),
                    outcome: outcome.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
            None,
            Occasion::Other,
        )
        .await
    }

    /// Execute only the exact released runtime-owned call. Content-dependent checks happen
    /// here, never during proposal parsing or in Claude Code's native validation.
    ///
    /// The file work runs on the blocking pool: it reads and hashes files, and a Process call
    /// may run a sandboxed command for two minutes. The runtime serves hooks and MCP on this
    /// executor, so none of that belongs on an async worker.
    #[cfg(feature = "daemon")]
    pub(super) async fn execute_file(&self, call: ProposedCall) -> Result<super::files::FileReply, EventError> {
        if self.inner.shared.files.is_none() {
            return Err(super::files::refused("file tools are not enabled"));
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
        let key = super::files::key(&open.id)?;
        let pin = ledger(self.inner.clone(), self.root.clone(), {
            let (actor, key) = (self.trajectory.0.clone(), key.clone());
            move |store| store.pin_for(&actor, &key)
        })
        .await?
        .ok_or_else(|| super::files::refused("the released call holds no file reservation"))?;
        let result = {
            let inner = self.inner.clone();
            let (root, call, pin) = (self.root.clone(), call.clone(), pin.clone());
            tokio::task::spawn_blocking(move || match inner.shared.files.as_ref() {
                Some(files) => files
                    .store(&inner.store, &root)
                    .map_err(|error| error.to_string())
                    .and_then(|store| super::files::perform(files, store.workspace(), &call, &pin)),
                None => Err("file tools are not enabled".to_string()),
            })
            .await
            .map_err(|error| super::files::refused(format!("the file operation did not complete: {error}")))?
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
                return Ok(super::files::FileReply::Failure(placeholder));
            }
        };
        Ok(if succeeded {
            super::files::FileReply::Value(value)
        } else {
            super::files::FileReply::Failure(value)
        })
    }

    fn refuse_substituted_embedded_read(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        outcome: &ToolOutcome,
    ) -> Result<(), EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let open = self.report_calls(call_id)?;
        let dispatch = match classify_report_identified(
            call,
            call_id,
            || policy.engine().canonical_bytes(call),
            &open,
            log.call_bindings(),
            &self.trajectory,
        ) {
            Ok(dispatch) => dispatch,
            Err(_) => return Ok(()),
        };
        let Some(basis) = super::embedded::basis_of(&log, &self.trajectory, &dispatch) else {
            return Ok(());
        };
        let appa_engine::value::FileBasis::Read(source) = &basis else {
            return Ok(());
        };
        let Ok(id) = super::embedded::EmbeddedPeerId::parse(&source.version) else {
            return Ok(());
        };
        let Some(row) = self
            .inner
            .store
            .load_embedded_peer(&self.root, id.as_str())
            .map_err(|error| EventError::Storage(error.to_string()))?
        else {
            return Ok(());
        };
        if row.recipient != self.trajectory.as_str()
            || row.status != appa_eventlog::embedded::EmbeddedStatus::Read
            || super::embedded::message_basis(&row, policy.engine().trust_chain()) != basis
        {
            return Ok(());
        }
        let reported = match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } => Some(body.as_str()),
            _ => None,
        };
        if reported != row.body.as_deref() {
            return Err(EventError::OutcomeMismatch);
        }
        Ok(())
    }

    pub async fn on_tool_result_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        o: ToolOutcome,
    ) -> Result<ToolResultDecision, EventError> {
        self.refuse_substituted_embedded_read(&call, call_id.as_deref(), &o)?;
        if self.inner.shared.files.is_some() && super::files::owns(&call) {
            super::files::operation(&call)?;
            let log = self.inner.log(&self.root)?;
            let deployment = self.deployment();
            let policy = self.inner.resolve_policy(&deployment, &log)?;
            let open = self.report_calls(call_id.as_deref())?;
            let dispatch = classify_report_identified(
                &call,
                call_id.as_deref(),
                || policy.engine().canonical_bytes(&call),
                &open,
                log.call_bindings(),
                &self.trajectory,
            )
            .map_err(UnreportableOutcome::refusal)?;
            let key = super::files::key(&dispatch)?;
            // Preserve actual failure text: the native failure hook cannot reliably replace it.
            // A missing observation is repaired after its writer has stopped.
            let o = match o {
                ToolOutcome::Success {
                    body: OutcomeBody::Unavailable | OutcomeBody::TooLarge { .. },
                } => ToolOutcome::Success {
                    body: OutcomeBody::Available(String::new()),
                },
                other => other,
            };
            if matches!(o, ToolOutcome::Success { .. }) {
                // Verify the physical version before admitting a successful result. A later
                // trajectory append failure cannot erase this already-published file's Label
                // from the authoritative workspace event stream.
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key) = (self.trajectory.0.clone(), key.clone());
                    move |store| store.finish(&actor, &key, true)
                })
                .await?;
            }
            let decision = self.report_outcome(&call, call_id.as_deref(), &o).await?;
            match &o {
                ToolOutcome::Indeterminate => {
                    self.repair_file_reservation(&dispatch).await;
                    return Err(super::files::refused(
                        "missing outcome; no file result can be delivered",
                    ));
                }
                ToolOutcome::Failure { .. } => {
                    ledger(self.inner.clone(), self.root.clone(), {
                        let (actor, key) = (self.trajectory.0.clone(), key.clone());
                        move |store| store.finish(&actor, &key, false)
                    })
                    .await?;
                }
                ToolOutcome::Success { .. } => {}
            }
            return outcome_decision(decision, &self.deployment().externals);
        }
        let o = self.cap_outcome(o);
        outcome_decision(
            self.report_outcome(&call, call_id.as_deref(), &o).await?,
            &self.deployment().externals,
        )
    }

    async fn report_outcome(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        o: &ToolOutcome,
    ) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            |context, evidence| {
                let dispatch = context.classify_report(call, call_id)?;
                Ok(EngineEvent::ToolOutcome {
                    dispatch,
                    outcome: o.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
            None,
            Occasion::Report { call_id },
        )
        .await
    }

    /// Resume the exact suspended call without releasing a second dispatch.
    /// Rebinding the same pair inherits the parent's current label in the child.
    pub fn on_spawn_resume(&self, call: ProposedCall, child: TrajectoryId) -> Result<(), EventError> {
        let opened = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &opened)?;
        let decision = self.drive(&policy, true, Opening::default(), |context| {
            let dispatch = context.classify_resume(&call, &child)?;
            let fork = appa_engine::value::ForkId::of(&dispatch);
            match context.fork_status(&fork) {
                ForkStatus::Bound(bound) if bound == child => {}
                _ => return Err(EventError::BindingMismatch),
            }
            match context.policy.engine().liveness(context.view, &child) {
                Liveness::Live => {}
                Liveness::Ended => return Err(EventError::TrajectoryEnded),
                Liveness::Unopened => return Err(EventError::SpawnNotTaken),
            }
            Ok(EngineEvent::BindFork {
                fork,
                child: child.clone(),
            })
        })?;
        match decision.then {
            Next::Done => Ok(()),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The spawn call's own result, keyed on where its fork stands and on
    /// the child the harness names. A prepared fork binds to the named
    /// child here: the harness's acknowledgement can land before the
    /// child's start, and a parallel spawn is told apart by name. A bound
    /// fork's result closes the dispatch; a message it carries either
    /// repeats the child's latest crossed return, and replays it, or was
    /// never checked at a stop — the harness delivered content the child
    /// never returned — and is withheld from the parent with nothing
    /// admitted.
    pub async fn on_spawn_result_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> Result<SpawnResultDecision, EventError> {
        let outcome = self.cap_outcome(outcome);
        // At most two engine events: the binding, then the result on the bound fork.
        for _ in 0..2 {
            // The attempt that commits is the one whose plan the delivery below
            // follows, so each attempt overwrites what the last one wrote.
            let mut plan: Option<SpawnPlan> = None;
            let decision = self
                .drive_with_evidence(
                    |context, evidence| {
                        let dispatch = context.classify_spawn_report(&call, call_id.as_deref())?;
                        let fork = appa_engine::value::ForkId::of(&dispatch);
                        let next = match (context.fork_status(&fork), &child) {
                            _ if matches!(outcome, ToolOutcome::Indeterminate) => SpawnPlan::Outcome,
                            (ForkStatus::Unprepared | ForkStatus::FanOut, _) => SpawnPlan::Outcome,
                            (ForkStatus::Prepared, Some(child)) => SpawnPlan::Bind {
                                fork: fork.clone(),
                                child: child.clone(),
                            },
                            // A failure that names no child: the subagent ended before it
                            // returned. No child bound to a prepared fork, so that subagent
                            // never started; a bound one stopped.
                            (ForkStatus::Prepared, None) if matches!(outcome, ToolOutcome::Failure { .. }) => {
                                SpawnPlan::Close(EventError::SpawnNeverStarted)
                            }
                            (ForkStatus::Bound(_), None) if matches!(outcome, ToolOutcome::Failure { .. }) => {
                                SpawnPlan::Close(EventError::SpawnStopped)
                            }
                            (ForkStatus::Prepared, None) | (ForkStatus::Failed | ForkStatus::ParentEnded, _) => {
                                SpawnPlan::Close(EventError::SpawnNotTaken)
                            }
                            (ForkStatus::Bound(bound), Some(child)) if *child == bound => match &value {
                                None => SpawnPlan::Outcome,
                                Some(said) if context.latest_return(child).as_deref() == Some(said.as_str()) => {
                                    SpawnPlan::Replay
                                }
                                Some(_) => SpawnPlan::Withheld,
                            },
                            (ForkStatus::Bound(_), _) => SpawnPlan::Close(EventError::BindingMismatch),
                        };
                        let event = match &next {
                            SpawnPlan::Outcome => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: outcome.clone(),
                                evidence,
                                entropy: fresh_entropy(),
                            },
                            SpawnPlan::Bind { fork, child } => EngineEvent::BindFork {
                                fork: fork.clone(),
                                child: child.clone(),
                            },
                            SpawnPlan::Replay | SpawnPlan::Withheld => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: ToolOutcome::Success {
                                    body: OutcomeBody::Unavailable,
                                },
                                evidence,
                                entropy: fresh_entropy(),
                            },
                            SpawnPlan::Close(refusal) => EngineEvent::ToolOutcome {
                                dispatch,
                                outcome: ToolOutcome::Failure {
                                    message: refusal.to_string(),
                                },
                                evidence,
                                entropy: fresh_entropy(),
                            },
                        };
                        plan = Some(next);
                        Ok(event)
                    },
                    None,
                    None,
                    Occasion::Report {
                        call_id: call_id.as_deref(),
                    },
                )
                .await?;
            // The engine decided on an event this handler built from the view.
            match plan.expect("the spawn result is typed before the engine decides") {
                SpawnPlan::Outcome => {
                    return outcome_decision(decision, &self.deployment().externals).map(SpawnResultDecision::Outcome);
                }
                SpawnPlan::Close(refusal) => return Err(refusal),
                SpawnPlan::Bind { child, .. } => match decision.then {
                    Next::Done => {
                        tracing::debug!(trajectory = %self.trajectory.0, child = %child.0, "the spawn result bound its child");
                    }
                    _ => return Err(EventError::UnexpectedDecision),
                },
                SpawnPlan::Replay => {
                    outcome_decision(decision, &self.deployment().externals)?;
                    return Ok(SpawnResultDecision::Return(ChildReturnDecision::Returned {
                        value: value.expect("a replay repeats a delivered message"),
                    }));
                }
                SpawnPlan::Withheld => {
                    outcome_decision(decision, &self.deployment().externals)?;
                    return Ok(SpawnResultDecision::Return(ChildReturnDecision::Blocked {
                        feedback: UNCHECKED_RETURN.to_string(),
                    }));
                }
            }
        }
        Err(EventError::UnexpectedDecision)
    }

    /// The model called the `execute_remedy_plan` MCP tool. Executes one
    /// offer by its canonical id, which the runtime
    /// resolved from the quoted form before this point. An offer this
    /// trajectory does not pursue is refused.
    pub async fn on_remedy(
        &self,
        offer: OfferId,
        arguments: RemedyArguments,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
    ) -> Result<RemedyDecision, EventError> {
        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.offer_pursuer(&offer).as_ref() != Some(&trajectory) {
                        return Err(EventError::UnknownOffer);
                    }
                    Ok(EngineEvent::ExecuteOffer {
                        trajectory: trajectory.clone(),
                        offer: offer.clone(),
                        arguments: arguments.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        cwd: self.inner.working_directory(&self.root),
                    })
                },
                elicitation,
                ruling,
                Occasion::Remedy { offer: &offer },
            )
            .await?;

        match decision.then {
            Next::Approved { tool, bytes } => Ok(RemedyDecision::Authorized {
                call: ExactCall { tool, bytes },
            }),
            Next::PresentToModel(Presentation::Value { value }) => Ok(RemedyDecision::Returned { value }),
            Next::PresentToModel(Presentation::Declined { feedback }) => Ok(RemedyDecision::Declined {
                presentation: RemedyPresentation {
                    feedback,
                    offers: Vec::new(),
                    review: Vec::new(),
                    display: Vec::new(),
                },
            }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            Next::PresentToModel(Presentation::Blocked {
                feedback,
                offers,
                review,
                display,
            }) => Ok(RemedyDecision::Declined {
                presentation: remedy_presentation(
                    feedback,
                    offers,
                    review,
                    display.into_iter().collect(),
                    &self.deployment().externals,
                ),
            }),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// A same-family spawned child agent started. `spawn` names the prepared fork the child binds
    /// to: the [`super::SpawnBinding`] the parent's spawn release handed the harness, or — for a
    /// harness whose start signal carries no reference to the spawn call — the fork already bound
    /// to this child, else the family's one spawn in flight. The engine's `BindFork` opens the
    /// child before its first engine event; the child exists exactly when the log's `ForkOpened`
    /// does and keeps this session's root. A start for a child already bound is the parent
    /// addressing it again: the parent's current label flows into the child, and a return
    /// derivation the child never echoed is dropped. This is not `Runtime::open_root_fork`, which
    /// opens an independent conversation root without a spawn or return contract.
    ///
    /// The child's session comes with the contract the child works under: what its return must
    /// look like, where the fork's policy shapes it. Nothing for a child whose return crosses as
    /// spoken. Read from the view that binds the child, so the start rebuilds nothing twice.
    pub fn start_child(&self, id: TrajectoryId, spawn: SpawnRef) -> Result<(Session, Option<String>), EventError> {
        let child = id.clone();
        self.bind_child(id, |context| match &spawn {
            SpawnRef::Binding(binding) => {
                crate::engine::parse_fork(context.log, binding).ok_or(EventError::SpawnNotTaken)
            }
            SpawnRef::InFlight => context.in_flight_fork(&child),
            SpawnRef::FanOut(prompt) => context.fan_out_fork(&child, prompt),
        })
    }

    /// Open a child whose start hook never arrived, or has not arrived yet:
    /// bind it to the family's one spawn in flight, as its start
    /// would. Whether this opened the child or found it already open tells the
    /// dispatcher whether the refused event was the missing start's, and is
    /// worth running once more, or the child's own answer.
    /// A fan-out spawn's child names the prompt its spawn was released under, and binds there.
    pub(crate) fn open_late(&self, child: TrajectoryId, prompt: Option<&PromptKey>) -> Result<LateOpen, EventError> {
        match self.liveness_of(&child)? {
            Liveness::Unopened => {
                let id = child.clone();
                self.bind_child(id, |context| match prompt {
                    Some(prompt) => context.fan_out_fork(&child, prompt),
                    None => context.in_flight_fork(&child),
                })
                .map(|_| LateOpen::Opened)
            }
            Liveness::Live | Liveness::Ended => Ok(LateOpen::AlreadyOpen),
        }
    }

    fn bind_child(
        &self,
        id: TrajectoryId,
        fork: impl Fn(&Decided<'_>) -> Result<appa_engine::value::ForkId, EventError>,
    ) -> Result<(Session, Option<String>), EventError> {
        let child = id.clone();
        let opened = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &opened)?;
        let mut contract = None;
        let decision = self.drive(&policy, true, Opening::default(), |context| {
            let fork = fork(context)?;
            match context.fork_status(&fork) {
                ForkStatus::Unprepared | ForkStatus::Failed | ForkStatus::ParentEnded => Err(EventError::SpawnNotTaken),
                ForkStatus::Prepared | ForkStatus::Bound(_) | ForkStatus::FanOut => {
                    contract = context.fork_return_contract(&fork);
                    Ok(EngineEvent::BindFork {
                        fork,
                        child: child.clone(),
                    })
                }
            }
        })?;
        match decision.then {
            // The child rides the parent event's snapshot, not a fresh one.
            Next::Done => Ok((
                Session::attach(
                    Arc::clone(&self.inner),
                    self.deployment(),
                    id,
                    self.root.clone(),
                    EmbeddedPresentationOptions::default(),
                ),
                contract,
            )),
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The child finished its turn and returns `value`; `None` returns no
    /// value. The return is the child's only channel to the parent and is
    /// checked before it may cross; it names the fork that opened the
    /// child, recovered from the log.
    ///
    /// Any call the child still has open got no outcome hook and never
    /// will, so each closes first as unreported, exactly as the child's
    /// turn end would close it.
    ///
    /// A child may stop more than once: each stop is judged under the
    /// fork's return policy as its own crossing, and a stop repeating the
    /// latest crossed value replays it, appending nothing. A child that
    /// ended by returning nothing has nothing left to cross: an empty stop
    /// replays, a message is held until the child gives it up.
    pub async fn on_child_end(&self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        if policy.engine().liveness(&view, &self.trajectory) == Liveness::Ended {
            return Ok(match value {
                None => ChildReturnDecision::NoValue,
                Some(_) => ChildReturnDecision::Blocked {
                    feedback: ENDED_CHILD.to_string(),
                },
            });
        }
        self.settle_open_call(&policy, &view).await?;
        self.cross_return(value).await
    }

    /// A return the child makes without ending, judged exactly as a stop's return: the calls
    /// it has open stay open, and a return made beside one is refused until they report.
    pub async fn on_child_return(&self, value: String) -> Result<ChildReturnDecision, EventError> {
        self.cross_return(Some(value)).await
    }

    async fn cross_return(&self, value: Option<String>) -> Result<ChildReturnDecision, EventError> {
        let child = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.parent_of(&child).is_none() {
                        return Err(EventError::NotAChild);
                    }
                    Ok(EngineEvent::ChildReturn {
                        child: child.clone(),
                        value: value.clone(),
                        evidence,
                    })
                },
                None,
                None,
                Occasion::Other,
            )
            .await?;

        return_decision(decision)
    }

    /// Close whatever calls this child still has open before it returns. Each got no outcome
    /// hook and closes as unreported, exactly as a turn end closes it.
    async fn settle_open_call(
        &self,
        policy: &crate::engine::PolicyEngine<'_>,
        view: &EngineView,
    ) -> Result<(), EventError> {
        for open in policy.engine().open_dispatches(view, &self.trajectory) {
            match self
                .abandon_dispatch(open.id.clone(), &ToolOutcome::Indeterminate)
                .await
            {
                Ok(_) => {
                    self.repair_file_reservation(&open.id).await;
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        dispatch = ?open.id,
                        tool = %open.tool,
                        "call closed as unreported at the child's end"
                    );
                }
                Err(EventError::UnknownDispatch) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn liveness_of(&self, trajectory: &TrajectoryId) -> Result<Liveness, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log).map_err(EventError::from)?;
        Ok(policy.engine().liveness(&view, trajectory))
    }

    fn cap_outcome(&self, outcome: ToolOutcome) -> ToolOutcome {
        match outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } if body.len() > self.deployment().config.externals.max_body_bytes => ToolOutcome::Success {
                body: OutcomeBody::TooLarge {
                    limit_bytes: Some(self.deployment().config.externals.max_body_bytes),
                },
            },
            other => other,
        }
    }

    fn refuse_report(&self, case: UnreportableOutcome, call: &ProposedCall, open: &[OpenDispatch]) -> EventError {
        tracing::warn!(
            trajectory = %self.trajectory.0,
            tool = %call.tool,
            dispatch = open.first().map(|d| format!("{:?}", d.id)).unwrap_or_else(|| "-".to_string()),
            open = open.len(),
            case = case.case(),
            "an outcome report named no reportable dispatch",
        );
        case.refusal()
    }

    /// Decide one event at the head. A decision made at a revision a publish moved past does
    /// not commit ([`EventError::PolicyMoved`]); when this view now serves the head, the event
    /// is decided afresh there, consults included: an answer gathered for the earlier
    /// revision's question is not carried to the next.
    async fn drive_with_evidence(
        &self,
        mut event: impl FnMut(&Decided<'_>, Vec<ExternalEvidence>) -> Result<EngineEvent, EventError>,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<EngineDecision, EventError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let deployment = self.deployment();
            match self
                .drive_at(&deployment, &mut event, elicitation, ruling, occasion)
                .await
            {
                Err(EventError::PolicyMoved { .. })
                    if attempt < PUBLISHES_FOLLOWED && self.follow_head(&deployment) =>
                {
                    tracing::debug!(
                        root = %self.root.0,
                        attempt,
                        "a publish moved the head under the decision: deciding again at the head"
                    );
                }
                decided => return decided,
            }
        }
    }

    async fn drive_at(
        &self,
        deployment: &Deployment,
        mut event: impl FnMut(&Decided<'_>, Vec<ExternalEvidence>) -> Result<EngineEvent, EventError>,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<EngineDecision, EventError> {
        let opening = match occasion {
            Occasion::Proposal { call_id, prompt } => Opening { call_id, prompt },
            Occasion::Report { .. } | Occasion::Remedy { .. } | Occasion::Other => Opening::default(),
        };
        let opened = self.inner.log(&self.root)?;
        let policy = self.inner.resolve_policy(deployment, &opened)?;
        tracing::Span::current().record(
            "appa.policy.id",
            match policy.engine().revision() {
                Some(revision) => format!("{}@{revision}", opened.deployment()),
                None => crate::engine::policy_file_key(opened.policy_file()),
            },
        );
        // External answers carry the exact call or group they answered for, and the
        // engine matches them only while that is still the one in front of it — a
        // rewritten call is annotated afresh by construction, so this loop carries
        // evidence blindly. Every round either decides or gathers an answer the engine
        // did not hold. Rounds are finite, so a round that changes nothing is the only
        // way this loop fails to converge.
        let mut evidence: Vec<ExternalEvidence> = Vec::new();
        for _ in 0..RESOLUTION_ROUNDS {
            let carried = evidence.clone();
            let entering = carried.is_empty();
            let decision = self.drive(&policy, entering, opening, |context| event(context, carried.clone()))?;
            match decision.then {
                // The engine batches every missing answer into one request set, and evidence
                // is matched by name, never by position. Without a reviewer the
                // consults run concurrently — an annotation consult can take a model call's
                // seconds, and the batch should cost its slowest member, not their sum. With a
                // reviewer they stay serial: one staged review on screen at a time.
                Next::ResolveExternal(requests) => match elicitation {
                    None => {
                        // Batch-terminal: every sibling settles first; any no-answer
                        // then aborts the invocation, discarding the siblings' answers,
                        // before another engine round or any append.
                        // Embedded hosts supply the ruling outside an MCP elicitation context.
                        let consults = requests
                            .into_iter()
                            .map(|request| self.consult(request, None, ruling, occasion));
                        for answered in crate::external::settle_batch(consults).await {
                            evidence.push(answered?);
                        }
                    }
                    Some(_) => {
                        for request in requests {
                            let answered = self.consult(request, elicitation, ruling, occasion).await?;
                            evidence.push(answered);
                        }
                    }
                },
                _ => return Ok(decision),
            }
            if evidence.len() == carried.len() {
                return Err(EventError::UnexpectedDecision);
            }
        }
        // Every round is supposed to close at least one ask for good; a run this long is a
        // gathering bug or a hostile external, and the invocation refuses operationally
        // rather than hold the turn lease against a source forever.
        Err(EventError::ResolutionDiverged {
            rounds: RESOLUTION_ROUNDS,
        })
    }

    fn drive(
        &self,
        policy: &crate::engine::PolicyEngine<'_>,
        entering: bool,
        opening: Opening<'_>,
        mut event: impl FnMut(&Decided<'_>) -> Result<EngineEvent, EventError>,
    ) -> Result<EngineDecision, EventError> {
        for attempt in 1..=REPLAY_LIMIT {
            // The read, the decision and the append are one step: the family's other writers
            // wait for it rather than lose their compare-and-swap to it.
            let step = self.inner.step(&self.root)?;
            let log = self.inner.step_log(&step, &self.root)?;
            let view = policy.rebuild_view(&log).map_err(EventError::from)?;
            let context = Decided {
                session: self,
                policy,
                view: &view,
                log: &log,
            };
            if entering {
                match policy.engine().liveness(&view, &self.trajectory) {
                    Liveness::Ended => return Err(EventError::TrajectoryEnded),
                    Liveness::Unopened => return Err(EventError::SpawnNotTaken),
                    Liveness::Live => {}
                }
            }
            let event = event(&context)?;
            if let EngineEvent::ChildReturn { child, .. } = &event
                && !policy.engine().open_dispatches(&view, child).is_empty()
            {
                return Err(EventError::ChildDispatchOpen);
            }
            let decision = policy
                .engine()
                .handle(&view, &self.trajectory, event, &self.presentation)
                .map_err(EventError::from)?;

            let Some(facts) = decision.append.as_ref() else {
                return Ok(decision);
            };
            let opens_dispatch = facts.iter().find_map(|fact| match fact {
                appa_engine::fact::Fact::DispatchOpened { dispatch, .. }
                    if dispatch.trajectory() == &self.trajectory =>
                {
                    Some(dispatch.clone())
                }
                _ => None,
            });
            // Only a batch that opens a dispatch can be the second one. An outcome closes a
            // dispatch and drives the count down, so refusing it would leave a parallel batch
            // with no way to drain: every report would be refused for the calls it is settling.
            //
            // Parallel calls need every one of them to carry an identity, not just the newest.
            // An unidentified call already open cannot be told apart from this one when its
            // outcome arrives, so the second dispatch is refused whichever end is unidentified.
            // An identified spawn is not counted: its result comes under its own call id.
            if opens_dispatch.is_some() {
                let spawns: Vec<_> = context
                    .open_dispatches()
                    .into_iter()
                    .filter(|open| context.identified_spawn(open))
                    .map(|open| open.id)
                    .collect();
                if policy
                    .engine()
                    .opens_a_second_dispatch(&view, &self.trajectory, facts, &spawns)
                    && (opening.call_id.is_none() || context.has_unbound_open_dispatch())
                {
                    return Err(EventError::CallOutstanding);
                }
            }
            let released = facts.iter().find_map(|fact| match fact {
                appa_engine::fact::Fact::ForkPrepared { kind, .. } => Some(*kind),
                _ => None,
            });
            // A single spawn binds whichever child starts next, so one waits at a time. A
            // fan-out spawn's children bind by the prompt it was released under, so it waits
            // only on another fan-out of the same prompt.
            let fan_out_prompt = match (opens_dispatch.is_some(), released) {
                (true, Some(appa_engine::transition::SpawnKind::Single))
                    if !policy.engine().forks_in_flight(&view).is_empty() =>
                {
                    return Err(EventError::SpawnOutstanding);
                }
                (true, Some(appa_engine::transition::SpawnKind::FanOut)) => match (opening.call_id, opening.prompt) {
                    (Some(_), Some(prompt)) if context.open_fan_out(prompt).is_some() => {
                        return Err(EventError::FanOutOutstanding);
                    }
                    (Some(_), Some(prompt)) => Some(prompt.0.clone()),
                    _ => return Err(EventError::FanOutUnkeyed),
                },
                _ => None,
            };
            let host_record = match (opening.call_id, opens_dispatch) {
                (Some(call_id), Some(dispatch)) => {
                    if call_id.is_empty()
                        || log
                            .call_bindings()
                            .any(|binding| *binding.trajectory == self.trajectory && binding.call_id == call_id)
                    {
                        return Err(EventError::CallIdReused);
                    }
                    Some(appa_eventlog::HostObservation::CallBound {
                        trajectory: self.trajectory.clone(),
                        call_id: call_id.to_string(),
                        dispatch,
                        prompt: fan_out_prompt,
                    })
                }
                // A decided proposal batch that opens nothing denied the call: bind the host
                // id to the decision's batch identity, so a repeat of this delivery is
                // answered from the record rather than decided again with fresh offers.
                (Some(call_id), None) if !call_id.is_empty() => facts.iter().find_map(|fact| match fact {
                    appa_engine::fact::Fact::ProposalBatchDecided { batch, released, .. } if released.is_empty() => {
                        Some(appa_eventlog::HostObservation::CallDenied {
                            trajectory: self.trajectory.clone(),
                            call_id: call_id.to_string(),
                            batch: batch.as_str().to_string(),
                        })
                    }
                    _ => None,
                }),
                _ => None,
            };
            let binds_read = facts.iter().any(|fact| {
                matches!(
                    fact,
                    appa_engine::fact::Fact::DispatchOpened { .. } | appa_engine::fact::Fact::ValueAdmitted { .. }
                )
            });
            let hold = super::embedded::EMBEDDED_READ_HOLD
                .try_with(|hold| hold.clone())
                .ok()
                .filter(|hold| hold.root == self.root.as_str());
            // A batch an engine at a revision decided carries that revision on its first record,
            // and commits only while the revision is still the deployment's head.
            let appended = if let Some(hold) = hold {
                step.append_holding_embedded_read(
                    &log,
                    facts,
                    host_record.as_ref(),
                    &appa_eventlog::embedded::EmbeddedAppendHold {
                        id: &hold.id,
                        call_id: &hold.call_id,
                        generation: hold.generation,
                        bind: binds_read,
                    },
                )
            } else {
                step.append(&log, facts, host_record.as_ref())
            };
            match appended {
                Ok(()) => {
                    self.inner.shared.spawn_holds.appended();
                    return Ok(decision);
                }
                Err(appa_eventlog::AppendError::ReadClaimLost) => {
                    return Err(EventError::OutcomeMismatch);
                }
                // A publish moved the head after this event read it: the decision was made at a
                // revision that no longer decides. Nothing was written; the event decides again
                // once this view serves the head.
                Err(appa_eventlog::AppendError::PolicyMoved { head }) => {
                    return Err(EventError::PolicyMoved {
                        deployment: self.inner.line().to_string(),
                        head: head.map_or_else(|| "none".to_string(), |head| head.to_string()),
                    });
                }
                Err(appa_eventlog::AppendError::Conflict { .. }) => {
                    tracing::debug!(
                        root = %self.root.0,
                        attempt,
                        "another writer won the append: discarding the decision and replaying the event"
                    );
                    continue;
                }
                Err(error) => {
                    self.inner
                        .note_store_error(Some(&self.root), crate::events::StoreOperation::Append, &error);
                    return Err(EventError::Storage(error.to_string()));
                }
            }
        }
        Err(EventError::Contended { attempts: REPLAY_LIMIT })
    }

    /// Run one consult, timed, and note what it cost.
    ///
    /// Every consult in this file goes through here rather than calling `externals.consult`
    /// directly. An external that is slow, unreachable, or answering nonsense is the single
    /// most common reason an agent appears to be stuck for no reason the trajectory's facts
    /// explain, and the duration is only knowable at the await.
    ///
    /// With a recorder attached, the consult is also transcribed and its record handed over
    /// once the outcome is known. `call` is the canonical call an annotation consult judges.
    #[tracing::instrument(target = "appa_telemetry", name = "appa.external.call", skip_all, fields(
        appa.trajectory.root = crate::telemetry::name(&self.root.0),
        appa.trajectory.id = crate::telemetry::name(&self.trajectory.0),
        appa.external.name = crate::telemetry::name(&consult.name),
    ))]
    async fn timed_consult(
        &self,
        consult: &Consult,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
        call: Option<&appa_engine::value::CanonicalDigest>,
    ) -> crate::external::ConsultOutcome {
        let started_at = std::time::SystemTime::now();
        let started = std::time::Instant::now();
        let deployment = self.deployment();
        let externals = &deployment.externals;
        let consulting = async {
            match &self.inner.recorder {
                // Boxed: the transport future is large, and every drive awaits it deep in the stack.
                Some(_) => Box::pin(externals.consult_transcribed(consult, elicitation, ruling)).await,
                None => (Box::pin(externals.consult(consult, elicitation, ruling)).await, None),
            }
        };
        #[cfg(feature = "archestra")]
        let (outcome, transcript) = {
            let attribution = crate::model::archestra::Attribution {
                root: self.root.clone(),
                trajectory: self.trajectory.clone(),
                call_id: occasion.call_id().map(str::to_owned),
            };
            crate::model::archestra::within(self.inner.archestra_context.as_deref(), attribution, consulting).await
        };
        #[cfg(not(feature = "archestra"))]
        let (outcome, transcript) = consulting.await;
        // A transport that read on for the record alone says when the outcome was known.
        let settled = transcript
            .as_ref()
            .and_then(|transcript| transcript.settled)
            .unwrap_or_else(std::time::Instant::now);
        let duration_ms = u64::try_from(settled.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX);
        // Filed under the family, never the acting trajectory: a subagent's slow authority is
        // part of its family's account, and `EventLog` reads one root's bucket.
        self.inner.record(
            Some(&self.root),
            crate::events::RuntimeEvent::External {
                role: consult.kind().into(),
                name: consult.name.clone(),
                outcome: (&outcome).into(),
                duration_ms,
                offer: None,
                dispatch: None,
            },
        );
        if let (Some(recorder), Some(transcript)) = (&self.inner.recorder, transcript) {
            // The host's code: whatever it does, the outcome below stands.
            let recorded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let context = ConsultContext {
                    root: self.root.0.clone(),
                    trajectory: self.trajectory.0.clone(),
                    call_id: occasion.call_id().map(str::to_string),
                    offer_id: match occasion {
                        Occasion::Remedy { offer } => Some(offer.0.clone()),
                        Occasion::Proposal { .. } | Occasion::Report { .. } | Occasion::Other => None,
                    },
                    call_digest: call.map(|call| crate::engine::hex(call.bytes())),
                };
                if let Some(record) =
                    ConsultRecord::new(consult, &outcome, transcript, started_at, duration_ms, context)
                {
                    recorder.record(record);
                }
            }));
            if recorded.is_err() {
                tracing::warn!(
                    external = consult.name,
                    "the consult recorder panicked; the record is dropped"
                );
            }
        }
        outcome
    }

    /// One external consult. Only an annotation failure is an error: the call cannot be
    /// judged without its annotation, no fact may be appended, and the refusal is operational
    /// — never a policy denial. Every other external keeps its no-answer evidence shape.
    async fn consult(
        &self,
        request: ExternalRequest,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<ExternalEvidence, EventError> {
        Ok(match &request {
            ExternalRequest::Authority {
                authority,
                declaration,
                artifact,
                review,
            } => {
                let consult = Consult {
                    name: authority.clone(),
                    body: ConsultBody::Authority {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let verdict = match self.timed_consult(&consult, elicitation, ruling, occasion, None).await {
                    ConsultOutcome::Answer(answer) => AuthorityVerdict::from_wire(&answer),
                    ConsultOutcome::NoAnswer(crate::external::NoAnswerReason::Unreachable) => {
                        AuthorityVerdict::Abstain(Abstention::Unreachable)
                    }
                    ConsultOutcome::NoAnswer(crate::external::NoAnswerReason::Unregistered) => {
                        AuthorityVerdict::Abstain(Abstention::Unregistered)
                    }
                    ConsultOutcome::NoAnswer(_) => AuthorityVerdict::Abstain(Abstention::Unanswered),
                };
                ExternalEvidence::Authority {
                    authority: authority.clone(),
                    verdict,
                    review: review.clone(),
                }
            }
            ExternalRequest::Sanitizer {
                sanitizer,
                source,
                declaration,
                artifact,
            } => {
                let consult = Consult {
                    name: sanitizer.clone(),
                    body: ConsultBody::Sanitizer {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let derived = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => SanitizerAnswer::from_wire(&answer).map(|answer| answer.body),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Sanitizer {
                    sanitizer: sanitizer.clone(),
                    source: *source,
                    derived,
                }
            }
            ExternalRequest::Annotation {
                annotator,
                call,
                declaration,
                args,
                context,
            } => {
                // Every context provider answers first, together; one that does not leaves
                // an error in the context and the Annotator is still asked.
                let deployment = self.deployment();
                let consults = deployment.externals.context_consults(context);
                let context = gather_context(&consults, |consult| {
                    self.timed_consult(consult, None, None, occasion, Some(call))
                })
                .await;
                let declaration = declaration
                    .clone()
                    .with_context_hints(&context, &deployment.config.externals.context_hints);
                let consult = Consult {
                    name: annotator.clone(),
                    body: ConsultBody::Annotation {
                        declaration: declaration.clone(),
                        artifact: AnnotationArtifact {
                            args: args.clone(),
                            context: context.clone(),
                        },
                    },
                };
                let answer = match self.timed_consult(&consult, None, None, occasion, Some(call)).await {
                    ConsultOutcome::Answer(answer) => AnnotationAnswer::from_wire(&answer, &declaration)
                        .map_err(crate::external::NoAnswerReason::MalformedAnswer),
                    ConsultOutcome::NoAnswer(reason) => Err(reason),
                };
                // Annotation failure is an operational refusal, never model feedback: the
                // call is not judged, nothing is appended, and the harness fails closed.
                let answer = match answer {
                    Ok(answer) => answer,
                    Err(reason) => {
                        match reason {
                            crate::external::NoAnswerReason::Unreachable | crate::external::NoAnswerReason::Timeout => {
                                tracing::warn!(annotator, ?reason, "an annotation consult produced no answer")
                            }
                            _ => tracing::debug!(annotator, ?reason, "an annotation consult produced no answer"),
                        }
                        return Err(EventError::annotation_refused(annotator.clone(), reason.diagnostic()));
                    }
                };
                ExternalEvidence::Annotation {
                    annotator: annotator.clone(),
                    // The evidence names the exact call it answered for: a rewritten call
                    // never consumes a stale annotation.
                    call: *call,
                    answer,
                    context,
                }
            }
            ExternalRequest::AudienceSource {
                provider,
                selector,
                templates,
            } => {
                let consult = Consult::audience_selector(provider, selector, templates.clone());
                let members = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => MembersAnswer::from_wire(&answer)
                        .map(|answer| answer.members.into_iter().map(ReaderId::new).collect())
                        .ok_or(crate::events::NoAnswerClass::Malformed),
                    ConsultOutcome::NoAnswer(reason) => Err((&reason).into()),
                };
                ExternalEvidence::AudienceSource {
                    provider: provider.clone(),
                    selector: selector.clone(),
                    members,
                }
            }
            ExternalRequest::MemberLookup {
                provider,
                member,
                answering,
                templates,
            } => {
                // The evidence stays keyed by the member's own provider, whichever entry
                // answered.
                let consult = Consult::member_lookup(answering, member, templates.clone());
                let principal = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => LookupAnswer::from_wire(&answer)
                        .map(|answer| answer.principal.map(ReaderId::new))
                        .ok_or(crate::events::NoAnswerClass::Malformed),
                    ConsultOutcome::NoAnswer(reason) => Err((&reason).into()),
                };
                ExternalEvidence::MemberLookup {
                    provider: provider.clone(),
                    member: member.clone(),
                    principal,
                }
            }
        })
    }
}

/// What one attempt of an event may read before it decides: the log as this
/// attempt rebuilt it. A branch's parent, the dispatch it has open, the
/// trajectory an offer belongs to — all are answered from here, so a
/// replay after a lost race reads the state that actually won rather
/// than the state it first saw.
pub(crate) struct Decided<'a> {
    session: &'a Session,
    policy: &'a crate::engine::PolicyEngine<'a>,
    view: &'a EngineView,
    log: &'a appa_eventlog::Log,
}

impl Decided<'_> {
    fn engine(&self) -> &crate::engine::RuntimeEngine {
        self.policy.engine()
    }

    fn open_dispatches(&self) -> Vec<OpenDispatch> {
        self.engine().open_dispatches(self.view, &self.session.trajectory)
    }

    fn identified_spawn(&self, open: &OpenDispatch) -> bool {
        identified_spawn(
            self.engine(),
            self.view,
            self.log.call_bindings(),
            &self.session.trajectory,
            open,
        )
    }

    fn canonical_bytes(&self, call: &ProposedCall) -> Option<Vec<u8>> {
        self.engine().canonical_bytes(call)
    }

    fn classify_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        // A report without a call id never names an identified spawn: its result comes under
        // its own call id.
        let named: Vec<OpenDispatch> = match call_id {
            Some(_) => open.clone(),
            None => open
                .iter()
                .filter(|open| !self.identified_spawn(open))
                .cloned()
                .collect(),
        };
        self.classify_among(call, call_id, &named, &open)
    }

    /// The spawn a resume names. A resume carries no call id, so the child it names picks
    /// the spawn whose fork is bound to it, when the call matches that spawn byte for byte;
    /// with no such spawn open, the resume is classified as any spawn's report.
    fn classify_resume(
        &self,
        call: &ProposedCall,
        child: &TrajectoryId,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        let bound: Vec<OpenDispatch> = open
            .iter()
            .filter(|open| {
                matches!(
                    self.fork_status(&appa_engine::value::ForkId::of(&open.id)),
                    ForkStatus::Bound(bound) if bound == *child
                )
            })
            .cloned()
            .collect();
        match bound.as_slice() {
            [spawn] if is_open_call(call, || self.canonical_bytes(call), spawn) => Ok(spawn.id.clone()),
            [_] => Err(self
                .session
                .refuse_report(UnreportableOutcome::ByteMismatch, call, &open)),
            _ => self.classify_spawn_report(call, None),
        }
    }

    /// A spawn's own report: its result, or a resume that names it by the call alone. Every
    /// open dispatch can be the one it names.
    fn classify_spawn_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        let open = self.open_dispatches();
        self.classify_among(call, call_id, &open, &open)
    }

    fn classify_among(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
        named: &[OpenDispatch],
        open: &[OpenDispatch],
    ) -> Result<appa_engine::value::DispatchId, EventError> {
        match classify_report_identified(
            call,
            call_id,
            || self.canonical_bytes(call),
            named,
            self.log.call_bindings(),
            &self.session.trajectory,
        ) {
            Ok(dispatch) => Ok(dispatch),
            // A bound id whose dispatch left the open set is the duplicate every crash
            // recovery produces: the report belongs to the closed occurrence, and the
            // engine re-answers it from the record. Only a byte-exact repeat is one.
            Err(UnreportableOutcome::NoOpenDispatch) => match self.classify_closed_report(call, call_id) {
                Some(dispatch) => Ok(dispatch),
                None => Err(self
                    .session
                    .refuse_report(UnreportableOutcome::NoOpenDispatch, call, open)),
            },
            Err(case) => Err(self.session.refuse_report(case, call, open)),
        }
    }

    /// The closed dispatch a repeated report belongs to: the one the host's call id is bound
    /// to, when the report carries that call byte for byte. Anything else — an unknown id, an
    /// id bound to different bytes — is no replay and classifies nothing.
    fn classify_closed_report(
        &self,
        call: &ProposedCall,
        call_id: Option<&str>,
    ) -> Option<appa_engine::value::DispatchId> {
        let call_id = call_id?;
        let binding = self
            .log
            .call_bindings()
            .find(|binding| *binding.trajectory == self.session.trajectory && binding.call_id == call_id)?;
        if self.open_dispatches().iter().any(|open| open.id == *binding.dispatch) {
            return None;
        }
        let recorded = self.engine().recorded_dispatch_call(self.view, binding.dispatch)?;
        is_open_call(call, || self.canonical_bytes(call), &recorded).then(|| binding.dispatch.clone())
    }

    /// The batch identity this proposal was already decided under, when the host's call id
    /// names a recorded decision: the batch that released the bound dispatch, or the batch a
    /// denial was recorded under while its block still stands. A bound id naming different
    /// bytes is a reused id, refused; a denied id whose block no longer stands is a fresh
    /// proposal, so the deny → remedy → re-propose round decides anew.
    fn replay_batch(&self, call: &ProposedCall, call_id: Option<&str>) -> Result<Option<ProposalBatchId>, EventError> {
        let Some(call_id) = call_id else {
            return Ok(None);
        };
        if let Some(binding) = self
            .log
            .call_bindings()
            .find(|binding| *binding.trajectory == self.session.trajectory && binding.call_id == call_id)
        {
            let recorded = self
                .engine()
                .recorded_dispatch_call(self.view, binding.dispatch)
                .ok_or_else(|| EventError::EngineInvariant("a bound call names no recorded dispatch".to_string()))?;
            if !is_open_call(call, || self.canonical_bytes(call), &recorded) {
                return Err(EventError::CallIdReused);
            }
            let batch = self
                .engine()
                .decided_batch_of(self.view, binding.dispatch)
                .ok_or_else(|| EventError::EngineInvariant("a bound call names no decided batch".to_string()))?;
            return Ok(Some(batch));
        }
        if let Some(batch) = self.log.denied_call_batch(&self.session.trajectory, call_id) {
            let batch = ProposalBatchId::new(batch);
            if self.engine().denial_stands(self.view, &batch) {
                return Ok(Some(batch));
            }
        }
        Ok(None)
    }

    /// Is a call this trajectory has open one the harness gave no identity for? Its outcome can
    /// only be matched by tool and bytes, which cannot tell two calls apart, so nothing else may
    /// open beside it.
    fn has_unbound_open_dispatch(&self) -> bool {
        self.open_dispatches().iter().any(|open| {
            !self
                .log
                .call_bindings()
                .any(|binding| *binding.trajectory == self.session.trajectory && *binding.dispatch == open.id)
        })
    }

    fn parent_of(&self, child: &TrajectoryId) -> Option<TrajectoryId> {
        self.engine().parent_of(self.view, child)
    }

    fn offer_pursuer(&self, offer: &OfferId) -> Option<TrajectoryId> {
        self.engine().offer_pursuer(self.view, offer)
    }

    /// What a child bound to `fork` is told at its start, where the fork's policy shapes its return.
    fn fork_return_contract(&self, fork: &appa_engine::value::ForkId) -> Option<String> {
        self.engine()
            .fork_return_contract(self.view, &self.session.trajectory, fork)
    }

    fn fork_status(&self, fork: &appa_engine::value::ForkId) -> ForkStatus {
        self.engine().fork_status(self.view, fork)
    }

    fn latest_return(&self, child: &TrajectoryId) -> Option<String> {
        self.engine().latest_return(self.view, child)
    }

    /// The fan-out spawn released under this prompt that still binds children: its fork
    /// prepared, its call not reported finished, its parent live.
    fn open_fan_out(&self, prompt: &PromptKey) -> Option<appa_engine::value::ForkId> {
        let settled = self.log.settled_calls();
        self.log
            .call_bindings()
            .filter(|binding| binding.prompt == Some(prompt.0.as_str()))
            .filter(|binding| !settled.contains(&(binding.trajectory, binding.call_id)))
            .map(|binding| appa_engine::value::ForkId::of(binding.dispatch))
            .find(|fork| matches!(self.fork_status(fork), ForkStatus::FanOut))
    }

    /// The fork a fan-out spawn's child binds to: the one it already has, else a new member
    /// of the fan-out released under its prompt.
    fn fan_out_fork(&self, child: &TrajectoryId, prompt: &PromptKey) -> Result<appa_engine::value::ForkId, EventError> {
        match self.engine().fork_of(self.view, child) {
            Some(fork) => Ok(fork),
            None => self.open_fan_out(prompt).ok_or(EventError::SpawnNotTaken),
        }
    }

    fn in_flight_fork(&self, child: &TrajectoryId) -> Result<appa_engine::value::ForkId, EventError> {
        if let Some(fork) = self.engine().fork_of(self.view, child) {
            return Ok(fork);
        }
        match self.engine().forks_in_flight(self.view).as_slice() {
            [fork] => Ok(fork.clone()),
            [] => Err(EventError::SpawnNotTaken),
            _ => Err(EventError::SpawnAmbiguous),
        }
    }
}

/// What the context providers answered about one call, the providers asked together. A
/// provider that answers `null` — the call is not its concern — is left out; one that gives
/// no answer is recorded with why, so the Annotator judges without that fact.
pub(super) async fn gather_context<'a, Asked>(
    consults: &'a [Consult],
    ask: impl Fn(&'a Consult) -> Asked,
) -> AnnotationContext
where
    Asked: std::future::Future<Output = ConsultOutcome>,
{
    let mut asked = Vec::with_capacity(consults.len());
    for consult in consults {
        asked.push(ask(consult));
    }
    let outcomes = crate::external::settle_batch(asked).await;
    let entries = consults
        .iter()
        .zip(outcomes)
        .filter_map(|(consult, outcome)| {
            let entry = match outcome {
                ConsultOutcome::Answer(serde_json::Value::Null) => return None,
                ConsultOutcome::Answer(answer) => ContextEntry::Answer(answer),
                ConsultOutcome::NoAnswer(reason) => {
                    tracing::debug!(
                        provider = consult.name,
                        ?reason,
                        "a context provider produced no answer"
                    );
                    ContextEntry::Error(reason.diagnostic())
                }
            };
            Some((ContextProviderName::new(consult.name.clone()), entry))
        })
        .collect();
    AnnotationContext::new(entries)
}

fn remedy_presentation(
    feedback: String,
    offers: Vec<appa_runtime_api::OfferedRemedy>,
    pending: Vec<PendingReview>,
    display: Vec<super::RemedyDisplay>,
    externals: &crate::external::ExternalServices,
) -> RemedyPresentation {
    RemedyPresentation {
        feedback,
        offers,
        review: reviews(&pending, externals),
        display,
    }
}

/// The reviews a harness with its own channel shows: one per offer whose plan consults a
/// `hitl` authority, the texts of several such authorities joined. Authorities of every
/// other backend answer themselves and need no person here.
fn join_review(feedback: &[Feedback], externals: &crate::external::ExternalServices) -> Vec<appa_runtime_api::Review> {
    reviews(
        &feedback
            .iter()
            .flat_map(|entry| entry.review.iter())
            .cloned()
            .collect::<Vec<_>>(),
        externals,
    )
}

pub(super) fn reviews(
    pending_reviews: &[PendingReview],
    externals: &crate::external::ExternalServices,
) -> Vec<appa_runtime_api::Review> {
    let mut reviews: Vec<appa_runtime_api::Review> = Vec::new();
    for pending in pending_reviews {
        if !externals.is_hitl(&pending.authority) {
            continue;
        }
        match reviews.iter_mut().find(|review| review.offer == pending.offer.0) {
            Some(review) => {
                review.text.push_str("\n\n");
                review.text.push_str(&pending.text);
            }
            None => reviews.push(appa_runtime_api::Review {
                offer: pending.offer.0.clone(),
                text: pending.text.clone(),
            }),
        }
    }
    reviews
}

fn join_feedback(feedback: &[Feedback]) -> String {
    feedback
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fixture arguments, from a `json!` value to the bytes a harness would
/// have sent. Production never takes this direction — the adapter holds
/// the harness's bytes already — so this is a test helper and not a
/// constructor on `ProposedCall`, which would invite the parse this
/// change exists to remove.
#[cfg(test)]
pub(crate) fn raw(value: serde_json::Value) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&value).expect("the fixture serializes")
}
#[cfg(test)]
mod real_engine_tests;

/// Writers of one family on PostgreSQL, each driving its own event at the same moment
/// through the runtime's shared store: the steps take turns on the family's row, so none
/// loses its append, and no connection is held past its step.
#[cfg(all(test, feature = "postgres"))]
mod postgres_family_writers;
