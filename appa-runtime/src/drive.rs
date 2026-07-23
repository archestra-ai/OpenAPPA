//! RP2 turn-drive: the state machine that turns one admitted user turn into a final assistant answer,
//! mediating every proposed tool call through the pure engine.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use appa_engine::admit::{AdmitError, CastAnswer, ResultAdmission};
use appa_engine::authority::CastResolution;
use appa_engine::branch::ChildReturn;
use appa_engine::check::{CheckOutcome, UnresolvedFact};
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall};
use appa_engine::label::{DimValue, Dimension};
use appa_engine::names::{CastName, SanitizerName};
use appa_engine::plan::PlanId;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, ChildReturnId, DispatchId, Provenance, RawResultDigest, ResolvedCall, ToolCallId, ToolName,
    TrajectoryId, ValueBody, ValueId,
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
    Refused,
    InvariantBreach,
    CancelSuppressed,
}

struct PendingBlock {
    handle: String,
    call: ResolvedCall,
    plan: PlanId,
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
        remedy_attempts: BTreeMap::new(),
        next_handle: 0,
    };
    drive.run(user_turn).await
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
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    next_handle: u32,
}

struct TurnCancelled;

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
                    let gaps = raw.requirement_gaps.len();
                    let curative: Vec<String> = planned.recommendations.iter().filter_map(redispatch_hint).collect();
                    let feedback = match planned.plans.first() {
                        Some(plan) => {
                            let id = plan.id;
                            drop(projection);
                            // A server-minted, turn-unique handle — never the model's tool-call id.
                            let handle = format!("remedy-{}", self.next_handle);
                            self.next_handle += 1;
                            self.pending.push(PendingBlock {
                                handle: handle.clone(),
                                call,
                                plan: id,
                            });
                            format!(
                                "blocked by policy ({gaps} requirement gap(s)); call execute_remedy_plan with plan_id \"{handle}\" to authorize"
                            )
                        }
                        None if !curative.is_empty() => {
                            format!(
                                "blocked by policy; run {} first, then re-propose this call",
                                curative.join(" or ")
                            )
                        }
                        None => "blocked by policy; no remedy is available for this call".to_string(),
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
        let Some(index) = self.pending.iter().position(|p| p.handle == handle) else {
            self.feedback(call_id, "no pending blocked call offers that plan_id")?;
            return Ok(CallGo);
        };
        if self.invocations >= self.rt.budgets().max_tool_invocations || self.past_deadline() {
            self.feedback(call_id, POLICY_STOP_BUDGET)?;
            return Ok(CallStop);
        }
        let block = self.pending.remove(index);
        let plan = block.plan;
        let handle = block.handle;
        let call = block.call;

        // Bound remedy retries per call — a model cannot loop authorize-attempts unboundedly.
        let attempts = self.remedy_attempts.entry(call.digest()).or_insert(0);
        *attempts += 1;
        if *attempts > self.rt.budgets().max_remedy_attempts_per_gap {
            self.feedback(call_id, "the remedy attempt limit for this call was reached")?;
            return Ok(CallGo);
        }

        let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(self.session);
        let required = self
            .rt
            .engine()
            .required_rulings(&views, &call)
            .expect("pending call is registered");
        let dispatch = DispatchId::new(
            self.session.clone(),
            call.digest(),
            views.dispatch_count(&call.digest()),
        );

        let mut rulings = Vec::new();
        for req in &required {
            let Some(backend) = self.rt.authority_backend(&req.authority) else {
                self.feedback(call_id, "an authority for this plan is not configured")?;
                return Ok(CallGo);
            };
            let request = AuthorityRequest::new(req.authority.clone(), &call, req.covers.clone());
            let answer = match self.wait(backend.rule(&request)).await {
                Err(TurnCancelled) => return Ok(CallCancelled(None)),
                Ok(answer) => answer.unwrap_or(AuthorityAnswer::Abstain),
            };
            match answer {
                AuthorityAnswer::Approve => rulings.push(Ruling {
                    dispatch: dispatch.clone(),
                    authority: req.authority.clone(),
                    issuer: Issuer::Authority,
                    covers: req.covers.clone(),
                }),
                AuthorityAnswer::Deny | AuthorityAnswer::Abstain => {
                    self.feedback(call_id, "the authority declined to authorize this call")?;
                    return Ok(CallGo);
                }
            }
        }

        let batch = match self.rt.engine().execute_plan(&views, plan, &call, &rulings, Sink::Tool) {
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
                // A concurrent branch advanced the revision; the model may retry the remedy.
                self.pending.push(PendingBlock { handle, call, plan });
                self.feedback(call_id, "the state changed; re-propose the call and remedy")?;
                return Ok(CallGo);
            }
            Err(e) => return Err(DriveError::Store(e)),
        }
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
        let body = arguments
            .get("value")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();

        let returned = match self.rt.config().child_return_sanitizer() {
            None => ChildReturn::Raw {
                body: ValueBody::new(body.clone()),
            },
            Some(sanitizer) => match self.derive_sanitized(sanitizer, &body).await {
                // Cancelled before any return was recorded: nothing crossed, only the seal is owed.
                Err(TurnCancelled) => return Ok(CallCancelled(None)),
                Ok(Some(derived)) => ChildReturn::Sanitized {
                    body: ValueBody::new(derived),
                    sanitizer: sanitizer.clone(),
                },
                Ok(None) => {
                    self.feedback(call_id, "the result could not be sanitized for return")?;
                    return Ok(CallGo);
                }
            },
        };

        let mut recorded = None;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            if self.cancel.is_cancelled() {
                return None;
            }
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let occurrence = views.returns_by(self.session);
            let batch = self.rt.engine().submit_child_return(&views, returned).ok()?;
            recorded = Some(ChildReturnId::new(self.session.clone(), occurrence));
            Some(batch)
        })?;
        let Some(return_id) = recorded else {
            if self.cancel.is_cancelled() {
                return Ok(CallCancelled(None));
            }
            self.feedback(call_id, "this session cannot submit a result")?;
            return Ok(CallGo);
        };

        let mut merged = true;
        if let Some(parent) = self.rt.store().parent_of(self.tenant, self.session)? {
            merged = false;
            self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
                if self.cancel.is_cancelled() {
                    return None;
                }
                let projection = Projection::build(facts, rev);
                let batch = self.rt.engine().merge(&projection.view(&parent), &return_id).ok()?;
                merged = true;
                Some(batch)
            })?;
        }
        if !merged {
            return Ok(CallCancelled(None));
        }
        self.feedback(call_id, "result submitted to the parent")?;
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
        let pending_cast = contract.and_then(|c| c.delta.pending_cast_dim());
        let bound_sanitizer = contract.and_then(|c| c.output_sanitizer.clone());
        let mut withheld: Option<&str> = None;
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
                    Ok(Some((cast, resolved))) => ResultAdmission::SuccessCast {
                        body: ValueBody::new(body.clone()),
                        cast,
                        resolved,
                    },
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
            Admission::Refused => {
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
                    AdmitError::UnknownSanitizer(_)
                    | AdmitError::SanitizerNotOutput(_)
                    | AdmitError::TransitionSourceUnmet
                    | AdmitError::OutputPendingCast
                    | AdmitError::OutputSanitizerBound
                    | AdmitError::NotBoundSanitizer
                    | AdmitError::NotPendingCast
                    | AdmitError::UnknownCast(_)
                    | AdmitError::ConstantMismatch
                    | AdmitError::CeilingExceeded,
                ) => {
                    result = Admission::Refused;
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
        self.append(vec![turn_end(self.session)])
    }

    fn finish_policy_stop(&self, message: &str) -> Result<TurnOutcome, DriveError> {
        self.append(vec![
            Fact::AssistantMessage {
                trajectory: self.session.clone(),
                content: Some(message.to_string()),
                calls: Vec::new(),
            },
            turn_end(self.session),
        ])?;
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

fn redispatch_hint(recommendation: &appa_engine::plan::Recommendation) -> Option<String> {
    match recommendation {
        appa_engine::plan::Recommendation::Redispatch { tool, .. } => Some(tool.as_str().to_string()),
        appa_engine::plan::Recommendation::Fork { .. } => None,
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
    use crate::tool::{BuiltinTool, HttpClient};
    use crate::wire::{ChatCompletionResponse, WireFunctionCall, WireMessage, WireToolCall};
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
        let (child, _) = rt
            .store()
            .fork(&tenant, &parent, |child, facts, rev| {
                let projection = Projection::build(facts, rev);
                rt.engine().seed_child(&projection.view(&parent), child)
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

    #[tokio::test]
    async fn pending_cast_output_resolves_and_admits_at_the_cast_label() {
        let config = Config::from_toml_str(
            r#"
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "scan"
delta = { trust = "unknown" }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        )
        .unwrap();
        let mut builtins = BTreeMap::new();
        builtins.insert(ToolName::new("scan"), BuiltinTool::Echo("mail body".to_string()));
        let (base, model) =
            spawn_scripted_model(vec![tool_call_round("1", "scan", "{}"), final_round("2", "scanned")]).await;
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
            Fact::OutputCastApplied {
                dimension: Dimension::Trust,
                ..
            }
        )));
        let admitted = log.iter().any(|f| {
            matches!(f, Fact::ValueAdmitted { value, provenance: Provenance::ToolResult { .. }, .. }
                if value.body.as_str() == "mail body"
                    && value.label.trust == appa_engine::label::Dim::Known(appa_engine::label::Trust::new(0)))
        });
        assert!(
            admitted,
            "the cast-resolved value should be admitted at the resolved label"
        );
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
                rt.engine().seed_child(&projection.view(&parent), child)
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
