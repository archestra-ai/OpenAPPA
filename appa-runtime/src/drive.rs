//! RP2 turn-drive: the state machine that turns one admitted user turn into a final assistant answer,
//! mediating every proposed tool call through the pure engine.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use appa_engine::admit::{AdmitError, CastAnswer, ResultAdmission};
use appa_engine::authority::CastResolution;
use appa_engine::branch::{ReturnCheck, ReturnPlan, ReturnSubmission};
use appa_engine::check::{CheckOutcome, Narrowing, UnresolvedFact};
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall, ReturnPolicy};
use appa_engine::label::{DimValue, Dimension};
use appa_engine::names::{CastName, SanitizerName};
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, Provenance, RawResultDigest, ResolvedCall, ToolCallId, ToolName, TrajectoryId,
    ValueBody, ValueId,
};

use tokio_util::sync::CancellationToken;

use crate::admission::UserTurn;
use crate::external::{
    AuthorityAnswer, AuthorityRequest, CastAnswer as BackendCast, CastInput, SanitizerAnswer, SanitizerInput,
};
use crate::runtime::{EXECUTE_REMEDY_PLAN, Runtime, SUBMIT_RESULT};
use crate::store::{StoreError, TenantId};
use crate::tool::{BodyDisposition, RenderedCall, ToolOutcome};
use crate::transcript::model_transcript;
use crate::wire::{ChatCompletionRequest, WireToolCall};

const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
const SEALED_UNRESOLVED: &str = "[tool result withheld: its label could not be established]";
const SEALED_UNSANITIZED: &str = "[tool result withheld: the bound sanitizer produced no derivation]";
const SEALED_FAILED: &str = "[tool call failed]";
const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";
const POLICY_STOP_BUDGET: &str = "This turn reached its resource budget and was stopped.";
const POLICY_STOP_INFERENCE: &str = "This turn could not continue: upstream inference was unavailable.";
const POLICY_STOP_CANCELLED: &str = "This turn was cancelled.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Final(String),
    PolicyStop(String),
}

/// A genuine infrastructure failure the drive cannot resolve (an unrecoverable store fault, a
/// dispatch that stopped matching its own call). Policy outcomes — blocks, denials, budget stops,
/// inference faults — are *not* errors; they are turn facts.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch identity no longer matches its call/branch — a drive invariant was breached")]
    DispatchIdentity,
}

enum Admission {
    Admitted,
    AlreadyClosed,
    Refused(AdmitError),
    InvariantBreach,
    CancelSuppressed,
}

struct PendingBlock {
    call: ResolvedCall,
    offers: Vec<(String, appa_engine::plan::RemedyPlan)>,
}

struct PendingReturn {
    parent: TrajectoryId,
    body: String,
    raw_digest: RawResultDigest,
    offers: Vec<(String, ReturnPlan)>,
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
    malformed: bool,
}

/// Drive one user turn to completion. `is_child` gates the `submit_result` reserved tool (RP6).
pub async fn drive_turn(
    rt: &Runtime,
    tenant: &TenantId,
    session: &TrajectoryId,
    is_child: bool,
    user_turn: UserTurn,
    cancel: CancellationToken,
) -> Result<TurnOutcome, DriveError> {
    let lease = rt.store().turn_lock(tenant, session)?;
    // Waiting for the lease is itself a cancellable state (RP2). Cancelled here the turn never
    // began: no fact of it exists, so the trajectory replays identically without it — a TurnEnd for
    // a turn that admitted nothing would be a spurious boundary, not added auditability. Biased so
    // a pre-cancelled token wins even over an immediately free lease.
    let _turn = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string())),
        guard = lease.lock() => guard,
    };
    let mut drive = Drive {
        rt,
        tenant,
        session,
        is_child,
        cancel,
        deadline: Instant::now() + rt.budgets().turn_deadline,
        rounds: 0,
        invocations: 0,
        pending: Vec::new(),
        pending_returns: Vec::new(),
        pending_casts: Vec::new(),
        remedy_attempts: BTreeMap::new(),
        return_derivation_attempts: BTreeMap::new(),
        next_handle: 0,
    };
    let outcome = drive.run(user_turn).await;
    if outcome.is_err() {
        if let Err(error) = drive.drain_on_error() {
            tracing::warn!(%error, "error-path drain could not land its closes");
        }
    }
    outcome
}

struct Drive<'a> {
    rt: &'a Runtime,
    tenant: &'a TenantId,
    session: &'a TrajectoryId,
    is_child: bool,
    cancel: CancellationToken,
    deadline: Instant,
    rounds: u32,
    invocations: u32,
    pending: Vec<PendingBlock>,
    pending_returns: Vec<PendingReturn>,
    pending_casts: Vec<PendingCast>,
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    return_derivation_attempts: BTreeMap<(RawResultDigest, SanitizerName), u32>,
    next_handle: u32,
}

struct TurnCancelled;

