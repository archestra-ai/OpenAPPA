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
use std::future::Future;
use std::time::{Duration, Instant};

use appa_engine::admit::{AdmitError, CastAnswer, ResultAdmission};
use appa_engine::authority::CastResolution;
use appa_engine::branch::{ReturnCheck, ReturnPlan, ReturnSubmission};
use appa_engine::check::{CheckOutcome, UnresolvedFact};
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall, ReturnPolicy};
use appa_engine::label::{DimValue, Dimension};
use appa_engine::names::{CastName, SanitizerName};
use appa_engine::plan::PlanId;
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

/// The fixed, model-visible terminals the drive seals in place of a raw result (RP3).
const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
const SEALED_UNRESOLVED: &str = "[tool result withheld: its label could not be established]";
const SEALED_UNSANITIZED: &str = "[tool result withheld: the bound sanitizer produced no derivation]";
const SEALED_FAILED: &str = "[tool call failed]";
const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";
const POLICY_STOP_BUDGET: &str = "This turn reached its resource budget and was stopped.";
const POLICY_STOP_INFERENCE: &str = "This turn could not continue: upstream inference was unavailable.";
const POLICY_STOP_CANCELLED: &str = "This turn was cancelled.";

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The model yielded a final assistant answer (its free text).
    Final(String),
    /// A budget or upstream fault ended the turn in a fixed, replayable terminal.
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

/// How a result admission landed, causes kept distinct (see [`Drive::admit_result`]).
enum Admission {
    Admitted,
    AlreadyClosed,
    Refused,
    InvariantBreach,
    /// A value-carrying admission suppressed because the turn's cancellation had already fired.
    CancelSuppressed,
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

/// A blocked child return awaiting the model's return-plan decision. A success consumes the whole
/// pending offer (an executed plan's merge moves the parent, so the engine's value-matched
/// re-derivation refuses the sibling handles), a stale refusal discards it, and end-of-turn
/// destroys it with the drive. The confined raw submission lives only here — never in feedback or
/// plan descriptions.
struct PendingReturn {
    parent: TrajectoryId,
    body: String,
    raw_digest: RawResultDigest,
    /// Sibling offers: server-minted handle → the return plan it names.
    offers: Vec<(String, ReturnPlan)>,
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
        remedy_attempts: BTreeMap::new(),
        return_derivation_attempts: BTreeMap::new(),
        next_handle: 0,
    };
    drive.run(user_turn).await
}

struct Drive<'a> {
    rt: &'a Runtime,
    tenant: &'a TenantId,
    session: &'a TrajectoryId,
    is_child: bool,
    /// The turn's cancellation signal (RP2): raced at every external await. The holder of the drive
    /// future must never abort it — cancellation transitions through [`Drive::finish_cancelled`],
    /// which lands the close/seal/terminal facts before the future completes.
    cancel: CancellationToken,
    deadline: Instant,
    rounds: u32,
    invocations: u32,
    pending: Vec<PendingBlock>,
    pending_returns: Vec<PendingReturn>,
    /// Remedy executions attempted per call this turn — bounds `max_remedy_attempts_per_gap`.
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    /// Return-derivation attempts this turn, keyed by (raw submission, sanitizer) — sanitizer A's
    /// failures never starve sanitizer B or Accept, and resubmitting the same body mints no fresh
    /// budget. Accept performs no fallible external work and is never charged.
    return_derivation_attempts: BTreeMap<(RawResultDigest, SanitizerName), u32>,
    /// Monotonic source of turn-unique remedy handles (a model tool-call id is untrusted and may
    /// collide, so it is never used as the handle).
    next_handle: u32,
}

/// The turn's cancellation fired while an external await was in flight.
struct TurnCancelled;

/// How a raw `submit_result` proceeds after the narrowing check.
enum RawReturnGo {
    /// No narrowing (or no parent): the raw crossing merges silently.
    Merge,
    /// The call was already answered — a block with offered plans, an unresolved notice, or a
    /// refusal. Nothing crosses now.
    Answered,
}

