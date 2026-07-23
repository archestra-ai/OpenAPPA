//! The SDK session: one trajectory, mediated serially at one lifecycle choke point.
//!
//! Structural invariants live in types ([`DispatchHandle`] cannot be cloned, serialized, or built by
//! the host; it is consumed by value). Temporal invariants live here, at the single runtime choke
//! point every public entry checks: the session is `Idle` (no active turn), `ReadyForCompletion`
//! (a turn is active, the next move is the model's), or `AwaitingOutcome` (exactly one surfaced
//! call is outstanding and nothing else may happen until its outcome is reported or abandoned).
//!
//! Every fact this module appends mirrors the sequence `appa-runtime`'s turn-drive writes for the
//! same situation, so `model_transcript` and the engine's `Projection` see exactly the shapes they
//! were built for: a user turn is one `ValueAdmitted` (user provenance, boundary label, no boundary
//! fact); an inference round is one `AssistantMessage` carrying its proposed calls; every proposed
//! call gets exactly one terminal response fact (`ValueAdmitted` for an admitted result, else
//! `BlockFeedback`); a turn ends with exactly one `TurnEnd` boundary.

use std::collections::{BTreeMap, VecDeque};

use thiserror::Error;

use appa_engine::admit::{AdmitError, ResultAdmission};
use appa_engine::check::CheckOutcome;
use appa_engine::engine::Engine;
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ProposedCall};
use appa_engine::label::Label;
use appa_engine::names::AuthorityName;
use appa_engine::plan::PlanId;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, LabeledValue, Provenance, ResolvedCall, ToolCallId, ToolName, TrajectoryId, ValueBody,
};

use appa_runtime::config::Config;
use appa_runtime::external::{AuthorityAnswer, AuthorityBackend, AuthorityRequest};
use appa_runtime::inference::Completion;
use appa_runtime::runtime::{EXECUTE_REMEDY_PLAN, SUBMIT_RESULT};
use appa_runtime::store::{SessionStore, StoreError, TenantId};
use appa_runtime::tool::{BodyDisposition, RenderedCall, ToolOutcome};
use appa_runtime::transcript::model_transcript;
use appa_runtime::wire::{WireMessage, WireTool, WireToolCall, WireToolSchema};

use crate::assemble;

// The fixed model-visible terminals, byte-identical to the turn-drive's (RP3).
const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
const SEALED_FAILED: &str = "[tool call failed]";
const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";
const TURN_CANCELLED: &str = "This turn was cancelled.";
const MALFORMED_ARGUMENTS: &str = "the tool call had malformed arguments and was not executed";

/// Session tuning. The remedy bound mirrors the runtime's default budget.
#[derive(Clone, Copy, Debug)]
pub struct SdkOptions {
    /// How many `execute_remedy_plan` attempts one call (by digest) may consume per turn.
    pub max_remedy_attempts_per_gap: u32,
    /// The most one authority consultation may take; a timeout fails closed (Abstain).
    pub per_external_timeout: std::time::Duration,
}

impl Default for SdkOptions {
    fn default() -> Self {
        SdkOptions {
            max_remedy_attempts_per_gap: 2,
            per_external_timeout: std::time::Duration::from_secs(30),
        }
    }
}

/// Why a session could not be opened on this policy.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The policy uses a feature the SDK v0 defers (sanitizers, casts, output bindings,
    /// pending-cast deltas, child config, or tool execution backends — SDK tools are host-executed).
    #[error("unsupported policy for the embedded SDK: {0}")]
    UnsupportedPolicy(String),
    #[error("registered tool {0} collides with a reserved tool name")]
    ReservedToolConflict(String),
}

/// An operation arrived in the wrong lifecycle state.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("the session is {actual}; this operation requires {required}")]
pub struct SessionBusy {
    pub actual: &'static str,
    pub required: &'static str,
}

/// Why the tool surface could not be bound.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolSurfaceError {
    #[error("tools are already bound for this session")]
    AlreadyBound,
    #[error("the tool surface advertises {0} but the policy does not register it")]
    UnknownTool(String),
    #[error("the policy registers {0} but the tool surface does not provide it")]
    MissingTool(String),
    #[error("duplicate tool name {0} in the provided surface")]
    Duplicate(String),
}