enum RawReturnGo {
    Merge,
    Answered,
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

impl Drive<'_> {
    async fn run(&mut self, user_turn: UserTurn) -> Result<TurnOutcome, DriveError> {
        self.admit_user_turn(user_turn)?;

        loop {
            let budgets = self.rt.budgets();
            if self.rounds >= budgets.max_inference_rounds || Instant::now() >= self.deadline {
                return self.finish_policy_stop(POLICY_STOP_BUDGET);
            }
            self.rounds += 1;

            let (log, _) = self.rt.store().snapshot(self.tenant, self.session)?;
            let messages = model_transcript(self.rt.preamble(), &log, self.session);
            let request = ChatCompletionRequest {
                model: String::new(),
                messages,
                tools: Some(self.rt.advertised_tools(self.is_child)),
                stream: None,
            };
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let completion = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return self.finish_cancelled(None, &[]),
                out = tokio::time::timeout(remaining, self.rt.inference().complete(request)) => match out {
                    Ok(Ok(completion)) => completion,
                    Ok(Err(_)) => return self.finish_policy_stop(POLICY_STOP_INFERENCE),
                    Err(_) => return self.finish_policy_stop(POLICY_STOP_BUDGET),
                },
            };

            if self.cancel.is_cancelled() {
                return self.finish_cancelled(None, &[]);
            }
            let proposals: Vec<Proposal> = completion.tool_calls.iter().map(proposal_of).collect();
            let calls: Vec<ProposedCall> = proposals.iter().map(|p| p.call.clone()).collect();
            self.append(vec![Fact::AssistantMessage {
                trajectory: self.session.clone(),
                content: completion.content.clone(),
                calls,
            }])?;

            if proposals.is_empty() {
                self.finish_turn_end()?;
                return Ok(TurnOutcome::Final(completion.content.unwrap_or_default()));
            }

            let mut budget_hit = false;
            for index in 0..proposals.len() {
                let proposal = &proposals[index];
                if self.cancel.is_cancelled() {
                    let unanswered: Vec<ToolCallId> = proposals[index..].iter().map(|p| p.call.id.clone()).collect();
                    return self.finish_cancelled(None, &unanswered);
                }
                if budget_hit {
                    self.feedback(&proposal.call.id, POLICY_STOP_BUDGET)?;
                    continue;
                }
                if proposal.malformed {
                    self.feedback(
                        &proposal.call.id,
                        "the tool call had malformed arguments and was not executed",
                    )?;
                    continue;
                }
                match self.handle_call(&proposal.call).await? {
                    CallGo => {}
                    CallStop => budget_hit = true,
                    CallCancelled(open) => {
                        let unanswered: Vec<ToolCallId> =
                            proposals[index..].iter().map(|p| p.call.id.clone()).collect();
                        return self.finish_cancelled(open, &unanswered);
                    }
                }
            }
            if budget_hit {
                return self.finish_policy_stop(POLICY_STOP_BUDGET);
            }
        }
    }

    async fn handle_call(&mut self, proposed: &ProposedCall) -> Result<CallProgress, DriveError> {
        let call_id = &proposed.id;
        match proposed.tool.as_str() {
            EXECUTE_REMEDY_PLAN => return self.handle_execute_remedy(call_id, &proposed.arguments).await,
            SUBMIT_RESULT => return self.handle_submit_result(call_id, &proposed.arguments).await,
            _ => {}
        }
        let call = ResolvedCall::new(proposed.tool.clone(), proposed.arguments.clone(), Vec::new());
        self.mediate(call_id, call).await
    }

    async fn mediate(&mut self, call_id: &ToolCallId, call: ResolvedCall) -> Result<CallProgress, DriveError> {
        loop {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            let views = projection.view(self.session);
            match self.rt.engine().check(&views, &call) {
                Err(_) => {
                    self.feedback(call_id, "no such tool is registered")?;
                    return Ok(CallGo);
                }
                Ok(CheckOutcome::Allow) => {
                    if self.invocations >= self.rt.budgets().max_tool_invocations || self.past_deadline() {
                        self.feedback(call_id, POLICY_STOP_BUDGET)?;
                        return Ok(CallStop);
                    }
                    drop(projection);
                    match self.open_dispatch(&call)? {
                        Some(dispatch) => return self.invoke_and_admit(dispatch, &call, call_id).await,
                        None => {
                            self.feedback(call_id, "the call could not be dispatched (the policy state changed)")?
                        }
                    }
                    return Ok(CallGo);
                }
                Ok(CheckOutcome::Unresolved(facts)) => {
                    drop(projection);
                    match self.resolve_unknown(&log, &facts).await {
                        Err(TurnCancelled) => return Ok(CallCancelled(None)),
                        Ok(resolved) => {
                            if resolved? {
                                continue; // a dimension was cast — re-check on the new revision
                            }
                        }
                    }
                    self.feedback(call_id, "the call has an unresolved label that no cast could resolve")?;
                    return Ok(CallGo);
                }
                Ok(CheckOutcome::Block(raw)) => {
                    let planned = self
                        .rt
                        .engine()
                        .plan(&views, &call, &raw)
                        .expect("checked tool is registered");
                    let feedback = if planned.plans.is_empty() {
                        crate::feedback::block_feedback(&raw, &planned, &[])
                    } else {
                        drop(projection);
                        let attempts = self.remedy_attempts.entry(call.digest()).or_insert(0);
                        *attempts += 1;
                        if *attempts > self.rt.budgets().max_remedy_attempts_per_gap {
                            self.feedback(call_id, "the remedy attempt limit for this call was reached")?;
                            return Ok(CallGo);
                        }
                        let offers: Vec<(String, appa_engine::plan::RemedyPlan)> = planned
                            .plans
                            .iter()
                            .map(|plan| {
                                let handle = format!("remedy-{}", self.next_handle);
                                self.next_handle += 1;
                                (handle, plan.clone())
                            })
                            .collect();
                        let feedback = crate::feedback::block_feedback(&raw, &planned, &offers);
                        self.pending.push(PendingBlock { call, offers });
                        feedback
                    };
                    self.feedback(call_id, &feedback)?;
                    return Ok(CallGo);
                }
            }
        }
    }

    async fn handle_execute_remedy(
        &mut self,
        call_id: &ToolCallId,
        arguments: &serde_json::Value,
    ) -> Result<CallProgress, DriveError> {
        let Some(handle) = arguments.get("plan_id").and_then(|v| v.as_str()) else {
            self.feedback(call_id, "execute_remedy_plan requires a string plan_id")?;
            return Ok(CallGo);
        };
        if let Some(index) = self
            .pending_returns
            .iter()
            .position(|p| p.offers.iter().any(|(h, _)| h == handle))
        {
            return self.handle_execute_return_remedy(call_id, index, handle).await;
        }
        if let Some(index) = self.pending_casts.iter().position(|p| p.handle == handle) {
            return self.handle_execute_cast_accept(call_id, index);
        }
        let Some(cohort_index) = self
            .pending
            .iter()
            .position(|p| p.offers.iter().any(|(h, _)| h == handle))
        else {
            self.feedback(call_id, "no pending blocked call offers that plan_id")?;
            return Ok(CallGo);
        };
        if self.invocations >= self.rt.budgets().max_tool_invocations || self.past_deadline() {
            self.feedback(call_id, POLICY_STOP_BUDGET)?;
            return Ok(CallStop);
        }
        let call = self.pending[cohort_index].call.clone();
        let chosen = self.pending[cohort_index]
            .offers
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, plan)| plan.clone())
            .expect("the cohort was found by this handle");

        let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(self.session);
        let still_offered = match self.rt.engine().check(&views, &call) {
            Ok(CheckOutcome::Block(raw)) => self
                .rt
                .engine()
                .plan(&views, &call, &raw)
                .expect("pending call is registered")
                .plans
                .contains(&chosen),
            _ => false,
        };
        if !still_offered {
            self.pending.remove(cohort_index);
            self.feedback(
                call_id,
                "the state changed and this offer no longer applies; re-propose the call",
            )?;
            return Ok(CallGo);
        }
        let dispatch = DispatchId::new(
            self.session.clone(),
            call.digest(),
            views.dispatch_count(&call.digest()),
        );

        let mut rulings = Vec::new();
        for req in &chosen.required {
            let Some(backend) = self.rt.authority_backend(&req.authority) else {
                self.feedback(call_id, "an authority for this plan is not configured")?;
                return Ok(CallGo);
            };
            let request = match AuthorityRequest::new(req.authority.clone(), &call, req.covers.clone(), &views) {
                Ok(request) => request,
                Err(_) => {
                    self.feedback(call_id, "the call's argument references no longer resolve")?;
                    return Ok(CallGo);
                }
            };
            let answer = match self.wait(backend.rule(&request)).await {
                Err(TurnCancelled) => return Ok(CallCancelled(None)),
                Ok(answer) => answer.unwrap_or(AuthorityAnswer::Abstain),
            };
            match answer {
                // The ruling records the review context put to the authority, verbatim.
                AuthorityAnswer::Approve => rulings.push(Ruling {
                    dispatch: dispatch.clone(),
                    authority: req.authority.clone(),
                    issuer: Issuer::Authority,
                    covers: req.covers.clone(),
                    reviewed: request.review(),
                }),
                AuthorityAnswer::Deny | AuthorityAnswer::Abstain => {
                    let cohort = &mut self.pending[cohort_index];
                    cohort.offers.retain(|(h, _)| h != handle);
                    let feedback = crate::feedback::denial_feedback(&cohort.offers);
                    if cohort.offers.is_empty() {
                        self.pending.remove(cohort_index);
                    }
                    self.feedback(call_id, &feedback)?;
                    return Ok(CallGo);
                }
            }
        }

        let batch = match self
            .rt
            .engine()
            .execute_plan(&views, &chosen, &call, &rulings, Sink::Tool)
        {
            Ok(batch) => batch,
            Err(_) => {
                self.feedback(call_id, "the remedy plan could not be executed on the current state")?;
                return Ok(CallGo);
            }
        };
        drop(projection);
        match self.rt.store().conditional_append(self.tenant, self.session, batch) {
            Ok(_) => {}
            Err(StoreError::Stale { .. }) => {
                self.feedback(call_id, "the state changed; retry the remedy")?;
                return Ok(CallGo);
            }
            Err(e) => return Err(DriveError::Store(e)),
        }
        self.pending.remove(cohort_index);
        self.invoke_and_admit(dispatch, &call, call_id).await
    }

    async fn handle_submit_result(
        &mut self,
        call_id: &ToolCallId,
        arguments: &serde_json::Value,
    ) -> Result<CallProgress, DriveError> {
        if !self.is_child {
            self.feedback(call_id, "submit_result is available only to a child session")?;
            return Ok(CallGo);
        }
        let exact_shape = arguments
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("value"));
        let body = match exact_shape {
            Some(serde_json::Value::String(value)) => value.clone(),
            Some(serde_json::Value::Null) => {
                self.feedback(call_id, "no result returned to the parent")?;
                return Ok(CallGo);
            }
            _ => {
                self.feedback(
                    call_id,
                    "submit_result takes exactly one key, `value`: a string result, or null to return nothing",
                )?;
                return Ok(CallGo);
            }
        };

        if let Some(parent) = self.rt.store().parent_of(self.tenant, self.session)? {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            if projection.view(&parent).returns_by(self.session) > 0 {
                self.feedback(
                    call_id,
                    "this session already returned its result — a child returns at most once",
                )?;
                return Ok(CallGo);
            }
        }

        let returned = match self.rt.config().child_return_policy() {
            ReturnPolicy::Sanitized(sanitizer) => match self.derive_sanitized(&sanitizer, &body).await {
                // Cancelled before any return was recorded: nothing crossed, only the seal is owed.
                Err(TurnCancelled) => return Ok(CallCancelled(None)),
                Ok(Some(derived)) => ReturnSubmission::Derived {
                    body: ValueBody::new(derived),
                    // The audit digest binds to the raw submission, not the derivation.
                    raw_digest: RawResultDigest::of(body.as_bytes()),
                },
                Ok(None) => {
                    self.feedback(call_id, "the result could not be sanitized for return")?;
                    return Ok(CallGo);
                }
            },
            ReturnPolicy::Raw => match self.check_raw_return(call_id, &body)? {
                RawReturnGo::Merge => ReturnSubmission::Raw {
                    body: ValueBody::new(body.clone()),
                },
                RawReturnGo::Answered => return Ok(CallGo),
            },
        };

        let Some(parent) = self.rt.store().parent_of(self.tenant, self.session)? else {
            self.feedback(call_id, "this session cannot submit a result")?;
            return Ok(CallGo);
        };
        let mut crossed = false;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            if self.cancel.is_cancelled() {
                return None;
            }
            let projection = Projection::build(facts, rev);
            let views = projection.view(&parent);
            let batch = self
                .rt
                .engine()
                .submit_child_return(&views, self.session, returned)
                .ok()?;
            crossed = true;
            Some(batch)
        })?;
        if !crossed {
            if self.cancel.is_cancelled() {
                return Ok(CallCancelled(None));
            }
            self.feedback(call_id, "this session cannot submit a result")?;
            return Ok(CallGo);
        }
        self.feedback(call_id, "result submitted to the parent")?;
        Ok(CallGo)
    }

    fn check_raw_return(&mut self, call_id: &ToolCallId, body: &str) -> Result<RawReturnGo, DriveError> {
        let Some(parent) = self.rt.store().parent_of(self.tenant, self.session)? else {
            return Ok(RawReturnGo::Merge);
        };
        let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&parent);
        match self.rt.engine().check_child_return(&views, self.session) {
            Ok(ReturnCheck::Allow) => Ok(RawReturnGo::Merge),
            Ok(ReturnCheck::Unresolved(_)) => {
                self.feedback(
                    call_id,
                    "the return cannot be decided: a label dimension is unresolved; resolve it first or return null",
                )?;
                Ok(RawReturnGo::Answered)
            }
            Ok(ReturnCheck::Block { plans, .. }) => {
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
                    .map(|(handle, plan)| format!("\"{handle}\" to {}", describe_return_plan(plan)))
                    .collect();
                let feedback = format!(
                    "returning this raw would narrow the parent; call execute_remedy_plan with plan_id {}; or submit_result null to return nothing",
                    menu.join(", ")
                );
                self.pending_returns.push(PendingReturn {
                    parent,
                    body: body.to_string(),
                    raw_digest: RawResultDigest::of(body.as_bytes()),
                    offers,
                });
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
    ) -> Result<CallProgress, DriveError> {
        let plan = self.pending_returns[index]
            .offers
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, plan)| plan.clone())
            .expect("caller located this handle in this pending return");

        let parent = self.pending_returns[index].parent.clone();
        {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            if projection.view(&parent).returns_by(self.session) > 0 {
                self.pending_returns.clear();
                self.feedback(
                    call_id,
                    "this session already returned its result — a child returns at most once",
                )?;
                return Ok(CallGo);
            }
        }

        let submission = match &plan {
            ReturnPlan::Accept(_) => ReturnSubmission::Raw {
                body: ValueBody::new(self.pending_returns[index].body.clone()),
            },
            ReturnPlan::Sanitize { sanitizer, .. } => {
                let sanitizer = sanitizer.clone();
                let key = (self.pending_returns[index].raw_digest, sanitizer.clone());
                let charges = self.return_derivation_attempts.entry(key).or_insert(0);
                *charges += 1;
                if *charges > self.rt.budgets().max_remedy_attempts_per_gap {
                    self.feedback(call_id, "the remedy attempt limit for this return was reached")?;
                    return Ok(CallGo);
                }
                let body = self.pending_returns[index].body.clone();
                match self.derive_sanitized(&sanitizer, &body).await {
                    // Cancelled before any crossing: the pending attempt dies with the turn.
                    Err(TurnCancelled) => return Ok(CallCancelled(None)),
                    Ok(Some(derived)) => ReturnSubmission::Derived {
                        body: ValueBody::new(derived),
                        raw_digest: self.pending_returns[index].raw_digest,
                    },
                    // Fail closed and restore: the plan stays offered within its budget.
                    Ok(None) => {
                        self.feedback(call_id, "the derivation failed; the return offer remains available")?;
                        return Ok(CallGo);
                    }
                }
            }
        };

        let mut executed = false;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            if self.cancel.is_cancelled() {
                return None;
            }
            let projection = Projection::build(facts, rev);
            let views = projection.view(&parent);
            let batch = self
                .rt
                .engine()
                .execute_child_return_plan(&views, self.session, plan.clone(), submission)
                .ok()?;
            executed = true;
            Some(batch)
        })?;
        if executed {
            self.pending_returns.clear();
            self.feedback(call_id, "result submitted to the parent")?;
            return Ok(CallGo);
        }
        if self.cancel.is_cancelled() {
            return Ok(CallCancelled(None));
        }
        self.pending_returns.remove(index);
        self.feedback(call_id, "the return offer is stale; submit the result again")?;
        Ok(CallGo)
    }

    async fn resolve_unknown(
        &self,
        log: &[Fact],
        facts: &[UnresolvedFact],
    ) -> Result<Result<bool, DriveError>, TurnCancelled> {
        let Some(target) = facts.first() else {
            return Ok(Ok(false));
        };
        let body = value_body(log, target.value).unwrap_or_default().to_string();

        for cast in &self.rt.config().registry_config().casts {
            let resolved: Option<DimValue> = match &cast.resolution {
                CastResolution::Constant(declared) if declared.dimension() == target.dimension => {
                    Some(declared.clone())
                }
                CastResolution::Constant(_) => None,
                CastResolution::Resolver { .. } => match self.rt.cast_backend(&cast.name) {
                    Some(backend) => {
                        let input = CastInput { body: body.clone() };
                        let resolve = backend.resolve(&input, self.rt.engine().registry().trust_chain());
                        match self.wait(resolve).await? {
                            Some(BackendCast::Resolved(dim)) if dim.dimension() == target.dimension => Some(dim),
                            _ => None, // unresolved, wrong dimension, or timed out → fail closed
                        }
                    }
                    None => None,
                },
            };
            let Some(resolved) = resolved else {
                continue;
            };

            let (fresh, rev) = match self.rt.store().snapshot(self.tenant, self.session) {
                Ok(snapshot) => snapshot,
                Err(e) => return Ok(Err(DriveError::Store(e))),
            };
            let projection = Projection::build(&fresh, rev);
            let views = projection.view(self.session);
            let answer = CastAnswer {
                cast: cast.name.clone(),
                resolved,
            };
            if let Ok(batch) = self.rt.engine().admit_cast(&views, target, answer) {
                drop(projection);
                match self.rt.store().conditional_append(self.tenant, self.session, batch) {
                    Ok(_) => return Ok(Ok(true)),
                    Err(StoreError::Stale { .. }) => return Ok(Ok(true)), // the re-check re-derives on the new revision
                    Err(e) => return Ok(Err(DriveError::Store(e))),
                }
            }
        }
        Ok(Ok(false))
    }

    async fn derive_sanitized(&self, sanitizer: &SanitizerName, body: &str) -> Result<Option<String>, TurnCancelled> {
        let Some(backend) = self.rt.sanitizer_backend(sanitizer) else {
            return Ok(None);
        };
        let input = SanitizerInput { body: body.to_string() };
        match self.wait(backend.derive(&input)).await? {
            Some(SanitizerAnswer::Derived(derived)) => Ok(Some(derived)),
            Some(SanitizerAnswer::Failed) | None => Ok(None),
        }
    }

    async fn resolve_output_cast(
        &self,
        body: &str,
        dimension: Dimension,
    ) -> Result<Option<(CastName, DimValue)>, TurnCancelled> {
        for cast in &self.rt.config().registry_config().casts {
            let resolved = match &cast.resolution {
                CastResolution::Constant(declared) if declared.dimension() == dimension => Some(declared.clone()),
                CastResolution::Constant(_) => None,
                CastResolution::Resolver { may_cast } => match self.rt.cast_backend(&cast.name) {
                    Some(backend) => {
                        let input = CastInput { body: body.to_string() };
                        let resolve = backend.resolve(&input, self.rt.engine().registry().trust_chain());
                        match self.wait(resolve).await? {
                            Some(BackendCast::Resolved(dim))
                                if dim.dimension() == dimension && may_cast.admits(&dim) =>
                            {
                                Some(dim)
                            }
                            _ => None, // unresolved, wrong dimension, out of ceiling, or timed out
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
    ) -> Result<(), DriveError> {
        let handle = format!("remedy-{}", self.next_handle);
        self.next_handle += 1;
        let feedback = crate::feedback::cast_offer_feedback(&handle, &narrowing);
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
        self.feedback(call_id, &feedback)
    }

    fn handle_execute_cast_accept(&mut self, call_id: &ToolCallId, index: usize) -> Result<CallProgress, DriveError> {
        if self.pending_casts[index].offered_round == self.rounds {
            let handle = self.pending_casts[index].handle.clone();
            self.feedback(
                call_id,
                &format!(
                    "this acceptance predates the offer it names; read the offer, then call execute_remedy_plan with plan_id \"{handle}\" in your next response"
                ),
            )?;
            return Ok(CallGo);
        }
        let pending = self.pending_casts.remove(index);
        let admission = ResultAdmission::SuccessCastAccepted {
            body: ValueBody::new(pending.body.clone()),
            cast: pending.cast.clone(),
            resolved: pending.resolved.clone(),
            accepted: pending.narrowing.clone(),
        };
        match self.admit_result(&pending.dispatch, &pending.call, admission)? {
            Admission::Admitted => Ok(CallGo),
            Admission::Refused(AdmitError::AcceptanceMismatch) => {
                let narrowing = {
                    let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
                    let projection = Projection::build(&log, rev);
                    self.rt
                        .engine()
                        .cast_narrowing(&projection.view(self.session), &pending.call, &pending.resolved)
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
                            Admission::Admitted => Ok(CallGo),
                            Admission::Refused(AdmitError::NarrowingUnaccepted) => {
                                self.re_offer_pending_cast(call_id, pending)
                            }
                            Admission::Refused(_) => {
                                self.admit_result(&pending.dispatch, &pending.call, ResultAdmission::SuccessNoValue)?;
                                self.feedback(call_id, SEALED_FAILED)?;
                                Ok(CallGo)
                            }
                            Admission::AlreadyClosed => {
                                self.feedback(call_id, "no pending result awaits acceptance for that plan_id")?;
                                Ok(CallGo)
                            }
                            Admission::CancelSuppressed => {
                                self.pending_casts.push(pending);
                                Ok(CallCancelled(None))
                            }
                            Admission::InvariantBreach => {
                                unreachable!("admit_result surfaces an identity breach as DriveError")
                            }
                        }
                    }
                    Some(_) => self.re_offer_pending_cast(call_id, pending),
                }
            }
            Admission::Refused(_) => {
                self.admit_result(&pending.dispatch, &pending.call, ResultAdmission::SuccessNoValue)?;
                self.feedback(call_id, SEALED_FAILED)?;
                Ok(CallGo)
            }
            Admission::AlreadyClosed => {
                self.feedback(call_id, "no pending result awaits acceptance for that plan_id")?;
                Ok(CallGo)
            }
            Admission::CancelSuppressed => {
                self.pending_casts.push(pending);
                Ok(CallCancelled(None))
            }
            Admission::InvariantBreach => unreachable!("admit_result surfaces an identity breach as DriveError"),
        }
    }

    fn re_offer_pending_cast(
        &mut self,
        call_id: &ToolCallId,
        pending: PendingCast,
    ) -> Result<CallProgress, DriveError> {
        let narrowing = {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            self.rt
                .engine()
                .cast_narrowing(&projection.view(self.session), &pending.call, &pending.resolved)
                .expect("dispatched call is registered")
        };
        let Some(narrowing) = narrowing else {
            let handle = pending.handle.clone();
            self.pending_casts.push(pending);
            self.feedback(
                call_id,
                &format!("the trajectory state changed; call execute_remedy_plan with plan_id \"{handle}\" again"),
            )?;
            return Ok(CallGo);
        };
        let handle = pending.handle.clone();
        let feedback = crate::feedback::cast_offer_feedback(&handle, &narrowing);
        // The re-derived narrowing is new information: acceptance again requires a later round.
        self.pending_casts.push(PendingCast {
            narrowing,
            offered_round: self.rounds,
            ..pending
        });
        self.feedback(call_id, &feedback)?;
        Ok(CallGo)
    }

    fn drain_facts(&self, views: &appa_engine::projection::Views) -> Vec<Fact> {
        let mut facts = Vec::new();
        for pending in &self.pending_casts {
            let admission = ResultAdmission::SuccessCastLapsed {
                body: ValueBody::new(pending.body.clone()),
                cast: pending.cast.clone(),
                resolved: pending.resolved.clone(),
            };
            match self
                .rt
                .engine()
                .admit_result(views, &pending.dispatch, &pending.call, admission)
            {
                Ok(batch) => facts.extend(batch.facts),
                // The intended skip: the dispatch raced closed and owes nothing.
                Err(AdmitError::NotOpen) => {}
                // Anything else is a lost lapse audit — never fatal in a terminal, never silent.
                Err(error) => {
                    tracing::warn!(%error, tool = pending.call.tool().as_str(), "pending-cast drain lost a lapse");
                }
            }
        }
        facts
    }

    fn drain_on_error(&self) -> Result<(), DriveError> {
        if self.pending_casts.is_empty() {
            return Ok(());
        }
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let drained = self.drain_facts(&views);
            if drained.is_empty() {
                None
            } else {
                Some(FactBatch::new(rev, drained))
            }
        })?;
        Ok(())
    }

    // --- appends -------------------------------------------------------------

    fn admit_user_turn(&self, user_turn: UserTurn) -> Result<(), DriveError> {
        let value = appa_engine::value::LabeledValue::new(
            ValueBody::new(user_turn.into_string()),
            self.rt.config().boundary_label().clone(),
        );
        self.append(vec![Fact::ValueAdmitted {
            trajectory: self.session.clone(),
            value,
            provenance: Provenance::UserInput,
        }])
    }

    fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, DriveError> {
        let mut dispatch = None;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let batch = self.rt.engine().open_dispatch(&views, call).ok()?;
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
    ) -> Result<CallProgress, DriveError> {
        self.invocations += 1;
        let rendered = RenderedCall::from_call(call);
        let outcome = match self.rt.tool_backend(call.tool()) {
            Some(backend) => {
                let invoke = backend.invoke(&rendered, self.rt.budgets().body_cap_bytes);
                match self.wait(invoke).await {
                    Err(TurnCancelled) => {
                        return Ok(CallCancelled(Some(OpenClose {
                            dispatch,
                            call: call.clone(),
                            close: CancelClose::Unobserved,
                        })));
                    }
                    Ok(outcome) => outcome.unwrap_or(ToolOutcome::Indeterminate),
                }
            }
            None => ToolOutcome::Failure,
        };
        if self.cancel.is_cancelled() {
            let close = match &outcome {
                ToolOutcome::Success { .. } => CancelClose::EffectsStand,
                ToolOutcome::Failure => CancelClose::Failed,
                ToolOutcome::Indeterminate => CancelClose::Unobserved,
            };
            return Ok(CallCancelled(Some(OpenClose {
                dispatch,
                call: call.clone(),
                close,
            })));
        }
        let contract = self.rt.engine().registry().tool(call.tool());
        let pending_cast = contract.and_then(|c| c.pending_cast_dim());
        let bound_sanitizer = contract.and_then(|c| c.output_sanitizer.clone());
        let mut withheld: Option<&str> = None;
        // Set on a cast admission so a raced `NarrowingUnaccepted` refusal can re-offer (D2).
        let mut cast_offer: Option<(CastName, DimValue, String)> = None;
        let admission = match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            } => match (pending_cast, bound_sanitizer) {
                (None, None) => ResultAdmission::SuccessRaw {
                    body: ValueBody::new(body.clone()),
                },
                (Some(dimension), _) => match self.resolve_output_cast(body, dimension).await {
                    Err(TurnCancelled) => {
                        return Ok(CallCancelled(Some(OpenClose {
                            dispatch,
                            call: call.clone(),
                            close: CancelClose::EffectsStand,
                        })));
                    }
                    Ok(Some((cast, resolved))) => {
                        let narrowing = {
                            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
                            let projection = Projection::build(&log, rev);
                            self.rt
                                .engine()
                                .cast_narrowing(&projection.view(self.session), call, &resolved)
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
                            return Ok(CallGo);
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
                (None, Some(sanitizer)) => match self.derive_sanitized(&sanitizer, body).await {
                    Err(TurnCancelled) => {
                        return Ok(CallCancelled(Some(OpenClose {
                            dispatch,
                            call: call.clone(),
                            close: CancelClose::EffectsStand,
                        })));
                    }
                    Ok(Some(derived)) => ResultAdmission::SuccessSanitized {
                        body: ValueBody::new(derived),
                        sanitizer,
                        raw_digest: RawResultDigest::of(body.as_bytes()),
                    },
                    Ok(None) => {
                        withheld = Some(SEALED_UNSANITIZED);
                        ResultAdmission::SuccessNoValue
                    }
                },
            },
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Failure => ResultAdmission::Failure,
            ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
        };
        let admitted = match self.admit_result(&dispatch, call, admission)? {
            Admission::Admitted => true,
            Admission::Refused(AdmitError::NarrowingUnaccepted) => {
                let (cast, resolved, body) = cast_offer.expect("NarrowingUnaccepted arises only from a cast admission");
                let narrowing = {
                    let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
                    let projection = Projection::build(&log, rev);
                    self.rt
                        .engine()
                        .cast_narrowing(&projection.view(self.session), call, &resolved)
                        .expect("dispatched call is registered")
                };
                match narrowing {
                    Some(narrowing) => {
                        self.offer_pending_cast(call_id, dispatch, call.clone(), body, cast, resolved, narrowing)?;
                        return Ok(CallGo);
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
            Admission::CancelSuppressed => {
                let close = match &outcome {
                    ToolOutcome::Success { .. } => CancelClose::EffectsStand,
                    ToolOutcome::Failure => CancelClose::Failed,
                    ToolOutcome::Indeterminate => CancelClose::Unobserved,
                };
                return Ok(CallCancelled(Some(OpenClose {
                    dispatch,
                    call: call.clone(),
                    close,
                })));
            }
            Admission::InvariantBreach => unreachable!("admit_result surfaces an identity breach as DriveError"),
        };

        match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(_),
            } => {
                if let Some(sealed) = withheld {
                    self.feedback(call_id, sealed)?;
                } else if !admitted {
                    self.feedback(call_id, SEALED_FAILED)?;
                }
            }
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => self.feedback(call_id, SEALED_WITHHELD)?,
            ToolOutcome::Failure => self.feedback(call_id, SEALED_FAILED)?,
            ToolOutcome::Indeterminate => self.feedback(call_id, SEALED_INDETERMINATE)?,
        }
        Ok(CallGo)
    }

    fn admit_result(
        &self,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<Admission, DriveError> {
        let value_carrying = matches!(
            admission,
            ResultAdmission::SuccessRaw { .. }
                | ResultAdmission::SuccessSanitized { .. }
                | ResultAdmission::SuccessCast { .. }
                | ResultAdmission::SuccessCastAccepted { .. }
        );
        let mut admission = Some(admission);
        let mut result = Admission::AlreadyClosed;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            if value_carrying && self.cancel.is_cancelled() {
                result = Admission::CancelSuppressed;
                return None;
            }
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let admission = admission.take()?;
            match self.rt.engine().admit_result(&views, dispatch, call, admission) {
                Ok(batch) => {
                    result = Admission::Admitted;
                    Some(batch)
                }
                Err(AdmitError::NotOpen) => None,
                Err(AdmitError::UnknownTool(_) | AdmitError::DigestMismatch | AdmitError::ForeignDispatch) => {
                    result = Admission::InvariantBreach;
                    None
                }
                // Value-policy refusals, exhaustively — a future identity-class error must be
                // classified here deliberately, not absorbed by a wildcard.
                Err(
                    error @ (AdmitError::UnknownSanitizer(_)
                    | AdmitError::SanitizerNotOutput(_)
                    | AdmitError::TransitionSourceUnmet
                    | AdmitError::OutputPendingCast
                    | AdmitError::OutputSanitizerBound
                    | AdmitError::NotBoundSanitizer
                    | AdmitError::NotPendingCast
                    | AdmitError::UnknownCast(_)
                    | AdmitError::ConstantMismatch
                    | AdmitError::CeilingExceeded
                    | AdmitError::NarrowingUnaccepted
                    | AdmitError::AcceptanceMismatch),
                ) => {
                    result = Admission::Refused(error);
                    None
                }
            }
        })?;
        if matches!(result, Admission::InvariantBreach) {
            return Err(DriveError::DispatchIdentity);
        }
        Ok(result)
    }

    fn feedback(&self, call_id: &ToolCallId, content: &str) -> Result<(), DriveError> {
        self.append(vec![Fact::BlockFeedback {
            trajectory: self.session.clone(),
            call_id: call_id.clone(),
            content: content.to_string(),
        }])
    }

    fn finish_turn_end(&self) -> Result<(), DriveError> {
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let mut terminal = self.drain_facts(&views);
            terminal.push(turn_end(self.session));
            Some(FactBatch::new(rev, terminal))
        })?;
        Ok(())
    }

    fn finish_policy_stop(&self, message: &str) -> Result<TurnOutcome, DriveError> {
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let mut terminal = self.drain_facts(&views);
            terminal.push(Fact::AssistantMessage {
                trajectory: self.session.clone(),
                content: Some(message.to_string()),
                calls: Vec::new(),
            });
            terminal.push(turn_end(self.session));
            Some(FactBatch::new(rev, terminal))
        })?;
        Ok(TurnOutcome::PolicyStop(message.to_string()))
    }

    fn append(&self, facts: Vec<Fact>) -> Result<(), DriveError> {
        self.rt
            .store()
            .finalize(self.tenant, self.session, |_, rev| Some(FactBatch::new(rev, facts)))?;
        Ok(())
    }

    fn past_deadline(&self) -> bool {
        Instant::now() >= self.deadline
    }

    async fn wait<F: Future>(&self, fut: F) -> Result<Option<F::Output>, TurnCancelled> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(TurnCancelled),
            out = tokio::time::timeout(self.external_budget(), fut) => Ok(out.ok()),
        }
    }

    fn finish_cancelled(&self, open: Option<OpenClose>, unanswered: &[ToolCallId]) -> Result<TurnOutcome, DriveError> {
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let mut terminal = Vec::new();
            if let Some(open) = &open {
                let admission = match open.close {
                    CancelClose::Unobserved => ResultAdmission::Indeterminate,
                    CancelClose::EffectsStand => ResultAdmission::SuccessNoValue,
                    CancelClose::Failed => ResultAdmission::Failure,
                };
                if let Ok(batch) = self
                    .rt
                    .engine()
                    .admit_result(&views, &open.dispatch, &open.call, admission)
                {
                    terminal = batch.facts;
                }
            }
            terminal.extend(self.drain_facts(&views));
            for call_id in unanswered {
                terminal.push(Fact::BlockFeedback {
                    trajectory: self.session.clone(),
                    call_id: call_id.clone(),
                    content: POLICY_STOP_CANCELLED.to_string(),
                });
            }
            terminal.push(Fact::AssistantMessage {
                trajectory: self.session.clone(),
                content: Some(POLICY_STOP_CANCELLED.to_string()),
                calls: Vec::new(),
            });
            terminal.push(turn_end(self.session));
            Some(FactBatch::new(rev, terminal))
        })?;
        Ok(TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string()))
    }

    fn external_budget(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(self.rt.budgets().per_external_timeout)
    }
}

