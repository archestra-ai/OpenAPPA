//! Canonical provider-neutral turn mediation.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use appa_engine::admit::{AdmitError, CastAnswer, ResultAdmission};
use appa_engine::authority::CastResolution;
use appa_engine::branch::{ReturnBlock, ReturnCheck, ReturnPlan, ReturnSubmission};
use appa_engine::check::{CheckOutcome, Narrowing, UnestablishedFact};
use appa_engine::contract::PinnedDynamicResolution;
use appa_engine::execute::Ruling;
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall, ReturnPolicy, Revision};
use appa_engine::label::{DimValue, Dimension};
use appa_engine::names::{CastName, SanitizerName};
use appa_engine::plan::{ExecutableRemedyPlan, RemedyPlan};
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, LabeledValue, Provenance, RawResultDigest, ResolvedCall, ToolCallId, ToolName,
    TrajectoryId, ValueBody, ValueId,
};
use thiserror::Error;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::common::uninformed_acceptance_feedback;
use crate::external::{
    AuthorityAnswer, AuthorityRequest, CastAnswer as BackendCast, CastInput, SanitizerAnswer, SanitizerInput,
};
use crate::mediator::{ForkedSession, Mediator};
use crate::store::{StoreError, StoreIdentity, TenantId};
use crate::tool::{BodyDisposition, DEFAULT_BODY_CAP_BYTES, EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT, ToolOutcome};
use crate::transcript::model_transcript;
use crate::wire::{WireMessage, WireTool, WireToolCall};

const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
const SEALED_UNRESOLVED: &str = "[tool result withheld: its label could not be established]";
const SEALED_UNSANITIZED: &str = "[tool result withheld: the sanitizer this plan bound could not derive from it]";
const SEALED_UNAVAILABLE: &str = "[tool result unavailable]";
const SEALED_FAILED: &str = "[tool call failed]";
const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";
const POLICY_STOP_BUDGET: &str = "This turn reached its resource budget and was stopped.";
const POLICY_STOP_INFERENCE: &str = "This turn could not continue: upstream inference was unavailable.";

const NO_SUCH_OFFER: &str = "no pending blocked call offers that plan_id — an offer belongs to the session whose call was blocked and does not cross a fork, so a plan named in inherited history is not yours to execute; propose the call this branch needs and accept the plan you are then offered";
const POLICY_STOP_CANCELLED: &str = "This turn was cancelled.";
const FORK_ONLY_FEEDBACK: &str =
    "fork must be the only call in its assistant round and take exactly one non-empty string task";
// A join tells the parent whether a value crossed — nothing more. Void, bare prose and
// abandonment share one response on purpose: a void return carries zero bits of child-derived
// content, and that indistinguishability is load-bearing for the confinement profile.
const FORK_RETURNED: &str = "the child session finished and returned its result";
const FORK_NO_RETURN: &str = "the child session finished without returning a value";
const FORK_FAILED: &str = "the child session could not be started";
const CHILD_FINISHED_REMAINDER: &str = "the call was not executed because the child session has finished";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<WireToolCall>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_inference_rounds: u32,
    pub max_tool_invocations: u32,
    pub max_blocked_proposals_per_call: u32,
    pub per_external_timeout: Duration,
    pub run_deadline: Duration,
    pub body_cap_bytes: usize,
    pub max_forks: u32,
    pub max_fork_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_inference_rounds: 16,
            max_tool_invocations: 32,
            max_blocked_proposals_per_call: 2,
            per_external_timeout: Duration::from_secs(30),
            run_deadline: Duration::from_secs(120),
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
            max_forks: 8,
            max_fork_depth: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("the shared run budget is exhausted")]
pub struct BudgetExhausted;

#[derive(Debug)]
pub struct RunBudget {
    limits: Limits,
    deadline: Instant,
    inference_rounds: u32,
    tool_invocations: u32,
    forks: u32,
}

impl RunBudget {
    pub fn new(limits: Limits) -> Self {
        RunBudget {
            limits,
            deadline: Instant::now() + limits.run_deadline,
            inference_rounds: 0,
            tool_invocations: 0,
            forks: 0,
        }
    }

    pub fn charge_inference(&mut self) -> Result<(), BudgetExhausted> {
        if self.deadline_elapsed() || self.inference_rounds >= self.limits.max_inference_rounds {
            return Err(BudgetExhausted);
        }
        self.inference_rounds += 1;
        Ok(())
    }

    pub fn charge_fork(&mut self) -> Result<(), BudgetExhausted> {
        if self.deadline_elapsed() || self.forks >= self.limits.max_forks {
            return Err(BudgetExhausted);
        }
        self.forks += 1;
        Ok(())
    }

    pub fn allows_fork_from_depth(&self, parent_depth: u32) -> bool {
        !self.deadline_elapsed() && self.forks < self.limits.max_forks && parent_depth < self.limits.max_fork_depth
    }