/// Why a mediation step failed. Policy outcomes (blocks, denials) are not errors — they become
/// feedback facts the model sees; these are lifecycle or infrastructure faults.
#[derive(Debug, Error)]
pub enum MediateError {
    #[error(transparent)]
    Busy(#[from] SessionBusy),
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

/// Why an outcome report failed.
#[derive(Debug, Error)]
pub enum ReportError {
    #[error(transparent)]
    Busy(#[from] SessionBusy),
    #[error("the handle does not identify the outstanding surfaced call")]
    UnknownHandle,
    #[error("the dispatch is no longer open")]
    DispatchNotOpen,
    #[error("dispatch identity no longer matches its call — an SDK invariant was breached")]
    DispatchIdentity,
    #[error("session store fault: {0}")]
    Store(#[from] StoreError),
}

/// The one outstanding surfaced call. Move-only by construction: no `Clone`, no serde, private
/// fields, no public constructor — the host can only pass it back whole to
/// [`AppaSession::report_outcome`] or [`AppaSession::abandon`]. (Boxed internally: the handle rides
/// inside [`Step`], which stays small.)
#[derive(Debug)]
pub struct DispatchHandle(Box<HandleInner>);

#[derive(Debug)]
struct HandleInner {
    id: u64,
    dispatch: DispatchId,
    call: ResolvedCall,
    /// The model tool-call this dispatch's terminal response answers — for a remedied call, the
    /// `execute_remedy_plan` proposal's id, not the originally blocked proposal's.
    response_call_id: ToolCallId,
}

impl DispatchHandle {
    /// Which repeat of this exact call (by canonical digest) this dispatch is.
    pub fn occurrence(&self) -> u32 {
        self.0.dispatch.occurrence()
    }

    pub fn tool(&self) -> &ToolName {
        self.0.call.tool()
    }
}

/// The next move the host must make.
#[derive(Debug)]
pub enum Step {
    /// The model yielded its final answer; the turn is over.
    Final { text: String },
    /// Execute exactly this call, then report through the handle before anything else.
    Execute { handle: DispatchHandle, call: RenderedCall },
    /// Nothing outstanding — run the next inference round from a fresh [`AppaSession::transcript`].
    Continue,
}

/// What entered (or was withheld from) the trajectory for a reported outcome, plus the next move.
#[derive(Debug)]
pub struct Outcome {
    pub result: AdmittedResult,
    pub next: Step,
}

/// The model-visible face of one tool result: the admitted value at its label, or a sealed token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmittedResult {
    Admitted { content: String, label: Label },
    Sealed { token: String },
}

/// The lifecycle gate — one private state checked by every public entry point.
#[derive(Debug)]
enum SessionState {
    /// No active turn: only `admit_user_turn` (and `transcript`) are valid.
    Idle,
    /// A turn is active and quiescent: the next move is a completion into `mediate` (or `stop_turn`).
    ReadyForCompletion,
    /// Exactly one surfaced call is outstanding; only `report_outcome`/`abandon` with its handle.
    AwaitingOutcome { handle_id: u64 },
}

impl SessionState {
    fn name(&self) -> &'static str {
        match self {
            SessionState::Idle => "Idle",
            SessionState::ReadyForCompletion => "ReadyForCompletion",
            SessionState::AwaitingOutcome { .. } => "AwaitingOutcome",
        }
    }
}

/// A proposed call held in the round queue, with its malformed-arguments flag preserved (invalid
/// JSON is recorded as `{}` but sealed, never repaired into a call the model did not encode).
#[derive(Debug)]
struct Proposal {
    call: ProposedCall,
    malformed: bool,
}

/// A blocked call awaiting the model's remedy decision, keyed by an SDK-minted turn-unique handle
/// (the engine's `PlanId` is block-local and never exposed to the model).
#[derive(Debug)]
struct PendingBlock {
    handle: String,
    call: ResolvedCall,
    plan: PlanId,
}

/// One mediated trajectory: the embedded outer layer a harness drives.
pub struct AppaSession {
    config: Config,
    engine: Engine,
    store: SessionStore,
    tenant: TenantId,
    session: TrajectoryId,
    authorities: BTreeMap<AuthorityName, AuthorityBackend>,
    options: SdkOptions,
    state: SessionState,
    round: VecDeque<Proposal>,
    pending_blocks: Vec<PendingBlock>,
    remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    tools: Option<Vec<WireTool>>,
    next_remedy_handle: u32,
    next_handle_id: u64,
}

impl AppaSession {
    /// Open a session on a loaded policy. Fails closed on any policy feature the SDK v0 defers, so
    /// a policy never half-works: what loads is fully enforced.
    pub fn open(config: Config, options: SdkOptions) -> Result<AppaSession, OpenError> {
        let rc = config.registry_config();
        if !rc.sanitizers.is_empty() {
            return Err(OpenError::UnsupportedPolicy("[[sanitizer]] declarations".into()));
        }
        if !rc.casts.is_empty() {
            return Err(OpenError::UnsupportedPolicy("[[cast]] declarations".into()));
        }
        if config.child_return_sanitizer().is_some() {
            return Err(OpenError::UnsupportedPolicy("[child] return_sanitizer".into()));
        }
        for tool in &rc.tools {
            let name = tool.name.as_str();
            if name == EXECUTE_REMEDY_PLAN || name == SUBMIT_RESULT {
                return Err(OpenError::ReservedToolConflict(name.to_string()));
            }
            if tool.output_sanitizer.is_some() {
                return Err(OpenError::UnsupportedPolicy(format!("tool {name} output_sanitizer")));
            }
            if tool.delta.pending_cast_dim().is_some() {
                return Err(OpenError::UnsupportedPolicy(format!(
                    "tool {name} pending-cast (\"unknown\") delta"
                )));
            }
            if config.tool_impl(&tool.name).is_some() {
                return Err(OpenError::UnsupportedPolicy(format!(
                    "tool {name} implementation — SDK tools are host-executed"
                )));
            }
        }

        let engine = Engine::new(config.registry().clone());
        let authorities = assemble::authority_backends(&config);
        let store = SessionStore::new();
        let tenant = TenantId::new("appa-sdk");
        let session = store.create_session(tenant.clone());
        Ok(AppaSession {
            config,
            engine,
            store,
            tenant,
            session,
            authorities,
            options,
            state: SessionState::Idle,
            round: VecDeque::new(),
            pending_blocks: Vec::new(),
            remedy_attempts: BTreeMap::new(),
            tools: None,
            next_remedy_handle: 0,
            next_handle_id: 0,
        })
    }

    /// Bind the tool surface, once: the host's schemas (e.g. proxied from an MCP server) validated
    /// name-for-name against the registry, plus the reserved `execute_remedy_plan` schema. The host
    /// must advertise exactly the returned list on every inference request, for the whole session —
    /// the remedy tool must be present from the start, not appear after a block.
    pub fn bind_tools(&mut self, surface: Vec<WireTool>) -> Result<&[WireTool], ToolSurfaceError> {
        if self.tools.is_some() {
            return Err(ToolSurfaceError::AlreadyBound);
        }
        let mut seen = std::collections::BTreeSet::new();
        for tool in &surface {
            if !seen.insert(tool.function.name.clone()) {
                return Err(ToolSurfaceError::Duplicate(tool.function.name.clone()));
            }
            if self
                .engine
                .registry()
                .tool(&ToolName::new(tool.function.name.clone()))
                .is_none()
            {
                return Err(ToolSurfaceError::UnknownTool(tool.function.name.clone()));
            }
        }
        for contract in self.engine.registry().tools() {
            if !seen.contains(contract.name.as_str()) {
                return Err(ToolSurfaceError::MissingTool(contract.name.as_str().to_string()));
            }
        }
        let mut bound = surface;
        bound.push(remedy_tool_schema());
        self.tools = Some(bound);
        Ok(self.tools.as_deref().expect("just bound"))
    }

    /// The bound tool surface, if bound.
    pub fn tools(&self) -> Option<&[WireTool]> {
        self.tools.as_deref()
    }

    /// Admit one user turn: exactly one `ValueAdmitted` with user provenance at the policy's
    /// boundary label (no boundary fact — a turn is closed by its `TurnEnd`, not opened).
    pub fn admit_user_turn(&mut self, text: impl Into<String>) -> Result<(), MediateError> {
        self.require(&SessionState::Idle, "Idle")?;
        let value = LabeledValue::new(ValueBody::new(text.into()), self.config.boundary_label().clone());
        self.append(vec![Fact::ValueAdmitted {
            trajectory: self.session.clone(),
            value,
            provenance: Provenance::UserInput,
        }])?;
        self.state = SessionState::ReadyForCompletion;
        Ok(())
    }

    /// The model-visible conversation, rebuilt from the log: the policy-pinned preamble, then the
    /// trajectory's turns. Quiescent-only — while a surfaced call is outstanding the current round
    /// is unpaired and must not be rendered.
    pub fn transcript(&self) -> Result<Vec<WireMessage>, SessionBusy> {
        if let SessionState::AwaitingOutcome { .. } = self.state {
            return Err(SessionBusy {
                actual: self.state.name(),
                required: "Idle or ReadyForCompletion",
            });
        }
        let (log, _) = self
            .store
            .snapshot(&self.tenant, &self.session)
            .expect("the session owns its store");
        Ok(model_transcript(self.config.preamble(), &log, &self.session))
    }

    /// Mediate one model completion: record the assistant round, then advance — check the proposed
    /// calls serially, surfacing the first allowed one, feeding back every blocked one. A completion
    /// with no tool calls ends the turn.
    pub async fn mediate(&mut self, completion: Completion) -> Result<Step, MediateError> {
        self.require(&SessionState::ReadyForCompletion, "ReadyForCompletion")?;
        debug_assert!(self.round.is_empty(), "ReadyForCompletion implies an empty round queue");

        let proposals: Vec<Proposal> = completion.tool_calls.iter().map(proposal_of).collect();
        let calls: Vec<ProposedCall> = proposals.iter().map(|p| p.call.clone()).collect();
        self.append(vec![Fact::AssistantMessage {
            trajectory: self.session.clone(),
            content: completion.content.clone(),
            calls,
        }])?;

        if proposals.is_empty() {
            self.finish_turn()?;
            return Ok(Step::Final {
                text: completion.content.unwrap_or_default(),
            });
        }
        self.round = proposals.into();
        self.advance().await
    }

    /// Report the outcome of the outstanding surfaced call: close its dispatch, admit or seal the
    /// result, then advance the round. The returned [`Outcome::next`] is binding — the session's
    /// state already reflects it.
    pub async fn report_outcome(
        &mut self,
        handle: DispatchHandle,
        outcome: ToolOutcome,
    ) -> Result<Outcome, ReportError> {
        self.take_outstanding(&handle)?;

        let admission = match &outcome {
            ToolOutcome::Success {
                body: BodyDisposition::Available(body),
            } => ResultAdmission::SuccessRaw {
                body: ValueBody::new(body.clone()),
            },
            ToolOutcome::Success {
                body: BodyDisposition::RejectedTooLarge,
            } => ResultAdmission::SuccessNoValue,
            ToolOutcome::Failure => ResultAdmission::Failure,
            ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
        };
        let admitted = match self.admit_result(&handle.0.dispatch, &handle.0.call, admission)? {
            Admission::Admitted(value) => value,
            Admission::NotOpen => return Err(ReportError::DispatchNotOpen),
            // The engine refused the value (a policy the SDK should have rejected at open, or a
            // future admission rule): close success-with-no-value so the successful call's effects
            // stand and nothing is orphaned, and seal the response.
            Admission::Refused => {
                match self.admit_result(&handle.0.dispatch, &handle.0.call, ResultAdmission::SuccessNoValue)? {
                    Admission::Admitted(_) | Admission::NotOpen => {}
                    Admission::Refused => return Err(ReportError::DispatchIdentity),
                }
                None
            }
        };

        // Exactly one terminal response per proposed call: the admitted value speaks for itself
        // (the transcript reads its `ValueAdmitted`); everything else gets its sealed token.
        let result = match (&outcome, admitted) {
            (
                ToolOutcome::Success {
                    body: BodyDisposition::Available(_),
                },
                Some((content, label)),
            ) => AdmittedResult::Admitted { content, label },
            (
                ToolOutcome::Success {
                    body: BodyDisposition::Available(_),
                },
                None,
            ) => self.seal(&handle.0.response_call_id, SEALED_FAILED)?,
            (
                ToolOutcome::Success {
                    body: BodyDisposition::RejectedTooLarge,
                },
                _,
            ) => self.seal(&handle.0.response_call_id, SEALED_WITHHELD)?,
            (ToolOutcome::Failure, _) => self.seal(&handle.0.response_call_id, SEALED_FAILED)?,
            (ToolOutcome::Indeterminate, _) => self.seal(&handle.0.response_call_id, SEALED_INDETERMINATE)?,
        };

        self.state = SessionState::ReadyForCompletion;
        let next = self.advance().await.map_err(|e| match e {
            MediateError::Busy(busy) => ReportError::Busy(busy),
            MediateError::Store(store) => ReportError::Store(store),
        })?;
        Ok(Outcome { result, next })
    }

    /// Abandon the outstanding surfaced call: one batch closes its dispatch `Indeterminate`, seals
    /// it and every remaining proposal of the round, lands the fixed cancelled terminal and the
    /// `TurnEnd` — the turn is over, the session is `Idle` and reusable.
    pub fn abandon(&mut self, handle: DispatchHandle) -> Result<(), ReportError> {
        self.take_outstanding(&handle)?;
        let unanswered: Vec<ToolCallId> = std::iter::once(handle.0.response_call_id.clone())
            .chain(self.round.iter().map(|p| p.call.id.clone()))
            .collect();
        self.store.finalize(&self.tenant, &self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(&self.session);
            let mut terminal = Vec::new();
            // A close that no longer applies is skipped, never fatal: the terminal still lands.
            if let Ok(batch) = self.engine.admit_result(
                &views,
                &handle.0.dispatch,
                &handle.0.call,
                ResultAdmission::Indeterminate,
            ) {
                terminal = batch.facts;
            }
            for call_id in &unanswered {
                terminal.push(Fact::BlockFeedback {
                    trajectory: self.session.clone(),
                    call_id: call_id.clone(),
                    content: TURN_CANCELLED.to_string(),
                });
            }
            terminal.push(Fact::AssistantMessage {
                trajectory: self.session.clone(),
                content: Some(TURN_CANCELLED.to_string()),
                calls: Vec::new(),
            });
            terminal.push(turn_end(&self.session));
            Some(FactBatch::new(rev, terminal))
        })?;
        self.clear_turn();
        Ok(())
    }

    /// End the active turn without an outstanding call (an inference fault, a host abort): seal any
    /// queued proposals, land a fixed terminal with `reason`, and close the turn.
    pub fn stop_turn(&mut self, reason: &str) -> Result<(), MediateError> {
        self.require(&SessionState::ReadyForCompletion, "ReadyForCompletion")?;
        let mut facts: Vec<Fact> = self
            .round
            .iter()
            .map(|p| Fact::BlockFeedback {
                trajectory: self.session.clone(),
                call_id: p.call.id.clone(),
                content: reason.to_string(),
            })
            .collect();
        facts.push(Fact::AssistantMessage {
            trajectory: self.session.clone(),
            content: Some(reason.to_string()),
            calls: Vec::new(),
        });
        facts.push(turn_end(&self.session));
        self.append(facts)?;
        self.clear_turn();
        Ok(())
    }

    // --- the advance loop ----------------------------------------------------

    /// Advance the round: process queued proposals serially against the live projection until one
    /// surfaces for execution or the round drains. Feedback for blocked/malformed/unknown calls
    /// lands as facts and reaches the model through the next transcript.
    async fn advance(&mut self) -> Result<Step, MediateError> {
        while let Some(proposal) = self.round.pop_front() {
            if proposal.malformed {
                self.feedback(&proposal.call.id, MALFORMED_ARGUMENTS)?;
                continue;
            }
            match proposal.call.tool.as_str() {
                EXECUTE_REMEDY_PLAN => {
                    if let Some(step) = self.execute_remedy(&proposal.call).await? {
                        return Ok(step);
                    }
                }
                SUBMIT_RESULT => {
                    self.feedback(&proposal.call.id, "submit_result is available only to a child session")?;
                }
                _ => {
                    if let Some(step) = self.mediate_ordinary(&proposal.call)? {
                        return Ok(step);
                    }
                }
            }
        }
        Ok(Step::Continue)
    }

    /// Check one ordinary call. `Some(step)` surfaces it; `None` means its terminal feedback landed
    /// and the round continues.
    fn mediate_ordinary(&mut self, proposed: &ProposedCall) -> Result<Option<Step>, MediateError> {
        let call = ResolvedCall::new(proposed.tool.clone(), proposed.arguments.clone(), Vec::new());
        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        match self.engine.check(&views, &call) {
            Err(_) => {
                self.feedback(&proposed.id, "no such tool is registered")?;
                Ok(None)
            }
            // Casts are refused at open, so an Unresolved label has no resolver — fail closed.
            Ok(CheckOutcome::Unresolved(_)) => {
                self.feedback(
                    &proposed.id,
                    "the call has an unresolved label that no cast could resolve",
                )?;
                Ok(None)
            }
            Ok(CheckOutcome::Block(raw)) => {
                let planned = self
                    .engine
                    .plan(&views, &call, &raw)
                    .expect("checked tool is registered");
                let gaps = raw.requirement_gaps.len();
                let curative: Vec<String> = planned
                    .recommendations
                    .iter()
                    .filter_map(|r| match r {
                        appa_engine::plan::Recommendation::Redispatch { tool, .. } => Some(tool.as_str().to_string()),
                        appa_engine::plan::Recommendation::Fork { .. } => None,
                    })
                    .collect();
                let feedback = match planned.plans.first() {
                    Some(plan) => {
                        let handle = format!("remedy-{}", self.next_remedy_handle);
                        self.next_remedy_handle += 1;
                        self.pending_blocks.push(PendingBlock {
                            handle: handle.clone(),
                            call,
                            plan: plan.id,
                        });
                        format!(
                            "blocked by policy ({gaps} requirement gap(s)); call execute_remedy_plan with plan_id \"{handle}\" to authorize"
                        )
                    }
                    None if !curative.is_empty() => format!(
                        "blocked by policy; run {} first, then re-propose this call",
                        curative.join(" or ")
                    ),
                    None => "blocked by policy; no remedy is available for this call".to_string(),
                };
                self.feedback(&proposed.id, &feedback)?;
                Ok(None)
            }
            Ok(CheckOutcome::Allow) => {
                drop(projection);
                match self.open_dispatch(&call)? {
                    Some(dispatch) => Ok(Some(self.surface(dispatch, call, proposed.id.clone()))),
                    None => {
                        self.feedback(
                            &proposed.id,
                            "the call could not be dispatched (the policy state changed)",
                        )?;
                        Ok(None)
                    }
                }
            }
        }
    }

    /// The reserved `execute_remedy_plan(plan_id)`: gather the pending block's rulings from its
    /// authorities and land the atomic authorize+dispatch batch; the authorized call surfaces from
    /// the same mediation, its terminal response answering *this* proposal.
    async fn execute_remedy(&mut self, proposed: &ProposedCall) -> Result<Option<Step>, MediateError> {
        let Some(handle) = proposed.arguments.get("plan_id").and_then(|v| v.as_str()) else {
            self.feedback(&proposed.id, "execute_remedy_plan requires a string plan_id")?;
            return Ok(None);
        };
        let Some(index) = self.pending_blocks.iter().position(|p| p.handle == handle) else {
            self.feedback(&proposed.id, "no pending blocked call offers that plan_id")?;
            return Ok(None);
        };
        let block = self.pending_blocks.remove(index);

        let attempts = self.remedy_attempts.entry(block.call.digest()).or_insert(0);
        *attempts += 1;
        if *attempts > self.options.max_remedy_attempts_per_gap {
            self.feedback(&proposed.id, "the remedy attempt limit for this call was reached")?;
            return Ok(None);
        }

        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        let required = self
            .engine
            .required_rulings(&views, &block.call)
            .expect("pending call is registered");
        // Rulings bind to the exact dispatch the plan will open: predicted from the same views the
        // plan executes against, and verified against the batch below.
        let dispatch = DispatchId::new(
            self.session.clone(),
            block.call.digest(),
            views.dispatch_count(&block.call.digest()),
        );

        let mut rulings = Vec::new();
        for req in &required {
            let Some(backend) = self.authorities.get(&req.authority) else {
                self.feedback(&proposed.id, "an authority for this plan is not configured")?;
                return Ok(None);
            };
            let request = AuthorityRequest::new(req.authority.clone(), &block.call, req.covers.clone());
            // Awaited outside any store lock; a slow or unreachable authority fails closed.
            let answer = tokio::time::timeout(self.options.per_external_timeout, backend.rule(&request))
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
                    self.feedback(&proposed.id, "the authority declined to authorize this call")?;
                    return Ok(None);
                }
            }
        }