enum CallProgress {
    Go,
    Stop,
    Cancelled(Option<OpenClose>),
}
use CallProgress::{Cancelled as CallCancelled, Go as CallGo, Stop as CallStop};

struct OpenClose {
    dispatch: DispatchId,
    call: ResolvedCall,
    close: CancelClose,
}

enum CancelClose {
    Unobserved,
    EffectsStand,
    Failed,
}

fn turn_end(session: &TrajectoryId) -> Fact {
    Fact::Boundary {
        trajectory: session.clone(),
        kind: BoundaryKind::TurnEnd,
    }
}

fn proposal_of(call: &WireToolCall) -> Proposal {
    let trimmed = call.function.arguments.trim();
    let (arguments, malformed) = if trimmed.is_empty() {
        (serde_json::json!({}), false)
    } else {
        match serde_json::from_str(trimmed) {
            Ok(value) => (value, false),
            Err(_) => (serde_json::json!({}), true),
        }
    };
    Proposal {
        call: ProposedCall {
            id: ToolCallId::new(call.id.clone()),
            tool: ToolName::new(call.function.name.clone()),
            arguments,
        },
        malformed,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::inference::Inference;
    use crate::runtime::Budgets;
    use crate::tool::{BuiltinTool, HttpClient};
    use crate::wire::{ChatCompletionResponse, WireFunctionCall, WireMessage, WireToolCall};
    use appa_engine::fact::{CloseOutcome, ReturnDerivation};
    use appa_engine::value::LabeledValue;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_scripted_model(responses: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let mut received = Vec::new();
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    received.extend_from_slice(&buf[..n]);
                    if let Some(pos) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&received[..pos]).to_lowercase();
                        let len: usize = header
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if received.len() >= pos + 4 + len {
                            break;
                        }
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn tool_call_round(id: &str, name: &str, args: &str) -> String {
        serde_json::to_string(&ChatCompletionResponse::single(
            id,
            WireMessage::assistant_tool_calls(vec![WireToolCall {
                id: format!("call_{id}"),
                kind: "function".to_string(),
                function: WireFunctionCall {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            }]),
            "tool_calls",
        ))
        .unwrap()
    }

    fn final_round(id: &str, text: &str) -> String {
        serde_json::to_string(&ChatCompletionResponse::single(
            id,
            WireMessage::assistant(text),
            "stop",
        ))
        .unwrap()
    }

    fn user_turn(text: &str) -> UserTurn {
        crate::admission::admit_north_request(&ChatCompletionRequest {
            model: String::new(),
            messages: vec![WireMessage::user(text)],
            tools: None,
            stream: None,
        })
        .unwrap()
    }

    async fn runtime_over(config: Config, builtins: BTreeMap<ToolName, BuiltinTool>, base: String) -> Runtime {
        let inference = Inference::new(base, "k", "m", Duration::from_secs(5), HttpClient::new());
        Runtime::new(config, inference, builtins).unwrap()
    }

    #[tokio::test]
    async fn allow_path_dispatches_and_admits_the_result() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "get_logs"
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(
            ToolName::new("get_logs"),
            BuiltinTool::Echo("CrashLoopBackOff".to_string()),
        );
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "get_logs", "{}"),
            final_round("2", "the pod is crashlooping"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("what is wrong?"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("the pod is crashlooping".to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let admitted = log.iter().any(|f| {
            matches!(f, Fact::ValueAdmitted { value, provenance: Provenance::ToolResult { .. }, .. }
                if value.body.as_str() == "CrashLoopBackOff")
        });
        assert!(admitted, "tool result should be admitted");
        model.await.unwrap();
    }

    #[tokio::test]
    async fn block_then_remedy_authorizes_and_dispatches() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire"
effects = ["finance.spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = { attends = ["signoff"] }
implementation = { builtin = "approve" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire"), BuiltinTool::Echo("transferred".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", r#"{"amount":100}"#),
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
            final_round("3", "the transfer is done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("wire the invoice"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("the transfer is done".to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(
            log.iter().any(|f| matches!(
                f,
                Fact::BlockFeedback { content, .. } if content.contains("officer")
            )),
            "the block feedback should name the eligible authority"
        );
        assert!(
            log.iter().any(|f| matches!(f, Fact::Ruling { .. })),
            "a ruling should land"
        );
        let committed = log.iter().any(|f| {
            matches!(f, Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects.iter().any(|e| e.as_str() == "finance.spend"))
        });
        assert!(committed, "the authorized dispatch should commit its effect");
        model.await.unwrap();
    }

    #[tokio::test]
    async fn an_http_authority_approval_lands_a_ruling_with_its_review() {
        let (auth_base, auth_server) = spawn_scripted_model(vec![r#"{"ruling":"approve"}"#.to_string()]).await;
        let config = Config::from_toml_str(&format!(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire"
effects = ["finance.spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{auth_base}/rule" }} }}
"#,
        ))
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire"), BuiltinTool::Echo("transferred".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", r#"{"amount":100}"#),
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
            final_round("3", "done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("wire the invoice"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("done".to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let reviewed = log.iter().find_map(|f| match f {
            Fact::Ruling {
                authority, reviewed, ..
            } if authority.as_str() == "officer" => Some(reviewed.clone()),
            _ => None,
        });
        let review = reviewed.expect("the HTTP approval lands a ruling");
        assert_eq!(review.tool, ToolName::new("wire"));
        assert_eq!(
            review.trajectory_label,
            appa_engine::label::Label::new(
                appa_engine::label::Dim::Known(appa_engine::label::Trust::new(1)),
                appa_engine::label::Dim::Known(appa_engine::label::Audience::Public),
            )
        );
        assert!(review.arg_refs.is_empty());
        let committed = log.iter().any(|f| {
            matches!(f, Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects.iter().any(|e| e.as_str() == "finance.spend"))
        });
        assert!(committed, "the authorized dispatch should commit its effect");
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        auth_server.await.unwrap();
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_denied_authority_leaves_the_sibling_offer_live() {
        let (deny_base, deny_server) = spawn_scripted_model(vec![r#"{"ruling":"deny"}"#.to_string()]).await;
        let config = Config::from_toml_str(&format!(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire"
effects = ["finance.spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "no-officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ resolver = {{ url = "{deny_base}/rule" }} }}

[[authority]]
name = "yes-officer"
mandate = {{ attends = ["signoff"] }}
implementation = {{ builtin = "approve" }}
"#,
        ))
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire"), BuiltinTool::Echo("transferred".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", r#"{"amount":100}"#),
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
            tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
            final_round("4", "done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("wire the invoice"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("done".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let rulings: Vec<_> = log
            .iter()
            .filter_map(|f| match f {
                Fact::Ruling { authority, .. } => Some(authority.as_str().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(rulings, vec!["yes-officer".to_string()]);
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { .. },
                ..
            }
        )));
        deny_server.await.unwrap();
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_success_consumes_only_its_own_cohort() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire"
effects = ["finance.spend"]
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = { attends = ["signoff"] }
implementation = { builtin = "approve" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire"), BuiltinTool::Echo("transferred".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", r#"{"amount":100}"#),
            tool_call_round("2", "wire", r#"{"amount":100}"#),
            tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
            tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
            final_round("5", "both done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("wire twice"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("both done".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert_eq!(log.iter().filter(|f| matches!(f, Fact::Ruling { .. })).count(), 2);
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchOpened { .. })).count(),
            2
        );
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_third_blocked_proposal_of_one_call_mints_no_offers() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire"
[tool.requires]
attention = ["signoff"]

[[authority]]
name = "officer"
mandate = { attends = ["signoff"] }
implementation = { builtin = "approve" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire"), BuiltinTool::Echo("t".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", "{}"),
            tool_call_round("2", "wire", "{}"),
            tool_call_round("3", "wire", "{}"),
            final_round("4", "stopped"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("wire it"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let feedbacks: Vec<&str> = log
            .iter()
            .filter_map(|f| match f {
                Fact::BlockFeedback { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(feedbacks.len(), 3);
        let plans_offered = |feedback: &str| -> usize {
            feedback
                .split_once('\n')
                .and_then(|(_, json)| serde_json::from_str::<serde_json::Value>(json).ok())
                .and_then(|payload| payload["plans"].as_array().map(Vec::len))
                .unwrap_or(0)
        };
        assert_eq!(plans_offered(feedbacks[0]), 1);
        assert_eq!(plans_offered(feedbacks[1]), 1);
        assert_eq!(plans_offered(feedbacks[2]), 0);
        assert!(!feedbacks[2].contains('\n'));
        assert!(!log.iter().any(|f| matches!(f, Fact::DispatchOpened { .. })));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn an_oversized_result_commits_effects_but_admits_no_value() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "dump"
effects = ["read"]
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("dump"), BuiltinTool::Oversized(300 * 1024));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "dump", "{}"),
            final_round("2", "could not read it"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("dump the file"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects.iter().any(|e| e.as_str() == "read")
        )));
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_WITHHELD
        )));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_tool_commits_nothing_and_seals() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "flaky"
effects = ["read"]
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("flaky"), BuiltinTool::Fail);
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "flaky", "{}"), final_round("2", "it failed")]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("try it"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed {
                outcome: appa_engine::fact::CloseOutcome::Success { .. },
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_FAILED
        )));
        model.await.unwrap();
    }

    fn two_call_round(id: &str, calls: &[(&str, &str, &str)]) -> String {
        serde_json::to_string(&ChatCompletionResponse::single(
            id,
            WireMessage::assistant_tool_calls(
                calls
                    .iter()
                    .map(|(cid, name, args)| WireToolCall {
                        id: cid.to_string(),
                        kind: "function".to_string(),
                        function: WireFunctionCall {
                            name: name.to_string(),
                            arguments: args.to_string(),
                        },
                    })
                    .collect(),
            ),
            "tool_calls",
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn a_child_submit_result_merges_into_the_parent() {
        let config = Config::from_toml_str("version = 1\ntrust_chain = [\"suspicious\", \"trusted\"]\n").unwrap();
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "submit_result", r#"{"value":"child findings"}"#),
            final_round("2", "done"),
        ])
        .await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let parent = rt.store().create_session(tenant.clone());
        admit_at(&rt, &tenant, &parent, 1, &[]);
        let (child, _) = rt
            .store()
            .fork(&tenant, &parent, |child, facts, rev| {
                let projection = Projection::build(facts, rev);
                rt.engine()
                    .seed_child(&projection.view(&parent), child, rt.config().child_return_policy())
            })
            .unwrap();

        drive_turn(
            &rt,
            &tenant,
            &child,
            true,
            user_turn("investigate"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let (log, _) = rt.store().snapshot(&tenant, &parent).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted { trajectory, provenance: Provenance::ChildReturn { .. }, .. } if trajectory == &parent
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        )));
        let transcript = crate::transcript::model_transcript(&[], &log, &parent);
        assert!(
            transcript
                .iter()
                .any(|m| m.content.as_deref() == Some("child findings")),
            "the merged child value should appear in the parent transcript"
        );
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_void_or_malformed_submit_result_crosses_nothing() {
        for args in [
            r#"{"value":null}"#,
            r#"{}"#,
            r#"{"value":42}"#,
            r#"{"value":true}"#,
            r#"{"value":{"k":"v"}}"#,
            r#"{"value":["x"]}"#,
        ] {
            let config = Config::from_toml_str("version = 1\ntrust_chain = [\"suspicious\", \"trusted\"]\n").unwrap();
            let (base, model) = spawn_scripted_model(vec![
                tool_call_round("1", "submit_result", args),
                final_round("2", "done"),
            ])
            .await;
            let rt = runtime_over(config, BTreeMap::new(), base).await;
            let tenant = TenantId::new("acme");
            let parent = rt.store().create_session(tenant.clone());
            let (child, _) = rt
                .store()
                .fork(&tenant, &parent, |child, facts, rev| {
                    let projection = Projection::build(facts, rev);
                    rt.engine()
                        .seed_child(&projection.view(&parent), child, rt.config().child_return_policy())
                })
                .unwrap();

            drive_turn(
                &rt,
                &tenant,
                &child,
                true,
                user_turn("investigate"),
                CancellationToken::new(),
            )
            .await
            .unwrap();

            let (log, _) = rt.store().snapshot(&tenant, &parent).unwrap();
            assert!(
                !log.iter().any(|f| matches!(f, Fact::ChildReturn { .. })),
                "no child return may be recorded for {args}"
            );
            assert!(
                !log.iter().any(|f| matches!(
                    f,
                    Fact::ValueAdmitted {
                        provenance: Provenance::ChildReturn { .. },
                        ..
                    }
                )),
                "nothing may be admitted to the parent for {args}"
            );
            assert!(
                !log.iter().any(|f| matches!(
                    f,
                    Fact::Boundary {
                        kind: BoundaryKind::Merge { .. },
                        ..
                    }
                )),
                "no merge boundary may land for {args}"
            );
            assert!(log.iter().any(|f| matches!(f, Fact::BlockFeedback { .. })));
            assert!(crate::transcript::model_transcript(&[], &log, &parent).is_empty());
            model.await.unwrap();
        }
    }

    fn blocked_return_config(sanitizer_impl: &str) -> Config {
        Config::from_toml_str(&format!(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[sanitizer]]
name = "pii"
on   = ["tool_output"]
[sanitizer.can_reduce]
audience = {{ from = {{ includes = ["internal"] }}, to = {{ exactly = ["public"] }} }}
[sanitizer.implementation]
{sanitizer_impl}
"#
        ))
        .unwrap()
    }

    fn admit_at(rt: &Runtime, tenant: &TenantId, session: &TrajectoryId, trust: u8, readers: &[&str]) {
        use appa_engine::label::{Audience, Dim, Label, ReaderId, Trust};
        let audience = if readers.is_empty() {
            Audience::Public
        } else {
            Audience::restricted(readers.iter().map(|r| ReaderId::new(*r)))
        };
        let (_, rev) = rt.store().snapshot(tenant, session).unwrap();
        let batch = FactBatch::new(
            rev,
            vec![Fact::ValueAdmitted {
                trajectory: session.clone(),
                value: LabeledValue::new(
                    ValueBody::new("ingested"),
                    Label::new(Dim::Known(Trust::new(trust)), Dim::Known(audience)),
                ),
                provenance: Provenance::UserInput,
            }],
        );
        rt.store().conditional_append(tenant, session, batch).unwrap();
    }

    async fn drive_child_rounds(config: Config, taint_child: bool, rounds: Vec<String>) -> (Vec<Fact>, TrajectoryId) {
        drive_child_rounds_from(config, 1, taint_child, rounds).await
    }

    async fn drive_child_rounds_from(
        config: Config,
        parent_trust: u8,
        taint_child: bool,
        rounds: Vec<String>,
    ) -> (Vec<Fact>, TrajectoryId) {
        let (base, model) = spawn_scripted_model(rounds).await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let parent = rt.store().create_session(tenant.clone());
        admit_at(&rt, &tenant, &parent, parent_trust, &[]);
        let (child, _) = rt
            .store()
            .fork(&tenant, &parent, |child, facts, rev| {
                let projection = Projection::build(facts, rev);
                rt.engine()
                    .seed_child(&projection.view(&parent), child, rt.config().child_return_policy())
            })
            .unwrap();
        if taint_child {
            admit_at(&rt, &tenant, &child, 0, &["internal"]);
        }
        drive_turn(
            &rt,
            &tenant,
            &child,
            true,
            user_turn("investigate"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &parent).unwrap();
        model.await.unwrap();
        (log, parent)
    }

    fn merged_child_values(log: &[Fact], parent: &TrajectoryId) -> Vec<LabeledValue> {
        log.iter()
            .filter_map(|f| match f {
                Fact::ValueAdmitted {
                    trajectory,
                    value,
                    provenance: Provenance::ChildReturn { .. },
                } if trajectory == parent => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_narrowing_raw_return_blocks_and_accept_merges_raw() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
                tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
                final_round("3", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1);
        assert!(
            merged[0].body.as_str().contains("eve@corp.com"),
            "Accept crosses the raw value"
        );
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Raw,
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturnAcceptance { trajectory, .. } if trajectory == &parent
        )));
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::restricted([
                appa_engine::label::ReaderId::new("internal")
            ]))
        );
    }

    #[tokio::test]
    async fn a_blocked_return_sanitize_offer_merges_the_derivation() {
        let raw = "report: ask eve@corp.com";
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                tool_call_round("1", "submit_result", &format!(r#"{{"value":"{raw}"}}"#)),
                tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                final_round("3", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].body.as_str().contains("eve@corp.com"));
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::Public),
            "the sanitized crossing keeps the parent public"
        );
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { sanitizer, raw_digest, .. },
                ..
            } if sanitizer.as_str() == "pii" && raw_digest == &RawResultDigest::of(raw.as_bytes())
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturnAcceptance { narrowing, .. }
                if narrowing.to.audience == appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        )));
        let transcript = crate::transcript::model_transcript(&[], &log, &parent);
        assert!(
            !transcript
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("eve@corp.com")))
        );
    }

    #[tokio::test]
    async fn a_blocked_return_left_unremedied_merges_nothing() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
                final_round("2", "giving up"),
            ],
        )
        .await;
        assert!(merged_child_values(&log, &parent).is_empty());
        assert!(!log.iter().any(|f| matches!(f, Fact::ChildReturn { .. })));
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::Boundary {
                kind: BoundaryKind::Merge { .. },
                ..
            }
        )));
    }

    #[tokio::test]
    async fn a_pending_return_attempt_is_discarded_after_a_sibling_merge() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"first"}"#),
                tool_call_round("2", "submit_result", r#"{"value":"second"}"#),
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-2"}"#),
                tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
                final_round("5", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1, "exactly one crossing");
        assert_eq!(merged[0].body.as_str(), "second");
        assert_eq!(
            log.iter()
                .filter(|f| matches!(
                    f,
                    Fact::Boundary {
                        kind: BoundaryKind::Merge { .. },
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_crossing_consumes_the_childs_return_channel() {
        let (log, parent) = drive_child_rounds_from(
            blocked_return_config("builtin = \"redact-email\""),
            0,
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"ask eve@corp.com"}"#),
                tool_call_round("2", "submit_result", r#"{"value":"ask bob@corp.com"}"#),
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-3"}"#),
                final_round("5", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1, "exactly one crossing");
        assert!(
            !merged[0].body.as_str().contains("corp.com"),
            "only the derivation crossed"
        );
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        );
        assert!(!log.iter().any(|f| matches!(f, Fact::ChildReturnAcceptance { .. })));
    }

    #[tokio::test]
    async fn a_failed_derivation_restores_the_offer_and_accept_is_never_charged() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("resolver = { url = \"http://127.0.0.1:1/derive\", timeout_ms = 200 }"),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
                tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("5", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
                final_round("6", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1);
        assert!(
            merged[0].body.as_str().contains("eve@corp.com"),
            "Accept crossed the raw value"
        );
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { .. },
                ..
            }
        )));
    }

    #[tokio::test]
    async fn a_non_narrowing_raw_return_merges_without_a_block() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            false,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"nothing read, nothing tainted"}"#),
                final_round("2", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 1);
        assert!(!log.iter().any(|f| matches!(f, Fact::ChildReturnAcceptance { .. })));
    }

    #[tokio::test]
    async fn distinct_remedy_handles_target_the_right_blocked_call() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "wire_a"
