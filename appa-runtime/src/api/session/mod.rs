//! One `Session` per trajectory: the six event handlers, each one
//! engine interaction.

use std::sync::Arc;

use crate::elicit::Elicitation;

use crate::consult::{
    AnnotationAnswer, AnnotationArtifact, Consult, ConsultBody, LookupAnswer, MembersAnswer, SanitizerAnswer,
};
use crate::engine::{
    Abstention, AuthorityVerdict, EngineDecision, EngineEvent, EngineView, ExternalEvidence, ExternalRequest,
    ForkStatus, Liveness, Next, OfferNonce, OpenDispatch, PendingReview, Presentation, RemedyArguments,
};
use crate::external::ConsultOutcome;
use appa_engine::contract::{AnnotationContext, ContextEntry};
use appa_engine::label::ReaderId;
use appa_engine::names::ContextProviderName;
use appa_engine::transition::{ProposalBatchId, ReturnClass, ReturnDecision, ReturnKind, SpawnMark};
use appa_runtime_api::{DeclaredReturn, ReturnOperation};
use std::collections::BTreeSet;

use super::host::AheadState;
use super::{
    ChildReturnDecision, ConsultContext, ConsultRecord, Deployment, EmbeddedPresentationOptions, EventError, ExactCall,
    Inner, OfferId, OutcomeBody, PromptKey, ProposedCall, RemedyDecision, RemedyPresentation, ReturnReference,
    SettlementDecision, SpawnKind, SpawnRef, SpawnResultDecision, ToolCallDecision, ToolOutcome, ToolResultDecision,
    TrajectoryId,
};

mod call;
mod child;
mod consult;
mod drive;
mod remedy;
mod result;

use drive::*;
use remedy::*;
use result::*;

pub(crate) use child::LateOpen;

/// Whether a stop was taken, or held on the host call ids of calls still running at it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Stop<T> {
    Taken(T),
    Held(Vec<String>),
}
#[cfg(feature = "daemon")]
pub(super) use consult::gather_context;
pub(super) use drive::RESOLUTION_ROUNDS;
#[cfg(feature = "daemon")]
pub(super) use remedy::reviews;

/// The runtime's own control tool, recognized by its one canonical
/// identity, `appa/execute_remedy_plan`: the served adapter identifies it
/// from the host's registered spelling of the runtime's MCP server, and
/// nothing else is identified as it. Selecting an offer is not a checked flow. A
/// lookalike on another server — say `mcp/evil/execute_remedy_plan` —
/// is an ordinary checked call.
pub(crate) fn is_control_tool(tool: &str) -> bool {
    tool == appa_runtime_api::CONTROL_TOOL
}

/// The event a drive runs for, as far as its consults' records join it: the host's call
/// id, or the offer being executed. A proposal's call id also binds the dispatch it opens,
/// and its prompt keys a fan-out spawn it releases.
#[derive(Debug, Clone, Copy)]
enum Occasion<'a> {
    Proposal {
        call_id: Option<&'a str>,
        prompt: Option<&'a PromptKey>,
        spawn: Option<SpawnKind>,
        /// `None` once this invocation's first decision took no declaration: only that
        /// decision takes one.
        invocation: Option<appa_eventlog::InvocationNonce>,
        took: &'a Took,
    },
    Report {
        call_id: Option<&'a str>,
    },
    Remedy {
        offer: &'a OfferId,
    },
    /// The declaration a spawn took, applied through its block's offer: a remedy whose
    /// approval also settles the declaration as applied.
    AheadApply {
        offer: &'a OfferId,
        claim: &'a AheadClaim<'a>,
    },
    /// A child's return without ending, under the host's identity for it.
    Return(KeyedReturn<'a>),
    Other,
}

/// A child's return without ending the host named by an operation: what a retry of the
/// operation is answered from, and what a deferral of the return binds.
#[derive(Debug, Clone, Copy)]
pub(super) struct KeyedReturn<'a> {
    operation: &'a ReturnOperation,
    value: &'a str,
}

/// The declaration ahead a blocked spawn took, and what its judgment reads: whether the spawn
/// starts several subagents, and the offers its block made.
#[derive(Debug)]
pub(super) struct AheadClaim<'a> {
    attempt: &'a appa_eventlog::AheadAttempt,
    spawn: SpawnKind,
    offers: &'a [appa_runtime_api::OfferedRemedy],
}

