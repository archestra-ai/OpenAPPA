//! Recovery routes: the advisory, bounded search over the remedies a block stands in (`RMD-20`).
//!
//! A remedy plan is one stage (`RMD-4`, `RMD-19`): the sound next alternatives from the live
//! candidate, each an executable offer. A **recovery route** looks further, over an abstract state
//! rather than the log: it composes the same steps — acceptance, rulings, an output sanitizer, an
//! input hop, and the tools an `RMD-13` plan names — and re-checks every later step in the state
//! the earlier steps produce. It appends no fact, mints no offer, and changes nothing in
//! `remedy_plans`, their order, or the emptiness assertion of `RMD-10`; every step it names still
//! passes its ordinary check when the agent attempts it.
//!
//! **What a route may plan over.** Call-bound decisions — a ruling, an acceptance of the blocked
//! call's narrowing, an input hop, a denial — bind the exact rendered call (`RUL-3`, `RMD-16`),
//! so they are planned for the blocked call only. A tool run first ([`RouteStep::Precede`]) has
//! no call yet: it contributes only what its registered contract fixes before any call exists —
//! [`StaticAnnotation`]: no annotator answers, no placeholder recipients, a declared and established
//! delta — evaluated in the success branch, where its `emits` are committed effects and its
//! declared narrowing has folded. Anything less determined ends the route as a
//! [`RouteOutcome::Prefix`] naming what must land before planning resumes ([`Resume`]).
//!
//! **Bound and totality.** [`RouteDepth`] is the number of calls a route spans, the blocked call
//! included. At `RouteDepth::ONE` the routes are exactly the RMD plans. A tool enters a route
//! only for a `prior` or cap gap of the call it immediately serves — the blocked call, or a
//! preceding tool that needs it first — as `RMD-13` names tool plans; a tool that clears nothing
//! for the call in front of it is never dispatched. A set of preceding tools is planned once, in
//! the first order the search meets (tool-name order): every order of the same tools folds the
//! same deltas and commits the same effects, so it reaches the same state and continues alike.
//! Within that shape enumeration is total: nothing is truncated, and an empty result asserts
//! only that no route exists within this abstraction and this depth, never that the block is
//! unliftable. Termination: the depth bounds the preceding tools, and no tool recurs on one
//! path. Cost grows with the sets of at most `depth − 1` registered tools, not their orderings;
//! the depth is the operator's bound (`CFG-26`), and the engine clamps nothing.
//!
//! **Order** (`RMD-20`). Least mandate power first, as `RMD-15` compares plans, then least
//! disclosure — the readers outside the committed audience that the route's rulings admit a flow
//! to — then fewer steps, then canonical bytes. The first two form a strict partial order; the
//! last two make the extraction total, so the list is deterministic.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::audience::{AudienceEvidence, EvidenceRefusal};
use crate::basis::SubjectKey;
use crate::candidate::CallStage;
use crate::check::{self, CallReads, CheckOutcome, Gap, Narrowing, RawBlock};
use crate::contract::{NotStatic, StaticAnnotation, ToolAnnotation};
use crate::engine::Engine;
use crate::fact::EffectKind;
use crate::label::{Audience, Expansions, Label, MembershipContext, MembershipNeeded, SymbolicAtom, WithinAssertions};
use crate::names::{AuthorityName, SanitizerName};
use crate::plan::{self, CallRole, ExecutableRemedyPlan, GapPower, RemedyStep};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{CanonicalDigest, ResolvedCall, ToolName};

/// How many calls a route may span, the blocked call included. `ONE` is today's direct
/// recovery: the RMD plans and nothing beyond them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteDepth(NonZeroU32);

impl RouteDepth {
    pub const ONE: RouteDepth = RouteDepth(NonZeroU32::MIN);

    pub fn new(calls: u32) -> Option<RouteDepth> {
        NonZeroU32::new(calls).map(RouteDepth)
    }

    pub fn calls(self) -> u32 {
        self.0.get()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRoute {
    pub steps: Vec<RouteStep>,
    pub outcome: RouteOutcome,
}

impl RecoveryRoute {
    /// `Guaranteed` when every requirement of the route is already determined — the agent's
    /// own acceptance and nothing external; otherwise the runtime outcomes it depends on.
    pub fn certainty(&self) -> Certainty {
        let contingencies: BTreeSet<Contingency> = self
            .steps
            .iter()
            .filter_map(|step| match step {
                RouteStep::Precede { tool, .. } => Some(Contingency::ToolOutcome { tool: tool.clone() }),
                RouteStep::Authorize { authority, .. } => Some(Contingency::AuthorityDecision {
                    authority: authority.clone(),
                }),
                RouteStep::Derive(sanitizer) | RouteStep::Sanitize(sanitizer) => Some(Contingency::SanitizerResult {
                    sanitizer: sanitizer.clone(),
                }),
                RouteStep::Accept(_) => None,
            })
            .collect();
        if contingencies.is_empty() {
            Certainty::Guaranteed
        } else {
            Certainty::Contingent(contingencies)
        }
    }
}

/// One step of a route, in the order the agent takes them. `Precede` runs another tool first;
/// the rest are the remedy steps of the blocked call's own stage, in their plan order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteStep {
    /// Dispatch this tool first: its own check passes in the state the route has reached, and
    /// on success its `emits` clear `clears` for the blocked call. `accepts` is the narrowing its
    /// declared delta commits, accepted at its own block (`RMD-13`).
    Precede {
        tool: ToolName,
        clears: Vec<Gap>,
        accepts: Option<Narrowing>,
    },
    Derive(SanitizerName),
    Accept(Narrowing),
    /// A ruling over `covers` for exactly this rendered call (`RUL-3`): the digest binds it.
    Authorize {
        authority: AuthorityName,
        covers: Vec<Gap>,
        call: CanonicalDigest,
    },
    Sanitize(SanitizerName),
}

/// `Complete` ends with the blocked call's release. `Prefix` stops earlier, where the next state
/// is not yet determined; the agent takes the prefix, and planning resumes from the realized log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteOutcome {
    Complete,
    Prefix(Resume),
}

/// What must land before planning can continue past a prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resume {
    /// The input hop's derived candidate: the next stage plans from it (`RMD-19`).
    DerivedCandidate { sanitizer: SanitizerName },
    /// The agent proposes `tool`, which clears `clears` for the blocked call; `halt` says why the
    /// search could not plan past that proposal.
    Propose {
        tool: ToolName,
        clears: Vec<Gap>,
        halt: Halt,
    },
}

/// Why a route stops at a tool it cannot plan across.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Halt {
    /// Its check reads the call's arguments (an annotator answer or a placeholder recipient).
    Arguments,
    /// Its own block carries call-bound gaps; its rulings or hops are planned at that block.
    Block(Vec<Gap>),
    /// The depth bound ends here.
    Depth,
}

/// A runtime outcome a route depends on. Planning assumes none of them: a ruling may deny, a tool
/// may fail, a sanitizer may derive nothing helpful.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Contingency {
    AuthorityDecision { authority: AuthorityName },
    ToolOutcome { tool: ToolName },
    SanitizerResult { sanitizer: SanitizerName },
}

/// What [`RecoveryRoute::certainty`] reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Certainty {
    Guaranteed,
    Contingent(BTreeSet<Contingency>),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("only a call subject stands in a block routes can be planned for")]
    NotACallSubject,
    #[error("no decided call stands for this subject in this trajectory")]
    UnknownSubject,
    #[error("the call passes its check; there is nothing to recover from")]
    NotBlocked,
    #[error("planning this block reads symbolic audiences no pinned answer decides: {0:?}")]
    MembershipNeeded(Vec<SymbolicAtom>),
    #[error(transparent)]
    Evidence(#[from] EvidenceRefusal),
    #[error("the view was built under another policy")]
    ForeignView,
}

