//! Shared engine/store operations behind the session facade.

use std::collections::BTreeMap;

use appa_engine::admit::{AdmitError, ResultAdmission};
use appa_engine::check::{CheckOutcome, Narrowing};
use appa_engine::engine::Engine;
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ReturnPolicy};
use appa_engine::label::Label;
use appa_engine::names::AuthorityName;
use appa_engine::plan::PlanId;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, LabeledValue, Provenance, ResolvedCall, ToolName, TrajectoryId, ValueBody,
};

use appa_runtime::config::Config;
use appa_runtime::external::{AuthorityAnswer, AuthorityBackend, AuthorityRequest};
use appa_runtime::runtime::{EXECUTE_REMEDY_PLAN, SUBMIT_RESULT};
use appa_runtime::store::{SessionStore, StoreError, TenantId};
use appa_runtime::tool::{BodyDisposition, ToolOutcome};
use appa_runtime::wire::{WireTool, WireToolSchema};

use crate::assemble;
use crate::types::{OpenError, SdkOptions, ToolSurfaceError};

// The fixed model-visible terminals, byte-identical to the runtime turn-drive's (RP3).
pub(crate) const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
pub(crate) const SEALED_FAILED: &str = "[tool call failed]";
pub(crate) const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";

/// A blocked call awaiting the model's remedy decision, keyed by an SDK-minted turn-unique handle
/// (the engine's `PlanId` is block-local and never exposed to the model).
#[derive(Debug)]
pub(crate) struct PendingBlock {
    pub(crate) handle: String,
    pub(crate) call: ResolvedCall,
    pub(crate) plan: PlanId,
}

/// The shared state and engine/store operations both facades hold. A facade embeds one `Core` and
/// layers its own orchestration state (a round queue, a lifecycle gate) on top.
pub(crate) struct Core {
    pub(crate) config: Config,
    pub(crate) engine: Engine,
    pub(crate) store: SessionStore,
    pub(crate) tenant: TenantId,
    pub(crate) session: TrajectoryId,
    pub(crate) authorities: BTreeMap<AuthorityName, AuthorityBackend>,
    pub(crate) options: SdkOptions,
    pub(crate) pending_blocks: Vec<PendingBlock>,
    pub(crate) remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    pub(crate) tools: Option<Vec<WireTool>>,
    next_remedy_handle: u32,
    next_handle_id: u64,
}

/// How a check resolved for the caller: a clean-allow dispatch to surface, or model-visible feedback
/// (a block with its remedy offer, an unresolved label, an unknown tool, or a lost race). The facade
/// decides how the feedback reaches the model — a `BlockFeedback` fact (turn) or a hook skip (call).
pub(crate) enum Checked {
    Allow(DispatchId),
    Feedback(String),
}

/// How a remedy resolved: the authorized dispatch and its rendered call to execute now, or
/// model-visible feedback.
pub(crate) enum Remedied {
    Authorized { dispatch: DispatchId, call: ResolvedCall },
    Feedback(String),
}

pub(crate) enum Admission {
    Admitted(Option<(String, Label)>),
    NotOpen,
    Refused,
}

pub(crate) struct DispatchIdentityBreach;

impl Core {
    /// Open on a loaded policy, rejecting every feature the SDK v0 defers so a policy never
    /// half-works. Shared verbatim by both facades.
    pub(crate) fn open(config: Config, options: SdkOptions) -> Result<Core, OpenError> {
        validate_policy(&config)?;
        let engine = Engine::new(config.registry().clone());
        let authorities = assemble::authority_backends(&config);
        let store = SessionStore::new();
        let tenant = TenantId::new("appa-sdk");
        let session = store.create_session(tenant.clone());
        Ok(Core {
            config,
            engine,
            store,
            tenant,
            session,
            authorities,
            options,
            pending_blocks: Vec::new(),
            remedy_attempts: BTreeMap::new(),
            tools: None,
            next_remedy_handle: 0,
            next_handle_id: 0,
        })
    }

