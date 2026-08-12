//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, CastAnswer, CastError, ResultAdmission};
use crate::branch::{self, BranchError, ReturnSubmission};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock, UnestablishedFact};
use crate::contract::ToolContract;
use crate::execute::{self, PlanError, Ruling};
use crate::fact::{Fact, FactBatch, ReturnPolicy, Revision};
use crate::label::DimValue;
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, PlannedBlock};
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::projection::Views;
use crate::registry::{LoadError, Registry};
use crate::value::{DispatchId, ResolvedCall, ToolName, TrajectoryId};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("no contract registered for tool {0}")]
    UnknownTool(String),
    #[error(
        "tool {0} is provider-run: it executes inside the inference call, so no executor of this deployment can run a proposed call naming it"
    )]
    ProviderRunTool(String),
    #[error("invalid call: {0}")]
    InvalidCall(ArgumentError),
    #[error("the call does not pass the check as-is — remedy or accept it first")]
    NotAllowed,
}

/// Why a family log's durable opening record cannot be trusted on cold replay: the
/// strict verifier refuses a log whose opening is missing, displaced, duplicated, foreign, or
/// inconsistent with the supplied policy. Distinct from [`ReplayError`], the per-dispatch payload
/// choke point — the complete transition validator is `T31`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OpeningReplayError {
    #[error("the family log carries no TrajectoryOpened record")]
    Missing,
    #[error("the TrajectoryOpened record is not the family's first record")]
    NotFirst,
    #[error("the family log carries more than one TrajectoryOpened record")]
    Duplicate,
    #[error("the opening record names trajectory {found}, not the root being replayed")]
    WrongTrajectory { found: String },
    #[error("the opening record carries policy dialect version {found}, which this engine does not support")]
    UnsupportedDialect { found: u32 },
    #[error("the opening record's policy digest does not match the supplied policy")]
    DigestMismatch,
    #[error("the opening record's declaration does not match the supplied policy's validated profile")]
    ProfileMismatch,
    #[error("the opening record's open vectors are not the set derived from its declaration")]
    VectorMismatch,
}

/// Why a persisted log cannot be trusted as replay input: a dispatched call whose payload
/// does not validate against the registered contract it names, or whose digest does not
/// match its own arguments. This is the minimal replay choke point a
/// payload-bearing `DispatchOpened` requires; the complete transition validator is `T31`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("dispatched call names unregistered tool {0}")]
    UnknownTool(String),
    #[error("dispatched call payload fails its registered schema: {0}")]
    InvalidPayload(ArgumentError),
    #[error("dispatched call digest does not match its persisted tool and arguments")]
    DigestMismatch,
}

/// The pure decision core, owning its static capability: the immutable registry (which carries
/// the validated deployment profile), the deployment's immutable child-return binding, and the
/// policy identity the durable opening binds.
#[derive(Clone, Debug)]
pub struct Engine {
    registry: Registry,
    identity: PolicyIdentityV1,
    dialect: PolicyDialectVersion,
    child_return: ReturnPolicy,
}

