//! Shared engine/store operations behind the session facade.

use std::collections::BTreeMap;

use appa_engine::admit::{AdmitError, ResultAdmission};
use appa_engine::check::CheckOutcome;
use appa_engine::contract::{
    AudienceDelta, AudienceRequirement, DynamicAudienceBinding, PinnedDynamicResolution, RecipientSpec, ToolContract,
};
use appa_engine::engine::{Engine, EngineError};
use appa_engine::execute::Ruling;
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ReturnPolicy};
use appa_engine::label::Label;
use appa_engine::names::{AuthorityName, DynamicResolverName};
use appa_engine::params::ArgumentError;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, LabeledValue, Provenance, ResolvedCall, ToolName, TrajectoryId, ValueBody,
};

use crate::assemble;
use crate::config::Config;
use crate::external::{AuthorityAnswer, AuthorityBackend, AuthorityRequest, DynamicResolverBackend};
use crate::store::{SessionStore, StoreError, TenantId};
use crate::tool::{BodyDisposition, EXECUTE_REMEDY_PLAN, FORK, SUBMIT_RESULT, ToolOutcome};
use crate::types::{OpenError, SdkOptions, ToolSurfaceError};
use crate::wire::{WireTool, WireToolSchema};

// The fixed model-visible terminals, byte-identical to the runtime turn-drive's (RP3).
pub(crate) const SEALED_WITHHELD: &str = "[tool result withheld: exceeds the size the policy admits]";
pub(crate) const SEALED_FAILED: &str = "[tool call failed]";
pub(crate) const SEALED_UNAVAILABLE: &str = "[tool result unavailable]";
pub(crate) const SEALED_INDETERMINATE: &str = "[tool call outcome unknown — it may or may not have run]";

/// A blocked call's cohort: every offered plan for one blocked proposal, each keyed by an
/// SDK-minted turn-unique handle (the engine's `PlanId` is block-local and never exposed to the
/// model). Mirrors the runtime: a success consumes the whole cohort, a consult that returns no
/// answer consumes nothing, a denial every offer naming that authority for this rendered call,
/// and an acceptance-carrying plan is informed — executable only in a round after
/// `offered_round` (the framework signals rounds through `begin_round`).
#[derive(Debug)]
pub(crate) struct PendingBlock {
    pub(crate) call: ResolvedCall,
    pub(crate) offers: Vec<(String, appa_engine::plan::ExecutableRemedyPlan)>,
    pub(crate) offered_round: u32,
}

/// The one refusal an uninformed acceptance gets, in the SDK as in the runtime — the wording must
/// not drift between deployments.
pub(crate) fn uninformed_acceptance_feedback(handle: &str) -> String {
    format!(
        "this acceptance predates the offer it names; read the offer, then call execute_remedy_plan with plan_id \"{handle}\" in your next response"
    )
}

pub(crate) struct Core {
    pub(crate) config: Config,
    pub(crate) store: SessionStore,
    pub(crate) tenant: TenantId,
    pub(crate) session: TrajectoryId,
    pub(crate) authorities: BTreeMap<AuthorityName, AuthorityBackend>,
    pub(crate) dynamic_resolvers: BTreeMap<DynamicResolverName, DynamicResolverBackend>,
    pub(crate) options: SdkOptions,
    pub(crate) pending_blocks: Vec<PendingBlock>,
    pub(crate) remedy_attempts: BTreeMap<CanonicalDigest, u32>,
    pub(crate) tools: Option<Vec<WireTool>>,
    /// The current inference round, advanced by the facade at each turn begin and each
    /// framework-signalled model completion. Offers stamp it; informed acceptance compares it.
    pub(crate) round: u32,
    next_remedy_handle: u32,
    next_handle_id: u64,
}

/// How a check resolved for the caller: a clean-allow dispatch to surface, or model-visible feedback
/// (a block with its remedy offer and any unestablished values named, an unknown tool, or a lost
/// race). The facade decides how the feedback reaches the model — a `BlockFeedback` fact (turn) or
/// a hook skip (call).
pub(crate) enum Checked {
    Allow(DispatchId),
    Feedback(String),
}