impl From<MembershipNeeded> for RouteError {
    fn from(needed: MembershipNeeded) -> RouteError {
        RouteError::MembershipNeeded(needed.needed)
    }
}

/// The blocked call as the engine would re-plan it: the standing candidate, its stage and role,
/// the denials bound to its digest, and the expansions the surfacing act consumed plus what the
/// caller answers now.
pub(crate) struct BlockContext {
    contract: ToolAnnotation,
    call: ResolvedCall,
    stage: CallStage,
    role: CallRole,
    denied: BTreeSet<AuthorityName>,
    expansions: Expansions,
    raw: RawBlock,
}

impl BlockContext {
    pub(crate) fn reconstruct(
        engine: &Engine,
        views: &Views,
        subject: &SubjectKey,
        answers: &AudienceEvidence,
    ) -> Result<BlockContext, RouteError> {
        let SubjectKey::Call { batch, .. } = subject else {
            return Err(RouteError::NotACallSubject);
        };
        let registry = engine.registry();
        let call = views.standing_call(subject).ok_or(RouteError::UnknownSubject)?.clone();
        let decided = views.decided_batch(batch).ok_or(RouteError::UnknownSubject)?;
        let contract = registry
            .annotation_of(&call)
            .ok_or(RouteError::UnknownSubject)?
            .into_owned();

        let mut evidence = answers.clone();
        if let Some((_, offers)) = views.pending_block(subject) {
            for (offer, _) in &offers {
                if let Some(recorded) = views.offer(offer) {
                    evidence = evidence.inheriting(&recorded.evidence)?;
                }
            }
        }
        evidence = evidence.inheriting(&decided.evidence)?;
        // A candidate an input hop derived may stand under another contract than the proposal;
        // the atoms that contract reads were pinned by the hop.
        evidence = evidence.inheriting(&views.candidate_evidence(subject))?;
        let expansions = registry.audience().expansions(&evidence)?;

        let stage = views.call_stage(subject);
        let role = views.call_role(subject);
        let audience = registry.audience();
        let membership = MembershipContext::new(audience.within_assertions(), audience.providers(), &expansions);
        let raw = match check::evaluate(&contract, views, &call, &stage, &membership) {
            Ok(CheckOutcome::Allow) => return Err(RouteError::NotBlocked),
            Ok(CheckOutcome::Block(raw)) => raw,
            Err(needed) => return Err(needed.into()),
        };
        let denied = views.denied_authorities(&call.digest()).cloned().unwrap_or_default();
        Ok(BlockContext {
            contract,
            call,
            stage,
            role,
            denied,
            expansions,
            raw,
        })
    }
}

/// The abstract security state a route has reached: the branch's label and the effect kinds the
/// route's preceding tools have committed on top of the log. Reservations are the log's own and never move: a tool the route runs is taken in its
/// success branch, where its reservation has become effects.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteState {
    label: Label,
    committed: BTreeSet<EffectKind>,
}

impl RouteState {
    fn after(&self, tool: &StaticAnnotation<'_>) -> RouteState {
        let contract = tool.annotation();
        RouteState {
            label: check::committed_label(contract, &self.label),
            committed: self.committed.iter().chain(contract.emits.iter()).cloned().collect(),
        }
    }
}

/// The steps taken so far — the tools the route has run are the `Precede` steps among them —
/// and the tools it is planning to run (the goal stack). Together they are the no-repeat rule
/// that, with the depth bound, ends the search; apart, they identify a visit (see
/// [`Search::first_visit`]).
#[derive(Clone, Default)]
struct Path {
    steps: Vec<RouteStep>,
    goals: BTreeSet<ToolName>,
}

impl Path {
    fn run(&self) -> BTreeSet<ToolName> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                RouteStep::Precede { tool, .. } => Some(tool.clone()),
                _ => None,
            })
            .collect()
    }

    fn excludes(&self, tool: &ToolName) -> bool {
        self.goals.contains(tool) || self.run().contains(tool)
    }

    fn extend(&self, steps: &[RouteStep]) -> Path {
        let mut next = self.clone();
        next.steps.extend_from_slice(steps);
        next
    }

    fn with_goal(&self, tool: &ToolName) -> Path {
        let mut next = self.clone();
        next.goals.insert(tool.clone());
        next
    }
}

/// One planning point: the call being planned (`None` for the blocked call), the tools run so
/// far, and the goals above it. The state and the budget at a point are functions of the tools
/// run, and the continuation past it of the goals, so a second visit adds only another order of
/// the same preceding tools.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Visit {
    tool: Option<ToolName>,
    run: BTreeSet<ToolName>,
    goals: BTreeSet<ToolName>,
}

/// One attempt to run a preceding tool: it ran (in the success branch) and the state moved, or
/// the route stops at it.
enum Run {
    Ran {
        steps: Vec<RouteStep>,
        state: RouteState,
        budget: u32,
    },
    Halted {
        steps: Vec<RouteStep>,
        resume: Resume,
    },
}

impl Run {
    fn prefixed(self, mut before: Vec<RouteStep>) -> Run {
        match self {
            Run::Ran { steps, state, budget } => {
                before.extend(steps);
                Run::Ran {
                    steps: before,
                    state,
                    budget,
                }
            }
            Run::Halted { steps, resume } => {
                before.extend(steps);
                Run::Halted { steps: before, resume }
            }
        }
    }
}

/// A route as found, with what its order needs: the audience the route's rulings disclose to and
/// the mandate power it assigns per requirement.
struct Found {
    route: RecoveryRoute,
    disclosure: Vec<Audience>,
    powers: Vec<(Gap, Power)>,
    /// The block's requirement keys plus every key `powers` assigns.
    keys: Vec<Gap>,
}

enum Power {
    Substitution(Audience),
    Ruling {
        authority: AuthorityName,
        reader_ceiling: Option<Audience>,
    },
}

/// Every route within `depth`, least mandate power first; empty asserts only that no route
/// exists within this abstraction and `depth`. `Err` names the groups a planned state reads
/// that no expansion answers — the caller answers them and asks again, as the engine refuses a
/// batch.
pub(crate) fn search(
    registry: &Registry,
    views: &Views,
    context: &BlockContext,
    depth: RouteDepth,
) -> Result<Vec<RecoveryRoute>, RouteError> {
    let mut search = Search {
        registry,
        views,
        context,
        visited: BTreeSet::new(),
    };
    let initial = RouteState {
        label: views.current_label(),
        committed: BTreeSet::new(),
    };
    let mut found = Vec::new();
    search.target(&initial, &Path::default(), depth.calls() - 1, &mut found)?;
    Ok(search.ordered(found))
}

struct Search<'a> {
    registry: &'a Registry,
    views: &'a Views<'a>,
    context: &'a BlockContext,
    visited: BTreeSet<Visit>,
}