    /// Validate and bind the tool surface once (both facades bind identically): every advertised
    /// name must be a registered tool and vice versa, no duplicates, plus the reserved
    /// `execute_remedy_plan` schema appended.
    pub(crate) fn bind_tools(&mut self, surface: Vec<WireTool>) -> Result<&[WireTool], ToolSurfaceError> {
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

    pub(crate) fn next_handle_id(&mut self) -> u64 {
        let id = self.next_handle_id;
        self.next_handle_id += 1;
        id
    }

    /// Admit one user turn: exactly one `ValueAdmitted` with user provenance at the policy's
    /// boundary label (no boundary fact — a turn is closed by its `TurnEnd`, not opened).
    pub(crate) fn admit_user_turn(&self, text: String) -> Result<(), StoreError> {
        let value = LabeledValue::new(ValueBody::new(text), self.config.boundary_label().clone());
        self.append(vec![Fact::ValueAdmitted {
            trajectory: self.session.clone(),
            value,
            provenance: Provenance::UserInput,
        }])
    }

    /// Check one ordinary tool call against the live projection. On a clean allow the dispatch is
    /// opened and its id returned; on a block the remedy handle is minted and pushed; every other
    /// case yields model-visible feedback. Never authors a `BlockFeedback` fact — the facade does.
    pub(crate) fn check_ordinary(&mut self, call: ResolvedCall) -> Result<Checked, StoreError> {
        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        match self.engine.check(&views, &call) {
            Err(_) => Ok(Checked::Feedback("no such tool is registered".to_string())),
            // Casts are refused at open, so an Unresolved label has no resolver — fail closed.
            Ok(CheckOutcome::Unresolved(_)) => Ok(Checked::Feedback(
                "the call has an unresolved label that no cast could resolve".to_string(),
            )),
            Ok(CheckOutcome::Block(raw)) => {
                let planned = self
                    .engine
                    .plan(&views, &call, &raw)
                    .expect("checked tool is registered");
                let gaps = raw.requirement_gaps.len();
                let narrowed = raw.narrowing.as_ref().map(narrowed_dims);
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
                        let via = authorize_via(plan);
                        let handle = format!("remedy-{}", self.next_remedy_handle);
                        self.next_remedy_handle += 1;
                        self.pending_blocks.push(PendingBlock {
                            handle: handle.clone(),
                            call,
                            plan: plan.id,
                        });
                        match (gaps, narrowed.as_deref()) {
                            (0, Some(dims)) => format!(
                                "narrowing: this call restricts the trajectory's {dims} label; call execute_remedy_plan with plan_id \"{handle}\" to accept and proceed"
                            ),
                            (n, Some(dims)) => format!(
                                "blocked by policy ({n} requirement gap(s), and narrows {dims}); call execute_remedy_plan with plan_id \"{handle}\" to authorize{via}"
                            ),
                            (n, None) => format!(
                                "blocked by policy ({n} requirement gap(s)); call execute_remedy_plan with plan_id \"{handle}\" to authorize{via}"
                            ),
                        }
                    }
                    None if !curative.is_empty() => format!(
                        "blocked by policy; run {} first, then re-propose this call",
                        curative.join(" or ")
                    ),
                    None => "blocked by policy; no remedy is available for this call".to_string(),
                };
                Ok(Checked::Feedback(feedback))
            }
            Ok(CheckOutcome::Allow) => {
                drop(projection);
                match self.open_dispatch(&call)? {
                    Some(dispatch) => Ok(Checked::Allow(dispatch)),
                    None => Ok(Checked::Feedback(
                        "the call could not be dispatched (the policy state changed)".to_string(),
                    )),
                }
            }
        }
    }

    /// Resolve the reserved `execute_remedy_plan(plan_id)`: gather the pending block's rulings from
    /// its authorities and land the atomic authorize+dispatch batch. On success the authorized
    /// dispatch is returned for the caller to execute and report; every failure yields feedback.
    pub(crate) async fn resolve_remedy(&mut self, plan_id: Option<&str>) -> Result<Remedied, StoreError> {
        let Some(plan_id) = plan_id else {
            return Ok(Remedied::Feedback(
                "execute_remedy_plan requires a string plan_id".to_string(),
            ));
        };
        let Some(index) = self.pending_blocks.iter().position(|p| p.handle == plan_id) else {
            return Ok(Remedied::Feedback(
                "no pending blocked call offers that plan_id".to_string(),
            ));
        };
        let block = self.pending_blocks.remove(index);

        let attempts = self.remedy_attempts.entry(block.call.digest()).or_insert(0);
        *attempts += 1;
        if *attempts > self.options.max_remedy_attempts_per_gap {
            return Ok(Remedied::Feedback(
                "the remedy attempt limit for this call was reached".to_string(),
            ));
        }

        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        let required = self
            .engine
            .required_rulings(&views, &block.call)
            .expect("pending call is registered");
        let dispatch = DispatchId::new(
            self.session.clone(),
            block.call.digest(),
            views.dispatch_count(&block.call.digest()),
        );

        let mut rulings = Vec::new();
        for req in &required {
            let Some(backend) = self.authorities.get(&req.authority) else {
                return Ok(Remedied::Feedback(
                    "an authority for this plan is not configured".to_string(),
                ));
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
                    return Ok(Remedied::Feedback(
                        "the authority declined to authorize this call".to_string(),
                    ));
                }
            }
        }

        let batch = match self
            .engine
            .execute_plan(&views, block.plan, &block.call, &rulings, Sink::Tool)
        {
            Ok(batch) => batch,
            Err(_) => {
                return Ok(Remedied::Feedback(
                    "the remedy plan could not be executed on the current state".to_string(),
                ));
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
                return Ok(Remedied::Feedback(
                    "the state changed; re-propose the call and remedy".to_string(),
                ));
            }
            Err(e) => return Err(e),
        }
        assert_eq!(
            opened, dispatch,
            "the executed plan opens the dispatch its rulings name"
        );
        Ok(Remedied::Authorized {
            dispatch,
            call: block.call,
        })
    }

    /// Open the dispatch for a clean-allow call through the store's serialized finalization: the
    /// engine re-checks and decides under the family lock at the live revision, and the returned id
    /// is exactly the dispatch those facts open.
    pub(crate) fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, StoreError> {
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
    pub(crate) fn admit_result(
        &self,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<Result<Admission, DispatchIdentityBreach>, StoreError> {
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
            return Ok(Err(DispatchIdentityBreach));
        }
        Ok(Ok(verdict))
    }

    /// Close the active turn: append the `TurnEnd` boundary and clear pending remedies (spec: a
    /// boundary bounds pending-plan lifetime).
    pub(crate) fn end_turn(&mut self) -> Result<(), StoreError> {
        self.append(vec![turn_end(&self.session)])?;
        self.pending_blocks.clear();
        self.remedy_attempts.clear();
        Ok(())
    }

    pub(crate) fn append(&self, facts: Vec<Fact>) -> Result<(), StoreError> {
        self.store
            .finalize(&self.tenant, &self.session, |_, rev| Some(FactBatch::new(rev, facts)))?;
        Ok(())
    }
}