impl Engine {
    /// The one validated constructor: policy and declaration validate together in one
    /// load — the structural registry lints and provider-run split, the profile-exact planner-cap
    /// bound, and the pure policy × profile coverage matrix. No profile-blind path to
    /// a check or a plan exists.
    pub fn open(policy: DeploymentPolicy) -> Result<Engine, LoadError> {
        let DeploymentPolicy {
            registry: config,
            planner_cap,
            dialect,
            child_return,
            profile: declaration,
        } = policy;
        let profile = DeploymentProfile::declare(declaration.clone())?;
        let identity = PolicyIdentityV1::of(&config, &child_return, &profile);
        let registry = Registry::build(config, planner_cap, profile)?;
        profile::validate_coverage(&registry, &declaration, &child_return)?;
        Ok(Engine {
            registry,
            identity,
            dialect,
            child_return,
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn profile(&self) -> &DeploymentProfile {
        self.registry.profile()
    }

    pub fn identity(&self) -> PolicyIdentityV1 {
        self.identity
    }

    /// The open vectors derived from the validated declaration and the registered tool set —
    /// recomputed, never stored, so they cannot drift from the profile.
    pub fn open_vectors(&self) -> Vec<OpenVector> {
        let tools = self
            .registry
            .tools()
            .map(|tool| &tool.name)
            .chain(self.registry.provider_run_contracts().map(|tool| &tool.name));
        profile::derive_open_vectors(self.profile(), tools)
    }

    /// The opening batch of a fresh root trajectory family: one `TrajectoryOpened`
    /// record against the empty log. The runtime appends it before any other family event.
    pub fn open_trajectory(&self, trajectory: &TrajectoryId) -> FactBatch {
        FactBatch::new(
            Revision::ZERO,
            vec![Fact::TrajectoryOpened {
                trajectory: trajectory.clone(),
                dialect: self.dialect,
                profile: self.profile().clone(),
                policy_digest: self.identity,
                open_vectors: self.open_vectors(),
            }],
        )
    }

    /// The strict cold-replay verifier of the durable opening: exactly one
    /// `TrajectoryOpened`, first in the family log, naming the replayed root, at a supported
    /// dialect, carrying the supplied policy's digest, declaration, and derived vectors. The
    /// recorded declaration must equal this engine's validated profile byte for byte — which
    /// subsumes re-running the coverage matrix over it — and the recorded vectors must rederive
    /// from it exactly.
    pub fn verify_opening(&self, facts: &[Fact], trajectory: &TrajectoryId) -> Result<(), OpeningReplayError> {
        let mut openings = facts.iter().enumerate().filter_map(|(index, fact)| match fact {
            Fact::TrajectoryOpened {
                trajectory,
                dialect,
                profile,
                policy_digest,
                open_vectors,
            } => Some((index, trajectory, dialect, profile, policy_digest, open_vectors)),
            _ => None,
        });
        let Some((index, recorded_trajectory, dialect, recorded_profile, policy_digest, open_vectors)) =
            openings.next()
        else {
            return Err(OpeningReplayError::Missing);
        };
        if openings.next().is_some() {
            return Err(OpeningReplayError::Duplicate);
        }
        if index != 0 {
            return Err(OpeningReplayError::NotFirst);
        }
        if recorded_trajectory != trajectory {
            return Err(OpeningReplayError::WrongTrajectory {
                found: recorded_trajectory.as_str().to_string(),
            });
        }
        if *dialect != self.dialect {
            return Err(OpeningReplayError::UnsupportedDialect { found: dialect.value() });
        }
        if policy_digest != &self.identity {
            return Err(OpeningReplayError::DigestMismatch);
        }
        if recorded_profile != self.profile() {
            return Err(OpeningReplayError::ProfileMismatch);
        }
        if open_vectors != &self.open_vectors() {
            return Err(OpeningReplayError::VectorMismatch);
        }
        Ok(())
    }

    /// Convert untrusted provider bytes into the only call representation accepted by this
    /// engine. Tool lookup, strict JSON scanning, schema validation, and RFC 8785 rendering
    /// happen together, so outer runtimes cannot construct a call under a different schema.
    pub fn resolve_call(&self, tool: ToolName, raw_arguments: &[u8]) -> Result<ResolvedCall, EngineError> {
        let contract = self.checkable_contract(&tool)?;
        let arguments =
            CanonicalArguments::from_raw(raw_arguments, &contract.parameters).map_err(EngineError::InvalidCall)?;
        Ok(ResolvedCall::new(tool, arguments))
    }

    /// Evaluate a proposed call: allow, or block carrying everything that stopped it at once —
    /// the requirement gaps, the narrowing where one fired, and the values whose consumed
    /// dimension no cast has established. Resolution is the runtime's job;
    /// the runtime re-checks after each landed cast, so a surfaced block is the residual.
    pub fn check(&self, views: &Views, call: &ResolvedCall) -> Result<CheckOutcome, EngineError> {
        let contract = self.validated_contract(call)?;
        Ok(check::evaluate(contract, views, call))
    }

    /// Open a dispatch for a call that **passes the check as-is**. Re-checks and refuses any
    /// block — unestablished values included (a narrowing is accepted through
    /// [`Engine::execute_remedy_plan`], not here), so
    /// the engine never emits an appendable dispatch for a call it would not allow. Folds nothing —
    /// the label folds only when the result value is admitted.
    pub fn open_dispatch(&self, views: &Views, call: &ResolvedCall) -> Result<FactBatch, EngineError> {
        let contract = self.validated_contract(call)?;
        match check::evaluate(contract, views, call) {
            CheckOutcome::Allow => {
                let (_, fact) = opened_dispatch(contract, views, call);
                Ok(FactBatch::new(views.revision(), vec![fact]))
            }
            _ => Err(EngineError::NotAllowed),
        }
    }

    /// Execute a remedy plan: land the covering rulings, the narrowing acceptance, and the dispatch
    /// as one atomic batch, enforcing the plan's exact grouped assignment and mandate coverage. The
    /// chosen plan is matched by value against the live offers — the return-path staleness story.
    pub fn execute_remedy_plan(
        &self,
        views: &Views,
        chosen: &plan::ExecutableRemedyPlan,
        call: &ResolvedCall,
        rulings: &[Ruling],
    ) -> Result<FactBatch, PlanError> {
        if self.registry.provider_run_contract(call.tool()).is_some() {
            return Err(PlanError::ProviderRunTool(call.tool().as_str().to_string()));
        }
        execute::execute_remedy_plan(&self.registry, views, chosen, call, rulings)
    }

    /// Close a dispatch and admit its result — raw, cast-resolved, or withheld. The label folds only
    /// from an admitted value, never from the close.
    pub fn admit_result(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        admission: ResultAdmission,
    ) -> Result<FactBatch, AdmitError> {
        admit::admit_result(&self.registry, views, dispatch, call, admission)
    }

    /// Record observed success for a still-open dispatch: its declared effects commit now, at the
    /// one append point the spec puts at success, while any value finalization — an
    /// output sanitizer derivation, a pending-cast resolution — is still in flight. See
    /// [`crate::admit::observe_success`].
    pub fn observe_success(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
    ) -> Result<FactBatch, AdmitError> {
        admit::observe_success(&self.registry, views, dispatch, call)
    }

    /// The narrowing admitting a cast-resolved value of `call` would fold into the live trajectory
    /// label, or `None` when it does not move it — the whole filled label, established dimensions
    /// included (see `admit::pending_cast_narrowing`). The runtime derives the acceptance offer
    /// from this; admission re-derives it under the family lock, so a stale offer refuses by value
    /// (D2).
    pub fn cast_narrowing(
        &self,
        views: &Views,
        dispatch: &DispatchId,
        call: &ResolvedCall,
        resolved: &DimValue,
    ) -> Result<Option<Narrowing>, EngineError> {
        let contract = self.validated_contract(call)?;
        Ok(admit::pending_cast_narrowing(
            views,
            &admit::cast_filled_dispatch_label(contract, views, dispatch, resolved),
        ))
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    pub fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        self.validated_contract(call)?;
        Ok(plan::plan(&self.registry, views, call, raw))
    }

    pub fn admit_cast(
        &self,
        views: &Views,
        target: &UnestablishedFact,
        answer: CastAnswer,
    ) -> Result<FactBatch, CastError> {
        admit::admit_cast(&self.registry, views, target, answer)
    }

    /// Seed a child branch at the parent's current label with an immutable fork binding carrying
    /// the deployment's `[child]` return policy — the binding is the engine's validated state,
    /// never a caller-supplied per-fork choice. Branching exists only where the
    /// deployment declares context control. See [`crate::branch`].
    pub fn seed_child(&self, parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
        branch::seed_child(&self.registry, parent, child, self.child_return.clone())
    }

    /// Record a child's returned value at an engine-derived label AND merge it into the direct
    /// parent — one atomic batch, no orphanable intermediate state. A raw crossing that would
    /// narrow the parent is refused (`ReturnNarrowsParent`): it exists only through an executed
    /// return plan. See [`crate::branch`].
    pub fn submit_child_return(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        ret: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::submit_child_return(&self.registry, parent, child, ret)
    }

    /// Decide whether a raw return by `child` may merge silently, and if not, which return plans
    /// could cross it. Both folds and the linkage come from the parent's one projection snapshot.
    /// See [`crate::branch`].
    pub fn check_child_return(&self, parent: &Views, child: &TrajectoryId) -> Result<branch::ReturnCheck, BranchError> {
        branch::check_child_return(&self.registry, parent, child)
    }

    /// Record a child's void return: the child-attributed terminal that ends the branch and
    /// crosses no value — no merge, no label contribution. A branch ends at most once.
    /// See [`crate::branch`].
    pub fn submit_void_return(&self, parent: &Views, child: &TrajectoryId) -> Result<FactBatch, BranchError> {
        branch::submit_void_return(parent, child)
    }

    /// The child fold's unestablished facts — what a cast must establish before this child's
    /// return can merge. Policy-independent: the runtime drives resolution *before*
    /// the return-policy split, so raw and sanitizer-bound returns resolve alike.
    pub fn child_fold_unestablished(&self, parent: &Views, child: &TrajectoryId) -> Vec<check::UnestablishedFact> {
        branch::child_fold_unestablished(parent, child)
    }

    /// Execute one offered return plan as a single atomic batch: crossing, acceptance where the
    /// plan carries one, and merge. Re-derives the block from the live views and refuses a chosen
    /// plan the fresh offers no longer contain. See [`crate::branch`].
    pub fn execute_child_return_plan(
        &self,
        parent: &Views,
        child: &TrajectoryId,
        chosen: branch::ReturnPlan,
        submission: ReturnSubmission,
    ) -> Result<FactBatch, BranchError> {
        branch::execute_child_return_plan(&self.registry, parent, child, chosen, submission)
    }

    /// Validate every persisted dispatched call against this registry before its log is
    /// trusted as replay input: the named tool must be registered, the persisted payload
    /// must satisfy that tool's compiled schema, and the recomputed digest must match the
    /// `DispatchId`. A `CanonicalArguments` cannot prove which schema
    /// it was validated against, so replay re-establishes the binding here.
    pub fn validate_replay(&self, facts: &[Fact]) -> Result<(), ReplayError> {
        for fact in facts {
            let Fact::DispatchOpened {
                dispatch,
                tool,
                arguments,
                ..
            } = fact
            else {
                continue;
            };
            let contract = self
                .registry
                .tool(tool)
                .ok_or_else(|| ReplayError::UnknownTool(tool.as_str().to_string()))?;
            contract
                .parameters
                .validate(arguments.value())
                .map_err(ReplayError::InvalidPayload)?;
            let recomputed = crate::value::CanonicalDigest::of_call(tool, arguments);
            if dispatch.digest() != &recomputed {
                return Err(ReplayError::DigestMismatch);
            }
        }
        Ok(())
    }

    fn checkable_contract(&self, tool: &ToolName) -> Result<&ToolContract, EngineError> {
        self.registry.tool(tool).ok_or_else(|| {
            if self.registry.provider_run_contract(tool).is_some() {
                EngineError::ProviderRunTool(tool.as_str().to_string())
            } else {
                EngineError::UnknownTool(tool.as_str().to_string())
            }
        })
    }

    fn contract(&self, call: &ResolvedCall) -> Result<&ToolContract, EngineError> {
        self.checkable_contract(call.tool())
    }

    fn validated_contract(&self, call: &ResolvedCall) -> Result<&ToolContract, EngineError> {
        let contract = self.contract(call)?;
        contract
            .parameters
            .validate(call.arguments())
            .map_err(EngineError::InvalidCall)?;
        Ok(contract)
    }
}

/// Build the `DispatchOpened` fact for a call: its proposed committed label, the effects it would
/// commit on success, and its occurrence (a repeat identical call is a new dispatch). Shared by the
/// clean-allow path ([`Engine::open_dispatch`]) and atomic plan execution ([`crate::execute`]).
pub(crate) fn opened_dispatch(contract: &ToolContract, views: &Views, call: &ResolvedCall) -> (DispatchId, Fact) {
    let digest = call.digest();
    let occurrence = views.dispatch_count(&digest);
    let dispatch = DispatchId::new(views.trajectory().clone(), digest, occurrence);
    let fact = Fact::DispatchOpened {
        trajectory: views.trajectory().clone(),
        dispatch: dispatch.clone(),
        tool: call.tool().clone(),
        arguments: call.canonical_arguments().clone(),
        proposed_label: check::committed_label_for_call(contract, &views.current_label(), call),
        proposed_effects: contract.emits.clone(),
        dynamic_resolutions: call.dynamic_resolutions().to_vec(),
    };
    (dispatch, fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Gap;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
    };
    use crate::fact::{EffectKind, EffectSet, Fact, Revision};
    use crate::label::{Audience, Dim, Dimension, Label, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn engine(tools: Vec<ToolContract>) -> Engine {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        open_engine(cfg)
    }

    fn open_engine(cfg: RegistryConfig) -> Engine {
        let profile = crate::profile::covering_declaration(&cfg);
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile,
        })
        .unwrap()
    }

    fn user_value(label: Label) -> Fact {
        Fact::ValueAdmitted {
            trajectory: traj(),
            value: LabeledValue::new(ValueBody::new("body"), label),
            provenance: Provenance::UserInput,
        }
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(Dim::Known(trust), Dim::Known(audience))
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
    }

    fn check(engine: &Engine, log: &[Fact], call: &ResolvedCall) -> CheckOutcome {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let t = traj();
        engine.check(&p.view(&t), call).unwrap()
    }

    fn crm_tool() -> ToolContract {
        ToolContract {
            name: ToolName::new("get_ticket"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        }
    }

    #[test]
    fn permuted_effect_declarations_produce_byte_identical_dispatch_facts() {
        let pay = |emits: [&str; 2]| ToolContract {
            name: ToolName::new("pay"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new(emits.map(EffectKind::new)).unwrap(),
            requires: Requires::default(),
        };
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let open = |contract: ToolContract| {
            engine(vec![contract])
                .open_dispatch(&p.view(&traj()), &call("pay", json!({})))
                .unwrap()
        };
        let ab = open(pay(["spend", "audit"]));
        let ba = open(pay(["audit", "spend"]));
        assert_eq!(
            serde_json::to_string(&ab.facts).unwrap(),
            serde_json::to_string(&ba.facts).unwrap()
        );
        let mut log_ab = log.clone();
        log_ab.extend(ab.facts);
        let mut log_ba = log;
        log_ba.extend(ba.facts);
        assert_eq!(
            Projection::build(&log_ab, Revision::new(2)),
            Projection::build(&log_ba, Revision::new(2))
        );
    }

    #[test]
    fn clean_call_allows() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.narrowing.is_some());
                assert!(b.requirement_gaps.is_empty());
            }
            other => panic!("expected narrowing block, got {other:?}"),
        }
    }