    pub fn is_exhausted(&self) -> bool {
        self.deadline_elapsed()
            || self.inference_rounds >= self.limits.max_inference_rounds
            || self.tool_invocations >= self.limits.max_tool_invocations
            || self.forks >= self.limits.max_forks
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn deadline_elapsed(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn can_invoke_tool(&self) -> bool {
        !self.deadline_elapsed() && self.tool_invocations < self.limits.max_tool_invocations
    }

    fn record_tool_invocation(&mut self) {
        self.tool_invocations += 1;
    }

    fn external_budget(&self) -> Duration {
        self.remaining().min(self.limits.per_external_timeout)
    }
}

impl Default for RunBudget {
    fn default() -> Self {
        RunBudget::new(Limits::default())
    }
}

#[derive(Debug, Error)]
pub enum BeginTurnError {
    #[error("the turn was cancelled before it acquired the session lease")]
    Cancelled,
    #[error("the reserved child belongs to another mediator")]
    ForeignFork,
    #[error("this session already ended its errand — an ended child is closed to new turns")]
    SessionEnded,
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

#[derive(Debug, Error)]
pub enum TurnError {
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch identity no longer matches its call/trajectory")]
    DispatchIdentity,
    #[error("cannot {operation} while the turn is {actual}")]
    Lifecycle {
        operation: &'static str,
        actual: &'static str,
    },
    #[error("the fork request does not belong to this turn's pending fork call")]
    ForkIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    InferenceFailure,
    BudgetExhausted,
    Cancelled,
}

#[derive(Debug)]
pub enum Step {
    Continue,
    Fork(ForkRequest),
    Final(String),
    ChildFinished,
    PolicyStop(String),
}

#[derive(Debug)]
pub struct ForkRequest {
    identity: ForkIdentity,
    task: String,
}

impl ForkRequest {
    pub fn task(&self) -> &str {
        &self.task
    }
}

enum ForkOutcome {
    Completed { child: TrajectoryId },
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForkIdentity {
    store: StoreIdentity,
    trajectory: TrajectoryId,
    turn_admission: Revision,
    serial: u32,
    call_id: ToolCallId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Ready,
    Mediating,
    AwaitingFork,
    Finished,
}

impl Lifecycle {
    fn name(self) -> &'static str {
        match self {
            Lifecycle::Ready => "ready",
            Lifecycle::Mediating => "mediating a completion",
            Lifecycle::AwaitingFork => "awaiting fork completion",
            Lifecycle::Finished => "finished",
        }
    }
}

struct PendingBlock {
    call: ResolvedCall,
    offers: Vec<(String, ExecutableRemedyPlan)>,
    offered_round: u32,
}

struct PendingReturn {
    parent: TrajectoryId,
    body: String,
    raw_digest: RawResultDigest,
    offers: Vec<(String, ReturnPlan)>,
    offered_round: u32,
}

struct PendingCast {
    handle: String,
    dispatch: DispatchId,
    call: ResolvedCall,
    body: String,
    cast: CastName,
    resolved: DimValue,
    narrowing: Narrowing,
    offered_round: u32,
}

struct Proposal {
    call: ProposedCall,
    raw: String,
    disposition: ArgumentDisposition,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgumentDisposition {
    Parsed,
    Malformed,
    Oversized,
}

#[derive(Clone)]
struct OpenClose {
    dispatch: DispatchId,
    call: ResolvedCall,
    close: CancelClose,
}

#[derive(Clone, Copy)]
enum CancelClose {
    Unobserved,
    EffectsStand,
    Failed,
}

enum Admission {
    Admitted,
    AlreadyClosed,
    Refused(AdmitError),
    CancelSuppressed,
    InvariantBreach,
}

struct TurnCancelled;

enum RawReturnGo {
    Merge,
    Answered,
}

enum CallProgress {
    Go,
    Stop,
    Cancelled,
    ChildFinished,
}

/// A live turn. It must reach a terminal [`Step`] or [`Turn::stop`]; Drop applies the fixed
/// cancellation terminal synchronously as a last-resort shield if an agent path abandons it.
#[must_use = "a live Turn must be finalized by mediate/complete_fork/fail_fork/stop"]
pub struct Turn {
    mediator: Arc<Mediator>,
    tenant: TenantId,
    session: TrajectoryId,
    turn_admission: Revision,
    is_child: bool,
    depth: u32,
    cancel: CancellationToken,
    _lease: OwnedMutexGuard<()>,
    lifecycle: Lifecycle,
    rounds: u32,
    pending: Vec<PendingBlock>,
    pending_returns: Vec<PendingReturn>,
    pending_casts: Vec<PendingCast>,
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    return_derivation_attempts: BTreeMap<(RawResultDigest, SanitizerName), u32>,
    next_handle: u32,
    next_fork: u32,
    fork_identity: Option<ForkIdentity>,
    unanswered: Vec<ToolCallId>,
    open_dispatch: Option<OpenClose>,
}

impl Mediator {
    /// Admit a user turn while holding the trajectory's owned lease for the returned [`Turn`]'s
    /// entire life. Waiting for the lease is cancellable and child status comes only from metadata.
    pub async fn begin_turn(
        self: &Arc<Self>,
        tenant: TenantId,
        session: TrajectoryId,
        text: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<Turn, BeginTurnError> {
        let lease = self.store().turn_lock(&tenant, &session)?;
        let guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BeginTurnError::Cancelled),
            guard = lease.lock_owned() => guard,
        };
        if cancel.is_cancelled() {
            return Err(BeginTurnError::Cancelled);
        }
        self.start_turn(tenant, session, text.into(), cancel, guard)
    }

    pub fn begin_forked_turn(
        self: &Arc<Self>,
        tenant: TenantId,
        forked: ForkedSession,
        text: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<Turn, BeginTurnError> {
        if self.store().identity() != forked.store_identity {
            return Err(BeginTurnError::ForeignFork);
        }
        self.start_turn(tenant, forked.session, text.into(), cancel, forked.lease)
    }

    fn start_turn(
        self: &Arc<Self>,
        tenant: TenantId,
        session: TrajectoryId,
        text: String,
        cancel: CancellationToken,
        guard: OwnedMutexGuard<()>,
    ) -> Result<Turn, BeginTurnError> {
        let mut depth = 0u32;
        let mut cursor = session.clone();
        let direct_parent = self.store().parent_of(&tenant, &cursor)?;
        let is_child = direct_parent.is_some();
        let mut next = direct_parent.clone();
        while let Some(parent) = next {
            depth = depth.saturating_add(1);
            cursor = parent;
            next = self.store().parent_of(&tenant, &cursor)?;
        }

        let value = LabeledValue::new(ValueBody::new(text), self.config().boundary_label().clone());
        let mut ended = false;
        let turn_admission = self.store().finalize(&tenant, &session, |facts, revision| {
            if let Some(parent) = &direct_parent {
                let projection = Projection::build(facts, revision);
                if projection.view(parent).has_ended(&session) {
                    ended = true;
                    return None;
                }
            }
            Some(FactBatch::new(
                revision,
                vec![Fact::ValueAdmitted {
                    trajectory: session.clone(),
                    value,
                    provenance: Provenance::UserInput,
                }],
            ))
        })?;
        if ended {
            return Err(BeginTurnError::SessionEnded);
        }

        Ok(Turn {
            mediator: self.clone(),
            tenant,
            session,
            turn_admission,
            is_child,
            depth,
            cancel,
            _lease: guard,
            lifecycle: Lifecycle::Ready,
            rounds: 0,
            pending: Vec::new(),
            pending_returns: Vec::new(),
            pending_casts: Vec::new(),
            remedy_attempts: BTreeMap::new(),
            return_derivation_attempts: BTreeMap::new(),
            next_handle: 0,
            next_fork: 0,
            fork_identity: None,
            unanswered: Vec::new(),
            open_dispatch: None,
        })
    }
}

impl Turn {
    pub fn depth(&self) -> u32 {
        self.depth
    }

    pub fn is_child(&self) -> bool {
        self.is_child
    }

    pub fn advertised_tools(&self, budget: &RunBudget) -> Result<Vec<WireTool>, TurnError> {
        self.require(Lifecycle::Ready, "build the tool surface")?;
        Ok(self
            .mediator
            .advertised_tools(self.is_child, budget.allows_fork_from_depth(self.depth)))
    }

    pub fn transcript(&self) -> Result<Vec<WireMessage>, TurnError> {
        self.require(Lifecycle::Ready, "build a transcript")?;
        let (log, _) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
        Ok(model_transcript(self.mediator.transcript_head(), &log, &self.session))
    }

    pub async fn mediate(&mut self, completion: Completion, budget: &mut RunBudget) -> Result<Step, TurnError> {
        self.require(Lifecycle::Ready, "mediate a completion")?;
        if self.cancel.is_cancelled() {
            return self.finish_cancelled();
        }
        if budget.deadline_elapsed() {
            return self.finish_policy_stop(POLICY_STOP_BUDGET);
        }
        self.lifecycle = Lifecycle::Mediating;
        self.rounds += 1;

        let proposals: Vec<Proposal> = completion.tool_calls.iter().map(proposal_of).collect();
        let calls = proposals.iter().map(|p| p.call.clone()).collect();
        self.unanswered = proposals.iter().map(|p| p.call.id.clone()).collect();
        self.append(vec![Fact::AssistantMessage {
            trajectory: self.session.clone(),
            content: completion.content.clone(),
            calls,
        }])?;

        if proposals.is_empty() {
            self.finish_turn_end()?;
            self.lifecycle = Lifecycle::Finished;
            return if self.is_child {
                Ok(Step::ChildFinished)
            } else {
                Ok(Step::Final(completion.content.unwrap_or_default()))
            };
        }

        if proposals.iter().any(|p| p.call.tool.as_str() == FORK) {
            return self.mediate_fork_round(proposals);
        }

        let mut budget_hit = false;
        for proposal in &proposals {
            if self.cancel.is_cancelled() {
                return self.finish_cancelled();
            }
            if budget_hit {
                self.feedback(&proposal.call.id, POLICY_STOP_BUDGET)?;
                continue;
            }
            match proposal.disposition {
                ArgumentDisposition::Malformed => {
                    self.feedback(
                        &proposal.call.id,
                        "the tool call had malformed arguments and was not executed",
                    )?;
                    continue;
                }
                ArgumentDisposition::Oversized => {
                    self.feedback(
                        &proposal.call.id,
                        &crate::common::invalid_call_feedback(&appa_engine::engine::EngineError::InvalidCall(
                            appa_engine::params::ArgumentError::TooLarge,
                        )),
                    )?;
                    continue;
                }
                ArgumentDisposition::Parsed => {}
            }
            match self.handle_call(proposal, budget).await? {
                CallProgress::Go => {}
                CallProgress::Stop => budget_hit = true,
                CallProgress::Cancelled => return self.finish_cancelled(),
                CallProgress::ChildFinished => return Ok(Step::ChildFinished),
            }
        }
        if budget_hit {
            return self.finish_policy_stop(POLICY_STOP_BUDGET);
        }
        self.lifecycle = Lifecycle::Ready;
        Ok(Step::Continue)
    }

    pub fn complete_fork(&mut self, request: ForkRequest, child: TrajectoryId) -> Result<Step, TurnError> {
        self.finish_fork(request, ForkOutcome::Completed { child })
    }

    pub fn fail_fork(&mut self, request: ForkRequest) -> Result<Step, TurnError> {
        self.finish_fork(request, ForkOutcome::Failed)
    }

    pub fn stop(&mut self, reason: StopReason) -> Result<Step, TurnError> {
        self.require(Lifecycle::Ready, "stop")?;
        let message = match reason {
            StopReason::InferenceFailure => POLICY_STOP_INFERENCE,
            StopReason::BudgetExhausted => POLICY_STOP_BUDGET,
            StopReason::Cancelled => POLICY_STOP_CANCELLED,
        };
        self.finish_policy_stop(message)
    }

    fn require(&self, required: Lifecycle, operation: &'static str) -> Result<(), TurnError> {
        if self.lifecycle == required {
            Ok(())
        } else {
            Err(TurnError::Lifecycle {
                operation,
                actual: self.lifecycle.name(),
            })
        }
    }

    fn mediate_fork_round(&mut self, proposals: Vec<Proposal>) -> Result<Step, TurnError> {
        let valid_task = match proposals.as_slice() {
            [proposal]
                if proposal.disposition == ArgumentDisposition::Parsed && proposal.call.tool.as_str() == FORK =>
            {
                exact_nonempty_task(&proposal.call.arguments)
            }
            _ => None,
        };
        let Some(task) = valid_task else {
            for proposal in &proposals {
                if self.cancel.is_cancelled() {
                    return self.finish_cancelled();
                }
                self.feedback(&proposal.call.id, FORK_ONLY_FEEDBACK)?;
            }
            self.lifecycle = Lifecycle::Ready;
            return Ok(Step::Continue);
        };

        let call_id = proposals[0].call.id.clone();
        let identity = ForkIdentity {
            store: self.mediator.store().identity(),
            trajectory: self.session.clone(),
            turn_admission: self.turn_admission,
            serial: self.next_fork,
            call_id,
        };
        self.next_fork += 1;
        self.fork_identity = Some(identity.clone());
        self.lifecycle = Lifecycle::AwaitingFork;
        Ok(Step::Fork(ForkRequest { identity, task }))
    }

    fn finish_fork(&mut self, request: ForkRequest, outcome: ForkOutcome) -> Result<Step, TurnError> {
        self.require(Lifecycle::AwaitingFork, "finish a fork")?;
        if self.fork_identity.as_ref() != Some(&request.identity) {
            return Err(TurnError::ForkIdentity);
        }
        let response = match outcome {
            ForkOutcome::Completed { child } => {
                let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
                let projection = Projection::build(&log, revision);
                if projection.view(&self.session).returns_by(&child) > 0 {
                    FORK_RETURNED
                } else {
                    FORK_NO_RETURN
                }
            }
            ForkOutcome::Failed => FORK_FAILED,
        };
        self.feedback(&request.identity.call_id, response)?;
        self.fork_identity = None;
        self.lifecycle = Lifecycle::Ready;
        Ok(Step::Continue)
    }

    async fn handle_call(&mut self, proposal: &Proposal, budget: &mut RunBudget) -> Result<CallProgress, TurnError> {
        let proposed = &proposal.call;
        match proposed.tool.as_str() {
            EXECUTE_REMEDY_PLAN => {
                self.handle_execute_remedy(&proposed.id, &proposed.arguments, budget)
                    .await
            }
            SUBMIT_RESULT => {
                self.handle_submit_result(&proposed.id, &proposed.arguments, budget)
                    .await
            }
            _ => {
                let call = match crate::common::resolve_raw_call(
                    self.mediator.engine(),
                    proposed.tool.clone(),
                    proposal.raw.as_bytes(),
                ) {
                    Ok(call) => call,
                    Err(error) => {
                        self.feedback(&proposed.id, &crate::common::invalid_call_feedback(&error))?;
                        return Ok(CallProgress::Go);
                    }
                };
                let call = match self.resolve_dynamic_call(call, budget).await {
                    Some(call) => call,
                    None => return Ok(CallProgress::Cancelled),
                };
                self.mediate_call(&proposed.id, call, budget).await
            }
        }
    }

    async fn resolve_dynamic_call(&self, call: ResolvedCall, budget: &RunBudget) -> Option<ResolvedCall> {
        let Some(contract) = self.mediator.engine().registry().tool(call.tool()) else {
            return Some(call);
        };
        let mut resolutions = Vec::new();
        for binding in crate::common::dynamic_bindings(contract) {
            if self.cancel.is_cancelled() || budget.external_budget().is_zero() {
                return None;
            }
            let value = call.arguments().get(&binding.argument).and_then(|value| value.as_str());
            let audience = if let (Some(value), Some(backend)) =
                (value, self.mediator.dynamic_resolver_backend(&binding.resolver))
            {
                tokio::select! {
                    _ = self.cancel.cancelled() => return None,
                    result = tokio::time::timeout(budget.external_budget(), backend.resolve(&binding.resolver, call.tool(), &binding.argument, value)) => result.ok().flatten(),
                }
            } else {
                None
            };
            resolutions.push(PinnedDynamicResolution::from_answer(binding, audience));
        }
        Some(call.with_dynamic_resolutions(resolutions))
    }

    async fn mediate_call(
        &mut self,
        call_id: &ToolCallId,
        call: ResolvedCall,
        budget: &mut RunBudget,
    ) -> Result<CallProgress, TurnError> {
        let mut casts_exhausted = false;
        loop {
            let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
            let projection = Projection::build(&log, revision);
            let views = projection.view(&self.session);
            match self.mediator.engine().check(&views, &call) {
                Err(_) => {
                    self.feedback(call_id, "no such tool is registered")?;
                    return Ok(CallProgress::Go);
                }
                Ok(CheckOutcome::Allow) => {
                    if !budget.can_invoke_tool() {
                        self.feedback(call_id, POLICY_STOP_BUDGET)?;
                        return Ok(CallProgress::Stop);
                    }
                    drop(projection);
                    match self.open_dispatch(&call)? {
                        Some(dispatch) => {
                            return self.invoke_and_admit(dispatch, &call, call_id, None, budget).await;
                        }
                        None => {
                            self.feedback(call_id, "the call could not be dispatched (the policy state changed)")?
                        }
                    }
                    return Ok(CallProgress::Go);
                }
                Ok(CheckOutcome::Block(raw)) => {
                    if !raw.unestablished.is_empty() && !casts_exhausted {
                        drop(projection);
                        match self.resolve_unknown(&log, &raw.unestablished, budget).await {
                            Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                            Ok(resolved) => {
                                if !resolved? {
                                    casts_exhausted = true;
                                }
                                continue;
                            }
                        }
                    }
                    let planned = self
                        .mediator
                        .engine()
                        .plan(&views, &call, &raw)
                        .expect("checked tool is registered");
                    let surface = if self.is_child {
                        crate::feedback::FeedbackSurface::Child
                    } else {
                        crate::feedback::FeedbackSurface::Root {
                            can_fork: budget.allows_fork_from_depth(self.depth),
                        }
                    };
                    let has_offers = planned.plans.iter().any(|plan| plan.executable().is_some());
                    let feedback = if !has_offers {
                        crate::feedback::block_feedback(
                            self.mediator.engine().registry(),
                            &raw,
                            &planned,
                            &[],
                            surface,
                            &views,
                        )
                    } else {
                        let attempts = self.remedy_attempts.entry(call.digest()).or_insert(0);
                        *attempts += 1;
                        if *attempts > budget.limits.max_blocked_proposals_per_call {
                            self.feedback(call_id, "the remedy attempt limit for this call was reached")?;
                            return Ok(CallProgress::Go);
                        }
                        let offers = planned
                            .plans
                            .iter()
                            .filter_map(RemedyPlan::executable)
                            .map(|plan| {
                                let handle = format!("remedy-{}", self.next_handle);
                                self.next_handle += 1;
                                (handle, plan.clone())
                            })
                            .collect::<Vec<_>>();
                        let feedback = crate::feedback::block_feedback(
                            self.mediator.engine().registry(),
                            &raw,
                            &planned,
                            &offers,
                            surface,
                            &views,
                        );
                        self.pending.push(PendingBlock {
                            call,
                            offers,
                            offered_round: self.rounds,
                        });
                        feedback
                    };
                    self.feedback(call_id, &feedback)?;
                    return Ok(CallProgress::Go);
                }
            }
        }
    }

    async fn handle_execute_remedy(
        &mut self,
        call_id: &ToolCallId,
        arguments: &serde_json::Value,
        budget: &mut RunBudget,
    ) -> Result<CallProgress, TurnError> {
        let Some(handle) = arguments.get("plan_id").and_then(|value| value.as_str()) else {
            self.feedback(call_id, "execute_remedy_plan requires a string plan_id")?;
            return Ok(CallProgress::Go);
        };
        if let Some(index) = self
            .pending_returns
            .iter()
            .position(|pending| pending.offers.iter().any(|(offer, _)| offer == handle))
        {
            return self.handle_execute_return_remedy(call_id, index, handle, budget).await;
        }
        if let Some(index) = self.pending_casts.iter().position(|pending| pending.handle == handle) {
            return self.handle_execute_cast_accept(call_id, index);
        }
        let Some(cohort_index) = self
            .pending
            .iter()
            .position(|pending| pending.offers.iter().any(|(offer, _)| offer == handle))
        else {
            self.feedback(call_id, NO_SUCH_OFFER)?;
            return Ok(CallProgress::Go);
        };
        if !budget.can_invoke_tool() {
            self.feedback(call_id, POLICY_STOP_BUDGET)?;
            return Ok(CallProgress::Stop);
        }

        let call = self.pending[cohort_index].call.clone();
        let chosen = self.pending[cohort_index]
            .offers
            .iter()
            .find(|(offer, _)| offer == handle)
            .map(|(_, plan)| plan.clone())
            .expect("the cohort was found by this handle");
        let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, revision);
        let views = projection.view(&self.session);
        let outcome = self.mediator.engine().check(&views, &call);
        if let Ok(CheckOutcome::Block(raw)) = &outcome
            && !raw.unestablished.is_empty()
        {
            let facts = crate::feedback::unestablished_gate_feedback(&raw.unestablished, &views);
            self.feedback(call_id, &facts)?;
            return Ok(CallProgress::Go);
        }
        let accepts_narrowing = chosen
            .steps
            .iter()
            .any(|step| matches!(step, appa_engine::plan::RemedyStep::Accept(_)));
        if accepts_narrowing && self.pending[cohort_index].offered_round == self.rounds {
            self.feedback(call_id, &uninformed_acceptance_feedback(handle))?;
            return Ok(CallProgress::Go);
        }
        let still_offered = match &outcome {
            Ok(CheckOutcome::Block(raw)) => self
                .mediator
                .engine()
                .plan(&views, &call, raw)
                .expect("pending call is registered")
                .plans
                .iter()
                .filter_map(appa_engine::plan::RemedyPlan::executable)
                .any(|offered| offered == &chosen),
            _ => false,
        };
        if !still_offered {
            self.pending.remove(cohort_index);
            self.feedback(
                call_id,
                "the state changed and this offer no longer applies; re-propose the call",
            )?;
            return Ok(CallProgress::Go);
        }
        let dispatch = DispatchId::new(
            self.session.clone(),
            call.digest(),
            views.dispatch_count(&call.digest()),
        );

        let mut rulings = Vec::new();
        for requirement in &chosen.required {
            let Some(backend) = self.mediator.authority_backend(&requirement.authority) else {
                self.feedback(call_id, "an authority for this plan is not configured")?;
                return Ok(CallProgress::Go);
            };
            let request =
                AuthorityRequest::new(requirement.authority.clone(), &call, requirement.covers.clone(), &views);
            let answer = match self.wait(budget, backend.rule(&request)).await {
                Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                Ok(answer) => answer.unwrap_or(AuthorityAnswer::Abstain),
            };
            match answer {
                AuthorityAnswer::Approve => rulings.push(Ruling {
                    dispatch: dispatch.clone(),
                    authority: requirement.authority.clone(),
                    covers: requirement.covers.clone(),
                    reviewed: request.review(),
                }),
                AuthorityAnswer::Deny => {
                    let denier = requirement.authority.clone();
                    let digest = self.pending[cohort_index].call.digest();
                    let survivors: Vec<(String, ExecutableRemedyPlan)> = self.pending[cohort_index]
                        .offers
                        .iter()
                        .filter(|(_, plan)| !plan.names_authority(&denier))
                        .cloned()
                        .collect();
                    let feedback = crate::feedback::denial_feedback(
                        self.mediator.engine().registry(),
                        &survivors,
                        self.acceptance_surface(),
                    );
                    self.append(vec![
                        Fact::Denial {
                            trajectory: self.session.clone(),
                            digest,
                            authority: denier.clone(),
                        },
                        feedback_fact(&self.session, call_id, &feedback),
                    ])?;
                    self.mark_answered(call_id);
                    for cohort in &mut self.pending {
                        if cohort.call.digest() == digest {
                            cohort.offers.retain(|(_, plan)| !plan.names_authority(&denier));
                        }
                    }
                    self.pending.retain(|cohort| !cohort.offers.is_empty());
                    return Ok(CallProgress::Go);
                }
                AuthorityAnswer::Abstain => {
                    let feedback = crate::feedback::no_answer_feedback(
                        self.mediator.engine().registry(),
                        handle,
                        &self.pending[cohort_index].offers,
                        self.acceptance_surface(),
                    );
                    self.feedback(call_id, &feedback)?;
                    return Ok(CallProgress::Go);
                }
            }
        }

        let batch = match self
            .mediator
            .engine()
            .execute_remedy_plan(&views, &chosen, &call, &rulings)
        {
            Ok(batch) => batch,
            Err(appa_engine::execute::PlanError::Unestablished(facts)) => {
                let gated = crate::feedback::unestablished_gate_feedback(&facts, &views);
                self.feedback(call_id, &gated)?;
                return Ok(CallProgress::Go);
            }
            Err(_) => {
                self.feedback(call_id, "the remedy plan could not be executed on the current state")?;
                return Ok(CallProgress::Go);
            }
        };
        drop(projection);
        match self
            .mediator
            .store()
            .conditional_append(&self.tenant, &self.session, batch)
        {
            Ok(_) => {}
            Err(StoreError::Stale { .. }) => {
                self.feedback(call_id, "the state changed; retry the remedy")?;
                return Ok(CallProgress::Go);
            }
            Err(error) => return Err(TurnError::Store(error)),
        }
        self.pending.remove(cohort_index);
        let bound = chosen.steps.iter().find_map(|step| match step {
            appa_engine::plan::RemedyStep::Sanitize(sanitizer) => Some(sanitizer.clone()),
            appa_engine::plan::RemedyStep::Authorize(_) | appa_engine::plan::RemedyStep::Accept(_) => None,
        });
        self.invoke_and_admit(dispatch, &call, call_id, bound, budget).await
    }

    async fn handle_submit_result(
        &mut self,
        call_id: &ToolCallId,
        arguments: &serde_json::Value,
        budget: &RunBudget,
    ) -> Result<CallProgress, TurnError> {
        if !self.is_child {
            self.feedback(call_id, "submit_result is available only to a child session")?;
            return Ok(CallProgress::Go);
        }
        let exact_shape = arguments
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("value"));
        let body = match exact_shape {
            Some(serde_json::Value::String(value)) => value.clone(),
            Some(serde_json::Value::Null) => {
                let Some(parent) = self.mediator.store().parent_of(&self.tenant, &self.session)? else {
                    self.feedback(call_id, "this session cannot submit a result")?;
                    return Ok(CallProgress::Go);
                };
                return self.finish_child_void(call_id, &parent);
            }
            _ => {
                self.feedback(
                    call_id,
                    "submit_result takes exactly one key, `value`: a string result, or null to return nothing",
                )?;
                return Ok(CallProgress::Go);
            }
        };

        if let Some(parent) = self.mediator.store().parent_of(&self.tenant, &self.session)? {
            let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
            let projection = Projection::build(&log, revision);
            if projection.view(&parent).has_ended(&self.session) {
                self.feedback(
                    call_id,
                    "this session already ended its errand — a child returns or voids at most once",
                )?;
                return Ok(CallProgress::Go);
            }
            drop(projection);

            loop {
                let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
                let projection = Projection::build(&log, revision);
                let facts = self
                    .mediator
                    .engine()
                    .child_fold_unestablished(&projection.view(&parent), &self.session);
                if facts.is_empty() {
                    break;
                }
                let residual = crate::feedback::unestablished_return_feedback(&facts, &projection.view(&self.session));
                drop(projection);
                match self.resolve_unknown(&log, &facts, budget).await {
                    Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                    Ok(progressed) => {
                        if !progressed? {
                            self.feedback(call_id, &residual)?;
                            return Ok(CallProgress::Go);
                        }
                    }
                }
            }
        }

        let returned = match self.mediator.config().child_return_policy() {
            ReturnPolicy::Sanitized(sanitizer) => match self.derive_sanitized(&sanitizer, &body, budget).await {
                Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                Ok(Some(derived)) => ReturnSubmission::Derived {
                    body: ValueBody::new(derived),
                    raw_digest: RawResultDigest::of(body.as_bytes()),
                },
                Ok(None) => {
                    self.feedback(call_id, "the result could not be sanitized for return")?;
                    return Ok(CallProgress::Go);
                }
            },
            ReturnPolicy::Raw => match self.check_raw_return(call_id, &body)? {
                RawReturnGo::Merge => ReturnSubmission::Raw {
                    body: ValueBody::new(body),
                },
                RawReturnGo::Answered => return Ok(CallProgress::Go),
            },
        };

        let Some(parent) = self.mediator.store().parent_of(&self.tenant, &self.session)? else {
            self.feedback(call_id, "this session cannot submit a result")?;
            return Ok(CallProgress::Go);
        };
        let remaining = self.remaining_after_current(call_id);
        let mut crossed = false;
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                if self.cancel.is_cancelled() {
                    return None;
                }
                let projection = Projection::build(facts, revision);
                let mut batch = self
                    .mediator
                    .engine()
                    .submit_child_return(&projection.view(&parent), &self.session, returned)
                    .ok()?;
                let mut drained = self.drain_pending_casts(&projection.view(&self.session));
                drained.append(&mut batch.facts);
                batch.facts = drained;
                batch
                    .facts
                    .push(feedback_fact(&self.session, call_id, "result submitted to the parent"));
                batch.facts.extend(
                    remaining
                        .iter()
                        .map(|id| feedback_fact(&self.session, id, CHILD_FINISHED_REMAINDER)),
                );
                batch.facts.push(turn_end(&self.session));
                crossed = true;
                Some(batch)
            })?;
        if crossed {
            self.unanswered.clear();
            self.pending.clear();
            self.pending_returns.clear();
            self.pending_casts.clear();
            self.lifecycle = Lifecycle::Finished;
            return Ok(CallProgress::ChildFinished);
        }
        if self.cancel.is_cancelled() {
            return Ok(CallProgress::Cancelled);
        }
        self.feedback(call_id, "this session cannot submit a result")?;
        Ok(CallProgress::Go)
    }

