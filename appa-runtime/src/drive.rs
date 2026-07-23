//! RP2 turn-drive: the state machine that turns one admitted user turn into a final assistant answer,
//! mediating every proposed tool call through the pure engine.
//!
//! The loop is `Infer → record the assistant round → per proposed call, SERIALLY, re-projecting after
//! each committed batch: Check → Allow(open→invoke→admit) | Unresolved(cast→recheck) |
//! Block(offer a remedy, feed it back) → Infer | Final`. Two reserved tools are server-handled, never
//! dispatched south: `execute_remedy_plan` gathers the block's rulings and lands the atomic
//! authorize+dispatch, and `submit_result` returns a child value to its parent (RP6). Budgets bound the
//! turn (inference rounds, south invocations, wall-clock); any exhaustion ends it in a fixed,
//! replayable policy-stop terminal. Every proposed call is answered by exactly one terminal response
//! fact (an admitted `ValueAdmitted` for an available result, else a `BlockFeedback`), so the
//! transcript (RP1) pairs responses to calls positionally.
//!
//! The drive owns IO and the clock; the engine owns every algebraic decision. A tool result's raw
//! bytes are confined — the model sees the admitted value, a sealed token, or safe feedback, never a
//! backend's error body (RP3).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use appa_engine::admit::{CastAnswer, ResultAdmission};
use appa_engine::authority::CastResolution;
use appa_engine::branch::ChildReturn;
use appa_engine::check::{CheckOutcome, UnresolvedFact};
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall};
use appa_engine::label::DimValue;
use appa_engine::plan::PlanId;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, ChildReturnId, DispatchId, Provenance, ResolvedCall, ToolCallId, ToolName, TrajectoryId,
    ValueBody, ValueId,
};

use crate::admission::UserTurn;
use crate::external::{AuthorityAnswer, AuthorityRequest, CastAnswer as BackendCast, CastInput};
use crate::runtime::{EXECUTE_REMEDY_PLAN, Runtime, SUBMIT_RESULT};
use crate::store::{StoreError, TenantId};
use crate::tool::{BodyDisposition, RenderedCall, ToolOutcome};
use crate::transcript::model_transcript;
use crate::wire::{ChatCompletionRequest, WireToolCall};

/// The fixed, model-visible terminals the drive seals in place of a raw result (RP3).
const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
const SEALED_FAILED: &str = "[tool call failed]";
const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";
const POLICY_STOP_BUDGET: &str = "This turn reached its resource budget and was stopped.";
const POLICY_STOP_INFERENCE: &str = "This turn could not continue: upstream inference was unavailable.";

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The model yielded a final assistant answer (its free text).
    Final(String),
    /// A budget or upstream fault ended the turn in a fixed, replayable terminal.
    PolicyStop(String),
}

/// A genuine infrastructure failure the drive cannot resolve (an unrecoverable store fault). Policy
/// outcomes — blocks, denials, budget stops, inference faults — are *not* errors; they are turn facts.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

/// A blocked call awaiting the model's remedy decision. Held in-turn (CC2 — no cross-request state):
/// when the model calls `execute_remedy_plan(handle)`, the drive finds the pending block with that
/// **server-minted, turn-unique handle** and lands its atomic authorize+dispatch. The handle
/// disambiguates two blocks in one round, which the engine's per-block `PlanId` (always 0) cannot, and
/// is never the model's own tool-call id (untrusted, may collide).
struct PendingBlock {
    handle: String,
    call: ResolvedCall,
    plan: PlanId,
}

/// One model-proposed call, with a flag for arguments that failed to parse (never repaired into an
/// executable call — RP3).
struct Proposal {
    call: ProposedCall,
    malformed: bool,
}