    #[test]
    fn repeat_at_same_label_is_not_a_narrowing() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(check(&e, &log, &call("get_ticket", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn pending_cast_output_dispatches_before_resolution() {
        let scan = ToolContract {
            name: ToolName::new("scan_inbox"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let e = engine(vec![scan]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        assert_eq!(check(&e, &log, &call("scan_inbox", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn trust_floor_gap_when_suspicious() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => assert!(b.requirement_gaps.contains(&Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            })),
            other => panic!("expected trust gap, got {other:?}"),
        }
    }

    #[test]
    fn includes_placeholder_resolves_from_arguments() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("egress")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let internal = Audience::restricted([ReaderId::new("auditor")]);
        let log = vec![user_value(known(TRUSTED, internal))];
        assert_eq!(
            check(&e, &log, &call("send_email", json!({ "to": "auditor" }))),
            CheckOutcome::Allow
        );
        match check(&e, &log, &call("send_email", json!({ "to": "stranger" }))) {
            CheckOutcome::Block(b) => assert!(matches!(
                b.requirement_gaps.as_slice(),
                [crate::check::Gap::Includes { .. }]
            )),
            other => panic!("expected includes gap, got {other:?}"),
        }
    }

    #[test]
    fn history_prior_and_no_prior() {
        let del = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![
                    HistoryRequirement::Prior(EffectKind::new("backup.done")),
                    HistoryRequirement::NoPrior(EffectKind::new("db.deleted")),
                ],
                ..Requires::default()
            },
        };
        let e = engine(vec![del]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("delete_db", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Prior(EffectKind::new("backup.done"))))
            }
            other => panic!("expected prior gap, got {other:?}"),
        }
    }

    #[test]
    fn an_includes_requirement_reads_the_committed_label() {
        let b_reader = Audience::restricted([ReaderId::new("b")]);
        let share = ToolContract {
            name: ToolName::new("share"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("a")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(b_reader.clone()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![share]);
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let log = vec![user_value(known(TRUSTED, both.clone()))];
        match check(&e, &log, &call("share", json!({}))) {
            CheckOutcome::Block(block) => {
                assert_eq!(block.requirement_gaps, vec![Gap::Includes { recipients: b_reader }]);
                assert_eq!(
                    block.narrowing,
                    Some(crate::check::Narrowing {
                        from: known(TRUSTED, both),
                        to: known(TRUSTED, Audience::restricted([ReaderId::new("a")])),
                    })
                );
                assert!(block.unestablished.is_empty());
            }
            other => panic!("expected the committed-label includes gap, got {other:?}"),
        }
    }

    #[test]
    fn a_trust_floor_reads_the_committed_label() {
        let risky = ToolContract {
            name: ToolName::new("risky"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![risky]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("risky", json!({}))) {
            CheckOutcome::Block(block) => {
                assert_eq!(
                    block.requirement_gaps,
                    vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS,
                    }]
                );
                assert!(block.narrowing.is_some());
            }
            other => panic!("expected the committed-label trust gap, got {other:?}"),
        }
    }

    #[test]
    fn a_read_that_narrows_into_the_cap_passes_the_cap() {
        let a_reader = Audience::restricted([ReaderId::new("a")]);
        let scoped = ToolContract {
            name: ToolName::new("scoped"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(a_reader.clone()).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(a_reader)],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![scoped]);
        let both = Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let log = vec![user_value(known(TRUSTED, both))];
        match check(&e, &log, &call("scoped", json!({}))) {
            CheckOutcome::Block(block) => {
                assert!(block.requirement_gaps.is_empty(), "narrowing into the cap is not a gap");
                assert!(block.narrowing.is_some());
            }
            other => panic!("expected a narrowing-only soft block, got {other:?}"),
        }
    }

    fn emitting(name: &str, kind: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new(kind)]).unwrap(),
            requires: Requires::default(),
        }
    }