impl<'a> Search<'a> {
    /// The membership context planning reads: the policy's assertions and providers beside
    /// the answers the block's pinned evidence recomputes to.
    fn membership(&self) -> MembershipContext<'a> {
        let audience = self.registry.audience();
        MembershipContext::new(
            audience.within_assertions(),
            audience.providers(),
            &self.context.expansions,
        )
    }

    fn first_visit(&mut self, tool: Option<&ToolName>, path: &Path) -> bool {
        let mut goals = path.goals.clone();
        if let Some(tool) = tool {
            goals.remove(tool);
        }
        self.visited.insert(Visit {
            tool: tool.cloned(),
            run: path.run(),
            goals,
        })
    }

    fn has_committed<'s>(&'s self, state: &'s RouteState) -> impl Fn(&EffectKind) -> bool + 's {
        move |kind| self.views.has_effect(kind) || state.committed.contains(kind)
    }

    fn has_reserved(&self) -> impl Fn(&EffectKind) -> bool + '_ {
        |kind| self.views.has_reservation(kind)
    }

    /// The blocked call's stage in `state`: its own plans, then every tool that may run first.
    /// `budget` is how many preceding tools the depth still allows.
    fn target(
        &mut self,
        state: &RouteState,
        path: &Path,
        budget: u32,
        found: &mut Vec<Found>,
    ) -> Result<(), MembershipNeeded> {
        if !self.first_visit(None, path) {
            return Ok(());
        }
        let context = self.context;
        let membership = self.membership();
        let eval = check::evaluate_state(
            &context.contract,
            &state.label,
            &self.has_committed(state),
            &self.has_reserved(),
            CallReads::Resolved(&context.call),
            &context.stage,
            &membership,
        )?;
        if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
            found.push(self.found(
                path.steps.clone(),
                RouteOutcome::Complete,
                state,
                &eval.requirement_gaps,
            ));
        } else {
            // The same gate live planning holds a surfaced block to: every atom the
            // enumeration may consult is answered, or the missing ones are the ask. Without
            // it, an unanswered mandate would silently drop this state's plans from the
            // advisory menu instead of refusing the search.
            let mut unanswered: Vec<SymbolicAtom> =
                plan::block_atoms(self.registry, &context.contract, &eval, context.role)
                    .into_iter()
                    .filter(|atom| !self.context.expansions.answered(atom))
                    .collect();
            if !unanswered.is_empty() {
                unanswered.sort();
                unanswered.dedup();
                return Err(MembershipNeeded { needed: unanswered });
            }
            for plan in plan::enumerate_plans(
                self.registry,
                &context.contract,
                &state.label,
                &self.has_committed(state),
                &self.has_reserved(),
                &context.call,
                &context.stage,
                context.role,
                &membership,
            )? {
                if context.denied.iter().any(|authority| plan.names_authority(authority)) {
                    continue;
                }
                let mut steps = path.steps.clone();
                match plan.hop() {
                    Some(sanitizer) => {
                        steps.push(RouteStep::Derive(sanitizer.clone()));
                        let resume = Resume::DerivedCandidate {
                            sanitizer: sanitizer.clone(),
                        };
                        found.push(self.found(steps, RouteOutcome::Prefix(resume), state, &eval.requirement_gaps));
                    }
                    None => {
                        steps.extend(self.plan_steps(&plan));
                        found.push(self.found(steps, RouteOutcome::Complete, state, &eval.requirement_gaps));
                    }
                }
            }
        }
        // A terminal plan covers every gap and no ruling covers `prior` or a cap (CHK-12), so a
        // tool that clears something exists only where no terminal plan does: the RMD-13
        // condition on tool plans holds here by construction, at every depth.
        for tool in self
            .registry
            .tools()
            .filter_map(crate::contract::ToolDeclaration::declared)
        {
            if path.excludes(&tool.name) {
                continue;
            }
            let mut undecided = plan::NeededAtoms::default();
            let clears = plan::direct_clears(tool, &eval.requirement_gaps, &state.label, &membership, &mut undecided);
            undecided.refuse_if_any()?;
            if clears.is_empty() {
                continue;
            }
            for run in self.run(tool, clears, state, path, budget)? {
                match run {
                    Run::Ran {
                        steps,
                        state: next,
                        budget: left,
                    } => self.target(&next, &path.extend(&steps), left, found)?,
                    Run::Halted { steps, resume } => {
                        let mut all = path.steps.clone();
                        all.extend(steps);
                        found.push(self.found(all, RouteOutcome::Prefix(resume), state, &eval.requirement_gaps));
                    }
                }
            }
        }
        Ok(())
    }

    /// Every way `tool` runs first from `state`, with `budget` preceding tools still allowed,
    /// this one included. Its own `prior`/cap gaps recurse into the tools that clear them; any
    /// other gap or a call-dependent contract halts the route here.
    fn run(
        &mut self,
        tool: &ToolAnnotation,
        clears: Vec<Gap>,
        state: &RouteState,
        path: &Path,
        budget: u32,
    ) -> Result<Vec<Run>, MembershipNeeded> {
        if !self.first_visit(Some(&tool.name), path) {
            return Ok(Vec::new());
        }
        let halt = |halt: Halt| {
            Ok(vec![Run::Halted {
                steps: Vec::new(),
                resume: Resume::Propose {
                    tool: tool.name.clone(),
                    clears: clears.clone(),
                    halt,
                },
            }])
        };
        if budget == 0 {
            return halt(Halt::Depth);
        }
        let membership = self.membership();
        let static_contract = match StaticAnnotation::of(tool) {
            Ok(static_contract) => static_contract,
            Err(NotStatic) => return halt(Halt::Arguments),
        };
        let eval = check::evaluate_static(
            &static_contract,
            &state.label,
            &self.has_committed(state),
            &self.has_reserved(),
            &membership,
        )?;
        if eval
            .requirement_gaps
            .iter()
            .any(|gap| !matches!(gap, Gap::Prior(_) | Gap::Cap { .. }))
        {
            return halt(Halt::Block(eval.requirement_gaps));
        }
        if eval.requirement_gaps.is_empty() {
            return Ok(vec![Run::Ran {
                steps: vec![RouteStep::Precede {
                    tool: tool.name.clone(),
                    clears,
                    accepts: eval.narrowing,
                }],
                state: state.after(&static_contract),
                budget: budget - 1,
            }]);
        }
        let goal = path.with_goal(&tool.name);
        let mut runs = Vec::new();
        for first in self
            .registry
            .tools()
            .filter_map(crate::contract::ToolDeclaration::declared)
        {
            if goal.excludes(&first.name) {
                continue;
            }
            let mut undecided = plan::NeededAtoms::default();
            let first_clears =
                plan::direct_clears(first, &eval.requirement_gaps, &state.label, &membership, &mut undecided);
            undecided.refuse_if_any()?;
            if first_clears.is_empty() {
                continue;
            }
            for run in self.run(first, first_clears, state, &goal, budget - 1)? {
                match run {
                    Run::Ran {
                        steps,
                        state: mid,
                        budget: left,
                    } => {
                        for then in self.run(tool, clears.clone(), &mid, &goal.extend(&steps), left + 1)? {
                            runs.push(then.prefixed(steps.clone()));
                        }
                    }
                    halted @ Run::Halted { .. } => runs.push(halted),
                }
            }
        }
        Ok(runs)
    }

    fn plan_steps(&self, plan: &ExecutableRemedyPlan) -> Vec<RouteStep> {
        plan.steps
            .iter()
            .map(|step| match step {
                RemedyStep::Accept(narrowing) => RouteStep::Accept(narrowing.clone()),
                RemedyStep::Authorize(authority) => RouteStep::Authorize {
                    authority: authority.clone(),
                    covers: plan
                        .required
                        .iter()
                        .find(|required| &required.authority == authority)
                        .map(|required| required.covers.clone())
                        .unwrap_or_default(),
                    call: self.context.call.digest(),
                },
                RemedyStep::Sanitize(sanitizer) => RouteStep::Sanitize(sanitizer.clone()),
                RemedyStep::Derive(sanitizer) => RouteStep::Derive(sanitizer.clone()),
            })
            .collect()
    }

    /// A route as emitted from `state`, whose requirement gaps are `gaps`.
    fn found(&self, steps: Vec<RouteStep>, outcome: RouteOutcome, state: &RouteState, gaps: &[Gap]) -> Found {
        let disclosure = self.disclosure(&steps, &state.label.audience);
        let powers = self.powers(&steps, gaps);
        let mut keys: Vec<Gap> = self.context.raw.requirement_gaps.iter().map(requirement_key).collect();
        for (key, _) in &powers {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        Found {
            route: RecoveryRoute { steps, outcome },
            disclosure,
            powers,
            keys,
        }
    }

    /// The recipient sets the route's rulings admit a flow to beyond the committed audience:
    /// one entry per ruling-covered `includes` gap not derivably inside `audience`. Nothing
    /// else discloses — a trust floor or a waiver escalates without widening the readership,
    /// and a hop clears its gap with derived bytes. Derivability ranks routes only; the exact
    /// comparison at release time is untouched.
    fn disclosure(&self, steps: &[RouteStep], audience: &Audience) -> Vec<Audience> {
        let within = self.registry.audience().within_assertions();
        let mut disclosed: Vec<Audience> = Vec::new();
        for step in steps {
            let RouteStep::Authorize { covers, .. } = step else {
                continue;
            };
            for gap in covers {
                let Gap::Includes { recipients } = gap else {
                    continue;
                };
                let admitted = Audience::of_declared(recipients);
                if admitted.derives_within_audience(audience, within) {
                    continue;
                }
                if !disclosed.contains(&admitted) {
                    disclosed.push(admitted);
                }
            }
        }
        disclosed
    }

    /// The mandate power the route assigns per requirement it clears by a ruling or a hop, as
    /// `RMD-15` assigns it to a plan: a ruling's mandate, or the hop's `to` for each `includes`
    /// among `gaps` — the gaps of the state the hop was chosen in — it improves. Tools run first
    /// and acceptance assign none.
    fn powers(&self, steps: &[RouteStep], gaps: &[Gap]) -> Vec<(Gap, Power)> {
        let within = self.registry.audience().within_assertions();
        let mut powers = Vec::new();
        for step in steps {
            match step {
                RouteStep::Authorize { authority, covers, .. } => {
                    let mandate = &self
                        .registry
                        .authority(authority)
                        .expect("a route names only registered authorities")
                        .mandate;
                    for gap in covers {
                        let reader_ceiling = match gap {
                            Gap::Includes { .. } => mandate.reader_ceiling.as_ref().map(Audience::of_declared),
                            _ => None,
                        };
                        powers.push((
                            requirement_key(gap),
                            Power::Ruling {
                                authority: authority.clone(),
                                reader_ceiling,
                            },
                        ));
                    }
                }
                RouteStep::Derive(sanitizer) => {
                    let transition = &self
                        .registry
                        .sanitizer(sanitizer)
                        .expect("a route names only registered sanitizers")
                        .transition;
                    let crate::authority::DeclaredTransition::Audience { to, .. } = transition else {
                        continue;
                    };
                    let to = Audience::of_declared(to);
                    for gap in gaps {
                        if let Gap::Includes { recipients } = gap
                            && Audience::of_declared(recipients).derives_within_audience(&to, within)
                        {
                            powers.push((requirement_key(gap), Power::Substitution(to.clone())));
                        }
                    }
                }
                RouteStep::Precede { .. } | RouteStep::Accept(_) | RouteStep::Sanitize(_) => {}
            }
        }
        powers
    }

    fn power_of<'f>(&'f self, found: &'f Found, key: &Gap) -> GapPower<'f> {
        match found.powers.iter().find(|(gap, _)| gap == key) {
            None => GapPower::None,
            Some((_, Power::Substitution(to))) => GapPower::Substitution(to),
            Some((
                _,
                Power::Ruling {
                    authority,
                    reader_ceiling,
                },
            )) => GapPower::Ruling {
                mandate: &self
                    .registry
                    .authority(authority)
                    .expect("a route names only registered authorities")
                    .mandate,
                reader_ceiling: reader_ceiling.as_ref(),
            },
        }
    }

    /// Route `a` strictly precedes `b`: no requirement assigns more power in `a`, `a` discloses
    /// no reader `b` does not, and at least one of those is strict. Requirements compared are the
    /// block's own plus any either route clears by a ruling (a route needing an extra ruling
    /// assigns power where the other assigns none).
    fn precedes(&self, a: &Found, b: &Found) -> bool {
        let within = self.registry.audience().within_assertions();
        let mut strictly_less = false;
        for key in a.keys.iter().chain(b.keys.iter().filter(|key| !a.keys.contains(key))) {
            match plan::gap_power_cmp(key, &self.power_of(a, key), &self.power_of(b, key), within) {
                Some(Ordering::Less) => strictly_less = true,
                Some(Ordering::Equal) => {}
                Some(Ordering::Greater) | None => return false,
            }
        }
        match disclosure_cmp(&a.disclosure, &b.disclosure, within) {
            Some(Ordering::Less) => strictly_less = true,
            Some(Ordering::Equal) => {}
            Some(Ordering::Greater) | None => return false,
        }
        strictly_less
    }

    /// The total order: repeatedly place the route no unplaced route precedes, breaking ties by
    /// step count, then the steps' canonical bytes, then the whole route's.
    fn ordered(&self, found: Vec<Found>) -> Vec<RecoveryRoute> {
        let ties: Vec<(usize, Vec<u8>, Vec<u8>)> = found
            .iter()
            .map(|found| {
                (
                    found.route.steps.len(),
                    serde_json_canonicalizer::to_vec(&found.route.steps).expect("steps canonicalize"),
                    serde_json_canonicalizer::to_vec(&found.route).expect("a route canonicalizes"),
                )
            })
            .collect();
        let mut after: Vec<Vec<usize>> = vec![Vec::new(); found.len()];
        let mut unplaced_before = vec![0usize; found.len()];
        for (j, before) in found.iter().enumerate() {
            for (i, later) in found.iter().enumerate() {
                if self.precedes(before, later) {
                    after[j].push(i);
                    unplaced_before[i] += 1;
                }
            }
        }
        let mut slots: Vec<Option<Found>> = found.into_iter().map(Some).collect();
        let mut ordered = Vec::with_capacity(slots.len());
        for _ in 0..slots.len() {
            let minimal = (0..slots.len())
                .filter(|&i| slots[i].is_some() && unplaced_before[i] == 0)
                .min_by_key(|&i| &ties[i])
                .expect("a strict partial order has a minimal element");
            for &i in &after[minimal] {
                unplaced_before[i] -= 1;
            }
            ordered.push(slots[minimal].take().expect("chosen among the unplaced").route);
        }
        ordered
    }
}