effects = ["spend.a"]
[tool.requires]
attention = ["sa"]

[[tool]]
name = "wire_b"
effects = ["spend.b"]
[tool.requires]
attention = ["sb"]

[[authority]]
name = "officer"
mandate = { attends = ["sa", "sb"] }
implementation = { builtin = "approve" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("wire_a"), BuiltinTool::Echo("a".to_string()));
        builtins.insert(ToolName::new("wire_b"), BuiltinTool::Echo("b".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            two_call_round("1", &[("h_a", "wire_a", "{}"), ("h_b", "wire_b", "{}")]),
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
            final_round("3", "b is done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("do both"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let committed = |effect: &str| {
            log.iter().any(|f| {
                matches!(f, Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                    if effects.iter().any(|e| e.as_str() == effect))
            })
        };
        assert!(committed("spend.b"), "the remedied call (b) should dispatch");
        assert!(!committed("spend.a"), "the un-remedied call (a) must not dispatch");
        model.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_arguments_are_sealed_not_executed() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "get_logs"
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("get_logs"), BuiltinTool::Echo("ok".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "get_logs", "{not valid"),
            final_round("2", "ok"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("read"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(!log.iter().any(|f| matches!(f, Fact::DispatchOpened { .. })));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::BlockFeedback { .. })).count(),
            1
        );
        model.await.unwrap();
    }

    fn pending_cast_config(extra: &str) -> Config {
        Config::from_toml_str(&format!(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
effects = ["read"]
delta = {{ trust = "unknown" }}

[[cast]]
name = "paranoid"
constant = {{ trust = "suspicious" }}
{extra}"#,
        ))
        .unwrap()
    }

    fn scan_builtins() -> BTreeMap<ToolName, BuiltinTool> {
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("scan"), BuiltinTool::Echo("mail body".to_string()));
        builtins
    }

    fn admitted_scan_body(log: &[Fact]) -> bool {
        log.iter().any(|f| {
            matches!(f, Fact::ValueAdmitted { value, provenance: Provenance::ToolResult { .. }, .. }
                if value.body.as_str() == "mail body"
                    && value.label.trust == appa_engine::label::Dim::Known(appa_engine::label::Trust::new(0)))
        })
    }

    #[tokio::test]
    async fn a_narrowing_pending_cast_offers_acceptance_and_admits_on_it() {
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "scan", "{}"),
            tool_call_round("2", EXECUTE_REMEDY_PLAN, r#"{"plan_id": "remedy-0"}"#),
            final_round("3", "scanned"),
        ])
        .await;
        let rt = runtime_over(pending_cast_config(""), scan_builtins(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("scanned".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::OutputCastApplied {
                dimension: Dimension::Trust,
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(f, Fact::OutputCastAccepted { .. })));
        assert!(admitted_scan_body(&log));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchClosed { .. })).count(),
            1
        );
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "call_1"
        )));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::BlockFeedback { .. })).count(),
            1
        );
        let transcript = model_transcript(&[], &log, &session);
        let scan_response = transcript
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call_1"))
            .expect("the scan call has a response");
        assert!(!scan_response.content.as_deref().unwrap_or("").contains("mail body"));
        let accept_response = transcript
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call_2"))
            .expect("the accept call has a response");
        assert_eq!(accept_response.content.as_deref(), Some("mail body"));
        assert_eq!(
            transcript
                .iter()
                .filter(|m| m.content.as_deref().is_some_and(|c| c.contains("mail body")))
                .count(),
            1
        );
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_same_round_acceptance_predating_the_offer_is_refused() {
        let (base, model) = spawn_scripted_model(vec![
            two_call_round(
                "1",
                &[
                    ("c1", "scan", "{}"),
                    ("c2", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#),
                ],
            ),
            tool_call_round("2", EXECUTE_REMEDY_PLAN, r#"{"plan_id":"remedy-0"}"#),
            final_round("3", "done"),
        ])
        .await;
        let rt = runtime_over(pending_cast_config(""), scan_builtins(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("done".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert_eq!(
            log.iter()
                .filter(|f| matches!(f, Fact::OutputCastAccepted { .. }))
                .count(),
            1
        );
        assert!(admitted_scan_body(&log));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { call_id, .. } if call_id.as_str() == "c2"
        )));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_non_narrowing_pending_cast_admits_directly() {
        let config = pending_cast_config("\n[boundary]\ntrust = \"suspicious\"\n");
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "scan", "{}"), final_round("2", "scanned")]).await;
        let rt = runtime_over(config, scan_builtins(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(f, Fact::OutputCastApplied { .. })));
        assert!(!log.iter().any(|f| matches!(f, Fact::OutputCastAccepted { .. })));
        assert!(admitted_scan_body(&log));
        assert!(!log.iter().any(|f| matches!(f, Fact::BlockFeedback { .. })));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn an_unaccepted_pending_cast_lapses_at_turn_end() {
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "scan", "{}"), final_round("2", "done")]).await;
        let rt = runtime_over(pending_cast_config(""), scan_builtins(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let lapse = log
            .iter()
            .position(|f| matches!(f, Fact::OutputCastLapsed { .. }))
            .expect("the unaccepted offer lapses");
        let close = log
            .iter()
            .position(|f| {
                matches!(
                    f,
                    Fact::DispatchClosed { outcome: CloseOutcome::Success { effects }, .. }
                        if effects == &[appa_engine::fact::EffectKind::new("read")]
                )
            })
            .expect("the dispatch closes successfully with its effects");
        let turn_end = log
            .iter()
            .position(|f| {
                matches!(
                    f,
                    Fact::Boundary {
                        kind: BoundaryKind::TurnEnd,
                        ..
                    }
                )
            })
            .expect("the turn ends");
        assert!(close < turn_end && lapse < turn_end);
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchClosed { .. })).count(),
            1
        );
        assert!(!admitted_scan_body(&log));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn pending_casts_lapse_when_the_turn_policy_stops() {
        let (base, model) = spawn_scripted_model(vec![tool_call_round("1", "scan", "{}")]).await;
        let inference = Inference::new(base, "k", "m", Duration::from_secs(5), HttpClient::new());
        let rt = Runtime::with_options(
            pending_cast_config(""),
            inference,
            scan_builtins(),
            Vec::new(),
            Budgets {
                max_inference_rounds: 1,
                ..Budgets::default()
            },
        )
        .unwrap();
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::PolicyStop(POLICY_STOP_BUDGET.to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(f, Fact::OutputCastLapsed { .. })));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed {
                outcome: CloseOutcome::Success { .. },
                ..
            }
        )));
        assert!(!admitted_scan_body(&log));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn two_pending_casts_lapse_independently() {
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "scan", "{}"),
            tool_call_round("2", "scan", r#"{"again": true}"#),
            final_round("3", "done"),
        ])
        .await;
        let rt = runtime_over(pending_cast_config(""), scan_builtins(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan twice"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert_eq!(
            log.iter()
                .filter(|f| matches!(f, Fact::OutputCastLapsed { .. }))
                .count(),
            2
        );
        assert_eq!(
            log.iter()
                .filter(|f| matches!(
                    f,
                    Fact::DispatchClosed {
                        outcome: CloseOutcome::Success { .. },
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(!admitted_scan_body(&log));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_moved_offer_is_rederived_and_accepted_live() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
delta = { trust = "unknown" }

[[tool]]
name = "read_note"
delta = { audience = { exactly = ["internal"] } }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        )
        .unwrap();
        let mut builtins = scan_builtins();
        builtins.insert(ToolName::new("read_note"), BuiltinTool::Echo("note".to_string()));
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "scan", "{}"),
            tool_call_round("2", "read_note", "{}"),
            tool_call_round("3", EXECUTE_REMEDY_PLAN, r#"{"plan_id": "remedy-1"}"#),
            tool_call_round("4", EXECUTE_REMEDY_PLAN, r#"{"plan_id": "remedy-0"}"#),
            tool_call_round("5", EXECUTE_REMEDY_PLAN, r#"{"plan_id": "remedy-0"}"#),
            final_round("6", "done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan then read"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("done".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let accepted = log.iter().find_map(|f| match f {
            Fact::OutputCastAccepted { narrowing, .. } => Some(narrowing.clone()),
            _ => None,
        });
        let narrowing = accepted.expect("the second acceptance lands");
        assert_eq!(
            narrowing.from.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::restricted([
                appa_engine::label::ReaderId::new("internal")
            ]))
        );
        assert!(admitted_scan_body(&log));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn pending_cast_without_a_matching_cast_seals_but_commits_effects() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
effects = ["read"]
delta = { trust = "unknown" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("scan"), BuiltinTool::Echo("secret mail".to_string()));
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "scan", "{}"), final_round("2", "done")]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan the inbox"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects == &[appa_engine::fact::EffectKind::new("read")]
        )));
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_UNRESOLVED
        )));
        model.await.unwrap();
    }

    const PII: &str = r#"
