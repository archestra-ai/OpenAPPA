//! The engine: a pure function of the log's views and the immutable registry.

use thiserror::Error;

use crate::admit::{self, AdmitError, CastAnswer, CastError, ResultAdmission};
use crate::branch::{self, BranchError, ReturnSubmission};
use crate::check::{self, CheckOutcome, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::execute::{self, PlanError, Ruling};
use crate::fact::{Fact, FactBatch, ReturnPolicy, Revision};
use crate::label::EstablishedLabel;
use crate::params::{ArgumentError, CanonicalArguments};
use crate::plan::{self, PlannedBlock};
use crate::profile::{self, DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyIdentityV1};
use crate::projection::Views;
use crate::registry::{LoadError, Registry};
use crate::transition::{
    Blocked, EngineDecision, EngineEvent, EngineView, FollowUp, ProposalBatch, Released, TransitionError,
    ValidatedFactBatch,
};
use crate::value::{CanonicalDigest, DispatchId, ResolvedCall, ToolName, TrajectoryId};

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
    #[error("cast record names a value not admitted earlier in the log")]
    CastBeforeSource,
    #[error("cast record's trajectory neither admitted nor inherited the value it resolves")]
    ForeignResolution,
    #[error("cast record resolves a source that is already fully established")]
    RepeatResolution,
    #[error("cast record names unregistered cast {0}")]
    UnknownCast(String),
    #[error("cast record's resolution is not admissible for its source under the registered cast")]
    InadmissibleResolution,
    #[error("cast record's scope does not cover the source's originating tool")]
    OutOfScopeResolution,
    #[error("admitted value names a dispatch not opened earlier in the log")]
    UnknownDispatch,
    #[error("admitted value names a dispatch of another trajectory")]
    ForeignDispatch,
    #[error("fork record's snapshot is not the parent's frozen basis at that point in the log")]
    ForkBasisMismatch,
    #[error("fork record precedes the fork that opened its parent")]
    OutOfOrderFork,
    #[error("fork record names a child trajectory the log already used")]
    ChildActiveBeforeFork,
    #[error("one proposal batch identity is bound to two different decisions")]
    BatchIdentityConflict,
    #[error("a decision record claims a release its log never opened")]
    UnbackedDecision,
    #[error("fork record's return policy is not the deployment's child-return binding")]
    ForkReturnPolicyMismatch,
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

    /// Build the working view over a persisted family log: the records are validated
    /// before anything reads them, so no caller decides against an untrusted stream.
    /// On cache loss the runtime rebuilds through this same call.
    pub fn view(&self, records: Vec<Fact>, revision: Revision) -> Result<EngineView, ReplayError> {
        self.validate_replay(&records)?;
        Ok(EngineView::over(records, revision))
    }

    /// The engine's one mutation boundary: decide one event against the view and return
    /// a sealed batch plus the typed follow-up. The engine owns semantic validation and constructs
    /// every fact; it owns no mutable state.
    pub fn handle(&self, view: &EngineView, event: EngineEvent) -> Result<EngineDecision, TransitionError> {
        match event {
            EngineEvent::Proposals(batch) => self.decide_proposals(view, &batch),
        }
    }

    fn decide_proposals(&self, view: &EngineView, batch: &ProposalBatch) -> Result<EngineDecision, TransitionError> {
        match batch.proposals.len() {
            0 => return Err(TransitionError::EmptyBatch),
            1 => {}
            _ => return Err(TransitionError::UncomposedBatch),
        }
        let views = view.projection().view(&batch.trajectory);
        let payload = CanonicalDigest::of_batch(&batch.proposals);
        if let Some(decided) = views.decided_batch(&batch.id) {
            if decided.trajectory != batch.trajectory || decided.payload != payload {
                return Err(TransitionError::BatchIdentityConflict);
            }
            let recorded = decided.released.clone();
            return Ok(EngineDecision {
                append: None,
                follow_up: self.decided_follow_up(&views, batch, &recorded)?,
            });
        }

        let mut opened = Vec::new();
        let mut released = Vec::new();
        let mut blocked = Vec::new();
        for call in &batch.proposals {
            let contract = self.validated_contract(call)?;
            match check::evaluate(contract, &views, call) {
                CheckOutcome::Allow => {
                    let (dispatch, fact) = opened_dispatch(contract, &views, call);
                    opened.push(fact);
                    released.push(Released {
                        dispatch,
                        call: call.clone(),
                    });
                }
                CheckOutcome::Block(raw) => blocked.push(Blocked {
                    call: call.clone(),
                    block: plan::plan(&self.registry, &views, call, &raw),
                }),
            }
        }
        let mut facts = vec![Fact::ProposalBatchDecided {
            trajectory: batch.trajectory.clone(),
            batch: batch.id.clone(),
            payload,
            released: released.iter().map(|release| release.dispatch.clone()).collect(),
        }];
        facts.extend(opened);
        Ok(EngineDecision {
            append: Some(ValidatedFactBatch::seal(FactBatch::new(views.revision(), facts))),
            follow_up: FollowUp::Proposals {
                released,
                blocked,
                spent: Vec::new(),
            },
        })
    }

    fn decided_follow_up(
        &self,
        views: &Views,
        batch: &ProposalBatch,
        recorded: &[DispatchId],
    ) -> Result<FollowUp, TransitionError> {
        let mut released = Vec::new();
        let mut blocked = Vec::new();
        let mut spent = Vec::new();
        for call in &batch.proposals {
            let contract = self.validated_contract(call)?;
            match recorded.iter().find(|dispatch| dispatch.digest() == &call.digest()) {
                Some(dispatch) if views.is_open(dispatch) && !views.is_succeeded(dispatch) => released.push(Released {
                    dispatch: dispatch.clone(),
                    call: call.clone(),
                }),
                Some(_) => {}
                None => match check::evaluate(contract, views, call) {
                    CheckOutcome::Block(raw) => blocked.push(Blocked {
                        call: call.clone(),
                        block: plan::plan(&self.registry, views, call, &raw),
                    }),
                    CheckOutcome::Allow => spent.push(call.clone()),
                },
            }
        }
        Ok(FollowUp::Proposals {
            released,
            blocked,
            spent,
        })
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

    /// The narrowing admitting a cast-resolved value of `call` would fold into the live
    /// established bound, or `None` when it does not move it — the whole resolved label,
    /// established dimensions included (see `admit::pending_cast_narrowing`). The runtime derives
    /// the acceptance offer from this; admission re-derives it under the family lock, so a stale
    /// offer refuses by value (D2).
    pub fn cast_narrowing(
        &self,
        views: &Views,
        call: &ResolvedCall,
        resolved: &EstablishedLabel,
    ) -> Result<Option<Narrowing>, EngineError> {
        self.validated_contract(call)?;
        Ok(admit::pending_cast_narrowing(views, resolved))
    }

    /// Attach the sound remedies to a raw block: executable plans and prose recommendations. An empty
    /// result (no plans, no curative recommendation) is a proof the block is unliftable over the
    /// implemented remedy subset — see [`crate::plan`].
    pub fn plan(&self, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> Result<PlannedBlock, EngineError> {
        self.validated_contract(call)?;
        Ok(plan::plan(&self.registry, views, call, raw))
    }

    /// Establish an admitted Unknown value's complete label by a validated whole-source cast
    /// answer: one `CastApplied` fact or nothing.
    pub fn admit_cast(
        &self,
        views: &Views,
        value: crate::value::ValueId,
        answer: CastAnswer,
    ) -> Result<FactBatch, CastError> {
        admit::admit_cast(&self.registry, views, value, answer)
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

    /// Validate persisted facts against this registry before the log is trusted as replay
    /// input: sequentially, failing at the first impossible record.
    ///
    /// Dispatched calls: the named tool must be registered, the persisted payload must satisfy
    /// that tool's compiled schema, and the recomputed digest must match the `DispatchId`.
    /// A `CanonicalArguments` cannot prove which schema it was validated against, so
    /// replay re-establishes the binding here.
    ///
    /// Cast records: a `CastApplied` must name a value admitted earlier in the log, must be
    /// recorded by a trajectory that admitted that value or inherited it at its fork (`BRN-14`
    /// — the same local-or-inherited gate live admission applies), must be that source's first
    /// resolution (`UNK-8` — a repeat or conflicting resolution is refused, and a value
    /// admitted fully established takes none), and its complete label must be admissible for
    /// the source under the registered cast, and the cast's scope must cover
    /// the source's originating tool (`SAN-9` — a child return or user value takes unscoped
    /// casts only).
    ///
    /// Fork records: the recorded snapshot must be the parent's own basis frozen at that point
    /// — its base, its frozen inherited set plus every value it had admitted, and the seed they
    /// derive. This one basis re-derivation is a deliberate, scoped
    /// exception to the boundary below, because the frozen inherited set is what later cast
    /// records are authorized against: taking it verbatim would leave `BRN-14`'s sibling and
    /// post-fork exclusion unenforced on every persisted log. `T31` absorbs it.
    ///
    /// `OutputCastApplied`/`OutputCastLapsed` are audit-only — the projection folds nothing
    /// from them, so a crafted record cannot alter a trusted view; their cross-record
    /// consistency belongs to `T31`'s complete transition validator. So does re-deriving
    /// persisted label state: admitted labels are taken verbatim here, checked only against the
    /// rules above — a store that can rewrite rows can forge one until `T31` replays every
    /// transition.
    pub fn validate_replay(&self, facts: &[Fact]) -> Result<(), ReplayError> {
        struct ReplayValue<'a> {
            admitted: &'a crate::label::Label,
            trajectory: &'a TrajectoryId,
            provenance: &'a crate::value::Provenance,
            resolved: Option<crate::label::Label>,
        }
        impl ReplayValue<'_> {
            fn label(&self) -> &crate::label::Label {
                self.resolved.as_ref().unwrap_or(self.admitted)
            }
        }
        static MISSING_SOURCE: crate::label::Label = crate::label::Label::unknown();
        static NO_INHERITED: std::collections::BTreeSet<crate::value::ValueId> = std::collections::BTreeSet::new();
        fn label_at<'v>(values: &'v [ReplayValue<'_>], id: crate::value::ValueId) -> &'v crate::label::Label {
            usize::try_from(id.index())
                .ok()
                .and_then(|i| values.get(i))
                .map_or(&MISSING_SOURCE, |value| value.label())
        }
        let mut values: Vec<ReplayValue<'_>> = Vec::new();
        // Opened dispatches, for resolving a result value's originating tool.
        let mut dispatch_contracts: std::collections::BTreeMap<&DispatchId, &ToolContract> =
            std::collections::BTreeMap::new();
        let mut snapshots: std::collections::BTreeMap<&TrajectoryId, &crate::fact::ForkSnapshot> =
            std::collections::BTreeMap::new();
        let mut local: std::collections::BTreeMap<&TrajectoryId, Vec<usize>> = std::collections::BTreeMap::new();
        let forked_children: std::collections::BTreeSet<&TrajectoryId> = facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Boundary {
                    trajectory,
                    kind: crate::fact::BoundaryKind::Fork { .. },
                } => Some(trajectory),
                _ => None,
            })
            .collect();
        let mut active: std::collections::BTreeSet<&TrajectoryId> = std::collections::BTreeSet::new();
        // Each decided batch identity and what it is bound to.
        let mut decided: std::collections::BTreeSet<&crate::transition::ProposalBatchId> =
            std::collections::BTreeSet::new();
        let mut pending_release: std::collections::BTreeSet<&DispatchId> = std::collections::BTreeSet::new();
        // Each decision's claimed releases and the payload digest they must reproduce.
        let mut claimed_payloads: Vec<(Vec<DispatchId>, CanonicalDigest)> = Vec::new();
        // Dispatch identities already opened: one opening per identity.
        let mut seen_openings: std::collections::BTreeSet<&DispatchId> = std::collections::BTreeSet::new();
        // The rendered call each opening recorded, for re-deriving a decision's payload.
        let mut opened_calls: std::collections::BTreeMap<&DispatchId, ResolvedCall> = std::collections::BTreeMap::new();
        for fact in facts {
            let opens_trajectory = active.insert(fact.trajectory());
            match fact {
                Fact::ProposalBatchDecided {
                    trajectory,
                    batch,
                    payload,
                    released,
                } => {
                    if !decided.insert(batch) {
                        return Err(ReplayError::BatchIdentityConflict);
                    }
                    for dispatch in released {
                        if dispatch.trajectory() != trajectory || !pending_release.insert(dispatch) {
                            return Err(ReplayError::UnbackedDecision);
                        }
                    }
                    claimed_payloads.push((released.clone(), *payload));
                }
                Fact::DispatchOpened {
                    dispatch,
                    tool,
                    arguments,
                    dynamic_resolutions,
                    ..
                } => {
                    if dispatch.trajectory() != fact.trajectory() {
                        return Err(ReplayError::ForeignDispatch);
                    }
                    if !seen_openings.insert(dispatch) {
                        return Err(ReplayError::UnbackedDecision);
                    }
                    pending_release.remove(dispatch);
                    opened_calls.insert(
                        dispatch,
                        ResolvedCall::new(tool.clone(), arguments.clone())
                            .with_dynamic_resolutions(dynamic_resolutions.clone()),
                    );
                    if dispatch.trajectory() != fact.trajectory() {
                        return Err(ReplayError::ForeignDispatch);
                    }
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
                    dispatch_contracts.insert(dispatch, contract);
                }
                Fact::ValueAdmitted {
                    trajectory,
                    value,
                    provenance,
                } => {
                    if let crate::value::Provenance::ToolResult { dispatch } = provenance {
                        if !dispatch_contracts.contains_key(dispatch) {
                            return Err(ReplayError::UnknownDispatch);
                        }
                        if dispatch.trajectory() != trajectory {
                            return Err(ReplayError::ForeignDispatch);
                        }
                    }
                    local.entry(trajectory).or_default().push(values.len());
                    values.push(ReplayValue {
                        admitted: &value.label,
                        trajectory,
                        provenance,
                        resolved: None,
                    });
                }
                Fact::CastApplied {
                    trajectory,
                    value,
                    resolved,
                    cast,
                } => {
                    let inherited = snapshots
                        .get(trajectory)
                        .map_or(&NO_INHERITED, |snapshot| snapshot.inherited());
                    let acting_may_resolve = inherited.contains(value);
                    let source = usize::try_from(value.index())
                        .ok()
                        .and_then(|i| values.get_mut(i))
                        .ok_or(ReplayError::CastBeforeSource)?;
                    if trajectory != source.trajectory && !acting_may_resolve {
                        return Err(ReplayError::ForeignResolution);
                    }
                    if source.resolved.is_some() || EstablishedLabel::from_label(source.admitted).is_some() {
                        return Err(ReplayError::RepeatResolution);
                    }
                    let registered = self
                        .registry
                        .cast(cast)
                        .ok_or_else(|| ReplayError::UnknownCast(cast.as_str().to_string()))?;
                    let applicable = match source.provenance {
                        crate::value::Provenance::ToolResult { dispatch } => dispatch_contracts
                            .get(dispatch)
                            .is_some_and(|contract| registered.scope.covers(&contract.tags)),
                        crate::value::Provenance::UserInput | crate::value::Provenance::ChildReturn { .. } => {
                            registered.scope.is_unscoped()
                        }
                    };
                    if !applicable {
                        return Err(ReplayError::OutOfScopeResolution);
                    }
                    if registered.resolution.validate(source.admitted, resolved).is_err() {
                        return Err(ReplayError::InadmissibleResolution);
                    }
                    source.resolved = Some(resolved.clone().into_label());
                }
                Fact::Boundary {
                    trajectory,
                    kind:
                        crate::fact::BoundaryKind::Fork {
                            parent,
                            snapshot,
                            return_policy,
                        },
                } => {
                    if !opens_trajectory {
                        return Err(ReplayError::ChildActiveBeforeFork);
                    }
                    if return_policy != &self.child_return {
                        return Err(ReplayError::ForkReturnPolicyMismatch);
                    }
                    if !snapshots.contains_key(parent) && forked_children.contains(parent) {
                        return Err(ReplayError::OutOfOrderFork);
                    }
                    // The recorded snapshot must be the parent's own basis frozen at this point:
                    // its base, its frozen inherited set, every value it had admitted, and the
                    // seed they derive. Re-deriving it here is deliberately
                    // narrower than `T31`'s transition validator — the fork basis is what later
                    // cast records are authorized against, so it cannot be taken verbatim.
                    let parent_fork = snapshots.get(parent);
                    let inherited = parent_fork.map_or(&NO_INHERITED, |snapshot| snapshot.inherited());
                    let own = local.get(parent).map_or(&[][..], Vec::as_slice);
                    let sources = inherited
                        .iter()
                        .copied()
                        .chain(own.iter().map(|i| crate::value::ValueId::new(*i as u64)))
                        .map(|id| (id, label_at(&values, id)));
                    let base = parent_fork.map_or_else(EstablishedLabel::top, |snapshot| snapshot.base().clone());
                    if crate::fact::ForkSnapshot::freeze(base, sources) != *snapshot {
                        return Err(ReplayError::ForkBasisMismatch);
                    }
                    snapshots.insert(trajectory, snapshot);
                }
                _ => {}
            }
        }
        if !pending_release.is_empty() {
            return Err(ReplayError::UnbackedDecision);
        }
        for (released, payload) in claimed_payloads {
            let calls: Option<Vec<&ResolvedCall>> = released.iter().map(|id| opened_calls.get(id)).collect();
            if let Some(calls) = calls
                && !calls.is_empty()
                && CanonicalDigest::of_batch(calls) != payload
            {
                return Err(ReplayError::UnbackedDecision);
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
        proposed_label: check::committed_label_for_call(contract, &views.current_label(), call)
            .bound()
            .clone(),
        proposed_effects: contract.emits.clone(),
        dynamic_resolutions: call.dynamic_resolutions().to_vec(),
    };
    (dispatch, fact)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::check::Gap;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
    };
    use crate::fact::{EffectKind, EffectSet, Fact, Revision};
    use crate::label::PartialLabel;
    use crate::label::{Audience, Dim, Dimension, Label, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody, ValueId};
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

    fn established(trust: Trust, audience: Audience) -> EstablishedLabel {
        EstablishedLabel::new(trust, audience)
    }

    fn partial(trust: Trust, audience: Audience) -> PartialLabel {
        PartialLabel::established(EstablishedLabel::new(trust, audience))
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
    fn the_boundary_releases_an_allowed_proposal_with_its_dispatch() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(records, Revision::new(1)).unwrap();
        let call = call("get_ticket", json!({}));

        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    proposals: vec![call.clone()],
                }),
            )
            .unwrap();

        let released = match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } if blocked.is_empty() => released.clone(),
            other => panic!("expected a release, got {other:?}"),
        };
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].call, call);
        let composed = e.open_dispatch(&view.projection().view(&traj()), &call).unwrap();
        let appended = decision.append.expect("an allowed call opens a dispatch");
        assert!(matches!(
            &appended.facts()[0],
            Fact::ProposalBatchDecided { batch, .. } if batch.as_str() == "b1"
        ));
        assert_eq!(&appended.facts()[1..], composed.facts.as_slice());
        assert!(matches!(
            &appended.facts()[1],
            Fact::DispatchOpened { dispatch, .. } if dispatch == &released[0].dispatch
        ));
    }

    #[test]
    fn a_repeated_batch_identity_returns_its_recorded_decision_and_a_reused_one_is_refused() {
        let e = engine(vec![crm_tool()]);
        let internal = Audience::restricted([ReaderId::new("internal")]);
        let records = vec![user_value(known(TRUSTED, internal))];
        let view = e.view(records.clone(), Revision::new(1)).unwrap();
        let batch = |proposals: Vec<ResolvedCall>| {
            EngineEvent::Proposals(ProposalBatch {
                id: crate::transition::ProposalBatchId::new("b1"),
                trajectory: traj(),
                proposals,
            })
        };
        let proposal = call("get_ticket", json!({}));
        let other = call("get_ticket", json!({ "id": "2" }));
        let call = proposal;

        let first = e.handle(&view, batch(vec![call.clone()])).unwrap();
        let appended_facts = first
            .append
            .clone()
            .expect("the first decision records itself")
            .into_unsealed()
            .facts;
        let decided = [records, appended_facts.clone()].concat();
        let after = e.view(decided, Revision::new(2)).unwrap();

        let repeat = e.handle(&after, batch(vec![call.clone()])).unwrap();
        assert_eq!(repeat.append, None);
        assert_eq!(repeat.follow_up, first.follow_up);

        assert_eq!(
            e.handle(&after, batch(vec![other.clone()])),
            Err(crate::transition::TransitionError::BatchIdentityConflict)
        );

        let decision = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b1"),
            payload: CanonicalDigest::of_batch([&call]),
            released,
        };
        assert_eq!(
            e.validate_replay(&[decision(vec![]), decision(vec![])]),
            Err(ReplayError::BatchIdentityConflict)
        );
        let FollowUp::Proposals { released, .. } = &first.follow_up;
        let dispatch = released[0].dispatch.clone();
        assert_eq!(
            e.validate_replay(&[decision(vec![dispatch.clone()])]),
            Err(ReplayError::UnbackedDecision)
        );
        let other_id = |released: Vec<DispatchId>| Fact::ProposalBatchDecided {
            trajectory: traj(),
            batch: crate::transition::ProposalBatchId::new("b2"),
            payload: CanonicalDigest::of_batch([&call]),
            released,
        };
        let opening = appended_facts[1].clone();
        assert_eq!(
            e.validate_replay(&[
                decision(vec![dispatch.clone()]),
                other_id(vec![dispatch.clone()]),
                opening.clone()
            ]),
            Err(ReplayError::UnbackedDecision)
        );
        assert_eq!(
            e.validate_replay(&[decision(vec![dispatch.clone(), dispatch.clone()]), opening.clone()]),
            Err(ReplayError::UnbackedDecision)
        );
        assert_eq!(
            e.validate_replay(&[
                Fact::ProposalBatchDecided {
                    trajectory: traj(),
                    batch: crate::transition::ProposalBatchId::new("b3"),
                    payload: CanonicalDigest::of_batch([&other]),
                    released: vec![dispatch],
                },
                opening
            ]),
            Err(ReplayError::UnbackedDecision)
        );
    }

    #[test]
    fn a_repeat_of_a_block_that_has_lifted_reports_a_spent_identity() {
        let e = engine(vec![crm_tool()]);
        let public = vec![user_value(known(TRUSTED, Audience::Public))];
        let call = call("get_ticket", json!({}));
        let event = EngineEvent::Proposals(ProposalBatch {
            id: crate::transition::ProposalBatchId::new("b1"),
            trajectory: traj(),
            proposals: vec![call.clone()],
        });

        let view = e.view(public.clone(), Revision::new(1)).unwrap();
        let decision = e.handle(&view, event.clone()).unwrap();
        let decided = decision.append.expect("the block records its decision").into_unsealed();

        let internal = Audience::restricted([ReaderId::new("internal")]);
        let later = [public, decided.facts, vec![user_value(known(TRUSTED, internal))]].concat();
        let after = e.view(later, Revision::new(3)).unwrap();

        let FollowUp::Proposals {
            released,
            blocked,
            spent,
        } = e.handle(&after, event).unwrap().follow_up;
        assert!(released.is_empty() && blocked.is_empty());
        assert_eq!(spent, vec![call]);
    }

    #[test]
    fn the_boundary_plans_a_blocked_proposal_and_opens_nothing() {
        let e = engine(vec![crm_tool(), plain_tool("send")]);
        let records = vec![user_value(known(TRUSTED, Audience::Public))];
        let view = e.view(records, Revision::new(1)).unwrap();
        let call = call("get_ticket", json!({}));

        let decision = e
            .handle(
                &view,
                EngineEvent::Proposals(ProposalBatch {
                    id: crate::transition::ProposalBatchId::new("b1"),
                    trajectory: traj(),
                    proposals: vec![call.clone()],
                }),
            )
            .unwrap();

        let appended = decision.append.clone().expect("the decision boundary is recorded");
        assert!(matches!(appended.facts(), [Fact::ProposalBatchDecided { .. }],));
        match &decision.follow_up {
            FollowUp::Proposals { released, blocked, .. } if released.is_empty() => {
                assert_eq!(blocked.len(), 1);
                assert_eq!(blocked[0].call, call);
                assert!(blocked[0].block.raw.narrowing.is_some());
            }
            other => panic!("expected a planned block, got {other:?}"),
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
                        from: established(TRUSTED, both),
                        to: established(TRUSTED, Audience::restricted([ReaderId::new("a")])),
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
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Trust]));
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
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Trust]));
            }
            other => panic!("expected a three-slot block, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_and_an_unestablished_source_split_by_dimension() {
        let vault = ToolContract {
            name: ToolName::new("vault"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: None,
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![crate::contract::AudienceRequirement::Cap(Audience::restricted([
                        ReaderId::new("internal"),
                    ]))],
                },
                ..Requires::default()
            },
        };
        let e = engine(vec![vault]);
        let log = vec![user_value(Label::new(Dim::Known(SUSPICIOUS), Dim::Unknown))];
        match check(&e, &log, &call("vault", json!({}))) {
            CheckOutcome::Block(b) => {
                assert_eq!(
                    b.requirement_gaps,
                    vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS,
                    }]
                );
                assert_eq!(b.unestablished.len(), 1);
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Audience]));
            }
            other => panic!("expected a gap+unestablished block, got {other:?}"),
        }
    }

    #[test]
    fn replay_refuses_malformed_cast_history() {
        let classifier = crate::authority::Cast {
            name: crate::names::CastName::new("classifier"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![classifier],
        };
        let e = open_engine(cfg);
        let cast_fact = |value: u64, resolved: EstablishedLabel, cast: &str| Fact::CastApplied {
            trajectory: traj(),
            value: crate::value::ValueId::new(value),
            resolved,
            cast: crate::names::CastName::new(cast),
        };
        let unknown_source = user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        let good = cast_fact(0, established(SUSPICIOUS, Audience::Public), "classifier");

        assert_eq!(e.validate_replay(&[unknown_source.clone(), good.clone()]), Ok(()));
        assert_eq!(
            e.validate_replay(std::slice::from_ref(&good)),
            Err(ReplayError::CastBeforeSource)
        );
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), good.clone(), good.clone()]),
            Err(ReplayError::RepeatResolution)
        );
        assert_eq!(
            e.validate_replay(&[user_value(known(TRUSTED, Audience::Public)), good.clone()]),
            Err(ReplayError::RepeatResolution)
        );
        assert!(matches!(
            e.validate_replay(&[
                unknown_source.clone(),
                cast_fact(0, established(SUSPICIOUS, Audience::Public), "bogus")
            ]),
            Err(ReplayError::UnknownCast(name)) if name == "bogus"
        ));
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::CastApplied {
                    trajectory: TrajectoryId::new("sibling"),
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("classifier"),
                }
            ]),
            Err(ReplayError::ForeignResolution)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source,
                cast_fact(0, established(TRUSTED, Audience::Public), "classifier")
            ]),
            Err(ReplayError::InadmissibleResolution)
        );
    }

    #[test]
    fn replay_holds_a_fork_to_its_parents_frozen_basis() {
        let classifier = crate::authority::Cast {
            name: crate::names::CastName::new("classifier"),
            resolution: crate::authority::CastResolution::Resolver {
                may_cast: crate::authority::CastCeiling {
                    trust: vec![SUSPICIOUS],
                    audience: Audience::Public,
                },
            },
            scope: crate::authority::Scope::default(),
        };
        let e = open_engine(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![classifier],
        });
        let child = TrajectoryId::new("child");
        let unknown_source = user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)));
        let fork = |snapshot: crate::fact::ForkSnapshot| Fact::Boundary {
            trajectory: child.clone(),
            kind: crate::fact::BoundaryKind::Fork {
                parent: traj(),
                snapshot,
                return_policy: crate::fact::ReturnPolicy::Raw,
            },
        };
        let basis_after = |log: &[Fact]| {
            Projection::build(log, crate::fact::Revision::new(log.len() as u64))
                .view(&traj())
                .freeze_basis()
        };
        let resolve = |trajectory: TrajectoryId, value: u64| Fact::CastApplied {
            trajectory,
            value: crate::value::ValueId::new(value),
            resolved: established(SUSPICIOUS, Audience::Public),
            cast: crate::names::CastName::new("classifier"),
        };

        let opened = vec![unknown_source.clone()];
        let snapshot = basis_after(&opened);
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(snapshot.clone())]),
            Ok(())
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                fork(snapshot.clone()),
                resolve(child.clone(), 0)
            ]),
            Ok(())
        );

        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(basis_after(&[]))]),
            Err(ReplayError::ForkBasisMismatch)
        );
        let late = vec![unknown_source.clone(), unknown_source.clone()];
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), fork(basis_after(&late))]),
            Err(ReplayError::ForkBasisMismatch)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                fork(snapshot.clone()),
                unknown_source.clone(),
                resolve(child.clone(), 1)
            ]),
            Err(ReplayError::ForeignResolution)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::ValueAdmitted {
                    trajectory: child.clone(),
                    value: LabeledValue::new(ValueBody::new("early"), known(SUSPICIOUS, Audience::Public)),
                    provenance: Provenance::UserInput,
                },
                fork(snapshot.clone())
            ]),
            Err(ReplayError::ChildActiveBeforeFork)
        );
        let refork = vec![unknown_source.clone(), fork(snapshot.clone()), unknown_source.clone()];
        let widened = basis_after(&refork[..3]);
        assert_eq!(
            e.validate_replay(&[refork.clone(), vec![fork(widened)]].concat()),
            Err(ReplayError::ChildActiveBeforeFork)
        );
        assert_eq!(
            e.validate_replay(&[
                unknown_source.clone(),
                Fact::Boundary {
                    trajectory: child.clone(),
                    kind: crate::fact::BoundaryKind::Fork {
                        parent: traj(),
                        snapshot: snapshot.clone(),
                        return_policy: crate::fact::ReturnPolicy::Sanitized(crate::names::SanitizerName::new("redact")),
                    },
                }
            ]),
            Err(ReplayError::ForkReturnPolicyMismatch)
        );

        let grandchild = Fact::Boundary {
            trajectory: TrajectoryId::new("grandchild"),
            kind: crate::fact::BoundaryKind::Fork {
                parent: child.clone(),
                snapshot: crate::fact::ForkSnapshot::freeze(EstablishedLabel::top(), std::iter::empty()),
                return_policy: crate::fact::ReturnPolicy::Raw,
            },
        };
        assert_eq!(
            e.validate_replay(&[unknown_source.clone(), grandchild.clone(), fork(snapshot.clone())]),
            Err(ReplayError::OutOfOrderFork)
        );
        assert_eq!(
            e.validate_replay(&[unknown_source, fork(snapshot), grandchild]),
            Err(ReplayError::ForkBasisMismatch)
        );
    }

    #[test]
    fn replay_refuses_an_out_of_scope_resolution() {
        let fetch = crate::contract::ToolContract {
            name: ToolName::new("fetch"),
            tags: vec![crate::names::TagName::new("web")],
            delta: Some(crate::contract::Delta {
                trust: Some(Dim::Unknown),
                audience: Some(Dim::Known(Audience::Public).into()),
            }),
            parameters: crate::params::ToolParameters::open(),
            emits: crate::fact::EffectSet::default(),
            requires: Default::default(),
        };
        let webby = crate::authority::Cast {
            name: crate::names::CastName::new("webby"),
            resolution: crate::authority::CastResolution::Constant(established(SUSPICIOUS, Audience::Public)),
            scope: crate::authority::Scope {
                tags: vec![crate::names::TagName::new("web")],
            },
        };
        let cfg = RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into()]),
            tools: vec![fetch],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![webby],
        };
        let e = open_engine(cfg);
        assert_eq!(
            e.validate_replay(&[
                user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public))),
                Fact::CastApplied {
                    trajectory: traj(),
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("webby"),
                }
            ]),
            Err(ReplayError::OutOfScopeResolution)
        );
        let fetch_call = crate::value::ResolvedCall::new(
            ToolName::new("fetch"),
            crate::params::test_arguments(&serde_json::json!({})),
        );
        let dispatch = DispatchId::new(traj(), fetch_call.digest(), 0);
        let sibling = TrajectoryId::new("sibling");
        assert_eq!(
            e.validate_replay(&[
                Fact::DispatchOpened {
                    trajectory: traj(),
                    dispatch: dispatch.clone(),
                    tool: fetch_call.tool().clone(),
                    arguments: fetch_call.canonical_arguments().clone(),
                    proposed_label: EstablishedLabel::top(),
                    proposed_effects: crate::fact::EffectSet::default(),
                    dynamic_resolutions: Vec::new(),
                },
                Fact::ValueAdmitted {
                    trajectory: sibling.clone(),
                    value: crate::value::LabeledValue::new(
                        crate::value::ValueBody::new("page"),
                        Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                    ),
                    provenance: crate::value::Provenance::ToolResult { dispatch },
                },
                Fact::CastApplied {
                    trajectory: sibling,
                    value: crate::value::ValueId::new(0),
                    resolved: established(SUSPICIOUS, Audience::Public),
                    cast: crate::names::CastName::new("webby"),
                }
            ]),
            Err(ReplayError::ForeignDispatch)
        );
        assert_eq!(
            e.validate_replay(&[Fact::ValueAdmitted {
                trajectory: traj(),
                value: crate::value::LabeledValue::new(
                    crate::value::ValueBody::new("page"),
                    Label::new(Dim::Unknown, Dim::Known(Audience::Public)),
                ),
                provenance: crate::value::Provenance::ToolResult {
                    dispatch: DispatchId::new(traj(), fetch_call.digest(), 7),
                },
            }]),
            Err(ReplayError::UnknownDispatch)
        );
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
        assert_eq!(current.bound(), &EstablishedLabel::new(TRUSTED, Audience::Public));
        assert!(!current.is_established(Dimension::Trust));
        assert!(!current.is_established(Dimension::Audience));
        assert!(current.unresolved(Dimension::Trust).any(|id| id == ValueId::new(1)));
        assert!(current.unresolved(Dimension::Audience).any(|id| id == ValueId::new(1)));
    }

    #[test]
    fn an_unknown_trajectory_blocks_only_requirement_consuming_calls() {
        let e = engine(vec![unannotated_tool("noop"), crm_tool()]);
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Unknown))];
        assert_eq!(check(&e, &log, &call("noop", json!({}))), CheckOutcome::Allow);
        match check(&e, &log, &call("get_ticket", json!({}))) {
            CheckOutcome::Block(b) => {
                assert!(b.requirement_gaps.is_empty());
                assert_eq!(
                    b.unestablished,
                    vec![crate::check::UnestablishedFact {
                        value: ValueId::new(0),
                        dimensions: BTreeSet::from([Dimension::Trust, Dimension::Audience]),
                    }]
                );
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
                assert_eq!(b.unestablished[0].dimensions, BTreeSet::from([Dimension::Audience]));
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
            proposed_label: established(TRUSTED, Audience::Public),
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
                trajectory_label: partial(SUSPICIOUS, Audience::Public),
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
                assert_eq!(*proposed_label, established(TRUSTED, internal));
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