impl<'a> Occasion<'a> {
    fn call_id(self) -> Option<&'a str> {
        match self {
            Occasion::Proposal { call_id, .. } | Occasion::Report { call_id } => call_id,
            Occasion::Remedy { .. } | Occasion::AheadApply { .. } | Occasion::Return(_) | Occasion::Other => None,
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
        self.close_turn(self.turn_calls()?).await
    }

    /// [`Session::on_turn_end`] for a harness that reports outcomes by call id after the
    /// stop: held while a call the turn end would close is not in `answered`.
    pub(crate) async fn on_turn_end_holding(&self, answered: &BTreeSet<String>) -> Result<Stop<()>, EventError> {
        let open = self.turn_calls()?;
        match self.unanswered(&open, answered)?.as_slice() {
            [] => self.close_turn(open).await.map(Stop::Taken),
            running => Ok(Stop::Held(running.to_vec())),
        }
    }

    async fn close_turn(&self, open: Vec<OpenDispatch>) -> Result<(), EventError> {
        if open.is_empty() {
            tracing::debug!(trajectory = %self.trajectory.0, "no call outstanding");
            return Ok(());
        }
        self.close_unreported(open, "call closed as unreported").await
    }

    /// Close each call as `Indeterminate` and repair its file reservation, in order. A call
    /// another writer closed first is skipped.
    async fn close_unreported(&self, open: Vec<OpenDispatch>, closed: &str) -> Result<(), EventError> {
        for dispatch in open {
            match self.abandon_dispatch(dispatch.id.clone()).await {
                Ok(()) => {
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        dispatch = ?dispatch.id,
                        tool = %dispatch.tool,
                        "{closed}",
                    );
                }
                Err(EventError::UnknownDispatch) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// The reader this session's family acts for, as its opening pinned it.
    pub(crate) fn principal(&self) -> Result<Option<ReaderId>, EventError> {
        self.with_view(|context| Ok(context.view.principal().cloned()))
    }

    /// The settings this session's family opening pinned, read from the opening as stored.
    pub(crate) fn settings(&self) -> Result<appa_runtime_api::SessionSettings, EventError> {
        Ok(crate::api::pinned_settings(&self.inner.log(&self.root)?).clone())
    }

    /// The host call ids of the calls in `closing` that `answered` does not name: calls a
    /// stop would close while they still run. A call with no host call id is never reported
    /// by id, so it never holds a stop. A stop judges and closes the same `closing` list, so
    /// a call opened after it was read is neither waited for nor closed.
    fn unanswered(&self, closing: &[OpenDispatch], answered: &BTreeSet<String>) -> Result<Vec<String>, EventError> {
        if closing.is_empty() {
            return Ok(Vec::new());
        }
        self.with_view(|context| {
            Ok(context
                .log
                .call_bindings()
                .filter(|binding| {
                    *binding.trajectory == self.trajectory
                        && !answered.contains(binding.call_id)
                        && closing.iter().any(|open| open.id == *binding.dispatch)
                })
                .map(|binding| binding.call_id.to_string())
                .collect())
        })
    }

    /// The calls a turn end closes: every carried call but a spawn whose child runs on past
    /// the turn. A spawn the harness did not identify closes: such a harness runs one call at
    /// a time, so the open spawn would refuse its next call.
    fn turn_calls(&self) -> Result<Vec<OpenDispatch>, EventError> {
        self.with_view(|context| {
            if context.engine().liveness(context.view, &self.trajectory) != Liveness::Live {
                return Ok(Vec::new());
            }
            Ok(context
                .open_dispatches()
                .into_iter()
                .filter(|open| {
                    !child_runs_on(
                        context.engine(),
                        context.view,
                        context.log.call_bindings(),
                        &self.trajectory,
                        open,
                    )
                })
                .collect())
        })
    }
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

/// Returns declared ahead across the replicas of one store.
#[cfg(test)]
mod ahead_tests;

/// Returns a single spawn's proposal declares.
#[cfg(test)]
mod declares_tests;

/// Writers of one family on PostgreSQL, each driving its own event at the same moment
/// through the runtime's shared store: the steps take turns on the family's row, so none
/// loses its append, and no connection is held past its step.
#[cfg(all(test, feature = "postgres"))]
mod postgres_family_writers;