    fn check_raw_return(&mut self, call_id: &ToolCallId, body: &str) -> Result<RawReturnGo, TurnError> {
        let Some(parent) = self.mediator.store().parent_of(&self.tenant, &self.session)? else {
            return Ok(RawReturnGo::Merge);
        };
        let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, revision);
        let views = projection.view(&parent);
        match self.mediator.engine().check_child_return(&views, &self.session) {
            Ok(ReturnCheck::Allow) => Ok(RawReturnGo::Merge),
            Ok(ReturnCheck::Block(ReturnBlock::Unestablished(facts))) => {
                let feedback = crate::feedback::unestablished_return_feedback(&facts, &views);
                self.feedback(call_id, &feedback)?;
                Ok(RawReturnGo::Answered)
            }
            Ok(ReturnCheck::Block(ReturnBlock::Narrowing { plans, .. })) => {
                let mut plans = plans;
                plans.sort_by_key(return_plan_cost);
                let offers: Vec<(String, ReturnPlan)> = plans
                    .into_iter()
                    .map(|plan| {
                        let handle = format!("remedy-{}", self.next_handle);
                        self.next_handle += 1;
                        (handle, plan)
                    })
                    .collect();
                let menu: Vec<String> = offers
                    .iter()
                    .map(|(handle, plan)| {
                        let informed = match plan {
                            // Acceptance-carrying plans are informed: never in the offering round.
                            ReturnPlan::Accept(_) | ReturnPlan::Sanitize { residual: Some(_), .. } => {
                                " (in a later response)"
                            }
                            ReturnPlan::Sanitize { residual: None, .. } => "",
                        };
                        format!("\"{handle}\" to {}{informed}", describe_return_plan(plan))
                    })
                    .collect();
                let free_crossing = offers
                    .iter()
                    .find(|(_, plan)| matches!(plan, ReturnPlan::Sanitize { residual: None, .. }))
                    .map(|(handle, _)| handle.as_str());
                let cost = "returning this raw narrows the parent, permanently for the parent session — every later parent step that needs the parent's current label is lost with it";
                let alternatives = match free_crossing {
                    Some(handle) => format!(
                        "You need not pay that to return a value: plan \"{handle}\" crosses one with the parent's label untouched. If the parent needs no value at all, submit_result null returns none and leaves the label untouched too (side effects this branch already committed hold either way)"
                    ),
                    None => "Every plan offered here narrows it too. Weigh that against submit_result null, which returns no value and leaves the parent's label untouched (side effects this branch already committed hold either way)".to_string(),
                };
                let feedback = format!(
                    "{cost}. {alternatives}. Call execute_remedy_plan with plan_id {}",
                    menu.join(", ")
                );
                self.pending_returns.push(PendingReturn {
                    parent,
                    body: body.to_string(),
                    raw_digest: RawResultDigest::of(body.as_bytes()),
                    offers,
                    offered_round: self.rounds,
                });
                drop(projection);
                self.feedback(call_id, &feedback)?;
                Ok(RawReturnGo::Answered)
            }
            Err(_) => {
                self.feedback(call_id, "this session cannot submit a result")?;
                Ok(RawReturnGo::Answered)
            }
        }
    }

    async fn handle_execute_return_remedy(
        &mut self,
        call_id: &ToolCallId,
        index: usize,
        handle: &str,
        budget: &RunBudget,
    ) -> Result<CallProgress, TurnError> {
        let plan = self.pending_returns[index]
            .offers
            .iter()
            .find(|(offer, _)| offer == handle)
            .map(|(_, plan)| plan.clone())
            .expect("the caller located this pending return offer");
        let accepts_narrowing = matches!(
            &plan,
            ReturnPlan::Accept(_) | ReturnPlan::Sanitize { residual: Some(_), .. }
        );
        if accepts_narrowing && self.pending_returns[index].offered_round == self.rounds {
            self.feedback(call_id, &uninformed_acceptance_feedback(handle))?;
            return Ok(CallProgress::Go);
        }
        let parent = self.pending_returns[index].parent.clone();
        {
            let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
            let projection = Projection::build(&log, revision);
            if projection.view(&parent).has_ended(&self.session) {
                self.pending_returns.clear();
                self.feedback(
                    call_id,
                    "this session already ended its errand — a child returns or voids at most once",
                )?;
                return Ok(CallProgress::Go);
            }
        }

        let submission = match &plan {
            ReturnPlan::Accept(_) => ReturnSubmission::Raw {
                body: ValueBody::new(self.pending_returns[index].body.clone()),
            },
            ReturnPlan::Sanitize { sanitizer, .. } => {
                let sanitizer = sanitizer.clone();
                let key = (self.pending_returns[index].raw_digest, sanitizer.clone());
                let attempts = self.return_derivation_attempts.entry(key).or_insert(0);
                *attempts += 1;
                if *attempts > budget.limits.max_blocked_proposals_per_call {
                    self.feedback(call_id, "the remedy attempt limit for this return was reached")?;
                    return Ok(CallProgress::Go);
                }
                let body = self.pending_returns[index].body.clone();
                match self.derive_sanitized(&sanitizer, &body, budget).await {
                    Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                    Ok(Some(derived)) => ReturnSubmission::Derived {
                        body: ValueBody::new(derived),
                        raw_digest: self.pending_returns[index].raw_digest,
                    },
                    Ok(None) => {
                        self.feedback(call_id, "the derivation failed; the return offer remains available")?;
                        return Ok(CallProgress::Go);
                    }
                }
            }
        };

        let remaining = self.remaining_after_current(call_id);
        let mut executed = false;
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                if self.cancel.is_cancelled() {
                    return None;
                }
                let projection = Projection::build(facts, revision);
                let mut batch = self
                    .mediator
                    .engine()
                    .execute_child_return_plan(&projection.view(&parent), &self.session, plan.clone(), submission)
                    .ok()?;
                let mut drained = self.drain_pending_casts(&projection.view(&self.session));
                drained.append(&mut batch.facts);
                batch.facts = drained;
                batch
                    .facts
                    .push(feedback_fact(&self.session, call_id, "result submitted to the parent"));
                batch.facts.extend(
                    remaining
                        .iter()
                        .map(|id| feedback_fact(&self.session, id, CHILD_FINISHED_REMAINDER)),
                );
                batch.facts.push(turn_end(&self.session));
                executed = true;
                Some(batch)
            })?;
        if executed {
            self.pending_returns.clear();
            self.pending.clear();
            self.pending_casts.clear();
            self.unanswered.clear();
            self.lifecycle = Lifecycle::Finished;
            return Ok(CallProgress::ChildFinished);
        }
        if self.cancel.is_cancelled() {
            return Ok(CallProgress::Cancelled);
        }
        self.pending_returns.remove(index);
        self.feedback(call_id, "the return offer is stale; submit the result again")?;
        Ok(CallProgress::Go)
    }

    fn finish_child_void(&mut self, call_id: &ToolCallId, parent: &TrajectoryId) -> Result<CallProgress, TurnError> {
        let remaining = self.remaining_after_current(call_id);
        let mut already_ended = false;
        let mut voided = false;
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                if self.cancel.is_cancelled() {
                    return None;
                }
                let projection = Projection::build(facts, revision);
                let batch = match self
                    .mediator
                    .engine()
                    .submit_void_return(&projection.view(parent), &self.session)
                {
                    Ok(batch) => batch,
                    Err(appa_engine::branch::BranchError::AlreadyEnded) => {
                        already_ended = true;
                        return None;
                    }
                    Err(_) => unreachable!("the store's fork linkage names this child's direct parent"),
                };
                let mut terminal = self.drain_pending_casts(&projection.view(&self.session));
                terminal.extend(batch.facts);
                terminal.push(feedback_fact(
                    &self.session,
                    call_id,
                    "no result returned to the parent",
                ));
                terminal.extend(
                    remaining
                        .iter()
                        .map(|id| feedback_fact(&self.session, id, CHILD_FINISHED_REMAINDER)),
                );
                terminal.push(turn_end(&self.session));
                voided = true;
                Some(FactBatch::new(revision, terminal))
            })?;
        if voided {
            self.unanswered.clear();
            self.pending.clear();
            self.pending_returns.clear();
            self.pending_casts.clear();
            self.lifecycle = Lifecycle::Finished;
            return Ok(CallProgress::ChildFinished);
        }
        if already_ended {
            self.feedback(
                call_id,
                "this session already ended its errand — a child returns or voids at most once",
            )?;
            return Ok(CallProgress::Go);
        }
        Ok(CallProgress::Cancelled)
    }

    async fn resolve_unknown(
        &self,
        log: &[Fact],
        facts: &[UnestablishedFact],
        budget: &RunBudget,
    ) -> Result<Result<bool, TurnError>, TurnCancelled> {
        let mut landed = false;
        for target in facts {
            if self.cancel.is_cancelled() {
                return Err(TurnCancelled);
            }
            let body = value_body(log, target.value).unwrap_or_default().to_string();

            for cast in &self.mediator.config().registry_config().casts {
                let resolved: Option<DimValue> = match &cast.resolution {
                    CastResolution::Constant(declared) if declared.dimension() == target.dimension => {
                        Some(declared.clone())
                    }
                    CastResolution::Constant(_) => None,
                    CastResolution::Resolver { .. } => match self.mediator.cast_backend(&cast.name) {
                        Some(backend) => {
                            let input = CastInput { body: body.clone() };
                            let resolve = backend.resolve(&input, self.mediator.engine().registry().trust_chain());
                            match self.wait(budget, resolve).await? {
                                Some(BackendCast::Resolved(dimension)) if dimension.dimension() == target.dimension => {
                                    Some(dimension)
                                }
                                _ => None,
                            }
                        }
                        None => None,
                    },
                };
                let Some(resolved) = resolved else {
                    continue;
                };

                let answer = CastAnswer {
                    cast: cast.name.clone(),
                    resolved,
                };
                let mut admitted = false;
                let outcome = self
                    .mediator
                    .store()
                    .finalize(&self.tenant, &self.session, |facts, revision| {
                        if self.cancel.is_cancelled() {
                            return None;
                        }
                        let projection = Projection::build(facts, revision);
                        let batch = self
                            .mediator
                            .engine()
                            .admit_cast(&projection.view(&self.session), target, answer)
                            .ok()?;
                        admitted = true;
                        Some(batch)
                    });
                if let Err(error) = outcome {
                    return Ok(Err(TurnError::Store(error)));
                }
                if self.cancel.is_cancelled() {
                    return Err(TurnCancelled);
                }
                if admitted {
                    landed = true;
                    break;
                }
            }
        }
        Ok(Ok(landed))
    }

    async fn derive_sanitized(
        &self,
        sanitizer: &SanitizerName,
        body: &str,
        budget: &RunBudget,
    ) -> Result<Option<String>, TurnCancelled> {
        let Some(backend) = self.mediator.sanitizer_backend(sanitizer) else {
            return Ok(None);
        };
        let input = SanitizerInput { body: body.to_string() };
        match self.wait(budget, backend.derive(&input)).await? {
            Some(SanitizerAnswer::Derived(derived)) => Ok(Some(derived)),
            Some(SanitizerAnswer::Failed) | None => Ok(None),
        }
    }

    async fn resolve_output_cast(
        &self,
        body: &str,
        dimension: Dimension,
        budget: &RunBudget,
    ) -> Result<Option<(CastName, DimValue)>, TurnCancelled> {
        for cast in &self.mediator.config().registry_config().casts {
            let resolved = match &cast.resolution {
                CastResolution::Constant(declared) if declared.dimension() == dimension => Some(declared.clone()),
                CastResolution::Constant(_) => None,
                CastResolution::Resolver { may_cast } => match self.mediator.cast_backend(&cast.name) {
                    Some(backend) => {
                        let input = CastInput { body: body.to_string() };
                        let resolve = backend.resolve(&input, self.mediator.engine().registry().trust_chain());
                        match self.wait(budget, resolve).await? {
                            Some(BackendCast::Resolved(resolved))
                                if resolved.dimension() == dimension && may_cast.admits(&resolved) =>
                            {
                                Some(resolved)
                            }
                            _ => None,
                        }
                    }
                    None => None,
                },
            };
            if let Some(resolved) = resolved {
                return Ok(Some((cast.name.clone(), resolved)));
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn offer_pending_cast(
        &mut self,
        call_id: &ToolCallId,
        dispatch: DispatchId,
        call: ResolvedCall,
        body: String,
        cast: CastName,
        resolved: DimValue,
        narrowing: Narrowing,
    ) -> Result<(), TurnError> {
        let handle = format!("remedy-{}", self.next_handle);
        self.next_handle += 1;
        let feedback = crate::feedback::cast_offer_feedback(&handle, &narrowing, self.acceptance_surface());
        self.pending_casts.push(PendingCast {
            handle,
            dispatch,
            call,
            body,
            cast,
            resolved,
            narrowing,
            offered_round: self.rounds,
        });
        // The pending-cast collection now owns this still-open dispatch for all terminal drains.
        self.open_dispatch = None;
        self.feedback(call_id, &feedback)
    }

    fn handle_execute_cast_accept(&mut self, call_id: &ToolCallId, index: usize) -> Result<CallProgress, TurnError> {
        if self.pending_casts[index].offered_round == self.rounds {
            let handle = self.pending_casts[index].handle.clone();
            self.feedback(call_id, &uninformed_acceptance_feedback(&handle))?;
            return Ok(CallProgress::Go);
        }

        let pending = self.pending_casts.remove(index);
        let admission = ResultAdmission::SuccessCastAccepted {
            body: ValueBody::new(pending.body.clone()),
            cast: pending.cast.clone(),
            resolved: pending.resolved.clone(),
            accepted: pending.narrowing.clone(),
        };
        match self.admit_result(&pending.dispatch, &pending.call, admission)? {
            Admission::Admitted => {
                self.mark_answered(call_id);
                Ok(CallProgress::Go)
            }
            Admission::Refused(AdmitError::AcceptanceMismatch) => {
                let narrowing = {
                    let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
                    let projection = Projection::build(&log, revision);
                    self.mediator
                        .engine()
                        .cast_narrowing(
                            &projection.view(&self.session),
                            &pending.dispatch,
                            &pending.call,
                            &pending.resolved,
                        )
                        .expect("dispatched call is registered")
                };
                match narrowing {
                    None => {
                        let plain = ResultAdmission::SuccessCast {
                            body: ValueBody::new(pending.body.clone()),
                            cast: pending.cast.clone(),
                            resolved: pending.resolved.clone(),
                        };
                        match self.admit_result(&pending.dispatch, &pending.call, plain)? {
                            Admission::Admitted => {
                                self.mark_answered(call_id);
                                Ok(CallProgress::Go)
                            }
                            Admission::Refused(AdmitError::NarrowingUnaccepted) => {
                                self.re_offer_pending_cast(call_id, pending)
                            }
                            Admission::Refused(_) => {
                                self.admit_result(&pending.dispatch, &pending.call, ResultAdmission::SuccessNoValue)?;
                                self.feedback(call_id, SEALED_FAILED)?;
                                Ok(CallProgress::Go)
                            }
                            Admission::AlreadyClosed => {
                                self.feedback(call_id, "no pending result awaits acceptance for that plan_id")?;
                                Ok(CallProgress::Go)
                            }
                            Admission::CancelSuppressed => {
                                self.pending_casts.push(pending);
                                Ok(CallProgress::Cancelled)
                            }
                            Admission::InvariantBreach => {
                                unreachable!("admit_result surfaces identity breaches as TurnError")
                            }
                        }
                    }
                    Some(_) => self.re_offer_pending_cast(call_id, pending),
                }
            }
            Admission::Refused(_) => {
                self.admit_result(&pending.dispatch, &pending.call, ResultAdmission::SuccessNoValue)?;
                self.feedback(call_id, SEALED_FAILED)?;
                Ok(CallProgress::Go)
            }
            Admission::AlreadyClosed => {
                self.feedback(call_id, "no pending result awaits acceptance for that plan_id")?;
                Ok(CallProgress::Go)
            }
            Admission::CancelSuppressed => {
                self.pending_casts.push(pending);
                Ok(CallProgress::Cancelled)
            }
            Admission::InvariantBreach => {
                unreachable!("admit_result surfaces identity breaches as TurnError")
            }
        }
    }

    fn re_offer_pending_cast(&mut self, call_id: &ToolCallId, pending: PendingCast) -> Result<CallProgress, TurnError> {
        let narrowing = {
            let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
            let projection = Projection::build(&log, revision);
            self.mediator
                .engine()
                .cast_narrowing(
                    &projection.view(&self.session),
                    &pending.dispatch,
                    &pending.call,
                    &pending.resolved,
                )
                .expect("dispatched call is registered")
        };
        let Some(narrowing) = narrowing else {
            let handle = pending.handle.clone();
            self.pending_casts.push(pending);
            self.feedback(
                call_id,
                &format!("the trajectory state changed; call execute_remedy_plan with plan_id \"{handle}\" again"),
            )?;
            return Ok(CallProgress::Go);
        };
        let feedback = crate::feedback::cast_offer_feedback(&pending.handle, &narrowing, self.acceptance_surface());
        self.pending_casts.push(PendingCast {
            narrowing,
            offered_round: self.rounds,
            ..pending
        });
        self.feedback(call_id, &feedback)?;
        Ok(CallProgress::Go)
    }

    fn drain_pending_casts(&self, views: &appa_engine::projection::Views) -> Vec<Fact> {
        let mut facts = Vec::new();
        for pending in &self.pending_casts {
            let admission = ResultAdmission::SuccessCastLapsed {
                body: ValueBody::new(pending.body.clone()),
                cast: pending.cast.clone(),
                resolved: pending.resolved.clone(),
            };
            match self
                .mediator
                .engine()
                .admit_result(views, &pending.dispatch, &pending.call, admission)
            {
                Ok(batch) => facts.extend(batch.facts),
                Err(AdmitError::NotOpen) => {}
                Err(error) => {
                    tracing::warn!(%error, tool = pending.call.tool().as_str(), "pending-cast drain lost a lapse");
                }
            }
        }
        facts
    }

    fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, TurnError> {
        let mut dispatch = None;
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                let projection = Projection::build(facts, revision);
                let views = projection.view(&self.session);
                let batch = self.mediator.engine().open_dispatch(&views, call).ok()?;
                dispatch = Some(DispatchId::new(
                    self.session.clone(),
                    call.digest(),
                    views.dispatch_count(&call.digest()),
                ));
                Some(batch)
            })?;
        Ok(dispatch)
    }

    async fn invoke_and_admit(
        &mut self,
        dispatch: DispatchId,
        call: &ResolvedCall,
        call_id: &ToolCallId,
        bound: Option<SanitizerName>,
        budget: &mut RunBudget,
    ) -> Result<CallProgress, TurnError> {
        budget.record_tool_invocation();
        self.open_dispatch = Some(OpenClose {
            dispatch: dispatch.clone(),
            call: call.clone(),
            close: CancelClose::Unobserved,
        });
        let outcome = match self.mediator.tool_backend(call.tool()) {
            Some(backend) => {
                let invoke = backend.invoke(call, budget.limits.body_cap_bytes);
                match self.wait(budget, invoke).await {
                    Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                    Ok(outcome) => outcome.unwrap_or(ToolOutcome::Indeterminate),
                }
            }
            None => ToolOutcome::Failure,
        };
        let close = match &outcome {
            ToolOutcome::Success { .. } => CancelClose::EffectsStand,
            ToolOutcome::Failure => CancelClose::Failed,
            ToolOutcome::Indeterminate => CancelClose::Unobserved,
        };
        self.open_dispatch
            .as_mut()
            .expect("this invocation opened a dispatch")
            .close = close;
        if matches!(outcome, ToolOutcome::Success { .. }) {
            self.mediator
                .store()
                .finalize(&self.tenant, &self.session, |facts, revision| {
                    let projection = Projection::build(facts, revision);
                    let views = projection.view(&self.session);
                    Some(
                        self.mediator
                            .engine()
                            .observe_success(&views, &dispatch, call)
                            .expect("the runtime holds this dispatch open and checkpoints it once"),
                    )
                })?;
        }
        if self.cancel.is_cancelled() {
            return Ok(CallProgress::Cancelled);
        }

        let contract = self.mediator.engine().registry().tool(call.tool());
        let pending_cast = contract.and_then(|contract| contract.pending_cast_dim());
        let mut withheld = None;
        let mut cast_offer: Option<(CastName, DimValue, String)> = None;
        let admission = match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            } if bound.is_some() => {
                let sanitizer = bound.expect("guarded by the arm condition");
                match self.derive_sanitized(&sanitizer, body, budget).await {
                    Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                    Ok(Some(derived)) => ResultAdmission::SuccessSanitized {
                        body: ValueBody::new(derived),
                        sanitizer,
                        raw_digest: RawResultDigest::of(body.as_bytes()),
                    },
                    Ok(None) => {
                        withheld = Some(SEALED_UNSANITIZED);
                        ResultAdmission::SuccessNoValue
                    }
                }
            }
            ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            } => match pending_cast {
                None => ResultAdmission::SuccessRaw {
                    body: ValueBody::new(body.clone()),
                },
                Some(dimension) => match self.resolve_output_cast(body, dimension, budget).await {
                    Err(TurnCancelled) => return Ok(CallProgress::Cancelled),
                    Ok(Some((cast, resolved))) => {
                        let narrowing = {
                            let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
                            let projection = Projection::build(&log, revision);
                            self.mediator
                                .engine()
                                .cast_narrowing(&projection.view(&self.session), &dispatch, call, &resolved)
                                .expect("dispatched call is registered")
                        };
                        if let Some(narrowing) = narrowing {
                            self.offer_pending_cast(
                                call_id,
                                dispatch,
                                call.clone(),
                                body.clone(),
                                cast,
                                resolved,
                                narrowing,
                            )?;
                            return Ok(CallProgress::Go);
                        }
                        cast_offer = Some((cast.clone(), resolved.clone(), body.clone()));
                        ResultAdmission::SuccessCast {
                            body: ValueBody::new(body.clone()),
                            cast,
                            resolved,
                        }
                    }
                    Ok(None) => {
                        withheld = Some(SEALED_UNRESOLVED);
                        ResultAdmission::SuccessNoValue
                    }
                },
            },
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Success {
                body: BodyDisposition::Unavailable,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Failure => ResultAdmission::Failure,
            ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
        };
        let admitted = match self.admit_result(&dispatch, call, admission)? {
            Admission::Admitted => true,
            Admission::Refused(AdmitError::NarrowingUnaccepted) => {
                let (cast, resolved, body) = cast_offer.expect("NarrowingUnaccepted arises only from a cast admission");
                let narrowing = {
                    let (log, revision) = self.mediator.store().snapshot(&self.tenant, &self.session)?;
                    let projection = Projection::build(&log, revision);
                    self.mediator
                        .engine()
                        .cast_narrowing(&projection.view(&self.session), &dispatch, call, &resolved)
                        .expect("dispatched call is registered")
                };
                match narrowing {
                    Some(narrowing) => {
                        self.offer_pending_cast(call_id, dispatch, call.clone(), body, cast, resolved, narrowing)?;
                        return Ok(CallProgress::Go);
                    }
                    None => {
                        self.admit_result(&dispatch, call, ResultAdmission::SuccessNoValue)?;
                        false
                    }
                }
            }
            Admission::Refused(_) => {
                self.admit_result(&dispatch, call, ResultAdmission::SuccessNoValue)?;
                false
            }
            Admission::AlreadyClosed => false,
            Admission::CancelSuppressed => return Ok(CallProgress::Cancelled),
            Admission::InvariantBreach => {
                unreachable!("admit_result surfaces identity breaches")
            }
        };
        self.open_dispatch = None;

        match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(_),
            } => {
                if let Some(token) = withheld {
                    self.feedback(call_id, token)?;
                } else if admitted {
                    self.mark_answered(call_id);
                } else {
                    self.feedback(call_id, SEALED_FAILED)?;
                }
            }
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => self.feedback(call_id, SEALED_WITHHELD)?,
            ToolOutcome::Success {
                body: BodyDisposition::Unavailable,
            } => self.feedback(call_id, SEALED_UNAVAILABLE)?,
            ToolOutcome::Failure => self.feedback(call_id, SEALED_FAILED)?,
            ToolOutcome::Indeterminate => self.feedback(call_id, SEALED_INDETERMINATE)?,
        }
        Ok(CallProgress::Go)
    }

    fn admit_result(
        &self,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<Admission, TurnError> {
        let value_carrying = matches!(
            admission,
            ResultAdmission::SuccessRaw { .. }
                | ResultAdmission::SuccessCast { .. }
                | ResultAdmission::SuccessCastAccepted { .. }
        );
        let mut admission = Some(admission);
        let mut result = Admission::AlreadyClosed;
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                if value_carrying && self.cancel.is_cancelled() {
                    result = Admission::CancelSuppressed;
                    return None;
                }
                let projection = Projection::build(facts, revision);
                let admission = admission.take()?;
                match self
                    .mediator
                    .engine()
                    .admit_result(&projection.view(&self.session), dispatch, call, admission)
                {
                    Ok(batch) => {
                        result = Admission::Admitted;
                        Some(batch)
                    }
                    Err(AdmitError::NotOpen) => None,
                    Err(
                        AdmitError::UnknownTool(_)
                        | AdmitError::DigestMismatch
                        | AdmitError::ForeignDispatch
                        | AdmitError::AlreadySucceeded
                        | AdmitError::SuccessContradicted,
                    ) => {
                        result = Admission::InvariantBreach;
                        None
                    }
                    Err(
                        error @ (AdmitError::OutputPendingCast
                        | AdmitError::NotPendingCast
                        | AdmitError::UnknownCast(_)
                        | AdmitError::ConstantMismatch
                        | AdmitError::CeilingExceeded
                        | AdmitError::NarrowingUnaccepted
                        | AdmitError::AcceptanceMismatch
                        | AdmitError::OutputSanitizerBound
                        | AdmitError::SanitizerBindingMismatch
                        | AdmitError::SanitizerTransitionUnmet),
                    ) => {
                        result = Admission::Refused(error);
                        None
                    }
                }
            })?;
        if matches!(result, Admission::InvariantBreach) {
            return Err(TurnError::DispatchIdentity);
        }
        Ok(result)
    }

    fn feedback(&mut self, call_id: &ToolCallId, content: &str) -> Result<(), TurnError> {
        self.append(vec![feedback_fact(&self.session, call_id, content)])?;
        self.mark_answered(call_id);
        Ok(())
    }

    fn mark_answered(&mut self, call_id: &ToolCallId) {
        if let Some(position) = self.unanswered.iter().position(|id| id == call_id) {
            self.unanswered.remove(position);
        }
    }

    fn acceptance_surface(&self) -> crate::feedback::FeedbackSurface {
        if self.is_child {
            crate::feedback::FeedbackSurface::Child
        } else {
            crate::feedback::FeedbackSurface::Root { can_fork: false }
        }
    }

    fn remaining_after_current(&self, call_id: &ToolCallId) -> Vec<ToolCallId> {
        let start = self
            .unanswered
            .iter()
            .position(|id| id == call_id)
            .map_or(self.unanswered.len(), |position| position + 1);
        self.unanswered[start..].to_vec()
    }

    fn append(&self, facts: Vec<Fact>) -> Result<(), TurnError> {
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |_, revision| {
                Some(FactBatch::new(revision, facts))
            })?;
        Ok(())
    }

    fn finish_turn_end(&mut self) -> Result<(), TurnError> {
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                let projection = Projection::build(facts, revision);
                let mut terminal = self.drain_pending_casts(&projection.view(&self.session));
                terminal.push(turn_end(&self.session));
                Some(FactBatch::new(revision, terminal))
            })?;
        self.pending.clear();
        self.pending_returns.clear();
        self.pending_casts.clear();
        Ok(())
    }

    fn finish_policy_stop(&mut self, message: &str) -> Result<Step, TurnError> {
        debug_assert!(self.open_dispatch.is_none());
        debug_assert!(self.unanswered.is_empty());
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                let projection = Projection::build(facts, revision);
                let mut terminal = self.drain_pending_casts(&projection.view(&self.session));
                terminal.push(Fact::AssistantMessage {
                    trajectory: self.session.clone(),
                    content: Some(message.to_string()),
                    calls: Vec::new(),
                });
                terminal.push(turn_end(&self.session));
                Some(FactBatch::new(revision, terminal))
            })?;
        self.pending.clear();
        self.pending_returns.clear();
        self.pending_casts.clear();
        self.lifecycle = Lifecycle::Finished;
        Ok(Step::PolicyStop(message.to_string()))
    }

    fn finish_cancelled(&mut self) -> Result<Step, TurnError> {
        let open = self.open_dispatch.clone();
        let unanswered = self.unanswered.clone();
        self.mediator
            .store()
            .finalize(&self.tenant, &self.session, |facts, revision| {
                let projection = Projection::build(facts, revision);
                let views = projection.view(&self.session);
                let mut terminal = Vec::new();
                if let Some(open) = &open {
                    let admission = match open.close {
                        CancelClose::Unobserved => ResultAdmission::Indeterminate,
                        CancelClose::EffectsStand => ResultAdmission::SuccessNoValue,
                        CancelClose::Failed => ResultAdmission::Failure,
                    };
                    if let Ok(batch) =
                        self.mediator
                            .engine()
                            .admit_result(&views, &open.dispatch, &open.call, admission)
                    {
                        terminal = batch.facts;
                    }
                }
                terminal.extend(self.drain_pending_casts(&views));
                terminal.extend(
                    unanswered
                        .iter()
                        .map(|call_id| feedback_fact(&self.session, call_id, POLICY_STOP_CANCELLED)),
                );
                terminal.push(Fact::AssistantMessage {
                    trajectory: self.session.clone(),
                    content: Some(POLICY_STOP_CANCELLED.to_string()),
                    calls: Vec::new(),
                });
                terminal.push(turn_end(&self.session));
                Some(FactBatch::new(revision, terminal))
            })?;
        self.open_dispatch = None;
        self.unanswered.clear();
        self.pending.clear();
        self.pending_returns.clear();
        self.pending_casts.clear();
        self.fork_identity = None;
        self.lifecycle = Lifecycle::Finished;
        Ok(Step::PolicyStop(POLICY_STOP_CANCELLED.to_string()))
    }

    async fn wait<F: Future>(&self, budget: &RunBudget, future: F) -> Result<Option<F::Output>, TurnCancelled> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(TurnCancelled),
            output = tokio::time::timeout(budget.external_budget(), future) => Ok(output.ok()),
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if self.lifecycle == Lifecycle::Finished {
            return;
        }
        if let Err(error) = self.finish_cancelled() {
            tracing::error!(error = %error, session = self.session.as_str(), "failed to shieldedly close an abandoned turn");
        }
    }
}

