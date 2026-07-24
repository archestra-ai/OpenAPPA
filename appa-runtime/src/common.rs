//! Shared engine/store operations behind the session facade.

use std::collections::BTreeMap;

use appa_engine::admit::{AdmitError, ResultAdmission};
use appa_engine::check::CheckOutcome;
use appa_engine::engine::Engine;
use appa_engine::execute::{Issuer, Ruling, Sink};
use appa_engine::fact::{BoundaryKind, Fact, FactBatch, ReturnPolicy};
use appa_engine::label::Label;
use appa_engine::names::AuthorityName;
use appa_engine::projection::Projection;
use appa_engine::value::{
    CanonicalDigest, DispatchId, LabeledValue, Provenance, ResolvedCall, ToolName, TrajectoryId, ValueBody,
};

use crate::assemble;
use crate::config::Config;
use crate::external::{AuthorityAnswer, AuthorityBackend, AuthorityRequest};
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
/// model). Mirrors the runtime: a success consumes the whole cohort, a denial only its offer,
/// and an acceptance-carrying plan is informed — executable only in a round after
/// `offered_round` (the framework signals rounds through `begin_round`).
#[derive(Debug)]
pub(crate) struct PendingBlock {
    pub(crate) call: ResolvedCall,
    pub(crate) offers: Vec<(String, appa_engine::plan::RemedyPlan)>,
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
    pub(crate) engine: Engine,
    pub(crate) store: SessionStore,
    pub(crate) tenant: TenantId,
    pub(crate) session: TrajectoryId,
    pub(crate) authorities: BTreeMap<AuthorityName, AuthorityBackend>,
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
    /// half-works.
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
                let surface = self.surface()?;
                let feedback = if planned.plans.is_empty() {
                    crate::feedback::block_feedback(&raw, &planned, &[], surface)
                } else {
                    let attempts = self.remedy_attempts.entry(call.digest()).or_insert(0);
                    *attempts += 1;
                    if *attempts > self.options.max_remedy_attempts_per_gap {
                        return Ok(Checked::Feedback(
                            "the remedy attempt limit for this call was reached".to_string(),
                        ));
                    }
                    let offers: Vec<(String, appa_engine::plan::RemedyPlan)> = planned
                        .plans
                        .iter()
                        .map(|plan| {
                            let handle = format!("remedy-{}", self.next_remedy_handle);
                            self.next_remedy_handle += 1;
                            (handle, plan.clone())
                        })
                        .collect();
                    let feedback = crate::feedback::block_feedback(&raw, &planned, &offers, surface);
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
                "no pending blocked call offers that plan_id".to_string(),
            ));
        };
        let call = self.pending_blocks[cohort_index].call.clone();
        let chosen = self.pending_blocks[cohort_index]
            .offers
            .iter()
            .find(|(h, _)| h == plan_id)
            .map(|(_, plan)| plan.clone())
            .expect("the cohort was found by this handle");

        let accepts_narrowing = chosen
            .steps
            .iter()
            .any(|step| matches!(step, appa_engine::plan::RemedyStep::Accept(_)));
        if accepts_narrowing && self.pending_blocks[cohort_index].offered_round == self.round {
            return Ok(Remedied::Feedback(uninformed_acceptance_feedback(plan_id)));
        }

        let (log, rev) = self.store.snapshot(&self.tenant, &self.session)?;
        let projection = Projection::build(&log, rev);
        let views = projection.view(&self.session);
        let still_offered = match self.engine.check(&views, &call) {
            Ok(CheckOutcome::Block(raw)) => self
                .engine
                .plan(&views, &call, &raw)
                .expect("pending call is registered")
                .plans
                .contains(&chosen),
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
            let Ok(request) = AuthorityRequest::new(req.authority.clone(), &call, req.covers.clone(), &views) else {
                return Ok(Remedied::Feedback(
                    "the call's argument references no longer resolve".to_string(),
                ));
            };
            // Awaited outside any store lock; a slow or unreachable authority fails closed.
            let answer = tokio::time::timeout(self.options.per_external_timeout, backend.rule(&request))
                .await
                .unwrap_or(AuthorityAnswer::Abstain);
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
                    // The denial consumes only this offer; siblings stay live and are re-listed.
                    let surface = self.surface()?;
                    let cohort = &mut self.pending_blocks[cohort_index];
                    cohort.offers.retain(|(h, _)| h != plan_id);
                    let feedback = crate::feedback::denial_feedback(&cohort.offers, surface);
                    if cohort.offers.is_empty() {
                        self.pending_blocks.remove(cohort_index);
                    }
                    return Ok(Remedied::Feedback(feedback));
                }
            }
        }

        let batch = match self.engine.execute_plan(&views, &chosen, &call, &rulings, Sink::Tool) {
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
                    | AdmitError::CeilingExceeded
                    | AdmitError::NarrowingUnaccepted
                    | AdmitError::AcceptanceMismatch
                    | AdmitError::AlreadySucceeded
                    | AdmitError::SuccessContradicted,
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