[[sanitizer]]
name = "pii"
on   = ["tool_output"]
[sanitizer.can_reduce]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#;

    #[tokio::test]
    async fn a_bound_tool_admits_the_derivation_never_the_raw() {
        let config = Config::from_toml_str(&format!(
            "version = 1\n[[tool]]\nname = \"export\"\ndelta = {{ audience = {{ exactly = [\"internal\"] }} }}\noutput_sanitizer = \"pii\"\n{PII}"
        ))
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(
            ToolName::new("export"),
            BuiltinTool::Echo("contact bob@corp.com for access".to_string()),
        );
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "export", "{}"), final_round("2", "exported")]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("export the ticket"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let admitted: Vec<_> = log
            .iter()
            .filter_map(|f| match f {
                Fact::ValueAdmitted {
                    value,
                    provenance: Provenance::ToolResult { .. },
                    ..
                } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(admitted.len(), 1);
        assert!(!admitted[0].body.as_str().contains("bob@corp.com"));
        assert_eq!(
            admitted[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        );
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::SanitizerApplied { raw_digest, .. }
                if raw_digest == &appa_engine::value::RawResultDigest::of(b"contact bob@corp.com for access")
        )));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_sanitizer_derivation_seals_but_commits_effects() {
        let config = Config::from_toml_str(
            r#"
version = 1

[[tool]]
name = "export"
effects = ["read"]
delta = { audience = { exactly = ["internal"] } }
output_sanitizer = "pii"

[[sanitizer]]
name = "pii"
on   = ["tool_output"]
[sanitizer.can_reduce]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
resolver = { url = "http://127.0.0.1:1/derive", timeout_ms = 200 }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("export"), BuiltinTool::Echo("secret ticket".to_string()));
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "export", "{}"), final_round("2", "done")]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("export"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects == &[appa_engine::fact::EffectKind::new("read")]
        )));
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_UNSANITIZED
        )));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_child_return_passes_the_configured_sanitizer() {
        let config =
            Config::from_toml_str(&format!("version = 1\n[child]\nreturn_sanitizer = \"pii\"\n{PII}")).unwrap();
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
            final_round("2", "submitted"),
        ])
        .await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let parent = rt.store().create_session(tenant.clone());
        let (child, _) = rt
            .store()
            .fork(&tenant, &parent, |child, facts, revision| {
                let projection = Projection::build(facts, revision);
                rt.engine()
                    .seed_child(&projection.view(&parent), child, rt.config().child_return_policy())
            })
            .unwrap();

        drive_turn(
            &rt,
            &tenant,
            &child,
            true,
            user_turn("investigate"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &parent).unwrap();
        let merged: Vec<_> = log
            .iter()
            .filter_map(|f| match f {
                Fact::ValueAdmitted {
                    trajectory,
                    value,
                    provenance: Provenance::ChildReturn { .. },
                } if trajectory == &parent => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].body.as_str().contains("eve@corp.com"));
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        );
        let audited: Vec<_> = log
            .iter()
            .filter_map(|f| match f {
                Fact::ChildReturn { derivation, .. } => Some(derivation),
                _ => None,
            })
            .collect();
        assert_eq!(
            audited,
            vec![&ReturnDerivation::Sanitized {
                sanitizer: appa_engine::names::SanitizerName::new("pii"),
                raw_digest: RawResultDigest::of(b"report: ask eve@corp.com"),
                from: appa_engine::label::Audience::restricted([appa_engine::label::ReaderId::new("internal")]),
                to: appa_engine::label::Audience::Public,
            }]
        );
        model.await.unwrap();
    }

    async fn spawn_hanging_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (socket, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => return,
                };
                tokio::spawn(async move {
                    let _hold = socket;
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn cancellation_during_a_south_invoke_closes_indeterminate_and_ends_the_turn() {
        let south = spawn_hanging_server().await;
        let config = Config::from_toml_str(&format!(
            "version = 1\n[[tool]]\nname = \"slow\"\n[tool.implementation.http]\nurl = \"{south}/run\"\n"
        ))
        .unwrap();
        let (base, _model) = spawn_scripted_model(vec![
            tool_call_round("1", "slow", "{}"),
            final_round("2", "next turn works"),
        ])
        .await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.cancel();
        });
        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("run it"), token)
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        let tail: Vec<&Fact> = log.iter().rev().take(4).collect();
        assert!(matches!(
            tail[3],
            Fact::DispatchClosed {
                outcome: appa_engine::fact::CloseOutcome::Indeterminate,
                ..
            }
        ));
        assert!(matches!(
            tail[2],
            Fact::BlockFeedback { content, .. } if content == POLICY_STOP_CANCELLED
        ));
        assert!(matches!(tail[1], Fact::AssistantMessage { calls, .. } if calls.is_empty()));
        assert!(matches!(
            tail[0],
            Fact::Boundary {
                kind: BoundaryKind::TurnEnd,
                ..
            }
        ));

        let outcome = drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("again"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("next turn works".to_string()));
    }

    #[tokio::test]
    async fn a_hostile_resolver_answer_is_discarded_and_the_dispatch_still_closes() {
        let (resolver, _r) = spawn_scripted_model(vec![r#"{"trust":"trusted"}"#.to_string()]).await;
        let config = Config::from_toml_str(&format!(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
effects = ["read"]
delta = {{ trust = "unknown" }}

[[cast]]
name     = "classifier"
resolver = {{ url = "{resolver}/resolve", may_cast = {{ trust = ["suspicious"] }} }}
"#
        ))
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("scan"), BuiltinTool::Echo("mailbox".to_string()));
        let (base, _model) =
            spawn_scripted_model(vec![tool_call_round("1", "scan", "{}"), final_round("2", "done")]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(
            &rt,
            &tenant,
            &session,
            false,
            user_turn("scan"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects == &[appa_engine::fact::EffectKind::new("read")]
        )));
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_UNRESOLVED
        )));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchOpened { .. })).count(),
            log.iter().filter(|f| matches!(f, Fact::DispatchClosed { .. })).count(),
        );
    }

    #[tokio::test]
    async fn cancellation_mid_round_seals_the_remaining_calls_without_dispatching_them() {
        let south = spawn_hanging_server().await;
        let config = Config::from_toml_str(&format!(
            "version = 1\n[[tool]]\nname = \"slow\"\n[tool.implementation.http]\nurl = \"{south}/run\"\n\n[[tool]]\nname = \"fast\"\n"
        ))
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("fast"), BuiltinTool::Echo("instant".to_string()));
        let round = serde_json::to_string(&ChatCompletionResponse::single(
            "1",
            WireMessage::assistant_tool_calls(vec![
                WireToolCall {
                    id: "call_a".to_string(),
                    kind: "function".to_string(),
                    function: WireFunctionCall {
                        name: "slow".to_string(),
                        arguments: "{}".to_string(),
                    },
                },
                WireToolCall {
                    id: "call_b".to_string(),
                    kind: "function".to_string(),
                    function: WireFunctionCall {
                        name: "fast".to_string(),
                        arguments: "{}".to_string(),
                    },
                },
            ]),
            "tool_calls",
        ))
        .unwrap();
        let (base, _model) = spawn_scripted_model(vec![round]).await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.cancel();
        });
        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("both"), token)
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchOpened { .. })).count(),
            1
        );
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        let sealed: Vec<&str> = log
            .iter()
            .filter_map(|f| match f {
                Fact::BlockFeedback { call_id, content, .. } if content == POLICY_STOP_CANCELLED => {
                    Some(call_id.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(sealed, vec!["call_a", "call_b"]);
    }

    #[tokio::test]
    async fn a_pre_cancelled_token_never_starts_the_turn() {
        let config = Config::from_toml_str("version = 1\n").unwrap();
        let (base, _model) = spawn_scripted_model(vec![]).await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let token = CancellationToken::new();
        token.cancel();
        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("hi"), token)
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_inference_ends_the_turn_with_no_dispatch() {
        let base = spawn_hanging_server().await;
        let config = Config::from_toml_str(
            "version = 1\n[[tool]]\nname = \"noop\"\n[tool.implementation.http]\nurl = \"http://127.0.0.1:1/x\"\n",
        )
        .unwrap();
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.cancel();
        });
        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("hi"), token)
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::PolicyStop(POLICY_STOP_CANCELLED.to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(!log.iter().any(|f| matches!(f, Fact::DispatchOpened { .. })));
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::Boundary {
                kind: BoundaryKind::TurnEnd,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn the_configured_preamble_pins_the_runtime() {
        let config =
            Config::from_toml_str("version = 1\n[[preamble]]\nrole = \"system\"\ncontent = \"you are confined\"\n")
                .unwrap();
        let (base, _model) = spawn_scripted_model(vec![]).await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        assert_eq!(rt.preamble(), &[WireMessage::system("you are confined")]);
    }

    #[tokio::test]
    async fn a_final_answer_with_no_tools_ends_the_turn() {
        let config = Config::from_toml_str("version = 1\ntrust_chain = [\"suspicious\", \"trusted\"]\n").unwrap();
        let (base, model) = spawn_scripted_model(vec![final_round("1", "hello, I need no tools")]).await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("hi"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("hello, I need no tools".to_string()));
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert_eq!(
            log.iter()
                .filter(|f| matches!(
                    f,
                    Fact::Boundary {
                        kind: BoundaryKind::TurnEnd,
                        ..
                    }
                ))
                .count(),
            1
        );
        model.await.unwrap();
    }
}