fn exact_nonempty_task(arguments: &serde_json::Value) -> Option<String> {
    let object = arguments.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let task = object.get("task")?.as_str()?;
    if task.is_empty() { None } else { Some(task.to_string()) }
}

fn proposal_of(call: &WireToolCall) -> Proposal {
    let trimmed = call.function.arguments.trim();
    let proposed = |arguments| ProposedCall {
        id: ToolCallId::new(call.id.clone()),
        tool: ToolName::new(call.function.name.clone()),
        arguments,
    };
    if trimmed.len() > appa_engine::params::MAX_ARGUMENT_BYTES {
        return Proposal {
            call: proposed(serde_json::json!({})),
            raw: "{}".to_string(),
            disposition: ArgumentDisposition::Oversized,
        };
    }
    let raw = if trimmed.is_empty() { "{}" } else { trimmed };
    let (arguments, disposition) = match serde_json::from_str(raw) {
        Ok(value) => (value, ArgumentDisposition::Parsed),
        Err(_) => (serde_json::json!({}), ArgumentDisposition::Malformed),
    };
    Proposal {
        call: proposed(arguments),
        raw: raw.to_string(),
        disposition,
    }
}

fn feedback_fact(session: &TrajectoryId, call_id: &ToolCallId, content: &str) -> Fact {
    Fact::BlockFeedback {
        trajectory: session.clone(),
        call_id: call_id.clone(),
        content: content.to_string(),
    }
}