    fn history_guarded(name: &str, requirement: HistoryRequirement) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![requirement],
                ..Requires::default()
            },
        }
    }

    fn open(e: &Engine, log: &mut Vec<Fact>, c: &ResolvedCall) -> crate::value::DispatchId {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let batch = e.open_dispatch(&p.view(&traj()), c).unwrap();
        let dispatch = batch
            .facts
            .iter()
            .find_map(|fact| match fact {
                Fact::DispatchOpened { dispatch, .. } => Some(dispatch.clone()),
                _ => None,
            })
            .expect("open_dispatch appends the open fact");
        log.extend(batch.facts);
        dispatch
    }

    fn close(
        e: &Engine,
        log: &mut Vec<Fact>,
        dispatch: &crate::value::DispatchId,
        c: &ResolvedCall,
        admission: crate::admit::ResultAdmission,
    ) {
        let p = Projection::build(log, Revision::new(log.len() as u64));
        let batch = e.admit_result(&p.view(&traj()), dispatch, c, admission).unwrap();
        log.extend(batch.facts);
    }

    fn reservation_tools() -> Vec<ToolContract> {
        vec![
            emitting("send", "email.sent"),
            history_guarded("guard", HistoryRequirement::NoPrior(EffectKind::new("email.sent"))),
            history_guarded("wants", HistoryRequirement::Prior(EffectKind::new("email.sent"))),
        ]
    }

    #[test]
    fn an_open_dispatch_reserves_its_emits_for_no_prior_only() {
        let e = engine(reservation_tools());
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected a reservation-failed no_prior, got {other:?}"),
        }
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior unfulfilled by a reservation, got {other:?}"),
        }
        close(
            &e,
            &mut log,
            &dispatch,
            &send,
            crate::admit::ResultAdmission::SuccessRaw {
                body: ValueBody::new("sent"),
            },
        );
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected a committed-effect no_prior failure, got {other:?}"),
        }
        assert_eq!(check(&e, &log, &call("wants", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn a_failed_dispatch_evaporates_its_reservation() {
        let e = engine(reservation_tools());
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        close(&e, &mut log, &dispatch, &send, crate::admit::ResultAdmission::Failure);
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior still unmet, got {other:?}"),
        }
    }

    #[test]
    fn an_indeterminate_close_keeps_the_reservation() {
        let e = engine(reservation_tools());
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let send = call("send", json!({}));
        let dispatch = open(&e, &mut log, &send);
        close(
            &e,
            &mut log,
            &dispatch,
            &send,
            crate::admit::ResultAdmission::Indeterminate,
        );
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        assert!(!p.view(&traj()).is_open(&dispatch), "the dispatch is closed");
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the reservation to outlive the close, got {other:?}"),
        }
        match check(&e, &log, &call("wants", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Prior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected prior unmet, got {other:?}"),
        }
    }

    #[test]
    fn two_reservations_of_one_kind_settle_independently() {
        let e = engine(reservation_tools());
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let send = call("send", json!({}));
        let first = open(&e, &mut log, &send);
        let second = open(&e, &mut log, &send);
        assert_ne!(first, second, "a repeat call is a new dispatch occurrence");
        close(&e, &mut log, &first, &send, crate::admit::ResultAdmission::Failure);
        match check(&e, &log, &call("guard", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the second reservation to hold, got {other:?}"),
        }
        close(&e, &mut log, &second, &send, crate::admit::ResultAdmission::Failure);
        assert_eq!(check(&e, &log, &call("guard", json!({}))), CheckOutcome::Allow);
    }

    #[test]
    fn a_calls_own_emits_never_fail_its_own_check() {
        let selfguard = ToolContract {
            name: ToolName::new("selfguard"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
        };
        let e = engine(vec![selfguard]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let c = call("selfguard", json!({}));
        assert_eq!(check(&e, &log, &c), CheckOutcome::Allow);
        let _dispatch = open(&e, &mut log, &c);
        match check(&e, &log, &c) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::NoPrior(EffectKind::new("email.sent"))])
            }
            other => panic!("expected the open dispatch to reserve, got {other:?}"),
        }
    }

    #[test]
    fn a_success_checkpoint_settles_while_the_dispatch_stays_open() {
        let scan = ToolContract {
            name: ToolName::new("scan"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Unknown),
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("read")]).unwrap(),
            requires: Requires::default(),
        };
        let tools = vec![
            scan,
            history_guarded("guard_read", HistoryRequirement::NoPrior(EffectKind::new("read"))),
            history_guarded("wants_read", HistoryRequirement::Prior(EffectKind::new("read"))),
        ];
        let e = engine(tools);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let scan_call = call("scan", json!({}));
        let dispatch = open(&e, &mut log, &scan_call);
        assert!(matches!(
            check(&e, &log, &call("guard_read", json!({}))),
            CheckOutcome::Block(_)
        ));
        assert!(matches!(
            check(&e, &log, &call("wants_read", json!({}))),
            CheckOutcome::Block(_)
        ));
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let batch = e.observe_success(&p.view(&traj()), &dispatch, &scan_call).unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        assert!(p.view(&traj()).is_open(&dispatch));
        assert_eq!(check(&e, &log, &call("wants_read", json!({}))), CheckOutcome::Allow);
        assert!(matches!(
            check(&e, &log, &call("guard_read", json!({}))),
            CheckOutcome::Block(_)
        ));
    }

    #[test]
    fn attention_is_always_a_gap() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![tool]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("wire", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.contains(&Gap::Attention(MarkName::new("signoff"))))
            }
            other => panic!("expected attention gap, got {other:?}"),
        }
    }

    #[test]
    fn unknown_label_is_unestablished_not_a_gap() {
        let e = engine(vec![crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert!(b.narrowing.is_some(), "the audience narrowing reports alongside");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn all_three_block_slots_coexist() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let e = engine(vec![vault]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];
        match check(&e, &log, &call("vault", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(b.requirement_gaps, vec![Gap::Attention(MarkName::new("signoff"))]);
                assert!(b.narrowing.is_some());
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected a three-slot block, got {other:?}"),
        }
    }

    fn unannotated_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: None,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn an_unannotated_tool_dispatches_and_its_result_admits_unknown() {
        let e = engine(vec![unannotated_tool("probe")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let proposed = call("probe", json!({}));
        assert_eq!(check(&e, &log, &proposed), CheckOutcome::Allow);

        let t = traj();
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let batch = e.open_dispatch(&p.view(&t), &proposed).unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let dispatch = DispatchId::new(t.clone(), proposed.digest(), 0);
        let batch = e
            .admit_result(
                &p.view(&t),
                &dispatch,
                &proposed,
                ResultAdmission::SuccessRaw {
                    body: ValueBody::new("raw"),
                },
            )
            .unwrap();
        log.extend(batch.facts);
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let current = p.view(&t).current_label();
        assert_eq!(current.trust, Dim::Unknown);
        assert_eq!(current.audience, Dim::Unknown);
    }

    #[test]
    fn an_unknown_trajectory_blocks_only_requirement_consuming_calls() {
        let e = engine(vec![unannotated_tool("noop"), crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Unknown))];
        assert_eq!(check(&e, &log, &call("noop", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Trust);
            }
            other => panic!("expected an unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_errors() {
        let e = engine(vec![]);
        let p = Projection::build(&[], Revision::ZERO);
        let t = traj();
        assert!(matches!(
            e.check(&p.view(&t), &call("ghost", json!({}))),
            Err(EngineError::UnknownTool(name)) if name == "ghost"
        ));
    }

    #[test]
    fn open_dispatch_refuses_a_blocked_call() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(SUSPICIOUS, internal))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        assert_eq!(
            e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))),
            Err(EngineError::NotAllowed)
        );
    }

    #[test]
    fn includes_missing_placeholder_fails_closed_on_public() {
        let send = ToolContract {
            name: ToolName::new("send_email"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![send]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => assert!(matches!(b.requirement_gaps.as_slice(), [Gap::Includes { .. }])),
            other => panic!("expected includes gap on a malformed call, got {other:?}"),
        }

        let log = vec![user_value(Label::new(Dim::Known(TRUSTED), Dim::Unknown))];
        match check(&e, &log, &call("send_email", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty(), "the sentinel gap must be masked");
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimension, Dimension::Audience);
            }
            other => panic!("expected an unestablished block on an Unknown audience, got {other:?}"),
        }
    }

    #[test]
    fn required_rulings_route_each_gap_to_its_authority() {
        use crate::authority::{Authority, Mandate, Scope};
        use crate::names::AuthorityName;

        let wire = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        };
        let e = open_engine(cfg);
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        let wire_call = call("wire", json!({}));
        let raw = match e.check(&p.view(&t), &wire_call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let planned = e.plan(&p.view(&t), &wire_call, &raw).unwrap();
        assert_eq!(planned.plans.len(), 1);
        let required = &planned.plans[0].executable().expect("an authority plan").required;
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].authority, AuthorityName::new("officer"));
        assert_eq!(
            required[0].covers,
            vec![Gap::TrustFloor {
                required: TRUSTED,
                actual: SUSPICIOUS,
            }]
        );
    }

    fn strict_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            parameters: crate::params::ToolParameters::compile(&json!({
                "type": "object",
                "properties": { "to": { "type": "string" } },
                "required": ["to"],
            }))
            .unwrap(),
            delta: Some(Delta::NONE),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    #[test]
    fn schema_invalid_arguments_are_an_invalid_call_at_every_fresh_entry_point() {
        let e = engine(vec![strict_tool("send")]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let bogus = call("send", json!({ "bogus": 1 }));
        assert!(matches!(e.check(&views, &bogus), Err(EngineError::InvalidCall(_))));
        assert!(matches!(
            e.open_dispatch(&views, &bogus),
            Err(EngineError::InvalidCall(_))
        ));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
            unestablished: vec![],
        };
        assert!(matches!(e.plan(&views, &bogus, &raw), Err(EngineError::InvalidCall(_))));
        let fabricated = plan::ExecutableRemedyPlan {
            id: plan::PlanId::new(0),
            steps: vec![],
            required: vec![],
        };
        assert!(matches!(
            e.execute_remedy_plan(&views, &fabricated, &bogus, &[]),
            Err(PlanError::InvalidCall(_))
        ));
        assert_eq!(
            e.check(&views, &call("send", json!({ "to": "hr" }))).unwrap(),
            CheckOutcome::Allow
        );
    }

    #[test]
    fn resolve_call_owns_tool_lookup_scanning_and_schema_binding() {
        let e = engine(vec![strict_tool("send")]);

        let resolved = e
            .resolve_call(ToolName::new("send"), br#"{ "to": "hr" }"#)
            .expect("the registered schema accepts the call");
        assert_eq!(resolved.canonical_arguments().canonical_text(), r#"{"to":"hr"}"#);

        assert!(matches!(
            e.resolve_call(ToolName::new("send"), br#"{"to":"hr","to":"finance"}"#),
            Err(EngineError::InvalidCall(ArgumentError::DuplicateKey(key))) if key == "to"
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("send"), br#"{"bogus":true}"#),
            Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
        ));
        assert_eq!(
            e.resolve_call(ToolName::new("ghost"), br#"{}"#),
            Err(EngineError::UnknownTool("ghost".to_string()))
        );
    }

    #[test]
    fn replay_refuses_a_corrupt_dispatched_call() {
        let e = engine(vec![strict_tool("send")]);
        let mut log = vec![user_value(known(TRUSTED, Audience::Public))];
        let good = call("send", json!({ "to": "hr" }));
        let p = Projection::build(&log, Revision::new(1));
        let batch = e.open_dispatch(&p.view(&traj()), &good).unwrap();
        log.extend(batch.facts);
        assert_eq!(e.validate_replay(&log), Ok(()));

        let opened = |tool: &str, payload: serde_json::Value, minted_from: &ResolvedCall| Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: DispatchId::new(traj(), minted_from.digest(), 0),
            tool: ToolName::new(tool),
            arguments: crate::params::test_arguments(&payload),
            proposed_label: known(TRUSTED, Audience::Public),
            proposed_effects: EffectSet::default(),
            dynamic_resolutions: vec![],
        };
        let ghost_call = call("ghost", json!({}));
        assert!(matches!(
            e.validate_replay(&[opened("ghost", json!({}), &ghost_call)]),
            Err(ReplayError::UnknownTool(name)) if name == "ghost"
        ));
        let smuggled = call("send", json!({ "bogus": 1 }));
        assert!(matches!(
            e.validate_replay(&[opened("send", json!({ "bogus": 1 }), &smuggled)]),
            Err(ReplayError::InvalidPayload(_))
        ));
        assert!(matches!(
            e.validate_replay(&[opened("send", json!({ "to": "hr" }), &smuggled)]),
            Err(ReplayError::DigestMismatch)
        ));
    }

    #[test]
    fn the_dispatched_payload_is_persisted_exactly_once() {
        use crate::authority::{Authority, Mandate, Scope};
        use crate::names::AuthorityName;
        let mut wire = strict_tool("wire");
        wire.requires.label.trust_floor = Some(TRUSTED);
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![wire],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        };
        let e = open_engine(cfg);
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let wire_call = call("wire", json!({ "to": "distinctive-recipient-hr" }));
        let raw = match e.check(&views, &wire_call).unwrap() {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let planned = e.plan(&views, &wire_call, &raw).unwrap();
        let chosen = planned.plans[0].executable().expect("an authority plan").clone();
        let ruling = crate::execute::Ruling {
            dispatch: DispatchId::new(t.clone(), wire_call.digest(), 0),
            authority: AuthorityName::new("officer"),
            covers: chosen.required[0].covers.clone(),
            reviewed: crate::execute::AuthorityReview {
                tool: ToolName::new("wire"),
                trajectory_label: known(SUSPICIOUS, Audience::Public),
            },
        };
        let batch = e.execute_remedy_plan(&views, &chosen, &wire_call, &[ruling]).unwrap();
        let serialized = serde_json::to_string(&batch.facts).unwrap();
        assert_eq!(serialized.matches("distinctive-recipient-hr").count(), 1);
        assert!(matches!(batch.facts.last().unwrap(), Fact::DispatchOpened { .. }));
        let restored: Vec<Fact> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, batch.facts);
    }

    #[test]
    fn open_dispatch_records_proposed_label_and_effects() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let log = vec![user_value(known(TRUSTED, internal.clone()))];
        let p = Projection::build(&log, Revision::new(log.len() as u64));
        let t = traj();
        let batch = e.open_dispatch(&p.view(&t), &call("get_ticket", json!({}))).unwrap();
        match &batch.facts[0] {
            Fact::DispatchOpened { proposed_label, .. } => {
                assert_eq!(*proposed_label, known(TRUSTED, internal));
            }
            other => panic!("expected DispatchOpened, got {other:?}"),
        }
    }

    fn plain_tool(name: &str) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn engine_with_provider_run(tools: Vec<ToolContract>, provider_run: &[&str]) -> Engine {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools,
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        let mut declaration = crate::profile::covering_declaration(&cfg);
        for name in provider_run {
            declaration
                .executor_exceptions
                .insert(ToolName::new(*name), crate::profile::ExecutorClass::ProviderRun);
            declaration.confined_results.remove(&ToolName::new(*name));
        }
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .unwrap()
    }

    #[test]
    fn a_proposal_naming_a_provider_run_tool_is_malformed_at_every_fresh_entry_point() {
        let e = engine_with_provider_run(vec![plain_tool("search")], &["search"]);
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let views = p.view(&t);
        let proposed = call("search", json!({}));
        assert!(matches!(
            e.check(&views, &proposed),
            Err(EngineError::ProviderRunTool(name)) if name == "search"
        ));
        assert!(matches!(
            e.open_dispatch(&views, &proposed),
            Err(EngineError::ProviderRunTool(_))
        ));
        let raw = crate::check::RawBlock {
            requirement_gaps: vec![],
            narrowing: None,
            unestablished: vec![],
        };
        assert!(matches!(
            e.plan(&views, &proposed, &raw),
            Err(EngineError::ProviderRunTool(_))
        ));
        assert!(matches!(
            e.resolve_call(ToolName::new("search"), b"{}"),
            Err(EngineError::ProviderRunTool(_))
        ));
        let fabricated = plan::ExecutableRemedyPlan {
            id: plan::PlanId::new(0),
            steps: vec![],
            required: vec![],
        };
        assert!(matches!(
            e.execute_remedy_plan(&views, &fabricated, &proposed, &[]),
            Err(PlanError::ProviderRunTool(name)) if name == "search"
        ));
    }

    #[test]
    fn provider_run_tools_leave_every_plan_family() {
        let mut target = plain_tool("wire");
        target.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut emitter = plain_tool("emit");
        emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let offered_tools = |e: &Engine| -> Vec<String> {
            let p = Projection::build(&log, Revision::new(1));
            let t = traj();
            let wire = call("wire", json!({}));
            let raw = match e.check(&p.view(&t), &wire).unwrap() {
                CheckOutcome::Block(raw) => raw,
                other => panic!("expected a block, got {other:?}"),
            };
            e.plan(&p.view(&t), &wire, &raw)
                .unwrap()
                .plans
                .iter()
                .filter_map(|plan| match plan {
                    plan::RemedyPlan::Redispatch(redispatch) => Some(redispatch.tool().as_str().to_string()),
                    plan::RemedyPlan::Executable(_) => None,
                })
                .collect()
        };
        let enforced = engine(vec![target.clone(), emitter.clone()]);
        assert_eq!(offered_tools(&enforced), ["emit"]);
        let split = engine_with_provider_run(vec![target, emitter], &["emit"]);
        assert_eq!(offered_tools(&split), Vec::<String>::new());
    }

    #[test]
    fn the_opening_batch_carries_the_identity_and_derived_vectors() {
        let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
        let t = traj();
        let batch = e.open_trajectory(&t);
        assert_eq!(batch.basis, Revision::ZERO);
        match batch.facts.as_slice() {
            [
                Fact::TrajectoryOpened {
                    trajectory,
                    dialect,
                    profile,
                    policy_digest,
                    open_vectors,
                },
            ] => {
                assert_eq!(trajectory, &t);
                assert_eq!(*dialect, PolicyDialectVersion::new(1));
                assert_eq!(profile, e.profile());
                assert_eq!(*policy_digest, e.identity());
                assert_eq!(open_vectors, &e.open_vectors());
                assert_eq!(open_vectors.len(), 1);
            }
            other => panic!("expected exactly the opening record, got {other:?}"),
        }
        let wire = serde_json::to_string(&batch.facts).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Fact>>(&wire).unwrap(), batch.facts);
    }

    #[test]
    fn cold_replay_verifies_the_opening_strictly() {
        let e = engine_with_provider_run(vec![plain_tool("send"), plain_tool("search")], &["search"]);
        let t = traj();
        let opening = e.open_trajectory(&t).facts.remove(0);
        let admitted = user_value(known(TRUSTED, Audience::Public));

        assert_eq!(e.verify_opening(&[opening.clone(), admitted.clone()], &t), Ok(()));
        assert_eq!(
            e.verify_opening(std::slice::from_ref(&admitted), &t),
            Err(OpeningReplayError::Missing)
        );
        assert_eq!(
            e.verify_opening(&[admitted.clone(), opening.clone()], &t),
            Err(OpeningReplayError::NotFirst)
        );
        assert_eq!(
            e.verify_opening(&[opening.clone(), opening.clone()], &t),
            Err(OpeningReplayError::Duplicate)
        );
        assert_eq!(
            e.verify_opening(std::slice::from_ref(&opening), &TrajectoryId::new("other")),
            Err(OpeningReplayError::WrongTrajectory { found: "t".to_string() })
        );

        let mutated = |mutate: &dyn Fn(&mut Fact)| {
            let mut fact = opening.clone();
            mutate(&mut fact);
            e.verify_opening(&[fact], &t)
        };
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { dialect, .. } = fact {
                    *dialect = PolicyDialectVersion::new(9);
                }
            }),
            Err(OpeningReplayError::UnsupportedDialect { found: 9 })
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { policy_digest, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *policy_digest = other.identity();
                }
            }),
            Err(OpeningReplayError::DigestMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { profile, .. } = fact {
                    let other = engine(vec![plain_tool("send")]);
                    *profile = other.profile().clone();
                }
            }),
            Err(OpeningReplayError::ProfileMismatch)
        );
        assert_eq!(
            mutated(&|fact| {
                if let Fact::TrajectoryOpened { open_vectors, .. } = fact {
                    open_vectors.clear();
                }
            }),
            Err(OpeningReplayError::VectorMismatch)
        );
    }

    #[test]
    fn branching_takes_declared_context_control() {
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("send")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        let mut declaration = crate::profile::covering_declaration(&cfg);
        declaration.context_control = false;
        let e = Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration,
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        assert_eq!(
            e.seed_child(&p.view(&t), &TrajectoryId::new("t:child")),
            Err(crate::branch::BranchError::ContextUncontrolled)
        );
    }

    #[test]
    fn a_fork_carries_the_deployments_child_return_binding() {
        let mut cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![plain_tool("send")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        };
        cfg.sanitizers = vec![crate::authority::Sanitizer {
            name: crate::names::SanitizerName::new("redactor"),
            on: crate::authority::SanitizerPoints {
                input: false,
                output: true,
            },
            transition: crate::authority::Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
            hint: None,
        }];
        let bound = ReturnPolicy::Sanitized(crate::names::SanitizerName::new("redactor"));
        let e = Engine::open(DeploymentPolicy {
            registry: cfg.clone(),
            planner_cap: crate::registry::PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: bound.clone(),
            profile: crate::profile::covering_declaration(&cfg),
        })
        .unwrap();
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let p = Projection::build(&log, Revision::new(1));
        let t = traj();
        let batch = e.seed_child(&p.view(&t), &TrajectoryId::new("t:child")).unwrap();
        match batch.facts.as_slice() {
            [
                Fact::Boundary {
                    kind: crate::fact::BoundaryKind::Fork { return_policy, .. },
                    ..
                },
            ] => assert_eq!(return_policy, &bound),
            other => panic!("expected the fork binding, got {other:?}"),
        }
    }

    #[test]
    fn an_opening_record_is_inert_in_projection_and_replay_validation() {
        let e = engine(vec![plain_tool("send")]);
        let t = traj();
        let opening = e.open_trajectory(&t).facts.remove(0);
        let admitted = user_value(known(SUSPICIOUS, Audience::Public));
        let with = [opening.clone(), admitted.clone()];
        let without = [admitted];
        let p_with = Projection::build(&with, Revision::new(2));
        let p_without = Projection::build(&without, Revision::new(1));
        assert_eq!(p_with.view(&t).current_label(), p_without.view(&t).current_label());
        assert_eq!(p_with.view(&t).boundary_count(), p_without.view(&t).boundary_count());
        assert_eq!(e.validate_replay(&with), Ok(()));
    }
}