/// Drive one user turn to completion. `is_child` gates the `submit_result` reserved tool (RP6).
///
/// Acquires the session's **turn lease** for the whole turn (the sanctioned per-session-mutex-across-
/// await pattern), so at most one turn runs per trajectory — the serialization the transcript's
/// positional pairing depends on.
pub async fn drive_turn(
    rt: &Runtime,
    tenant: &TenantId,
    session: &TrajectoryId,
    is_child: bool,
    user_turn: UserTurn,
) -> Result<TurnOutcome, DriveError> {
    let lease = rt.store().turn_lock(tenant, session)?;
    let _turn = lease.lock().await;
    let mut drive = Drive {
        rt,
        tenant,
        session,
        is_child,
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
    deadline: Instant,
    rounds: u32,
    invocations: u32,
    pending: Vec<PendingBlock>,
    /// Remedy executions attempted per call this turn — bounds `max_remedy_attempts_per_gap`.
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    /// Monotonic source of turn-unique remedy handles (a model tool-call id is untrusted and may
    /// collide, so it is never used as the handle).
    next_handle: u32,
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
            // Bound inference by the remaining turn time too, so a slow (or long-configured-timeout)
            // model cannot overrun the whole-turn wall-clock ceiling.
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let completion = match tokio::time::timeout(remaining, self.rt.inference().complete(request)).await {
                Ok(Ok(completion)) => completion,
                Ok(Err(_)) => return self.finish_policy_stop(POLICY_STOP_INFERENCE),
                Err(_) => return self.finish_policy_stop(POLICY_STOP_BUDGET),
            };

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

            // Process calls serially. Once a budget stop is hit, the remaining calls in this round get
            // one fixed feedback each and nothing more — no execution, cast, authorization, or return
            // after the turn has entered its policy-stop condition.
            let mut budget_hit = false;
            for proposal in &proposals {
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
                if self.handle_call(&proposal.call).await? == CallStop {
                    budget_hit = true;
                }
            }
            if budget_hit {
                return self.finish_policy_stop(POLICY_STOP_BUDGET);
            }
        }
    }

    /// Route one proposed call. Returns [`CallStop`] if a budget was hit (the turn ends after this
    /// round), [`CallGo`] otherwise. Always emits exactly one terminal response for the call.
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

    /// Check → dispatch/cast/block for an ordinary tool call, resolving casts in a bounded inner loop.
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
                    // Open returns the exact dispatch it appended, or None if the state raced to
                    // not-allowed — in which case we seal (one terminal response) and never invoke.
                    match self.open_dispatch(&call)? {
                        Some(dispatch) => self.invoke_and_admit(dispatch, &call, call_id).await?,
                        None => {
                            self.feedback(call_id, "the call could not be dispatched (the policy state changed)")?
                        }
                    }
                    return Ok(CallGo);
                }
                Ok(CheckOutcome::Unresolved(facts)) => {
                    drop(projection);
                    if self.resolve_unknown(&log, &facts).await? {
                        continue; // a dimension was cast — re-check on the new revision
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
                        // No atomic plan, but a curative redispatch may unblock it — say "no remedy"
                        // only when the block is genuinely uncurable over the remedy subset.
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

    /// The reserved `execute_remedy_plan(plan_id)` tool: gather the pending block's rulings from its
    /// authorities and land the atomic authorize+dispatch, then invoke and admit.
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
            // Bound the authority wait by the remaining turn time — a slow authority cannot overrun
            // the turn deadline; a timeout fails closed (Abstain).
            let answer = tokio::time::timeout(self.external_budget(), backend.rule(&request))
                .await
                .unwrap_or(AuthorityAnswer::Abstain);
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
        self.invoke_and_admit(dispatch, &call, call_id).await?;
        Ok(CallGo)
    }

    /// The reserved `submit_result(value)` tool (RP6): return one raw value to the parent at the child
    /// fold. A sanitized (audience-relabeled) return needs a policy-declared sanitizer binding — a
    /// follow-up; v1 returns raw only.
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

        // Record the child's return (server-derived label, trust never rises), capturing its id.
        let return_id = loop {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            let views = projection.view(self.session);
            let occurrence = views.returns_by(self.session);
            let batch = match self.rt.engine().submit_child_return(
                &views,
                ChildReturn::Raw {
                    body: ValueBody::new(body.clone()),
                },
            ) {
                Ok(batch) => batch,
                Err(_) => {
                    self.feedback(call_id, "this session cannot submit a result")?;
                    return Ok(CallGo);
                }
            };
            drop(projection);
            match self.rt.store().conditional_append(self.tenant, self.session, batch) {
                Ok(_) => break ChildReturnId::new(self.session.clone(), occurrence),
                Err(StoreError::Stale { .. }) => continue,
                Err(e) => return Err(DriveError::Store(e)),
            }
        };

        // Merge it into the direct parent so the parent actually receives the value (RP6). The engine
        // derives the parent's new label (never widening) and enforces once-only; finalize keeps the
        // merge bounded under the shared family lock.
        if let Some(parent) = self.rt.store().parent_of(self.tenant, self.session)? {
            self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
                let projection = Projection::build(facts, rev);
                self.rt.engine().merge(&projection.view(&parent), &return_id).ok()
            })?;
        }
        self.feedback(call_id, "result submitted to the parent")?;
        Ok(CallGo)
    }

    /// Try to resolve the first unresolved dimension by a registered cast (registration order): a
    /// constant cast applies its declared value; a resolver cast asks its backend, bounded by the
    /// engine's `may_cast`. Returns `true` if a cast was admitted (the caller re-checks). The engine
    /// re-validates every proposal against `may_cast`, so a misbehaving resolver cannot widen a label.
    async fn resolve_unknown(&self, log: &[Fact], facts: &[UnresolvedFact]) -> Result<bool, DriveError> {
        let Some(target) = facts.first() else {
            return Ok(false);
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
                        match tokio::time::timeout(self.external_budget(), resolve).await {
                            Ok(BackendCast::Resolved(dim)) if dim.dimension() == target.dimension => Some(dim),
                            _ => None, // unresolved, wrong dimension, or timed out → fail closed
                        }
                    }
                    None => None,
                },
            };
            let Some(resolved) = resolved else {
                continue;
            };

            let (fresh, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&fresh, rev);
            let views = projection.view(self.session);
            let answer = CastAnswer {
                cast: cast.name.clone(),
                resolved,
            };
            if let Ok(batch) = self.rt.engine().admit_cast(&views, target, answer) {
                drop(projection);
                match self.rt.store().conditional_append(self.tenant, self.session, batch) {
                    Ok(_) => return Ok(true),
                    Err(StoreError::Stale { .. }) => return Ok(true), // the re-check re-derives on the new revision
                    Err(e) => return Err(DriveError::Store(e)),
                }
            }
        }
        Ok(false)
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

    /// Open the dispatch for a clean-allow call, returning the **exact** [`DispatchId`] appended (so the
    /// admit path closes that dispatch, not a raced sibling). Returns `None` if the state raced to
    /// not-allowed — the caller then seals instead of invoking. Retries only on a concurrent-branch CAS
    /// loss.
    fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, DriveError> {
        loop {
            let (log, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let projection = Projection::build(&log, rev);
            let views = projection.view(self.session);
            let Ok(batch) = self.rt.engine().open_dispatch(&views, call) else {
                return Ok(None); // no longer allowed under a raced revision
            };
            // The occurrence is computed from the same views the batch was built against, so this id
            // is exactly the dispatch these facts open.
            let dispatch = DispatchId::new(
                self.session.clone(),
                call.digest(),
                views.dispatch_count(&call.digest()),
            );
            drop(projection);
            match self.rt.store().conditional_append(self.tenant, self.session, batch) {
                Ok(_) => return Ok(Some(dispatch)),
                Err(StoreError::Stale { .. }) => continue,
                Err(e) => return Err(DriveError::Store(e)),
            }
        }
    }

    async fn invoke_and_admit(
        &mut self,
        dispatch: DispatchId,
        call: &ResolvedCall,
        call_id: &ToolCallId,
    ) -> Result<(), DriveError> {
        self.invocations += 1;
        let rendered = RenderedCall::from_call(call);
        // Bound the south invocation by the remaining turn time — a slow tool cannot overrun the turn
        // deadline; an outer timeout drops the request and is treated as indeterminate.
        let outcome = match self.rt.tool_backend(call.tool()) {
            Some(backend) => {
                let invoke = backend.invoke(&rendered, self.rt.budgets().body_cap_bytes);
                tokio::time::timeout(self.external_budget(), invoke)
                    .await
                    .unwrap_or(ToolOutcome::Indeterminate)
            }
            None => ToolOutcome::Failure,
        };
        let admission = match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            } => ResultAdmission::SuccessRaw {
                body: ValueBody::new(body.clone()),
            },
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Failure | ToolOutcome::Indeterminate => ResultAdmission::Failure,
        };
        let admitted = self.admit_result(&dispatch, call, admission)?;

        match &outcome {
            // An available result IS the model-visible response — read from its ValueAdmitted. But if
            // the admission did not land (a would-be-invariant engine error), seal so the call still
            // gets exactly one terminal response and the transcript stays paired.
            ToolOutcome::Success {
                body: BodyDisposition::Available(_),
            } => {
                if admitted {
                    Ok(())
                } else {
                    self.feedback(call_id, SEALED_FAILED)
                }
            }
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => self.feedback(call_id, SEALED_WITHHELD),
            ToolOutcome::Failure => self.feedback(call_id, SEALED_FAILED),
            ToolOutcome::Indeterminate => self.feedback(call_id, SEALED_INDETERMINATE),
        }
    }

    /// Close the dispatch and admit (or seal) its result through the store's **shielded finalization**
    /// (CC5/RP2): one lock acquisition, no CAS-loop, so the close lands in bounded steps even under
    /// continuous sibling appends — it cannot livelock or orphan an open dispatch. The engine derives
    /// the admission facts under the lock at the current revision; a dispatch already closed by a prior
    /// attempt is an idempotent no-op.
    /// Returns whether the engine's admission actually landed this call (a batch was appended). `false`
    /// means the dispatch was already closed or the admission hit a would-be-invariant engine error —
    /// the caller then guarantees a terminal response itself rather than assuming the value landed.
    fn admit_result(
        &self,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<bool, DriveError> {
        let mut admission = Some(admission);
        let mut admitted = false;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let admission = admission.take()?;
            match self.rt.engine().admit_result(&views, dispatch, call, admission) {
                Ok(batch) => {
                    admitted = true;
                    Some(batch)
                }
                Err(_) => None,
            }
        })?;
        Ok(admitted)
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

    /// Append revision-independent facts under CAS, retrying on a concurrent-branch loss.
    fn append(&self, facts: Vec<Fact>) -> Result<(), DriveError> {
        loop {
            let (_, rev) = self.rt.store().snapshot(self.tenant, self.session)?;
            let batch = FactBatch::new(rev, facts.clone());
            match self.rt.store().conditional_append(self.tenant, self.session, batch) {
                Ok(_) => return Ok(()),
                Err(StoreError::Stale { .. }) => continue,
                Err(e) => return Err(DriveError::Store(e)),
            }
        }
    }

    fn past_deadline(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// The time budget for one external await: the smaller of the remaining turn time and the
    /// per-external cap, so no single wait can overrun the whole-turn deadline.
    fn external_budget(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(self.rt.budgets().per_external_timeout)
    }
}