/// The model-facing description of one offered return plan. Never includes the submitted bytes.
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
            // Bound inference by the remaining turn time too, so a slow (or long-configured-timeout)
            // model cannot overrun the whole-turn wall-clock ceiling.
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let completion = tokio::select! {
                biased;
                // Between rounds no dispatch is open and every prior call is answered, so a
                // cancelled inference needs only the terminal.
                _ = self.cancel.cancelled() => return self.finish_cancelled(None, &[]),
                out = tokio::time::timeout(remaining, self.rt.inference().complete(request)) => match out {
                    Ok(Ok(completion)) => completion,
                    Ok(Err(_)) => return self.finish_policy_stop(POLICY_STOP_INFERENCE),
                    Err(_) => return self.finish_policy_stop(POLICY_STOP_BUDGET),
                },
            };

            // Cancellation decided before the round is recorded: a token that fired while
            // inference was completing discards the whole round (nothing of it exists to replay)
            // rather than recording calls the turn will never answer outside the terminal.
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

            // Process calls serially. Once a budget stop is hit, the remaining calls in this round get
            // one fixed feedback each and nothing more — no execution, cast, authorization, or return
            // after the turn has entered its policy-stop condition.
            let mut budget_hit = false;
            for index in 0..proposals.len() {
                let proposal = &proposals[index];
                // Cancellation first — before budget/malformed feedback and before the call runs.
                // Checked per proposal, not only inside await races: a synchronous path (a builtin
                // tool, a raw submit_result) would otherwise run and cross data after the
                // disconnect that cancelled the turn.
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
                    // Cancelled mid-round: this call and every remaining one in the round still get
                    // their one sealed response, inside the terminal batch.
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
        // A handle names either a blocked child return's offer or a blocked tool call's plan —
        // both are minted from the same turn-unique counter, so a lookup is unambiguous.
        if let Some(index) = self
            .pending_returns
            .iter()
            .position(|p| p.offers.iter().any(|(h, _)| h == handle))
        {
            return self.handle_execute_return_remedy(call_id, index, handle).await;
        }
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
            // the turn deadline; a timeout fails closed (Abstain). No dispatch is open yet, so a
            // cancellation here owes only the seal and terminal.
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

    /// The reserved `submit_result(value)` tool (RP6): return one value to the parent — raw at the
    /// child fold, or, with `[child] return_sanitizer` configured, only as that sanitizer's
    /// derivation at its exact declared label.
    async fn handle_submit_result(
        &mut self,
        call_id: &ToolCallId,
        arguments: &serde_json::Value,
    ) -> Result<CallProgress, DriveError> {
        if !self.is_child {
            self.feedback(call_id, "submit_result is available only to a child session")?;
            return Ok(CallGo);
        }
        // Strict wire shape: exactly one key, `value` — a string result, or an explicit null to
        // return nothing. Anything else (missing, wrongly typed, or extra fields) is rejected —
        // the old lenient path coerced a missing/malformed value to "" and merged it carrying the
        // full child fold, a silent taint crossing for an empty body.
        let exact_shape = arguments
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("value"));
        let body = match exact_shape {
            Some(serde_json::Value::String(value)) => value.clone(),
            Some(serde_json::Value::Null) => {
                // Void return: the child ends its errand without returning a value. Nothing is
                // recorded and nothing merges — no label propagates to the parent, exactly as if
                // the branch had been abandoned; the child log alone carries the audit.
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

        // Server policy decides how the value crosses (RP6): a `[child]` static sanitizer binding
        // is the child's only return channel, at the binding's exact engine-derived label; the raw
        // submitted text never leaves the child and the model never chooses the path. A failed
        // derivation fails closed: nothing returns. With no binding, the narrowing check decides
        // whether the raw crossing is silent — a narrowing return is blocked with return plans.
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

        // The crossing: record, parent admission, and merge boundary land as ONE engine batch in
        // ONE finalization (CC5) — no orphanable intermediate state. The cancellation token is
        // consulted **inside** the closure: this is the single commit point at which the value
        // crosses, so a token that fired first suppresses the whole crossing atomically under the
        // family lock (a token firing after the closure ran linearizes after the crossing).
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

    /// Run the engine's return check for a raw submission and, on a block, stash the pending
    /// attempt and answer the call with the offered plans. `Merge` means the raw crossing is
    /// silent (no narrowing, or no parent at all); `Answered` means this call already got its
    /// response (block feedback or an unresolved/refusal notice).
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

    /// Execute one offered return plan: derive outside the family lock where the plan needs a
    /// sanitizer, then land the engine's atomic crossing+acceptance+merge batch inside
    /// finalization. A success consumes the whole pending offer (every sibling handle); a stale
    /// refusal discards it (the child must submit afresh); a failed derivation restores the plan
    /// to pending within its turn-wide (raw submission, sanitizer) budget.
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

        // Only externally-fallible work is budgeted: Accept is free, and charges key on the raw
        // submission + sanitizer, so one sanitizer's failures never starve a sibling and
        // resubmitting the same body mints no fresh budget.
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

        // The commit point: cancellation and staleness are both decided under the family lock —
        // the engine re-derives the block from the live views and refuses by value a chosen plan
        // the fresh offers no longer contain, so nothing crosses on a stale offer.
        let parent = self.pending_returns[index].parent.clone();
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
            // The offer is consumed: every sibling handle dies with it.
            self.pending_returns.remove(index);
            self.feedback(call_id, "result submitted to the parent")?;
            return Ok(CallGo);
        }
        if self.cancel.is_cancelled() {
            return Ok(CallCancelled(None));
        }
        // Stale: the family state moved since the offer. Discard the whole attempt — the child
        // must submit afresh against the new state, never retry an offer computed for an old one.
        self.pending_returns.remove(index);
        self.feedback(call_id, "the return offer is stale; submit the result again")?;
        Ok(CallGo)
    }

    /// Try to resolve the first unresolved dimension by a registered cast (registration order): a
    /// constant cast applies its declared value; a resolver cast asks its backend, bounded by the
    /// engine's `may_cast`.
    ///
    /// Reachability: no admission path of the in-memory runtime currently mints a value with an
    /// Unknown dimension (boundary labels are Known, and a pending-cast output admits only
    /// resolved), so this check-time path is engine-generality held for future Unknown sources —
    /// e.g. a rehydrated/persisted log — not an active turn state today.
    /// `Ok(Some(true))` means a cast was admitted (the caller re-checks);
    /// `Err(TurnCancelled)` surfaces a cancellation during a resolver await (no dispatch is open on
    /// this path). The engine re-validates every proposal against `may_cast`, so a misbehaving
    /// resolver cannot widen a label.
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

    /// Derive the bound sanitizer's output from a confined raw body, deadline-bounded. `Ok(None)`
    /// fails closed (no backend, a failed derivation, or a timeout): the caller withholds the
    /// value. The derived bytes carry the sanitizer's declared label — the engine computes it at
    /// admission.
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

    /// Resolve a pending-cast output dimension over the registered casts (registration order): a
    /// constant cast answers its declared value; a resolver cast is asked with the confined raw
    /// body, deadline-bounded. The engine re-validates the winning answer at admission, so a
    /// misbehaving resolver cannot widen the label past its declared ceiling.
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
                            // An answer outside the declared may_cast ceiling is discarded here like
                            // any other non-answer (the engine re-validates at admission either
                            // way): a hostile resolver must not be able to poison the admission of
                            // an otherwise-successful dispatch.
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

    /// Open the dispatch for a clean-allow call, returning the **exact** [`DispatchId`] appended (so
    /// the admit path closes that dispatch, not a raced sibling). Returns `None` if the current state
    /// no longer allows the call — the caller then seals instead of invoking. Runs through the
    /// store's serialized finalization (CC5): the engine decides under the family lock at the live
    /// revision, one acquisition, no CAS retry to be starved by sibling appends.
    fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, DriveError> {
        let mut dispatch = None;
        self.rt.store().finalize(self.tenant, self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(self.session);
            let batch = self.rt.engine().open_dispatch(&views, call).ok()?;
            // The occurrence is computed from the same views the batch was built against, so this id
            // is exactly the dispatch these facts open.
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
        // Bound the south invocation by the remaining turn time — a slow tool cannot overrun the turn
        // deadline; a timeout drops the request and is treated as indeterminate. A cancellation here
        // finds the dispatch open with its outcome unobserved: the terminal batch closes it
        // `Indeterminate` (the tool may or may not have run).
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
        // Cancellation decided again now that the outcome is observed, before any admission
        // commits: a token that fired while the tool was finishing keeps its result out of the
        // trajectory, and the terminal closes the dispatch honestly for what was observed.
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
        // The contract's Phase-2 discipline for an available raw body (mutually exclusive by load
        // validation): a pending-cast output confines it until a registered cast establishes its
        // label (RP5); a bound output sanitizer confines it and admits only the derivation (RP4).
        // Either failing withholds the value while the successful call's effects stand.
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
                // Cancellation during a derivation finds success already observed: the terminal
                // batch closes success-with-no-value, so the committed effects stand (RP4/RP5).
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
            // A refused value admission (a resolution the engine rejects) leaves the dispatch open:
            // close it success-with-no-value so effects stand and nothing is orphaned. An
            // already-closed dispatch needs no retry.
            Admission::Refused => {
                self.admit_result(&dispatch, call, ResultAdmission::SuccessNoValue)?;
                false
            }
            Admission::AlreadyClosed => false,
            // The token fired before the value could cross: the cancelled terminal closes the
            // still-open dispatch for what was actually observed.
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
            // An available result IS the model-visible response — read from its ValueAdmitted. But if
            // the admission did not land (a would-be-invariant engine error), seal so the call still
            // gets exactly one terminal response and the transcript stays paired.
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

    /// Close the dispatch and admit (or seal) its result through the store's **shielded finalization**
    /// (CC5/RP2): one lock acquisition, no CAS-loop, so the close lands in bounded steps even under
    /// continuous sibling appends — it cannot livelock or orphan an open dispatch. The engine derives
    /// the admission facts under the lock at the current revision. The refusal causes are kept
    /// distinct: an already-closed dispatch is an idempotent no-op, a value-policy rejection leaves
    /// the dispatch open for the caller to close another way, and an identity mismatch (a dispatch
    /// that does not belong to this call/branch) is a drive invariant breach surfaced loudly, never
    /// absorbed.
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
            // The value admission is the confinement commit point, so the cancellation token is
            // consulted here, atomically under the family lock: a token that fired first keeps the
            // value out of the trajectory (the caller closes the dispatch through the cancelled
            // terminal instead). Value-less closes are owed regardless and never suppressed.
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

    /// Append revision-independent facts through the store's serialized finalization (CC5): one
    /// family-lock acquisition, no CAS retry — bounded even under continuous sibling appends, so a
    /// terminal (or any turn fact) can never be starved out by a busy child branch.
    fn append(&self, facts: Vec<Fact>) -> Result<(), DriveError> {
        self.rt
            .store()
            .finalize(self.tenant, self.session, |_, rev| Some(FactBatch::new(rev, facts)))?;
        Ok(())
    }

    fn past_deadline(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Race one external await against the turn's cancellation and the per-external deadline:
    /// `Err(TurnCancelled)` on cancellation, `Ok(None)` on timeout (each site fails closed its own
    /// way), `Ok(Some(_))` on completion.
    async fn wait<F: Future>(&self, fut: F) -> Result<Option<F::Output>, TurnCancelled> {
        // Biased: a fired cancellation wins even against a future that is already ready, so an
        // instantly-completing backend cannot slip a result past a cancelled turn.
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(TurnCancelled),
            out = tokio::time::timeout(self.external_budget(), fut) => Ok(out.ok()),
        }
    }

    /// The one serialized finalization a cancelled turn lands (RP2/CC5): the open dispatch's close
    /// (if any), one sealed response per still-unanswered proposed call, the fixed policy-stop
    /// message, and the `TurnEnd` — a single batch, so cancellation can never orphan a dispatch or
    /// leave a call without its terminal response.
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
                // A close that no longer applies (the dispatch raced closed) is skipped, never fatal:
                // the terminal still lands.
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

    /// The time budget for one external await: the smaller of the remaining turn time and the
    /// per-external cap, so no single wait can overrun the whole-turn deadline.
    fn external_budget(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(self.rt.budgets().per_external_timeout)
    }
}

/// The progress of one handled call: continue, stop after this round (a budget), or the turn was
/// cancelled — carrying the open dispatch the terminal batch must still close, if any.
enum CallProgress {
    Go,
    Stop,
    Cancelled(Option<OpenClose>),
}
use CallProgress::{Cancelled as CallCancelled, Go as CallGo, Stop as CallStop};

/// An open dispatch a cancelled turn closes inside its serialized terminal batch.
struct OpenClose {
    dispatch: DispatchId,
    call: ResolvedCall,
    close: CancelClose,
}

/// What the cancelled turn knows about the open dispatch: the south outcome was never observed
/// (close `Indeterminate`); success was already observed and only the value derivation was cut
/// short (close success-with-no-value — effects stand, RP4/RP5); or a failure was already observed
/// (close `Failure` — audit-honest, not collapsed into indeterminate).
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
    use appa_engine::fact::ReturnDerivation;
    use appa_engine::value::LabeledValue;
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
        // The parent folds a boundary-label value of its own, so the child's user turn (same
        // label) makes a non-narrowing return — the silent-merge path under test.
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

    /// A void (`value: null`) or malformed `value` crosses nothing — structurally: no child-return
    /// fact, no parent admission, no merge boundary, no parent transcript message. The old lenient
    /// path coerced these to "" and merged the full child fold.
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
            // The model got a response for the call (the child stays drivable) …
            assert!(log.iter().any(|f| matches!(f, Fact::BlockFeedback { .. })));
            // … and the parent transcript gained nothing.
            assert!(crate::transcript::model_transcript(&[], &log, &parent).is_empty());
            model.await.unwrap();
        }
    }

    /// Config for blocked-return tests: a registered (unbound) `pii` output sanitizer only. Taint
    /// is injected directly as an admitted child value (a quarantined read's fold), so no tool
    /// soft-block competes for remedy handles.
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

    /// Admit one value to `session` at the given label — a test stand-in for a read's fold.
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

    /// Drive one child turn over the scripted rounds and return the family log and parent id. The
    /// parent holds a boundary-label value (as any driven parent would), so a clean child does not
    /// narrow it; `taint_child` admits a suspicious+internal value to the child first.
    async fn drive_child_rounds(config: Config, taint_child: bool, rounds: Vec<String>) -> (Vec<Fact>, TrajectoryId) {
        drive_child_rounds_from(config, 1, taint_child, rounds).await
    }

    /// [`drive_child_rounds`] with the parent's own fold at `parent_trust` (public audience): a
    /// suspicious parent makes a tainted child's narrowing audience-only, so a sanitizer can fully
    /// clear it.
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
        // The parent's own fold — a fresh top() parent would make every child user turn a
        // narrowing, which is not the scenario under test.
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

    /// A narrowing raw return blocks; executing the Accept offer merges the raw value under a
    /// return-scoped acceptance naming the full narrowing.
    #[tokio::test]
    async fn a_narrowing_raw_return_blocks_and_accept_merges_raw() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
                // Offers: remedy-0 Accept, remedy-1 Sanitize(pii, residual).
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
        // The parent narrowed to the accepted label.
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::restricted([
                appa_engine::label::ReaderId::new("internal")
            ]))
        );
    }

    /// Executing the sanitize offer merges only the derivation: the raw text never reaches the
    /// parent, the crossing audits the sanitizer against the raw digest, and the acceptance names
    /// the residual (trust) — the parent keeps its audience.
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
        // The residual acceptance is trust-only: audience in the accepted label stays public.
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::ChildReturnAcceptance { narrowing, .. }
                if narrowing.to.audience == appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        )));
        // The raw text is nowhere in the parent's model transcript.
        let transcript = crate::transcript::model_transcript(&[], &log, &parent);
        assert!(
            !transcript
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("eve@corp.com")))
        );
    }

    /// Walking away from a blocked return merges nothing — fail-closed, like abandonment.
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

    /// Two blocked submissions are independent pending offers with sibling-distinct handles;
    /// after one merges and narrows the parent, the other's offers no longer match the moved
    /// state (refused by value) and are discarded — exactly one value crosses.
    #[tokio::test]
    async fn a_pending_return_attempt_is_discarded_after_a_sibling_merge() {
        let (log, parent) = drive_child_rounds(
            blocked_return_config("builtin = \"redact-email\""),
            true,
            vec![
                // Attempt A: offers remedy-0 (Accept) / remedy-1 (sanitize).
                tool_call_round("1", "submit_result", r#"{"value":"first"}"#),
                // Attempt B: offers remedy-2 (Accept) / remedy-3 (sanitize).
                tool_call_round("2", "submit_result", r#"{"value":"second"}"#),
                // Merge attempt B raw…
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-2"}"#),
                // …then replay attempt A: stale, discarded, nothing crosses.
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

    /// The value-staleness counterpart: a suspicious parent makes the tainted child's narrowing
    /// audience-only, so the sanitize offer is residual-free and its crossing leaves the parent's
    /// label untouched — the sibling pending offer then still matches the live state and executes.
    /// Two label-neutral crossings, no acceptance facts.
    #[tokio::test]
    async fn a_label_neutral_crossing_leaves_the_sibling_offer_executable() {
        let (log, parent) = drive_child_rounds_from(
            blocked_return_config("builtin = \"redact-email\""),
            0,
            true,
            vec![
                // Offers: remedy-0 (Accept) / remedy-1 (residual-free sanitize).
                tool_call_round("1", "submit_result", r#"{"value":"ask eve@corp.com"}"#),
                // Offers: remedy-2 (Accept) / remedy-3 (residual-free sanitize).
                tool_call_round("2", "submit_result", r#"{"value":"ask bob@corp.com"}"#),
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-3"}"#),
                final_round("5", "done"),
            ],
        )
        .await;
        let merged = merged_child_values(&log, &parent);
        assert_eq!(merged.len(), 2, "both derivations crossed");
        for value in &merged {
            assert!(!value.body.as_str().contains("corp.com"), "only derivations crossed");
            assert_eq!(
                value.label.audience,
                appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
            );
        }
        // Residual-free: no narrowing was accepted on either crossing.
        assert!(!log.iter().any(|f| matches!(f, Fact::ChildReturnAcceptance { .. })));
    }

    /// A failed derivation fails closed and leaves the offer pending within its own budget;
    /// exhausting one sanitize offer's budget never blocks Accept (charged never), which still
    /// crosses the raw value.
    #[tokio::test]
    async fn a_failed_derivation_restores_the_offer_and_accept_is_never_charged() {
        // Unreachable resolver: every derivation attempt fails closed quickly.
        let (log, parent) = drive_child_rounds(
            blocked_return_config("resolver = { url = \"http://127.0.0.1:1/derive\", timeout_ms = 200 }"),
            true,
            vec![
                tool_call_round("1", "submit_result", r#"{"value":"report: ask eve@corp.com"}"#),
                // Two failed derivations (budget max_remedy_attempts_per_gap = 2)…
                tool_call_round("2", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                tool_call_round("3", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                // …a third is over budget…
                tool_call_round("4", "execute_remedy_plan", r#"{"plan_id":"remedy-1"}"#),
                // …and Accept still works.
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
        // No sanitized crossing ever happened.
        assert!(!log.iter().any(|f| matches!(
            f,
            Fact::ChildReturn {
                derivation: ReturnDerivation::Sanitized { .. },
                ..
            }
        )));
    }

    /// A non-narrowing raw return still merges silently — no block, no acceptance facts.
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
        // Never dispatched, but the proposed call still got exactly one terminal response.
        assert!(!log.iter().any(|f| matches!(f, Fact::DispatchOpened { .. })));
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::BlockFeedback { .. })).count(),
            1
        );
        model.await.unwrap();
    }

    #[tokio::test]
    async fn pending_cast_output_resolves_and_admits_at_the_cast_label() {
        // `scan` declares its output trust pending-cast; the constant cast establishes it as
        // suspicious, so the body is admitted (model-visible) at the resolved label.
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
        // No registered cast can establish `scan`'s output trust: the raw body stays confined
        // (never admitted, sealed to the model) while the successful call's effects stand.
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
        // Effects committed (the tool ran) …
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::DispatchClosed { outcome: appa_engine::fact::CloseOutcome::Success { effects }, .. }
                if effects == &[appa_engine::fact::EffectKind::new("read")]
        )));
        // … but no value entered, and the call's one terminal response is the unresolved seal.
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
        // The admitted value is the derivation — the raw address never enters the trajectory —
        // and it carries the sanitizer's declared audience (public), not the raw delta's.
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
        // The application is audited against the raw result's digest.
        assert!(log.iter().any(|f| matches!(
            f,
            Fact::SanitizerApplied { raw_digest, .. }
                if raw_digest == &appa_engine::value::RawResultDigest::of(b"contact bob@corp.com for access")
        )));
        model.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_sanitizer_derivation_seals_but_commits_effects() {
        // The bound sanitizer's backend is an unreachable HTTP resolver: derivation fails closed —
        // effects stand, no value enters, the model sees the sealed token.
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
        // With `[child] return_sanitizer` set, the parent receives the derivation at the
        // sanitizer's declared label — the raw submitted text never crosses.
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
        // The merged label is parent.combine(sanitizer's declared output) — public here on both
        // sides, so the parent stays public rather than narrowing to the child's raw fold.
        assert_eq!(
            merged[0].label.audience,
            appa_engine::label::Dim::Known(appa_engine::label::Audience::Public)
        );
        // The crossing is audited against the RAW submission's digest, not the derivation's.
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

    /// An HTTP endpoint that accepts connections and never answers — a hanging backend.
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

        // The serialized terminal: the open dispatch closed Indeterminate, the proposed call got its
        // sealed response, and the turn ended — nothing orphaned.
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

        // The lease was released and the session is usable: the next turn completes normally.
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
        // The resolver answers an in-chain rank ABOVE its declared may_cast ceiling. The answer is
        // discarded (drive prefilter; the engine would refuse it at admission anyway), the value is
        // withheld, and the successful dispatch still closes with its effects — never orphaned.
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
        // Closed with effects standing, no value admitted, the call sealed.
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
        // No dispatch is left open: every open has its close.
        assert_eq!(
            log.iter().filter(|f| matches!(f, Fact::DispatchOpened { .. })).count(),
            log.iter().filter(|f| matches!(f, Fact::DispatchClosed { .. })).count(),
        );
    }

    #[tokio::test]
    async fn cancellation_mid_round_seals_the_remaining_calls_without_dispatching_them() {
        // One round proposes two calls; cancellation fires while the first hangs south. The second
        // call must never dispatch (a ready builtin would otherwise run after the disconnect) and
        // both calls get their one sealed response in the terminal.
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
        // Only the hanging call ever dispatched; the ready builtin never ran after the cancel.
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
        // Both proposed calls got exactly one sealed response.
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
        // The turn never began: no fact of it exists, so the trajectory replays identically.
        let (log, _) = rt.store().snapshot(&tenant, &session).unwrap();
        assert!(log.is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_inference_ends_the_turn_with_no_dispatch() {
        // The upstream model hangs; cancellation lands the terminal without any dispatch opened.
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
        // Runtime::new sources the pinned preamble from config (the transcript tests pin that the
        // preamble heads every rebuilt model request).
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