pub(crate) fn outcome_to_admission(outcome: &ToolOutcome) -> ResultAdmission {
    match outcome {
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
    }
}

fn narrowed_dims(narrowing: &Narrowing) -> String {
    let mut dims = Vec::new();
    if narrowing.from.trust != narrowing.to.trust {
        dims.push("trust");
    }
    if narrowing.from.audience != narrowing.to.audience {
        dims.push("audience");
    }
    if dims.is_empty() {
        "label".to_string()
    } else {
        dims.join(" and ")
    }
}

fn authorize_via(plan: &appa_engine::plan::RemedyPlan) -> String {
    let authorities: Vec<&str> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            appa_engine::plan::RemedyStep::Authorize(name) => Some(name.as_str()),
            appa_engine::plan::RemedyStep::Accept => None,
        })
        .collect();
    if authorities.is_empty() {
        String::new()
    } else {
        format!(" via {}", authorities.join(", "))
    }
}

pub(crate) fn sealed_token(outcome: &ToolOutcome, admitted: bool) -> Option<&'static str> {
    match outcome {
        ToolOutcome::Success {
            body: BodyDisposition::Available(_),
        } => (!admitted).then_some(SEALED_FAILED),
        ToolOutcome::Success {
            body: BodyDisposition::RejectedTooLarge,
        } => Some(SEALED_WITHHELD),
        ToolOutcome::Failure => Some(SEALED_FAILED),
        ToolOutcome::Indeterminate => Some(SEALED_INDETERMINATE),
    }
}

fn validate_policy(config: &Config) -> Result<(), OpenError> {
    let rc = config.registry_config();
    if !rc.sanitizers.is_empty() {
        return Err(OpenError::UnsupportedPolicy("[[sanitizer]] declarations".into()));
    }
    if !rc.casts.is_empty() {
        return Err(OpenError::UnsupportedPolicy("[[cast]] declarations".into()));
    }
    if config.child_return_policy() != ReturnPolicy::Raw {
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
        if tool.pending_cast_dim().is_some() {
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
    Ok(())
}

pub(crate) fn turn_end(session: &TrajectoryId) -> Fact {
    Fact::Boundary {
        trajectory: session.clone(),
        kind: BoundaryKind::TurnEnd,
    }
}

pub(crate) fn remedy_tool_schema() -> WireTool {
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