/// The progress of one handled call: whether the turn may continue or must stop after this round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallProgress {
    Go,
    Stop,
}
use CallProgress::{Go as CallGo, Stop as CallStop};

fn turn_end(session: &TrajectoryId) -> Fact {
    Fact::Boundary {
        trajectory: session.clone(),
        kind: BoundaryKind::TurnEnd,
    }
}

/// Parse a WireToolCall into a proposal. An empty argument string is the no-argument call `{}`; a
/// **non-empty but invalid** JSON string is flagged `malformed` — the drive seals it rather than
/// repairing it into a `{}` call the model did not encode (RP3). The call is still recorded (with `{}`)
/// so the transcript keeps one response per proposed call.
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

/// The tool a curative `Redispatch` recommendation names, for safe model-visible feedback.
fn redispatch_hint(recommendation: &appa_engine::plan::Recommendation) -> Option<String> {
    match recommendation {
        appa_engine::plan::Recommendation::Redispatch { tool, .. } => Some(tool.as_str().to_string()),
        appa_engine::plan::Recommendation::Fork { .. } => None,
    }
}

/// The body of the `id`-th admitted value in log order (a `ValueId` indexes the value sequence).
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

    /// A canned OpenAI server that answers each request with the next scripted response body, one per
    /// connection (`Connection: close`, so reqwest opens a fresh socket per round).
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

        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("what is wrong?"))
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("the pod is crashlooping".to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        // The tool result was admitted as a value the model can see.
        let admitted = log.iter().any(|f| {
            matches!(f, Fact::ValueAdmitted { value, provenance: Provenance::ToolResult { .. }, .. }
                if value.body.as_str() == "CrashLoopBackOff")
        });
        assert!(admitted, "tool result should be admitted");
        model.await.unwrap();
    }

    #[tokio::test]
    async fn block_then_remedy_authorizes_and_dispatches() {
        // `wire` demands an attention mark (always a gap); the officer attends it and clears via builtin
        // approve (a cover-free mandate). Round 1 blocks; round 2 authorizes; round 3 answers.
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
        // The first (only) block gets the server-minted handle "remedy-0"; round 2 authorizes by it.
        let (base, model) = spawn_scripted_model(vec![
            tool_call_round("1", "wire", r#"{"amount":100}"#),
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-0"}"#),
            final_round("3", "the transfer is done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("wire the invoice"))
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("the transfer is done".to_string()));

        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        // A ruling landed and the finance effect committed — the authorized dispatch actually ran.
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

    /// A tool whose 2xx body exceeds the cap: effects commit (the tool succeeded) but no value is
    /// admitted — the model sees a sealed token, never the oversized bytes (RP3).
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

        drive_turn(&rt, &tenant, &session, false, user_turn("dump the file"))
            .await
            .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        // The success committed its effect...
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects.iter().any(|e| e.as_str() == "read")
        )));
        // ...but no tool-result value was admitted (the bytes never became a value)...
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ValueAdmitted {
                provenance: Provenance::ToolResult { .. },
                ..
            }
        )));
        // ...and the model saw a sealed token.
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::BlockFeedback { content, .. } if content == SEALED_WITHHELD
        )));
        model.await.unwrap();
    }

    /// A failing tool commits no effect and admits no value; the model sees a sealed failure token.
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

        drive_turn(&rt, &tenant, &session, false, user_turn("try it"))
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

    /// A child's `submit_result` must actually reach the parent as a merged value (RP6), not just be
    /// recorded on the child.
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

        drive_turn(&rt, &tenant, &child, true, user_turn("investigate"))
            .await
            .unwrap();

        let (log, _) = rt.store().snapshot(&tenant, &parent).unwrap();
        // The parent received the returned value and a merge boundary landed.
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
        // ...and the merged body is visible in the parent's model transcript (usable, not just a label).
        let transcript = crate::transcript::model_transcript(&[], &log, &parent);
        assert!(
            transcript
                .iter()
                .any(|m| m.content.as_deref() == Some("child findings")),
            "the merged child value should appear in the parent transcript"
        );
        model.await.unwrap();
    }

    /// Two blocked calls in one round get distinct remedy handles; remedying one authorizes exactly
    /// that call, not the other (the plan-id ambiguity fix).
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
            // wire_a blocks first (handle "remedy-0"), wire_b second ("remedy-1"); remedy only b.
            tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
            final_round("3", "b is done"),
        ])
        .await;
        let rt = runtime_over(config, builtins, base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        drive_turn(&rt, &tenant, &session, false, user_turn("do both"))
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

    /// Malformed (non-empty invalid JSON) tool arguments are sealed with feedback, never repaired into
    /// an executable call.
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

        drive_turn(&rt, &tenant, &session, false, user_turn("read"))
            .await
            .unwrap();
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        // Never dispatched, but the proposed call still got exactly one terminal response.
        assert!(!log.iter().any(|f| matches!(f, Fact::DispatchOpened { .. })));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::BlockFeedback { .. })).count(),
            1
        );
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_final_answer_with_no_tools_ends_the_turn() {
        let config = Config::from_toml_str("version = 1\ntrust_chain = [\"suspicious\", \"trusted\"]\n").unwrap();
        let (base, model) = spawn_scripted_model(vec![final_round("1", "hello, I need no tools")]).await;
        let rt = runtime_over(config, BTreeMap::new(), base).await;
        let tenant = TenantId::new("acme");
        let session = rt.store().create_session(tenant.clone());

        let outcome = drive_turn(&rt, &tenant, &session, false, user_turn("hi"))
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Final("hello, I need no tools".to_string()));
        // Exactly one user turn and one turn-end boundary landed.
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