/// A requirement's identity for comparing power across routes: a trust floor names its floor
/// only, since the `actual` rank moves with the state a route reaches.
fn requirement_key(gap: &Gap) -> Gap {
    match gap {
        Gap::TrustFloor { required, .. } => Gap::TrustFloor {
            required: *required,
            actual: *required,
        },
        other => other.clone(),
    }
}

fn disclosure_cmp(a: &[Audience], b: &[Audience], within: &WithinAssertions) -> Option<Ordering> {
    let covered = |of: &[Audience], by: &[Audience]| {
        of.iter()
            .all(|entry| by.iter().any(|holder| entry.derives_within_audience(holder, within)))
    };
    plan::inclusion_cmp(covered(a, b), covered(b, a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, Mandate, Sanitizer, SanitizerPoints, Scope};
    use crate::contract::{AudienceRequirement, Delta, HistoryRequirement, RecipientSpec, Requires, ToolAnnotation};
    use crate::fact::{CloseOutcome, EffectSet, Fact};
    use crate::label::DeclaredAudience;
    use crate::label::{Label, ReaderId, Trust};
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{DispatchId, TrajectoryId};
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);
    const VETTED: Trust = Trust::new(2);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn readers(names: &[&str]) -> Audience {
        Audience::restricted(names.iter().map(|name| ReaderId::new(*name)))
    }

    fn effect(kind: &str) -> EffectKind {
        EffectKind::new(kind)
    }

    fn tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn emitting(name: &str, kinds: &[&str]) -> ToolAnnotation {
        let mut contract = tool(name);
        contract.emits = EffectSet::new(kinds.iter().map(|kind| effect(kind))).unwrap();
        contract
    }

    fn requiring_prior(mut contract: ToolAnnotation, kinds: &[&str]) -> ToolAnnotation {
        contract
            .requires
            .history
            .extend(kinds.iter().map(|kind| HistoryRequirement::Prior(effect(kind))));
        contract
    }

    fn requiring_no_prior(mut contract: ToolAnnotation, kind: &str) -> ToolAnnotation {
        contract
            .requires
            .history
            .push(HistoryRequirement::NoPrior(effect(kind)));
        contract
    }

    fn requiring_trust(mut contract: ToolAnnotation, floor: Trust) -> ToolAnnotation {
        contract.requires.label.trust_floor = Some(floor);
        contract
    }

    fn requiring_includes(mut contract: ToolAnnotation, recipients: Audience) -> ToolAnnotation {
        contract
            .requires
            .label
            .audience
            .push(AudienceRequirement::Includes(RecipientSpec::Static(
                DeclaredAudience::literal(recipients),
            )));
        contract
    }

    fn requiring_cap(mut contract: ToolAnnotation, cap: Audience) -> ToolAnnotation {
        contract
            .requires
            .label
            .audience
            .push(AudienceRequirement::Cap(DeclaredAudience::literal(cap)));
        contract
    }

    fn narrowing_to(mut contract: ToolAnnotation, trust: Option<Trust>, audience: Option<Audience>) -> ToolAnnotation {
        contract.delta = Delta {
            trust,
            audience: audience.map(DeclaredAudience::literal),
        };
        contract
    }

    fn authority(name: &str, mandate: Mandate) -> Authority {
        Authority {
            name: AuthorityName::new(name),
            mandate,
            scope: Scope::default(),
            hint: None,
        }
    }

    fn trust_authority(name: &str, ceiling: Trust) -> Authority {
        authority(
            name,
            Mandate {
                trust_ceiling: Some(ceiling),
                ..Mandate::default()
            },
        )
    }

    fn reader_authority(name: &str, ceiling: Audience) -> Authority {
        authority(
            name,
            Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(ceiling)),
                ..Mandate::default()
            },
        )
    }

    fn input_redaction(name: &str, from: Audience, to: Audience) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: crate::authority::DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(from),
                to: DeclaredAudience::literal(to),
            },
            scope: Scope::default(),
            hint: None,
        }
    }

    struct Deployment {
        tools: Vec<ToolAnnotation>,
        authorities: Vec<Authority>,
        sanitizers: Vec<Sanitizer>,
        audience: crate::audience::AudienceConfig,
    }

    impl Deployment {
        fn of(tools: Vec<ToolAnnotation>) -> Deployment {
            Deployment {
                tools,
                authorities: vec![],
                sanitizers: vec![],
                audience: crate::audience::AudienceConfig::default(),
            }
        }

        fn authorities(mut self, authorities: Vec<Authority>) -> Deployment {
            self.authorities = authorities;
            self
        }

        fn sanitizers(mut self, sanitizers: Vec<Sanitizer>) -> Deployment {
            self.sanitizers = sanitizers;
            self
        }

        fn registry(self) -> Registry {
            Registry::build_covered(RegistryConfig {
                trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into(), "vetted".into()]),
                tools: self
                    .tools
                    .into_iter()
                    .map(crate::contract::ToolDeclaration::Declared)
                    .collect(),
                annotators: vec![],
                authorities: self.authorities,
                sanitizers: self.sanitizers,
                audience: self.audience,
            })
            .unwrap()
        }
    }

    fn opened(trust: Trust, audience: Audience) -> Fact {
        crate::profile::opening_at(traj(), Label::new(trust, audience))
    }

    fn seed_call(tag: &str) -> ResolvedCall {
        ResolvedCall::new(
            ToolName::new("seed"),
            crate::params::test_arguments(&json!({ "k": tag })),
        )
    }

    fn committed(kind: &str) -> Fact {
        Fact::DispatchClosed {
            trajectory: traj(),
            dispatch: DispatchId::new(traj(), seed_call(kind).digest(), 0),
            outcome: CloseOutcome::Success {
                effects: EffectSet::new([effect(kind)]).unwrap(),
            },
        }
    }

    fn dispatched(tag: &str, kinds: &[&str]) -> Fact {
        let seed = seed_call(tag);
        Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: DispatchId::new(traj(), seed.digest(), 0),
            tool: seed.tool().clone(),
            declaration: seed.declaration_id(),
            arguments: seed.canonical_arguments().clone(),
            proposed_label: Label::new(TRUSTED, Audience::public()),
            receiving: Label::new(TRUSTED, Audience::public()),
            proposed_effects: EffectSet::new(kinds.iter().map(|kind| effect(kind))).unwrap(),
            annotation: None,
            subject: crate::basis::fixture_subject(&traj()),
            evidence: crate::audience::AudienceEvidence::default(),
        }
    }

    fn reserved(kind: &str) -> Fact {
        dispatched(kind, &[kind])
    }

    fn call(name: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(name), crate::params::test_arguments(&args))
    }

    fn depth(calls: u32) -> RouteDepth {
        RouteDepth::new(calls).unwrap()
    }

    fn raw_block(registry: &Registry, views: &Views, call: &ResolvedCall) -> RawBlock {
        let contract = registry.annotation_of(call).unwrap();
        let parts = crate::label::TestContext::default();
        match check::evaluate(&contract, views, call, &CallStage::default(), &parts.context()) {
            Ok(CheckOutcome::Block(raw)) => raw,
            other => panic!("expected a block, got {other:?}"),
        }
    }

    /// The routes of a blocked call, planned as the engine reconstructs the block from a log.
    fn routes(registry: &Registry, log: &[Fact], call: &ResolvedCall, depth: RouteDepth) -> Vec<RecoveryRoute> {
        routes_with(registry, log, call, depth, &AudienceEvidence::default())
            .expect("no planned state reads a symbolic audience")
    }

    fn routes_with(
        registry: &Registry,
        log: &[Fact],
        call: &ResolvedCall,
        depth: RouteDepth,
        answers: &AudienceEvidence,
    ) -> Result<Vec<RecoveryRoute>, RouteError> {
        let projection = Projection::build(log, log.len() as u64);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let context = BlockContext {
            contract: registry.annotation_of(call).unwrap().into_owned(),
            call: call.clone(),
            stage: CallStage::default(),
            role: CallRole::Ordinary,
            denied: views.denied_authorities(&call.digest()).cloned().unwrap_or_default(),
            expansions: registry.audience().expansions(answers).expect("well-formed answers"),
            raw: raw_block(registry, &views, call),
        };
        search(registry, &views, &context, depth)
    }

    fn precede(name: &str, clears: Vec<Gap>) -> RouteStep {
        RouteStep::Precede {
            tool: ToolName::new(name),
            clears,
            accepts: None,
        }
    }

    fn prior(kind: &str) -> Gap {
        Gap::Prior(effect(kind))
    }

    fn propose(name: &str, clears: Vec<Gap>, halt: Halt) -> RouteOutcome {
        RouteOutcome::Prefix(Resume::Propose {
            tool: ToolName::new(name),
            clears,
            halt,
        })
    }

    fn contingent(contingencies: impl IntoIterator<Item = Contingency>) -> Certainty {
        Certainty::Contingent(contingencies.into_iter().collect())
    }

    fn tool_outcome(name: &str) -> Contingency {
        Contingency::ToolOutcome {
            tool: ToolName::new(name),
        }
    }

    fn decision(name: &str) -> Contingency {
        Contingency::AuthorityDecision {
            authority: AuthorityName::new(name),
        }
    }

    /// An expected route; its certainty follows from its steps, so the expectation states it and
    /// the helper checks it.
    fn route(steps: Vec<RouteStep>, outcome: RouteOutcome, certainty: Certainty) -> RecoveryRoute {
        let route = RecoveryRoute { steps, outcome };
        assert_eq!(route.certainty(), certainty);
        route
    }

    fn shape(route: &RecoveryRoute) -> Vec<String> {
        route
            .steps
            .iter()
            .map(|step| match step {
                RouteStep::Precede { tool, .. } => format!("precede:{}", tool.as_str()),
                RouteStep::Derive(sanitizer) => format!("derive:{}", sanitizer.as_str()),
                RouteStep::Authorize { authority, .. } => format!("authorize:{}", authority.as_str()),
                RouteStep::Accept(_) => "accept".to_string(),
                RouteStep::Sanitize(sanitizer) => format!("sanitize:{}", sanitizer.as_str()),
            })
            .collect()
    }

    #[test]
    fn a_prior_gap_finds_the_emitter_as_a_preceding_call_beyond_depth_one() {
        let registry = Deployment::of(vec![
            emitting("backup", &["backup"]),
            requiring_prior(tool("wipe"), &["backup"]),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];
        let wipe = call("wipe", json!({}));

        assert_eq!(
            routes(&registry, &log, &wipe, RouteDepth::ONE),
            vec![route(
                vec![],
                propose("backup", vec![prior("backup")], Halt::Depth),
                Certainty::Guaranteed
            )],
            "depth one stops at the redispatch the block already names"
        );
        assert_eq!(
            routes(&registry, &log, &wipe, depth(2)),
            vec![route(
                vec![precede("backup", vec![prior("backup")])],
                RouteOutcome::Complete,
                contingent([tool_outcome("backup")])
            )],
            "depth two plans across the emitter's successful call"
        );
    }

    #[test]
    fn prior_derivations_compose_until_the_depth_ends_them() {
        let registry = Deployment::of(vec![
            emitting("snapshot", &["snapshot"]),
            requiring_prior(emitting("backup", &["backup"]), &["snapshot"]),
            requiring_prior(tool("wipe"), &["backup"]),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];
        let wipe = call("wipe", json!({}));

        assert_eq!(
            routes(&registry, &log, &wipe, depth(3)),
            vec![route(
                vec![
                    precede("snapshot", vec![prior("snapshot")]),
                    precede("backup", vec![prior("backup")]),
                ],
                RouteOutcome::Complete,
                contingent([tool_outcome("snapshot"), tool_outcome("backup")])
            )]
        );
        assert_eq!(
            routes(&registry, &log, &wipe, depth(2)),
            vec![route(
                vec![],
                propose("snapshot", vec![prior("snapshot")], Halt::Depth),
                Certainty::Guaranteed
            )],
            "the prerequisite of the prerequisite is where depth two ends: the honest prefix names it"
        );

        let realized = [log, vec![committed("snapshot")]].concat();
        assert_eq!(
            routes(&registry, &realized, &wipe, depth(2)),
            vec![route(
                vec![precede("backup", vec![prior("backup")])],
                RouteOutcome::Complete,
                contingent([tool_outcome("backup")])
            )],
            "re-planning from the realized log continues where the prefix stopped"
        );
    }

    #[test]
    fn mutually_prerequisite_emitters_end_the_search_with_no_route() {
        let registry = Deployment::of(vec![
            requiring_prior(emitting("a", &["a"]), &["b"]),
            requiring_prior(emitting("b", &["b"]), &["a"]),
            requiring_prior(emitting("self", &["self"]), &["self"]),
            requiring_prior(tool("wipe"), &["a", "self"]),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];

        assert!(routes(&registry, &log, &call("wipe", json!({})), depth(50)).is_empty());
    }

    #[test]
    fn a_preceding_call_that_drops_the_trust_floor_invalidates_the_route_it_would_complete() {
        let tools = vec![
            narrowing_to(emitting("backup", &["backup"]), Some(SUSPICIOUS), None),
            requiring_trust(requiring_prior(tool("wipe"), &["backup"]), TRUSTED),
        ];
        let log = vec![opened(TRUSTED, Audience::public())];
        let wipe = call("wipe", json!({}));

        let unruled = Deployment::of(tools.clone()).registry();
        assert!(
            routes(&unruled, &log, &wipe, depth(2)).is_empty(),
            "after the emitter runs, the floor no longer holds and nothing covers it"
        );

        let ruled = Deployment::of(tools)
            .authorities(vec![trust_authority("officer", TRUSTED)])
            .registry();
        assert_eq!(
            routes(&ruled, &log, &wipe, depth(2)),
            vec![route(
                vec![
                    RouteStep::Precede {
                        tool: ToolName::new("backup"),
                        clears: vec![prior("backup")],
                        accepts: Some(Narrowing {
                            from: Label::new(TRUSTED, Audience::public()),
                            to: Label::new(SUSPICIOUS, Audience::public()),
                        }),
                    },
                    RouteStep::Authorize {
                        authority: AuthorityName::new("officer"),
                        covers: vec![Gap::TrustFloor {
                            required: TRUSTED,
                            actual: SUSPICIOUS,
                        }],
                        call: wipe.digest(),
                    },
                ],
                RouteOutcome::Complete,
                contingent([tool_outcome("backup"), decision("officer")])
            )],
            "the gap the preceding call opens is re-planned in the state it produces"
        );
    }

    #[test]
    fn a_reservation_holds_across_the_route_and_a_narrowing_delta_reopens_an_audience_gap() {
        let partner = readers(&["partner"]);
        let internal = readers(&["insider"]);
        let wipe = call("wipe", json!({}));

        let reserving = Deployment::of(vec![
            emitting("backup", &["backup"]),
            requiring_no_prior(requiring_prior(tool("wipe"), &["backup"]), "email.sent"),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public()), reserved("email.sent")];
        assert!(
            routes(&reserving, &log, &wipe, depth(2)).is_empty(),
            "an open reservation blocks `no_prior` on every state a route reaches"
        );

        let tools = vec![
            narrowing_to(emitting("backup", &["backup"]), None, Some(internal.clone())),
            requiring_includes(requiring_prior(tool("wipe"), &["backup"]), partner.clone()),
        ];
        let log = vec![opened(TRUSTED, Audience::public())];
        let unruled = Deployment::of(tools.clone()).registry();
        assert!(
            routes(&unruled, &log, &wipe, depth(2)).is_empty(),
            "the emitter narrows the audience below the recipients, and nothing vouches for them"
        );
        let ruled = Deployment::of(tools)
            .authorities(vec![reader_authority("officer", Audience::public())])
            .registry();
        assert_eq!(
            routes(&ruled, &log, &wipe, depth(2)),
            vec![route(
                vec![
                    RouteStep::Precede {
                        tool: ToolName::new("backup"),
                        clears: vec![prior("backup")],
                        accepts: Some(Narrowing {
                            from: Label::new(TRUSTED, Audience::public()),
                            to: Label::new(TRUSTED, internal),
                        }),
                    },
                    RouteStep::Authorize {
                        authority: AuthorityName::new("officer"),
                        covers: vec![Gap::Includes {
                            recipients: DeclaredAudience::literal(partner)
                        }],
                        call: wipe.digest(),
                    },
                ],
                RouteOutcome::Complete,
                contingent([tool_outcome("backup"), decision("officer")])
            )]
        );
    }

    #[test]
    fn a_cap_gap_is_cleared_by_the_call_whose_committed_label_stays_within_it() {
        let internal = readers(&["insider"]);
        let registry = Deployment::of(vec![
            narrowing_to(tool("read_internal"), None, Some(internal.clone())),
            requiring_cap(tool("wipe"), internal.clone()),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];

        assert_eq!(
            routes(&registry, &log, &call("wipe", json!({})), depth(2)),
            vec![route(
                vec![RouteStep::Precede {
                    tool: ToolName::new("read_internal"),
                    clears: vec![Gap::Cap {
                        cap: DeclaredAudience::literal(internal.clone())
                    }],
                    accepts: Some(Narrowing {
                        from: Label::new(TRUSTED, Audience::public()),
                        to: Label::new(TRUSTED, internal),
                    }),
                }],
                RouteOutcome::Complete,
                contingent([tool_outcome("read_internal")])
            )]
        );
    }

    #[test]
    fn every_way_a_preceding_call_stays_undetermined_ends_the_route_as_a_prefix() {
        let wipe = call("wipe", json!({}));
        let log = vec![opened(TRUSTED, Audience::public())];
        let prefix = |halt: Halt| {
            vec![route(
                vec![],
                propose("backup", vec![prior("backup")], halt),
                Certainty::Guaranteed,
            )]
        };

        let mut placeholder = emitting("backup", &["backup"]);
        placeholder.parameters = crate::params::test_string_argument_schema("to");
        placeholder
            .requires
            .label
            .audience
            .push(AudienceRequirement::Includes(RecipientSpec::Placeholder(
                "to".to_string(),
            )));
        let registry = Deployment::of(vec![placeholder, requiring_prior(tool("wipe"), &["backup"])]).registry();
        assert_eq!(routes(&registry, &log, &wipe, depth(2)), prefix(Halt::Arguments));

        let partner = readers(&["partner"]);
        let blocked = requiring_includes(emitting("backup", &["backup"]), partner.clone());
        let registry = Deployment::of(vec![blocked, requiring_prior(tool("wipe"), &["backup"])]).registry();
        let log_internal = vec![opened(TRUSTED, readers(&["insider"]))];
        assert_eq!(
            routes(&registry, &log_internal, &wipe, depth(2)),
            prefix(Halt::Block(vec![Gap::Includes {
                recipients: DeclaredAudience::literal(partner)
            }])),
            "a call-bound gap of the preceding call is planned at its own block"
        );
    }

    #[test]
    fn an_input_hop_is_a_prefix_that_resumes_from_the_derived_candidate() {
        let internal = readers(&["insider"]);
        let partner = readers(&["partner"]);
        let registry = Deployment::of(vec![requiring_includes(tool("wire"), partner.clone())])
            .sanitizers(vec![input_redaction(
                "redact",
                internal.clone(),
                readers(&["insider", "partner"]),
            )])
            .registry();
        let log = vec![opened(TRUSTED, internal)];

        assert_eq!(
            routes(&registry, &log, &call("wire", json!({})), depth(3)),
            vec![route(
                vec![RouteStep::Derive(SanitizerName::new("redact"))],
                RouteOutcome::Prefix(Resume::DerivedCandidate {
                    sanitizer: SanitizerName::new("redact"),
                }),
                contingent([Contingency::SanitizerResult {
                    sanitizer: SanitizerName::new("redact"),
                }])
            )]
        );
    }

    #[test]
    fn an_acceptance_alone_is_the_one_guaranteed_route() {
        let internal = readers(&["insider"]);
        let registry =
            Deployment::of(vec![narrowing_to(tool("read_internal"), None, Some(internal.clone()))]).registry();
        let log = vec![opened(TRUSTED, Audience::public())];

        assert_eq!(
            routes(&registry, &log, &call("read_internal", json!({})), depth(2)),
            vec![route(
                vec![RouteStep::Accept(Narrowing {
                    from: Label::new(TRUSTED, Audience::public()),
                    to: Label::new(TRUSTED, internal),
                })],
                RouteOutcome::Complete,
                Certainty::Guaranteed
            )]
        );
    }

    #[test]
    fn a_ruling_binds_to_the_rendered_call_and_a_denial_on_that_digest_excludes_its_authority() {
        let registry = Deployment::of(vec![requiring_trust(tool("wire"), TRUSTED)])
            .authorities(vec![trust_authority("officer", TRUSTED)])
            .registry();
        let first = call("wire", json!({ "to": "a" }));
        let second = call("wire", json!({ "to": "b" }));
        let log = vec![opened(SUSPICIOUS, Audience::public())];

        let bound = |call: &ResolvedCall| {
            vec![route(
                vec![RouteStep::Authorize {
                    authority: AuthorityName::new("officer"),
                    covers: vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS,
                    }],
                    call: call.digest(),
                }],
                RouteOutcome::Complete,
                contingent([decision("officer")]),
            )]
        };
        assert_eq!(routes(&registry, &log, &first, depth(2)), bound(&first));
        assert_eq!(routes(&registry, &log, &second, depth(2)), bound(&second));
        assert_ne!(first.digest(), second.digest());

        let denied = [
            log,
            vec![Fact::Denial {
                trajectory: traj(),
                digest: first.digest(),
                authority: AuthorityName::new("officer"),
            }],
        ]
        .concat();
        assert!(routes(&registry, &denied, &first, depth(2)).is_empty());
        assert_eq!(
            routes(&registry, &denied, &second, depth(2)),
            bound(&second),
            "a denial is sticky for its digest only"
        );
    }

    #[test]
    fn two_narrow_rulings_precede_one_broad_ruling_whatever_the_step_count() {
        let registry = Deployment::of(vec![requiring_no_prior(
            requiring_trust(tool("wire"), TRUSTED),
            "email.sent",
        )])
        .authorities(vec![
            authority(
                "broad",
                Mandate {
                    trust_ceiling: Some(VETTED),
                    waivers: vec![effect("email.sent")],
                    ..Mandate::default()
                },
            ),
            trust_authority("trust", TRUSTED),
            authority(
                "audit",
                Mandate {
                    waivers: vec![effect("email.sent")],
                    ..Mandate::default()
                },
            ),
        ])
        .registry();
        let log = vec![opened(SUSPICIOUS, Audience::public()), committed("email.sent")];

        let found = routes(&registry, &log, &call("wire", json!({})), depth(2));
        let shapes: Vec<Vec<String>> = found.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                vec!["authorize:trust", "authorize:audit"],
                vec!["authorize:trust", "authorize:broad"],
                vec!["authorize:broad"],
                vec!["authorize:broad", "authorize:audit"],
            ],
            "the two-ruling routes that keep the trust ceiling at the floor precede the one broad \
             ruling despite their length; on the `no_prior` gap the broad waiver equals the narrow \
             one, so those two tie by bytes"
        );
    }

    #[test]
    fn a_substitution_precedes_a_ruling_and_less_disclosure_precedes_more() {
        let internal = readers(&["insider"]);
        let registry = Deployment::of(vec![
            emitting("zbackup", &["backup"]),
            narrowing_to(emitting("abackup", &["backup"]), None, Some(readers(&["carol"]))),
            requiring_includes(
                requiring_prior(tool("wire"), &["backup"]),
                readers(&["insider", "partner"]),
            ),
        ])
        .authorities(vec![reader_authority("officer", Audience::public())])
        .sanitizers(vec![input_redaction(
            "redact",
            internal.clone(),
            readers(&["insider", "partner"]),
        )])
        .registry();
        let log = vec![opened(TRUSTED, internal)];

        let found = routes(&registry, &log, &call("wire", json!({})), depth(2));
        let shapes: Vec<Vec<String>> = found.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                vec!["derive:redact"],
                vec!["precede:zbackup", "derive:redact"],
                vec!["precede:zbackup", "authorize:officer"],
                vec!["precede:abackup", "authorize:officer"],
            ],
            "substitutions first; the ruling after `zbackup` discloses `partner` only, the one after \
             `abackup` — whose delta empties the audience — discloses both recipients; and no hop \
             applies once the audience no longer includes what the sanitizer takes"
        );
    }

    #[test]
    fn hops_over_a_gap_a_preceding_call_opens_rank_by_their_substitution_power() {
        let internal = readers(&["insider"]);
        let partner = readers(&["partner"]);
        let registry = Deployment::of(vec![
            narrowing_to(emitting("backup", &["backup"]), None, Some(internal.clone())),
            requiring_includes(requiring_prior(tool("wire"), &["backup"]), partner),
        ])
        .sanitizers(vec![
            input_redaction("broad", internal.clone(), Audience::public()),
            input_redaction("narrow", internal, readers(&["insider", "partner"])),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];

        let found = routes(&registry, &log, &call("wire", json!({})), depth(2));
        let shapes: Vec<Vec<String>> = found.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                vec!["precede:backup", "derive:narrow"],
                vec!["precede:backup", "derive:broad"],
            ],
            "the `includes` gap exists only after `backup` narrows the audience; the hop that \
             substitutes the smaller audience still ranks first"
        );
    }

    #[test]
    fn the_search_is_total_within_the_bound() {
        let registry = Deployment::of(vec![
            emitting("b1", &["backup"]),
            emitting("b2", &["backup"]),
            requiring_prior(emitting("s1", &["snapshot"]), &["seal"]),
            emitting("seal", &["seal"]),
            requiring_prior(tool("wipe"), &["backup", "snapshot"]),
        ])
        .registry();
        let log = vec![opened(TRUSTED, Audience::public())];
        let wipe = call("wipe", json!({}));

        let first = routes(&registry, &log, &wipe, depth(4));
        let complete: Vec<Vec<String>> = first
            .iter()
            .filter(|route| route.outcome == RouteOutcome::Complete)
            .map(shape)
            .collect();
        assert_eq!(
            complete,
            vec![
                vec!["precede:b1", "precede:seal", "precede:s1"],
                vec!["precede:b2", "precede:seal", "precede:s1"],
            ],
            "each backup once, in the first order met; `seal, s1, b1` reaches the same state and \
             is not reported again"
        );
        assert!(
            routes(&registry, &log, &wipe, depth(3))
                .iter()
                .all(|route| route.outcome != RouteOutcome::Complete),
            "three calls cannot span seal, s1 and a backup beside the blocked call"
        );
    }

    #[test]
    fn depth_one_routes_are_exactly_the_stage_plans() {
        let internal = readers(&["insider"]);
        let partner = readers(&["partner"]);
        let registry = Deployment::of(vec![
            emitting("backup", &["backup"]),
            requiring_includes(requiring_prior(tool("wire"), &["backup"]), partner.clone()),
            requiring_trust(tool("gate"), TRUSTED),
        ])
        .authorities(vec![trust_authority("officer", TRUSTED)])
        .sanitizers(vec![input_redaction(
            "redact",
            internal.clone(),
            readers(&["insider", "partner"]),
        )])
        .registry();
        let log = vec![opened(SUSPICIOUS, internal)];

        for proposal in [call("wire", json!({})), call("gate", json!({}))] {
            let projection = Projection::build(&log, log.len() as u64);
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let parts = crate::label::TestContext::default();
            let contract = registry
                .annotation_of(&proposal)
                .expect("the fixture registers the tool");
            let planned = plan::plan(
                &registry,
                &views,
                plan::BlockedCall {
                    call: &proposal,
                    contract: &contract,
                    raw: &raw_block(&registry, &views, &proposal),
                    stage: &CallStage::default(),
                    role: CallRole::Ordinary,
                },
                &parts.context(),
            )
            .expect("the fixture's audiences are literal");
            let expected: Vec<RecoveryRoute> = planned
                .plans
                .iter()
                .map(|plan| match plan {
                    plan::RemedyPlan::Executable(plan) => match plan.hop() {
                        Some(sanitizer) => route(
                            vec![RouteStep::Derive(sanitizer.clone())],
                            RouteOutcome::Prefix(Resume::DerivedCandidate {
                                sanitizer: sanitizer.clone(),
                            }),
                            contingent([Contingency::SanitizerResult {
                                sanitizer: sanitizer.clone(),
                            }]),
                        ),
                        None => route(
                            plan.steps
                                .iter()
                                .map(|step| match step {
                                    RemedyStep::Authorize(authority) => RouteStep::Authorize {
                                        authority: authority.clone(),
                                        covers: plan.required[0].covers.clone(),
                                        call: proposal.digest(),
                                    },
                                    RemedyStep::Accept(narrowing) => RouteStep::Accept(narrowing.clone()),
                                    RemedyStep::Sanitize(sanitizer) => RouteStep::Sanitize(sanitizer.clone()),
                                    RemedyStep::Derive(sanitizer) => RouteStep::Derive(sanitizer.clone()),
                                })
                                .collect(),
                            RouteOutcome::Complete,
                            contingent(
                                plan.required
                                    .iter()
                                    .map(|required| decision(required.authority.as_str())),
                            ),
                        ),
                    },
                    plan::RemedyPlan::Redispatch(redispatch) => route(
                        vec![],
                        propose(redispatch.tool().as_str(), redispatch.clears().to_vec(), Halt::Depth),
                        Certainty::Guaranteed,
                    ),
                })
                .collect();
            assert!(!expected.is_empty());
            let found = routes(&registry, &log, &proposal, RouteDepth::ONE);
            assert_eq!(found.len(), expected.len());
            assert!(expected.iter().all(|route| found.contains(route)), "{found:?}");
            let executable = |routes: &[RecoveryRoute]| -> Vec<RecoveryRoute> {
                routes
                    .iter()
                    .filter(|route| !matches!(route.outcome, RouteOutcome::Prefix(Resume::Propose { .. })))
                    .cloned()
                    .collect()
            };
            assert_eq!(executable(&found), executable(&expected));
        }
    }
}