        let batch = match self
            .engine
            .execute_plan(&views, block.plan, &block.call, &rulings, Sink::Tool)
        {
            Ok(batch) => batch,
            Err(_) => {
                self.feedback(
                    &proposed.id,
                    "the remedy plan could not be executed on the current state",
                )?;
                return Ok(None);
            }
        };
        let opened = batch
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("an executed plan opens its dispatch");
        drop(projection);
        match self.store.conditional_append(&self.tenant, &self.session, batch) {
            Ok(_) => {}
            Err(StoreError::Stale { .. }) => {
                self.pending_blocks.push(block);
                self.feedback(&proposed.id, "the state changed; re-propose the call and remedy")?;
                return Ok(None);
            }
            Err(e) => return Err(MediateError::Store(e)),
        }
        assert_eq!(
            opened, dispatch,
            "the executed plan opens the dispatch its rulings name"
        );
        Ok(Some(self.surface(dispatch, block.call, proposed.id.clone())))
    }

    // --- appends and small helpers -------------------------------------------

    /// Mint the handle for an opened dispatch and enter `AwaitingOutcome`.
    fn surface(&mut self, dispatch: DispatchId, call: ResolvedCall, response_call_id: ToolCallId) -> Step {
        let id = self.next_handle_id;
        self.next_handle_id += 1;
        let rendered = RenderedCall::from_call(&call);
        self.state = SessionState::AwaitingOutcome { handle_id: id };
        Step::Execute {
            handle: DispatchHandle(Box::new(HandleInner {
                id,
                dispatch,
                call,
                response_call_id,
            })),
            call: rendered,
        }
    }

    /// Open the dispatch for a clean-allow call through the store's serialized finalization: the
    /// engine re-checks and decides under the family lock at the live revision, and the returned id
    /// is exactly the dispatch those facts open.
    fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, StoreError> {
        let mut dispatch = None;
        self.store.finalize(&self.tenant, &self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(&self.session);
            let batch = self.engine.open_dispatch(&views, call).ok()?;
            dispatch = Some(DispatchId::new(
                self.session.clone(),
                call.digest(),
                views.dispatch_count(&call.digest()),
            ));
            Some(batch)
        })?;
        Ok(dispatch)
    }

    /// Close the dispatch and admit (or refuse) its result under the store's serialized
    /// finalization, returning the admitted value's body and label when one landed.
    fn admit_result(
        &self,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<Admission, ReportError> {
        let mut slot = Some(admission);
        let mut verdict = Admission::NotOpen;
        let mut identity_breach = false;
        self.store.finalize(&self.tenant, &self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(&self.session);
            let admission = slot.take()?;
            match self.engine.admit_result(&views, dispatch, call, admission) {
                Ok(batch) => {
                    let value = batch.facts.iter().find_map(|fact| match fact {
                        Fact::ValueAdmitted {
                            value,
                            provenance: Provenance::ToolResult { .. },
                            ..
                        } => Some((value.body.as_str().to_string(), value.label.clone())),
                        _ => None,
                    });
                    verdict = Admission::Admitted(value);
                    Some(batch)
                }
                Err(AdmitError::NotOpen) => None,
                Err(AdmitError::UnknownTool(_) | AdmitError::DigestMismatch | AdmitError::ForeignDispatch) => {
                    identity_breach = true;
                    None
                }
                // Value-policy refusals, exhaustively — a future identity-class error must be
                // classified deliberately, not absorbed by a wildcard.
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
                    verdict = Admission::Refused;
                    None
                }
            }
        })?;
        if identity_breach {
            return Err(ReportError::DispatchIdentity);
        }
        Ok(verdict)
    }

    /// Append the sealed terminal response for a call and return it as the reported result.
    fn seal(&self, call_id: &ToolCallId, token: &str) -> Result<AdmittedResult, ReportError> {
        self.feedback(call_id, token).map_err(store_to_report)?;
        Ok(AdmittedResult::Sealed {
            token: token.to_string(),
        })
    }

    fn feedback(&self, call_id: &ToolCallId, content: &str) -> Result<(), MediateError> {
        self.append(vec![Fact::BlockFeedback {
            trajectory: self.session.clone(),
            call_id: call_id.clone(),
            content: content.to_string(),
        }])
    }

    fn finish_turn(&mut self) -> Result<(), MediateError> {
        self.append(vec![turn_end(&self.session)])?;
        self.clear_turn();
        Ok(())
    }

    fn clear_turn(&mut self) {
        self.round.clear();
        self.pending_blocks.clear();
        self.remedy_attempts.clear();
        self.state = SessionState::Idle;
    }

    /// Append revision-independent facts through the store's serialized finalization.
    fn append(&self, facts: Vec<Fact>) -> Result<(), MediateError> {
        self.store
            .finalize(&self.tenant, &self.session, |_, rev| Some(FactBatch::new(rev, facts)))?;
        Ok(())
    }

    fn require(&self, expected: &SessionState, required: &'static str) -> Result<(), SessionBusy> {
        if std::mem::discriminant(&self.state) == std::mem::discriminant(expected) {
            Ok(())
        } else {
            Err(SessionBusy {
                actual: self.state.name(),
                required,
            })
        }
    }

    fn take_outstanding(&self, handle: &DispatchHandle) -> Result<(), ReportError> {
        match &self.state {
            SessionState::AwaitingOutcome { handle_id } if *handle_id == handle.0.id => Ok(()),
            SessionState::AwaitingOutcome { .. } => Err(ReportError::UnknownHandle),
            other => Err(ReportError::Busy(SessionBusy {
                actual: other.name(),
                required: "AwaitingOutcome",
            })),
        }
    }
}

/// How a close landed engine-side. `Admitted(None)` is a value-less close (sealed/failed/oversized).
enum Admission {
    Admitted(Option<(String, Label)>),
    NotOpen,
    Refused,
}

fn store_to_report(e: MediateError) -> ReportError {
    match e {
        MediateError::Busy(busy) => ReportError::Busy(busy),
        MediateError::Store(store) => ReportError::Store(store),
    }
}

fn turn_end(session: &TrajectoryId) -> Fact {
    Fact::Boundary {
        trajectory: session.clone(),
        kind: BoundaryKind::TurnEnd,
    }
}

/// Parse one wire tool call, preserving the malformed-arguments distinction: an empty argument
/// string is the no-argument call `{}`; non-empty invalid JSON is recorded as `{}` but flagged, so
/// it is sealed rather than repaired into a call the model did not encode.
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

/// The reserved remedy tool's schema, advertised from the start of every session.
fn remedy_tool_schema() -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireToolSchema {
            name: EXECUTE_REMEDY_PLAN.to_string(),
            description: Some(
                "Execute a remedy plan offered after a blocked tool call. Pass the plan_id quoted in the block feedback.".to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": { "plan_id": { "type": "string" } },
                "required": ["plan_id"]
            })),
        },
    }
}