/// How a remedy resolved: the authorized dispatch and its rendered call to execute now, a consult
/// that returned no answer (the offer stands, `RMD-6`), or other model-visible feedback.
pub(crate) enum Remedied {
    Authorized { dispatch: DispatchId, call: ResolvedCall },
    NoAnswer(String),
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
    /// half-works.
    pub(crate) fn open(config: Config, options: SdkOptions) -> Result<Core, OpenError> {
        validate_policy(&config)?;
        let authorities = assemble::authority_backends(&config);
        let dynamic_resolvers = assemble::dynamic_resolver_backends(&config);
        let store = SessionStore::new();
        let tenant = TenantId::new("appa-sdk");
        let session = store.create_session(tenant.clone());
        Ok(Core {
            config,
            store,
            tenant,
            session,
            authorities,
            dynamic_resolvers,
            options,
            pending_blocks: Vec::new(),
            remedy_attempts: BTreeMap::new(),
            tools: None,
            round: 0,
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
                .config
                .engine()
                .registry()
                .tool(&ToolName::new(tool.function.name.clone()))
                .is_none()
            {
                return Err(ToolSurfaceError::UnknownTool(tool.function.name.clone()));
            }
        }
        for contract in self.config.engine().registry().tools() {
            if !seen.contains(contract.name.as_str()) {
                return Err(ToolSurfaceError::MissingTool(contract.name.as_str().to_string()));
            }
        }
        let mut bound = surface;
        for tool in &mut bound {
            let contract = self
                .config
                .engine()
                .registry()
                .tool(&ToolName::new(tool.function.name.clone()))
                .expect("the surface names only registered tools");
            tool.function.parameters = Some(contract.parameters.normalized());
        }
        bound.push(remedy_tool_schema());
        self.tools = Some(bound);
        Ok(self.tools.as_deref().expect("just bound"))
    }

    pub(crate) fn next_handle_id(&mut self) -> u64 {
        let id = self.next_handle_id;
        self.next_handle_id += 1;
        id
    }

    /// Resolve every dynamic audience binding once for this proposed call. The returned call owns
    /// the pinned answers consumed by checks, remedies, dispatch, and admission. A
    /// missing argument, missing backend, timeout, or malformed answer pins no audience and leaves
    /// the engine's fail-closed dynamic gap or Unknown output standing.
    pub(crate) async fn resolve_dynamic_call(&self, call: ResolvedCall) -> ResolvedCall {
        let Some(contract) = self.config.engine().registry().tool(call.tool()) else {
            return call;
        };
        let mut resolutions = Vec::new();
        for binding in dynamic_bindings(contract) {
            let value = call
                .arguments()
                .get(&binding.argument)
                .and_then(serde_json::Value::as_str);
            let audience = if let (Some(value), Some(backend)) = (value, self.dynamic_resolvers.get(&binding.resolver))
            {
                tokio::time::timeout(
                    self.options.per_external_timeout,
                    backend.resolve(&binding.resolver, call.tool(), &binding.argument, value),
                )
                .await
                .ok()
                .flatten()
            } else {
                None
            };
            resolutions.push(PinnedDynamicResolution::from_answer(binding, audience));
        }
        call.with_dynamic_resolutions(resolutions)
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

    fn surface(&self) -> Result<crate::feedback::FeedbackSurface, StoreError> {
        Ok(if self.store.parent_of(&self.tenant, &self.session)?.is_some() {
            crate::feedback::FeedbackSurface::Child
        } else {
            crate::feedback::FeedbackSurface::Root { can_fork: false }
        })
    }

    /// Check one ordinary tool call against the live projection. On a clean allow the dispatch is
    /// opened and its id returned; on a block the remedy handle is minted and pushed; every other
    /// case yields model-visible feedback. Never authors a `BlockFeedback` fact — the facade does.
    pub(crate) fn check_ordinary(&mut self, call: ResolvedCall) -> Result<Checked, StoreError> {
        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        match self.config.engine().check(&views, &call) {
            Err(_) => Ok(Checked::Feedback("no such tool is registered".to_string())),
            Ok(CheckOutcome::Block(raw)) => {
                let planned = self
                    .config
                    .engine()
                    .plan(&views, &call, &raw)
                    .expect("checked tool is registered");
                let surface = self.surface()?;
                let has_offers = planned.plans.iter().any(|plan| plan.executable().is_some());
                let feedback = if !has_offers {
                    crate::feedback::block_feedback(
                        self.config.engine().registry(),
                        &raw,
                        &planned,
                        &[],
                        surface,
                        &views,
                    )
                } else {
                    let attempts = self.remedy_attempts.entry(call.digest()).or_insert(0);
                    *attempts += 1;
                    if *attempts > self.options.max_blocked_proposals_per_call {
                        return Ok(Checked::Feedback(
                            "the remedy attempt limit for this call was reached".to_string(),
                        ));
                    }
                    let offers: Vec<(String, appa_engine::plan::ExecutableRemedyPlan)> = planned
                        .plans
                        .iter()
                        .filter_map(appa_engine::plan::RemedyPlan::executable)
                        .map(|plan| {
                            let handle = format!("remedy-{}", self.next_remedy_handle);
                            self.next_remedy_handle += 1;
                            (handle, plan.clone())
                        })
                        .collect();
                    let feedback = crate::feedback::block_feedback(
                        self.config.engine().registry(),
                        &raw,
                        &planned,
                        &offers,
                        surface,
                        &views,
                    );
                    self.pending_blocks.push(PendingBlock {
                        call,
                        offers,
                        offered_round: self.round,
                    });
                    feedback
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
        let Some(cohort_index) = self
            .pending_blocks
            .iter()
            .position(|p| p.offers.iter().any(|(h, _)| h == plan_id))
        else {
            return Ok(Remedied::Feedback(
                "no pending blocked call offers that plan_id — an offer belongs to the session whose call was blocked and does not cross a fork, so a plan named in inherited history is not yours to execute; propose the call this branch needs and accept the plan you are then offered".to_string(),
            ));
        };
        let call = self.pending_blocks[cohort_index].call.clone();
        let chosen = self.pending_blocks[cohort_index]
            .offers
            .iter()
            .find(|(h, _)| h == plan_id)
            .map(|(_, plan)| plan.clone())
            .expect("the cohort was found by this handle");

        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        let outcome = self.config.engine().check(&views, &call);
        if let Ok(CheckOutcome::Block(raw)) = &outcome
            && !raw.unestablished.is_empty()
        {
            return Ok(Remedied::Feedback(crate::feedback::unestablished_gate_feedback(
                &raw.unestablished,
                &views,
            )));
        }
        let accepts_narrowing = chosen
            .steps
            .iter()
            .any(|step| matches!(step, appa_engine::plan::RemedyStep::Accept(_)));
        if accepts_narrowing && self.pending_blocks[cohort_index].offered_round == self.round {
            return Ok(Remedied::Feedback(uninformed_acceptance_feedback(plan_id)));
        }

        let still_offered = match &outcome {
            Ok(CheckOutcome::Block(raw)) => self
                .config
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
            self.pending_blocks.remove(cohort_index);
            return Ok(Remedied::Feedback(
                "the state changed and this offer no longer applies; re-propose the call".to_string(),
            ));
        }
        let dispatch = DispatchId::new(
            self.session.clone(),
            call.digest(),
            views.dispatch_count(&call.digest()),
        );

        let mut rulings = Vec::new();
        for req in &chosen.required {
            let Some(backend) = self.authorities.get(&req.authority) else {
                return Ok(Remedied::Feedback(
                    "an authority for this plan is not configured".to_string(),
                ));
            };
            let request = AuthorityRequest::new(req.authority.clone(), &call, req.covers.clone(), &views);
            // Awaited outside any store lock; a slow or unreachable authority fails closed.
            let answer = tokio::time::timeout(self.options.per_external_timeout, backend.rule(&request))
                .await
                .unwrap_or(AuthorityAnswer::Abstain);
            match answer {
                // The ruling records the review context put to the authority, verbatim.
                AuthorityAnswer::Approve => rulings.push(Ruling {
                    dispatch: dispatch.clone(),
                    authority: req.authority.clone(),
                    covers: req.covers.clone(),
                    reviewed: request.review(),
                }),
                AuthorityAnswer::Deny => {
                    let surface = self.surface()?;
                    let denier = req.authority.clone();
                    let digest = self.pending_blocks[cohort_index].call.digest();
                    self.append(vec![Fact::Denial {
                        trajectory: self.session.clone(),
                        digest,
                        authority: denier.clone(),
                    }])?;
                    for cohort in &mut self.pending_blocks {
                        if cohort.call.digest() == digest {
                            cohort.offers.retain(|(_, plan)| !plan.names_authority(&denier));
                        }
                    }
                    let feedback = crate::feedback::denial_feedback(
                        self.config.engine().registry(),
                        &self.pending_blocks[cohort_index].offers,
                        surface,
                    );
                    self.pending_blocks.retain(|cohort| !cohort.offers.is_empty());
                    return Ok(Remedied::Feedback(feedback));
                }
                AuthorityAnswer::Abstain => {
                    let feedback = crate::feedback::no_answer_feedback(
                        self.config.engine().registry(),
                        plan_id,
                        &self.pending_blocks[cohort_index].offers,
                        self.surface()?,
                    );
                    return Ok(Remedied::NoAnswer(feedback));
                }
            }
        }

        let batch = match self
            .config
            .engine()
            .execute_remedy_plan(&views, &chosen, &call, &rulings)
        {
            Ok(batch) => batch,
            Err(appa_engine::execute::PlanError::Unestablished(facts)) => {
                return Ok(Remedied::Feedback(crate::feedback::unestablished_gate_feedback(
                    &facts, &views,
                )));
            }
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
                // The cohort is untouched — the model may retry the same offer on fresh state.
                return Ok(Remedied::Feedback("the state changed; retry the remedy".to_string()));
            }
            Err(e) => return Err(e),
        }
        assert_eq!(
            opened, dispatch,
            "the executed plan opens the dispatch its rulings name"
        );
        // The executed plan's dispatch consumes the whole cohort.
        self.pending_blocks.remove(cohort_index);
        Ok(Remedied::Authorized { dispatch, call })
    }

    /// Open the dispatch for a clean-allow call through the store's serialized finalization: the
    /// engine re-checks and decides under the family lock at the live revision, and the returned id
    /// is exactly the dispatch those facts open.
    pub(crate) fn open_dispatch(&self, call: &ResolvedCall) -> Result<Option<DispatchId>, StoreError> {
        let mut dispatch = None;
        self.store.finalize(&self.tenant, &self.session, |facts, rev| {
            let projection = Projection::build(facts, rev);
            let views = projection.view(&self.session);
            let batch = self.config.engine().open_dispatch(&views, call).ok()?;
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
            match self.config.engine().admit_result(&views, dispatch, call, admission) {
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
                    AdmitError::OutputPendingCast
                    | AdmitError::NotPendingCast
                    | AdmitError::UnknownCast(_)
                    | AdmitError::ConstantMismatch
                    | AdmitError::CeilingExceeded
                    | AdmitError::NonLiteralAnswer
                    | AdmitError::EstablishedMismatch
                    | AdmitError::OutOfScopeCast
                    | AdmitError::NarrowingUnaccepted
                    | AdmitError::AcceptanceMismatch
                    | AdmitError::AlreadySucceeded
                    | AdmitError::SuccessContradicted
                    // And this one: with no checkpoint there is no observation to contradict.
                    | AdmitError::ObservationMismatch
                    | AdmitError::OutputSanitizerBound
                    | AdmitError::SanitizerBindingMismatch
                    | AdmitError::SanitizerTransitionUnmet,
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
        }
        | ToolOutcome::Success {
            body: BodyDisposition::Unavailable,
        } => ResultAdmission::SuccessNoValue,
        ToolOutcome::Failure => ResultAdmission::Failure,
        ToolOutcome::Indeterminate => ResultAdmission::Indeterminate,
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
        ToolOutcome::Success {
            body: BodyDisposition::Unavailable,
        } => Some(SEALED_UNAVAILABLE),
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
        if matches!(name, EXECUTE_REMEDY_PLAN | FORK | SUBMIT_RESULT) {
            return Err(OpenError::ReservedToolConflict(name.to_string()));
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

/// Render one host-parsed call against its schema. The SDK path: the host hands a
/// parsed value, so this deprecated adapter first renders it back to bytes. The engine still owns
/// the only call constructor and applies the registered contract.
pub(crate) fn resolve_call(
    engine: &Engine,
    tool: ToolName,
    arguments: &serde_json::Value,
) -> Result<ResolvedCall, EngineError> {
    let raw = serde_json::to_vec(arguments)
        .map_err(|error| EngineError::InvalidCall(ArgumentError::Syntax(error.to_string())))?;
    engine.resolve_call(tool, &raw)
}

/// Render one wire proposal from the provider's raw argument text: the strict scanner
/// sees the bytes themselves, so duplicate keys and trailing data are refused rather than
/// collapsed by a lossy parse.
pub(crate) fn resolve_raw_call(engine: &Engine, tool: ToolName, raw: &[u8]) -> Result<ResolvedCall, EngineError> {
    engine.resolve_call(tool, raw)
}

pub(crate) fn invalid_call_feedback(error: &EngineError) -> String {
    error.to_string()
}

#[cfg(test)]
pub(crate) fn test_call(tool: &str, arguments: serde_json::Value) -> ResolvedCall {
    let policy = format!("version = 1\n[[tool]]\nname = {tool:?}\n");
    let config = Config::from_toml_str(&policy).expect("the test tool policy loads");
    let engine = config.engine().clone();
    let raw = serde_json::to_vec(&arguments).expect("test arguments serialize");
    engine
        .resolve_call(ToolName::new(tool), &raw)
        .expect("test arguments are dialect-valid")
}

/// Every distinct dynamic audience binding one proposed call consumes, in contract order. A
/// source delta and sink requirement may deliberately share one binding; one proposal resolves it
/// once and pins that answer for both uses.
pub(crate) fn dynamic_bindings(contract: &ToolContract) -> Vec<DynamicAudienceBinding> {
    let mut bindings = Vec::new();
    if let Some(AudienceDelta::Dynamic(binding)) = contract.delta.as_ref().and_then(|delta| delta.audience.as_ref()) {
        bindings.push(binding.clone());
    }
    for requirement in &contract.requires.label.audience {
        if let AudienceRequirement::Includes(RecipientSpec::Dynamic(binding)) = requirement
            && !bindings.contains(binding)
        {
            bindings.push(binding.clone());
        }
    }
    bindings
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
                "Execute a remedy plan offered after a blocked tool call. Pass the plan_id quoted in the block feedback. Accepting a narrowing permanently restricts this trajectory; run any later step that needs its current label first.".to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": { "plan_id": { "type": "string" } },
                "required": ["plan_id"],
                "additionalProperties": false
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    async fn spawn_denying_authority() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
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
            let body = r#"{"ruling":"deny"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/rule")
    }

    #[tokio::test]
    async fn a_denial_appends_exactly_one_fact_through_the_facade() {
        let url = spawn_denying_authority().await;
        let policy = format!(
            r#"
version = 1
trust_chain = ["suspicious", "internal"]
[boundary]
trust = "suspicious"
[[tool]]
name = "send_email"
effects = ["egress"]
requires = {{ trust = "internal" }}
delta = {{}}
[[authority]]
name = "security-officer"
mandate = {{ can_cover_trust_to = "internal" }}
implementation = {{ resolver = {{ url = "{url}", timeout_ms = 2000 }} }}
"#
        );
        let config = Config::from_toml_str(&policy).expect("policy parses");
        let mut core = Core::open(config, SdkOptions::default()).expect("policy is SDK-supported");
        core.admit_user_turn("send it".to_string()).unwrap();
        let send = resolve_call(
            core.config.engine(),
            ToolName::new("send_email"),
            &serde_json::json!({"to": "x"}),
        )
        .expect("arguments are dialect-valid");
        let Checked::Feedback(_) = core.check_ordinary(send.clone()).unwrap() else {
            panic!("the send blocks on its trust floor");
        };
        let Remedied::Feedback(_) = core.resolve_remedy(Some("remedy-0")).await.unwrap() else {
            panic!("the authority denies");
        };
        let (log, _) = core.store.snapshot(&core.tenant, &core.session).unwrap();
        let denials: Vec<&Fact> = log.iter().filter(|fact| matches!(fact, Fact::Denial { .. })).collect();
        assert_eq!(denials.len(), 1);
        assert!(matches!(
            denials[0],
            Fact::Denial { digest, authority, .. }
                if *digest == send.digest() && authority.as_str() == "security-officer"
        ));

        core.end_turn().unwrap();
        core.admit_user_turn("again".to_string()).unwrap();
        let Checked::Feedback(feedback) = core.check_ordinary(send).unwrap() else {
            panic!("the send still blocks");
        };
        assert!(!feedback.contains("remedy-"));
    }
}