fn turn_end(session: &TrajectoryId) -> Fact {
    Fact::Boundary {
        trajectory: session.clone(),
        kind: BoundaryKind::TurnEnd,
    }
}

fn return_plan_cost(plan: &ReturnPlan) -> u8 {
    match plan {
        // Crosses the value and narrows nothing.
        ReturnPlan::Sanitize { residual: None, .. } => 0,
        // Crosses a derivation, narrowing the parent by whatever the sanitizer could not shed.
        ReturnPlan::Sanitize { residual: Some(_), .. } => 1,
        // Crosses the value raw, narrowing the parent to this branch's label.
        ReturnPlan::Accept(_) => 2,
    }
}

fn describe_return_plan(plan: &ReturnPlan) -> String {
    match plan {
        ReturnPlan::Accept(_) => "accept the narrowing and return the result raw".to_string(),
        ReturnPlan::Sanitize {
            sanitizer,
            residual: None,
        } => format!("return the {} derivation instead", sanitizer.as_str()),
        ReturnPlan::Sanitize {
            sanitizer,
            residual: Some(_),
        } => format!(
            "return the {} derivation, accepting the residual narrowing",
            sanitizer.as_str()
        ),
    }
}

fn value_body(log: &[Fact], id: ValueId) -> Option<&str> {
    log.iter()
        .filter_map(|fact| match fact {
            Fact::ValueAdmitted { value, .. } => Some(value.body.as_str()),
            _ => None,
        })
        .nth(id.index() as usize)
}
