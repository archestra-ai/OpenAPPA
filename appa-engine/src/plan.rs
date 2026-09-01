//! Remedy planning: turning a raw block into the sound remedies the agent may act on.
//!
//! A [`PlannedBlock`] carries the block as found plus **executable plans** (atomic
//! `Authorize`/`Accept` compositions run through `execute_remedy_plan`) and **id-less redispatch
//! recommendations** (`Redispatch` — call another tool first, then re-propose; `Fork` — advisory
//! only). The security claim lives here: an **empty** set of executable plans *and* direct
//! redispatches is a *proof* that the block is unliftable — relative to the implemented remedy
//! subset, read at the current stage against registered configuration and recorded denials
//! An empty list is a proof, not a shrug.
//!
//! **A redispatch clears its gap directly or is not offered**. A plan for a failed
//! `prior(k)` names a registered tool whose `emits` includes `k`. A plan for a failed cap names a
//! tool whose own established restrictive audience contribution, folded into the current label,
//! lands within the cap. Only a declared audience delta can prove that: an Annotated tool's
//! contribution exists only per call, so it claims nothing at load. The blocked target's own
//! delta takes no part in the claim — audience only
//! narrows under `combine`, so a candidate that clears on its own contribution still clears when
//! the target's delta folds after it, while one that needs the target's help is target-assisted
//! advice this module does not offer. The named tool is dispatched as an ordinary separately-checked
//! call: its own requirements, denials, and reservations bite at its own block, when it is
//! actually proposed — a directly clearing tool is sound advice even when its own call would
//! block today.
//!
//! **Alternatives.** A clearable block offers **every sound alternative**: each unique grouped
//! authority assignment (per-gap choice among competent authorities) crossed with each way of
//! settling the narrowing — acceptance, or an applicable output sanitizer with the residual it
//! cannot shed. Enumeration is made total by the registry's load-time bound ([`crate::registry`]'s
//! `PlannerCap`), which spans both factors — no runtime truncation. A separate
//! assignment-set property checks the enumeration set-equal against an independent reference
//! enumerator, and the redispatch list is checked against an independent direct-set reference.
//!
//! **Implemented remedy subset (the honest bound).** `Authorize` (trust floor via `trust_ceiling`,
//! `includes` via `reader_ceiling`, `no_prior` via `waivers`, attention via `attends`), `Accept`
//! (narrowing), `Sanitize` (an output sanitizer's relabel standing in for the raw crossing),
//! `Derive` (an input sanitizer's substitution of the whole argument set), and `Redispatch` over direct `prior(k)` emitters and static cap-narrowing tools, in
//! name order — the agent picks, and each redispatch is separately checked for real.
//! The empty-proof is complete over exactly this subset.
//!
//! **A sanitize step settles a narrowing and never a requirement gap**. The gaps are
//! evaluated on the raw committed label even in a plan that sanitizes: a requirement gates the
//! dispatch, and the dispatch happens before any derivation exists, so a promise to clean the
//! result afterwards cannot justify the release. That is the fail-closed direction, and it is why
//! adding the step leaves `is_curable` unchanged — acceptance was already always available for a
//! narrowing, so the sanitizer adds alternatives, never remedies for a requirement gap.
//!
//! Blocked **child returns** are planned by [`return_stage`] with their own closed vocabulary —
//! acceptance of the current residual, or one applicable helpful sanitizer hop. A return crossing
//! has no dispatch, no gaps, and no authorities, so none of this module's tool-block machinery
//! applies to it. The tool-output plans here mirror its shape deliberately.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::authority::{Authority, DeclaredTransition, Mandate, Sanitizer};
use crate::candidate::{CallStage, SanitizerLineage};
use crate::check::{self, Gap, Narrowing, RawBlock};
use crate::contract::ToolAnnotation;
use crate::fact::EffectKind;
use crate::label::{Audience, Evaluation, Label, MembershipContext, MembershipNeeded, SymbolicAtom, WithinAssertions};
use crate::names::{AuthorityName, SanitizerName, TagName};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ResolvedCall, ToolName};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlanId(u32);

impl PlanId {
    pub const fn new(id: u32) -> Self {
        PlanId(id)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

/// One engine-side act in an executable plan. All three are atomic: `Authorize` records a ruling
/// that admits the dispatch despite a gap; `Accept` records the agent's acceptance of the
/// narrowing; `Sanitize` binds an output sanitizer to the dispatch. None edits a trajectory label —
/// `Sanitize` changes only which value is admitted when the result comes back, and the fold does
/// the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemedyStep {
    Authorize(AuthorityName),
    Accept(Narrowing),
    Sanitize(SanitizerName),
    Derive(SanitizerName),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableRemedyPlan {
    pub id: PlanId,
    pub steps: Vec<RemedyStep>,
    pub required: Vec<RequiredRuling>,
}

impl ExecutableRemedyPlan {
    /// The narrowing this plan asks the agent to accept, where its route carries one.
    /// A raw release path orders that acceptance before its rulings; an output-sanitizer path
    /// accepts no residual up front and has none here.
    pub fn narrowing(&self) -> Option<&Narrowing> {
        self.steps.iter().find_map(|step| match step {
            RemedyStep::Accept(narrowing) => Some(narrowing),
            RemedyStep::Authorize(_) | RemedyStep::Sanitize(_) | RemedyStep::Derive(_) => None,
        })
    }

    pub fn sanitizer(&self) -> Option<&SanitizerName> {
        self.steps.iter().find_map(|step| match step {
            RemedyStep::Sanitize(sanitizer) => Some(sanitizer),
            RemedyStep::Authorize(_) | RemedyStep::Accept(_) | RemedyStep::Derive(_) => None,
        })
    }

    /// The sanitizer this plan runs on the offer's own candidate, if it is a progress hop.
    /// A hop is exactly one `Derive` step and nothing else: it clears no requirement
    /// gap and settles no narrowing, so a plan pairing it with anything is not one this engine
    /// enumerates.
    pub fn hop(&self) -> Option<&SanitizerName> {
        match self.steps.as_slice() {
            [RemedyStep::Derive(sanitizer)] if self.required.is_empty() => Some(sanitizer),
            _ => None,
        }
    }
}

/// An id-less redispatch recommendation: the named tool and the displayed
/// gaps its own contribution clears. The validating constructor is the only way to build one, so
/// an empty claim or a gap species redispatch cannot clear is unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedispatchPlan {
    tool: ToolName,
    clears: Vec<Gap>,
}

impl RedispatchPlan {
    /// `None` unless at least one gap is claimed and every claimed gap is a redispatch species —
    /// `prior(k)` or a cap, the two redispatch shapes.
    pub fn new(tool: ToolName, clears: Vec<Gap>) -> Option<RedispatchPlan> {
        let valid = !clears.is_empty() && clears.iter().all(|gap| matches!(gap, Gap::Prior(_) | Gap::Cap { .. }));
        valid.then_some(RedispatchPlan { tool, clears })
    }

    pub fn tool(&self) -> &ToolName {
        &self.tool
    }

    pub fn clears(&self) -> &[Gap] {
        &self.clears
    }
}

/// One way out of a block. A plan with an engine-side step is an executable object with
/// an id, run through `execute_remedy_plan`; one without names a call the agent makes for itself
/// and carries no id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemedyPlan {
    Executable(ExecutableRemedyPlan),
    Redispatch(RedispatchPlan),
}

impl RemedyPlan {
    pub fn executable(&self) -> Option<&ExecutableRemedyPlan> {
        match self {
            RemedyPlan::Executable(plan) => Some(plan),
            RemedyPlan::Redispatch(_) => None,
        }
    }
}

impl ExecutableRemedyPlan {
    /// Does executing this plan consult `authority`? The one home of the predicate: a
    /// denial consumes every offered plan naming the denying authority for this rendered call.
    pub fn names_authority(&self, authority: &AuthorityName) -> bool {
        self.required.iter().any(|required| &required.authority == authority)
    }
}

/// A block with its remedies attached: the raw gaps/narrowing, the one remedy list, and the
/// advisory fork hint. [`PlannedBlock::is_curable`] is the security-relevant verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedBlock {
    pub raw: RawBlock,
    pub plans: Vec<RemedyPlan>,
    /// Advice, never a remedy: a child begins at the same label, so a fork cures no requirement.
    /// Kept out of `plans` so the emptiness assertion stays about remedies.
    pub fork_advice: Option<String>,
}

impl PlannedBlock {
    /// Is any remedy available? **Empty is a proof no plan exists** over the implemented remedy
    /// subset — the assertion concerns requirement gaps and narrowing. Fork
    /// advice is not a remedy and never enters this verdict.
    pub fn is_curable(&self) -> bool {
        !self.plans.is_empty()
    }
}

/// What the planned call is to its deployment: an ordinary tool call, or the one proposal the
/// runtime marked as the context-controlled spawn. The mark is a property of the
/// subject, fixed when its batch was decided, so a substituted successor keeps its predecessor's
/// role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallRole {
    Ordinary,
    MarkedSpawn,
}

/// Plan the remedies for a raw block. Emits the executable plans when the block clears in one
/// atomic step, and every direct redispatch when only a prior tool call unlocks it. Both land in
/// the one `plans` list; fork advice is separate and never a remedy.
/// See the module docs for the direct-clearing model.
/// One refused call as planning receives it: the call, the contract its check resolved,
/// the block that check found, and the stage and role it was found at. The contract is
/// carried rather than looked up again — an empty plan set is read as a proof that the
/// block is unliftable, and a lookup that missed would have said the same thing.
pub(crate) struct BlockedCall<'a> {
    pub(crate) call: &'a ResolvedCall,
    pub(crate) contract: &'a ToolAnnotation,
    pub(crate) raw: &'a RawBlock,
    pub(crate) stage: &'a CallStage,
    pub(crate) role: CallRole,
}

pub(crate) fn plan(
    registry: &Registry,
    views: &Views,
    blocked: BlockedCall<'_>,
    context: &MembershipContext<'_>,
) -> Result<PlannedBlock, MembershipNeeded> {
    let BlockedCall {
        call,
        contract,
        raw,
        stage,
        role,
    } = blocked;
    let current = views.current_label();
    let no_denials = BTreeSet::new();
    let denied = views.denied_authorities(&call.digest()).unwrap_or(&no_denials);

    let has_committed = |kind: &EffectKind| views.has_effect(kind);
    let has_reserved = |kind: &EffectKind| views.has_reservation(kind);
    let mut plans: Vec<RemedyPlan> = enumerate_plans(
        registry,
        contract,
        &current,
        &has_committed,
        &has_reserved,
        call,
        stage,
        role,
        context,
    )?
    .into_iter()
    .filter(|plan| !denied.iter().any(|authority| plan.names_authority(authority)))
    .map(RemedyPlan::Executable)
    .collect();

    let terminal = |plan: &RemedyPlan| plan.executable().is_some_and(|plan| plan.hop().is_none());
    if !plans.iter().any(terminal) && !raw.requirement_gaps.is_empty() {
        let mut needs = NeededAtoms::default();
        let redispatches = direct_redispatches(registry, &current, raw, context, &mut needs);
        needs.refuse_if_any()?;
        plans.extend(redispatches.into_iter().map(RemedyPlan::Redispatch));
    }
    let fork_reason = match (&raw.narrowing, raw.requirement_gaps.is_empty()) {
        (Some(_), true) => {
            "If this trajectory's harness advertises a child-session tool, delegate this call and all work that uses its result there.\nFinish there by returning nothing, or return only a sanitized derivation. Returning the raw value applies the same change to this session."
        }
        (Some(_), false) => {
            "If this trajectory's harness advertises a child-session tool, delegate this call, its required remedies, and all work that uses its result there.\nFinish there by returning nothing, or return only a sanitized derivation. Returning the raw value applies the same change to this session."
        }
        (None, _) => {
            "If this trajectory's harness advertises a child-session tool, handle the work there if isolation is useful.\nA child inherits the same session label, so delegation does not clear these requirements."
        }
    };
    Ok(PlannedBlock {
        raw: raw.clone(),
        plans,
        fork_advice: Some(fork_reason.to_string()),
    })
}

/// The executable plans of one stage over an explicit state: the branch's label and the
/// history predicates. The log supplies them for a live block; a recovery route supplies the
/// state it has reached (RMD-20). A membership read no pinned answer decides refuses the
/// whole enumeration with the union of every undecided atom, never a smaller menu.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enumerate_plans(
    registry: &Registry,
    contract: &ToolAnnotation,
    current: &Label,
    has_committed: &impl Fn(&EffectKind) -> bool,
    has_reserved: &impl Fn(&EffectKind) -> bool,
    call: &ResolvedCall,
    stage: &CallStage,
    role: CallRole,
    context: &MembershipContext<'_>,
) -> Result<Vec<ExecutableRemedyPlan>, MembershipNeeded> {
    let block = check::evaluate_state(
        contract,
        current,
        has_committed,
        has_reserved,
        check::CallReads::Resolved(call),
        stage,
        context,
    )?;
    if block.requirement_gaps.is_empty() && block.narrowing.is_none() {
        return Ok(Vec::new());
    }

    let mut needs = NeededAtoms::default();
    let mut candidates: Vec<PlanCandidate> = input_hops(
        registry,
        contract,
        stage,
        role,
        &block.requirement_gaps,
        current,
        context,
        &mut needs,
    )
    .into_iter()
    .map(|sanitizer| PlanCandidate {
        steps: vec![RemedyStep::Derive(sanitizer)],
        required: Vec::new(),
    })
    .collect();
    if let Some(assignments) =
        enumerate_assignments(registry, &block.requirement_gaps, &contract.tags, context, &mut needs)
    {
        let settlements = narrowing_remedies(
            registry,
            current,
            contract,
            block.narrowing.as_ref(),
            context,
            &mut needs,
        );
        for required in assignments {
            for settlement in &settlements {
                let mut steps: Vec<RemedyStep> = match settlement {
                    NarrowingSettlement::Accept(narrowing) => vec![RemedyStep::Accept(narrowing.clone())],
                    NarrowingSettlement::Nothing | NarrowingSettlement::Sanitize(_) => Vec::new(),
                };
                steps.extend(required.iter().map(|r| RemedyStep::Authorize(r.authority.clone())));
                if let NarrowingSettlement::Sanitize(sanitizer) = settlement {
                    steps.push(RemedyStep::Sanitize(sanitizer.clone()));
                }
                candidates.push(PlanCandidate {
                    steps,
                    required: required.clone(),
                });
            }
        }
    }
    needs.refuse_if_any()?;
    Ok(
        least_mandate_first(registry, &block.requirement_gaps, candidates, context)
            .into_iter()
            .enumerate()
            .map(|(position, candidate)| ExecutableRemedyPlan {
                id: PlanId(position as u32),
                steps: candidate.steps,
                required: candidate.required,
            })
            .collect(),
    )
}

/// The membership atoms enumeration read without a pinned answer, aggregated across every
/// read actually performed. Enumeration never treats an undecided read as an inapplicable
/// remedy: the atoms collect here and the whole enumeration refuses with their union, so a
/// missing answer is an ask, never a silently smaller menu.
#[derive(Default)]
pub(crate) struct NeededAtoms(BTreeSet<SymbolicAtom>);

impl NeededAtoms {
    pub(crate) fn absorb(&mut self, needed: MembershipNeeded) {
        self.0.extend(needed.needed);
    }

    pub(crate) fn refuse_if_any(self) -> Result<(), MembershipNeeded> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(MembershipNeeded {
                needed: self.0.into_iter().collect(),
            })
        }
    }
}

struct PlanCandidate {
    steps: Vec<RemedyStep>,
    required: Vec<RequiredRuling>,
}

impl PlanCandidate {
    fn hop(&self) -> Option<&SanitizerName> {
        match self.steps.as_slice() {
            [RemedyStep::Derive(sanitizer)] if self.required.is_empty() => Some(sanitizer),
            _ => None,
        }
    }
}

fn least_mandate_first(
    registry: &Registry,
    gaps: &[Gap],
    candidates: Vec<PlanCandidate>,
    context: &MembershipContext<'_>,
) -> Vec<PlanCandidate> {
    let reads = RankingReads::resolve(registry, gaps, &candidates);
    // Each candidate's power on each gap, computed once: the selection compares them many times.
    let powers: Vec<Vec<GapPower<'_>>> = candidates
        .iter()
        .map(|candidate| {
            gaps.iter()
                .map(|gap| assigned_power(registry, candidate, gap, &reads, context.within))
                .collect()
        })
        .collect();
    let mut ordered: Vec<usize> = Vec::with_capacity(candidates.len());
    let mut used = vec![false; candidates.len()];
    for _ in 0..candidates.len() {
        let next = (0..candidates.len())
            .filter(|&index| !used[index])
            .find(|&index| {
                (0..candidates.len())
                    .filter(|&other| !used[other] && other != index)
                    .all(|other| !plan_precedes(gaps, &powers[other], &powers[index], context.within))
            })
            .expect("a finite strict partial order has a minimal element");
        used[next] = true;
        ordered.push(next);
    }
    let mut slots: Vec<Option<PlanCandidate>> = candidates.into_iter().map(Some).collect();
    ordered
        .into_iter()
        .map(|index| slots[index].take().expect("each candidate is selected once"))
        .collect()
}

fn plan_precedes(gaps: &[Gap], a: &[GapPower<'_>], b: &[GapPower<'_>], within: &WithinAssertions) -> bool {
    let mut strictly_less = false;
    for ((gap, a), b) in gaps.iter().zip(a).zip(b) {
        match gap_power_cmp(gap, a, b, within) {
            Some(Ordering::Less) => strictly_less = true,
            Some(Ordering::Equal) => {}
            Some(Ordering::Greater) | None => return false,
        }
    }
    strictly_less
}

pub(crate) enum GapPower<'a> {
    None,
    Substitution(&'a Audience),
    Ruling {
        mandate: &'a Mandate,
        reader_ceiling: Option<&'a Audience>,
    },
}

struct RankingReads<'a> {
    ceilings: BTreeMap<&'a AuthorityName, Option<Audience>>,
    targets: BTreeMap<&'a SanitizerName, Audience>,
}

impl<'a> RankingReads<'a> {
    fn resolve(registry: &'a Registry, gaps: &[Gap], candidates: &'a [PlanCandidate]) -> RankingReads<'a> {
        let mut reads = RankingReads {
            ceilings: BTreeMap::new(),
            targets: BTreeMap::new(),
        };
        if !gaps.iter().any(|gap| matches!(gap, Gap::Includes { .. })) {
            return reads;
        }
        for candidate in candidates {
            for required in &candidate.required {
                if required.covers.iter().any(|gap| matches!(gap, Gap::Includes { .. })) {
                    reads.ceilings.entry(&required.authority).or_insert_with(|| {
                        registry
                            .authority(&required.authority)
                            .expect("assignments name only registered authorities")
                            .mandate
                            .reader_ceiling
                            .as_ref()
                            .map(Audience::of_declared)
                    });
                }
            }
            if let Some(sanitizer) = candidate.hop() {
                let transition = &registry
                    .sanitizer(sanitizer)
                    .expect("hops name only registered sanitizers")
                    .transition;
                if let DeclaredTransition::Audience { to, .. } = transition {
                    reads
                        .targets
                        .entry(sanitizer)
                        .or_insert_with(|| Audience::of_declared(to));
                }
            }
        }
        reads
    }
}

fn assigned_power<'a>(
    registry: &'a Registry,
    candidate: &'a PlanCandidate,
    gap: &Gap,
    reads: &'a RankingReads<'a>,
    within: &WithinAssertions,
) -> GapPower<'a> {
    if let Some(required) = candidate.required.iter().find(|ruling| ruling.covers.contains(gap)) {
        let mandate = &registry
            .authority(&required.authority)
            .expect("assignments name only registered authorities")
            .mandate;
        let reader_ceiling = match gap {
            Gap::Includes { .. } => reads
                .ceilings
                .get(&required.authority)
                .expect("every authority assigned to an includes gap resolved its ceiling")
                .as_ref(),
            _ => None,
        };
        return GapPower::Ruling {
            mandate,
            reader_ceiling,
        };
    }
    let (Some(sanitizer), Gap::Includes { recipients }) = (candidate.hop(), gap) else {
        return GapPower::None;
    };
    match reads.targets.get(sanitizer) {
        // A trust hop clears no `includes` gap. The ranking derives from policy facts only,
        // so an underivable inclusion ranks as no power — order, never admission.
        None => GapPower::None,
        Some(to) if Audience::of_declared(recipients).derives_within_audience(to, within) => GapPower::Substitution(to),
        Some(_) => GapPower::None,
    }
}

pub(crate) fn gap_power_cmp(
    gap: &Gap,
    a: &GapPower<'_>,
    b: &GapPower<'_>,
    within: &WithinAssertions,
) -> Option<Ordering> {
    let ((a, a_readers), (b, b_readers)) = match (a, b) {
        (GapPower::None, GapPower::None) => return Some(Ordering::Equal),
        (GapPower::None, _) => return Some(Ordering::Less),
        (_, GapPower::None) => return Some(Ordering::Greater),
        (GapPower::Substitution(a), GapPower::Substitution(b)) => {
            return inclusion_cmp(
                a.derives_within_audience(b, within),
                b.derives_within_audience(a, within),
            );
        }
        (GapPower::Substitution(_), GapPower::Ruling { .. }) => return Some(Ordering::Less),
        (GapPower::Ruling { .. }, GapPower::Substitution(_)) => return Some(Ordering::Greater),
        (
            GapPower::Ruling {
                mandate: a,
                reader_ceiling: a_readers,
            },
            GapPower::Ruling {
                mandate: b,
                reader_ceiling: b_readers,
            },
        ) => ((a, a_readers), (b, b_readers)),
    };
    match gap {
        Gap::TrustFloor { .. } => {
            let (a, b) = (a.trust_ceiling, b.trust_ceiling);
            let a = a.expect("a competent trust-floor authority declares a trust ceiling");
            let b = b.expect("a competent trust-floor authority declares a trust ceiling");
            Some(a.cmp(&b))
        }
        Gap::Includes { .. } => {
            let a = a_readers.expect("a competent includes authority declares a reader ceiling");
            let b = b_readers.expect("a competent includes authority declares a reader ceiling");
            inclusion_cmp(
                a.derives_within_audience(b, within),
                b.derives_within_audience(a, within),
            )
        }
        Gap::NoPrior(_) => {
            let a: BTreeSet<&EffectKind> = a.waivers.iter().collect();
            let b: BTreeSet<&EffectKind> = b.waivers.iter().collect();
            inclusion_cmp(a.is_subset(&b), b.is_subset(&a))
        }
        Gap::Attention(_) => Some(Ordering::Equal),
        // These gaps have no covering authority by construction (`enumerate_assignments` returns
        // `None`), so no assignment reaching this comparison carries one.
        Gap::Prior(_) | Gap::Cap { .. } => Some(Ordering::Equal),
    }
}

pub(crate) fn inclusion_cmp(a_in_b: bool, b_in_a: bool) -> Option<Ordering> {
    match (a_in_b, b_in_a) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (false, false) => None,
    }
}

fn enumerate_assignments(
    registry: &Registry,
    gaps: &[Gap],
    tags: &[TagName],
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Option<Vec<Vec<RequiredRuling>>> {
    let mut choices: Vec<Vec<&AuthorityName>> = Vec::with_capacity(gaps.len());
    for gap in gaps {
        let competent: Vec<&AuthorityName> = registry
            .authorities()
            .iter()
            .filter(|authority| match gap_cover(authority, gap, tags, context) {
                Evaluation::Holds => true,
                Evaluation::Fails => false,
                Evaluation::Needs(needed) => {
                    needs.absorb(needed);
                    false
                }
            })
            .map(|authority| &authority.name)
            .collect();
        if competent.is_empty() {
            return None;
        }
        choices.push(competent);
    }

    let mut assignments: Vec<Vec<RequiredRuling>> = Vec::new();
    let mut assignment = vec![0usize; choices.len()];
    loop {
        // Group this combination's per-gap choices into per-authority covers, in gap order.
        let mut required: Vec<RequiredRuling> = Vec::new();
        for (index, gap) in gaps.iter().enumerate() {
            let authority = choices[index][assignment[index]].clone();
            match required.iter_mut().find(|r| r.authority == authority) {
                Some(existing) => existing.covers.push(gap.clone()),
                None => required.push(RequiredRuling {
                    authority,
                    covers: vec![gap.clone()],
                }),
            }
        }
        if !assignments.contains(&required) {
            assignments.push(required);
        }
        // Odometer over the per-gap choice indices.
        let mut position = choices.len();
        loop {
            if position == 0 {
                return Some(assignments);
            }
            position -= 1;
            assignment[position] += 1;
            if assignment[position] < choices[position].len() {
                break;
            }
            assignment[position] = 0;
        }
    }
}

/// One way a block's narrowing settles: the agent accepts it, or a bound output
/// sanitizer withholds the raw result and the confined stage settles whatever residual its relabel
/// cannot shed. Never both — a sanitizer route accepts no guessed residual — and a block with no
/// narrowing settles nothing. The settlement carries *what* is settled; [`enumerate_plans`]
/// composes it into the canonical step order.
pub(crate) enum NarrowingSettlement {
    Nothing,
    Accept(Narrowing),
    Sanitize(SanitizerName),
}

/// The ways this block's narrowing can be settled: acceptance always, then one
/// settlement per applicable output sanitizer, in registry name order. A block with no narrowing
/// yields one empty settlement, so the caller's cross product still produces the plain authority
/// plans.
pub(crate) fn narrowing_remedies(
    registry: &Registry,
    current: &Label,
    contract: &ToolAnnotation,
    narrowing: Option<&Narrowing>,
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Vec<NarrowingSettlement> {
    let Some(narrowing) = narrowing else {
        return vec![NarrowingSettlement::Nothing];
    };
    let mut settlements = vec![NarrowingSettlement::Accept(narrowing.clone())];
    if !registry.profile().confines_result(&contract.name) {
        return settlements;
    }
    let output = contract.output_label();
    for sanitizer in applicable_output_sanitizers(registry, contract, &output, context, needs) {
        if sanitized_commit(current, &output, sanitizer).is_none() {
            continue;
        }
        settlements.push(NarrowingSettlement::Sanitize(sanitizer.name.clone()));
    }
    settlements
}

/// Every input-substitution progress hop this call stage offers, in registry name order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn input_hops(
    registry: &Registry,
    contract: &ToolAnnotation,
    stage: &CallStage,
    role: CallRole,
    gaps: &[Gap],
    current: &Label,
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Vec<SanitizerName> {
    if role == CallRole::MarkedSpawn {
        return Vec::new();
    }
    if !gaps.iter().any(|gap| matches!(gap, Gap::Includes { .. })) {
        return Vec::new();
    }
    let released = stage.released(current);
    registry
        .sanitizers()
        .filter(|sanitizer| !stage.lineage().contains(&sanitizer.name))
        .filter(|sanitizer| {
            // An undecided admission or clearing joins the aggregate refusal instead of
            // shrinking the menu.
            match sanitizer.derive_input(&released, &contract.tags, context) {
                Ok(Some(derived)) => clears_a_recipient(&derived, gaps, context, needs),
                Ok(None) => false,
                Err(needed) => {
                    needs.absorb(needed);
                    false
                }
            }
        })
        .map(|sanitizer| sanitizer.name.clone())
        .collect()
}

/// Does a validated substitution strictly improve the call candidate it replaces?
pub(crate) fn substitution_helps(before: &RawBlock, after: &check::CheckOutcome) -> bool {
    let after = match after {
        check::CheckOutcome::Allow => return !before.requirement_gaps.is_empty(),
        check::CheckOutcome::Block(raw) => raw,
    };
    after.requirement_gaps.len() < before.requirement_gaps.len()
        && after
            .requirement_gaps
            .iter()
            .all(|gap| before.requirement_gaps.contains(gap))
}

fn clears_a_recipient(derived: &Label, gaps: &[Gap], context: &MembershipContext<'_>, needs: &mut NeededAtoms) -> bool {
    gaps.iter().any(|gap| match gap {
        Gap::Includes { recipients } => match derived.covers(recipients, context) {
            Evaluation::Holds => true,
            Evaluation::Fails => false,
            Evaluation::Needs(needed) => {
                needs.absorb(needed);
                false
            }
        },
        _ => false,
    })
}

fn applicable_output_sanitizers<'r>(
    registry: &'r Registry,
    contract: &ToolAnnotation,
    output: &Label,
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Vec<&'r Sanitizer> {
    registry
        .sanitizers()
        .filter(|sanitizer| !sanitizer.name.is_attest_schema())
        .filter(
            |sanitizer| match sanitizer.derive_output(output, &contract.tags, context) {
                Ok(derived) => derived.is_some(),
                Err(needed) => {
                    needs.absorb(needed);
                    false
                }
            },
        )
        .collect()
}

/// The established bound a sanitized dispatch of `contract` would commit at `current`, when such
/// a plan would be offered at all. `None` when the tool does not narrow here (no narrowing, so no
/// block and no plan to attach the sanitizer to) or when the relabel lands exactly where the raw
/// crossing would (a sanitizer that changes nothing about the merged outcome is not
/// offered). The one home of this arithmetic.
fn sanitized_commit(current: &Label, output: &Label, sanitizer: &Sanitizer) -> Option<Label> {
    let raw = current.combine(output);
    if &raw == current {
        return None;
    }
    let sanitized = current.combine(&sanitizer.transition.applied().derive(output));
    (sanitized != raw).then_some(sanitized)
}

/// Is a further output sanitizer helpful on a confined candidate?
pub(crate) fn confined_hop_helps(receiving: &Label, candidate: &Label, derived: &Label) -> bool {
    candidate.combine(derived) == *candidate && receiving.combine(derived) != receiving.combine(candidate)
}

/// The next stage of one confined candidate: a progress hop for every registered output
/// sanitizer that still helps, then acceptance of exactly the residual this candidate leaves.
///
/// Total by construction once every membership read is answered: acceptance is always
/// available, so a confined candidate is never stuck whatever the catalogue holds, and no chain
/// is precomputed — each hop persists one candidate and this runs again from it. An undecided
/// membership read refuses the stage with the aggregate of missing atoms instead of dropping
/// hops. The order is presentation only; hops come first because a hop costs the trajectory
/// nothing and acceptance costs exactly the residual.
pub(crate) fn confined_stage(
    registry: &Registry,
    contract: &ToolAnnotation,
    receiving: &Label,
    candidate: &Label,
    residual: &Narrowing,
    lineage: &SanitizerLineage,
    context: &MembershipContext<'_>,
) -> Result<Vec<ExecutableRemedyPlan>, MembershipNeeded> {
    let mut needs = NeededAtoms::default();
    let mut plans: Vec<ExecutableRemedyPlan> = Vec::new();
    let hops = registry
        .sanitizers()
        .filter(|sanitizer| !lineage.contains(&sanitizer.name))
        .filter(|sanitizer| !sanitizer.name.is_attest_schema())
        .filter(
            |sanitizer| match sanitizer.derive_output(candidate, &contract.tags, context) {
                Ok(derived) => derived.is_some_and(|derived| confined_hop_helps(receiving, candidate, &derived)),
                Err(needed) => {
                    needs.absorb(needed);
                    false
                }
            },
        );
    for sanitizer in hops {
        plans.push(ExecutableRemedyPlan {
            id: PlanId(plans.len() as u32),
            steps: vec![RemedyStep::Derive(sanitizer.name.clone())],
            required: Vec::new(),
        });
    }
    needs.refuse_if_any()?;
    plans.push(ExecutableRemedyPlan {
        id: PlanId(plans.len() as u32),
        steps: vec![RemedyStep::Accept(residual.clone())],
        required: Vec::new(),
    });
    Ok(plans)
}

/// The next stage of one pending child return: a progress hop for
/// every registered output sanitizer that is applicable and still helps, then acceptance of
/// exactly the residual the candidate leaves. Total like [`confined_stage`], and planned from the
/// candidate standing now — the submitted fold's bound, or a derived successor's label.
#[allow(clippy::too_many_arguments)]
pub(crate) fn return_stage(
    registry: &Registry,
    views: &Views,
    child: &crate::value::TrajectoryId,
    candidate: &Label,
    body: &crate::value::ValueBody,
    residual: &Narrowing,
    lineage: &SanitizerLineage,
    context: &MembershipContext<'_>,
) -> Result<Vec<ExecutableRemedyPlan>, MembershipNeeded> {
    let mut needs = NeededAtoms::default();
    let mut plans: Vec<ExecutableRemedyPlan> = Vec::new();
    if registry.profile().confines_child_return() {
        for sanitizer in registry.sanitizers() {
            if lineage.contains(&sanitizer.name) {
                continue;
            }
            if !(sanitizer.on.output && sanitizer.applies_to(&[])) {
                continue;
            }
            if sanitizer.name.is_attest_schema() && !attest_applicable(views, child, body, &sanitizer.transition) {
                continue;
            }
            let helps = match sanitizer.derive_output(candidate, &[], context) {
                Ok(derived) => derived.is_some_and(|derived| confined_hop_helps(&residual.from, candidate, &derived)),
                Err(needed) => {
                    needs.absorb(needed);
                    false
                }
            };
            if helps {
                plans.push(ExecutableRemedyPlan {
                    id: PlanId(plans.len() as u32),
                    steps: vec![RemedyStep::Derive(sanitizer.name.clone())],
                    required: Vec::new(),
                });
            }
        }
    }
    needs.refuse_if_any()?;
    plans.push(ExecutableRemedyPlan {
        id: PlanId(plans.len() as u32),
        steps: vec![RemedyStep::Accept(residual.clone())],
        required: Vec::new(),
    });
    Ok(plans)
}

/// The preconditions of the reserved `attest-schema` builtin, checked as applicability:
/// the fork bound a shape — every compiled shape is shape-bounded by construction —
/// the candidate body is exactly a value that shape admits in canonical form, and the parent's
/// fork-time trust rank covers the mandate `to`, because the answer cannot come back cleaner
/// than the context that asked. All three are engine-held facts; no resolver variant exists.
pub(crate) fn attest_applicable(
    views: &Views,
    child: &crate::value::TrajectoryId,
    body: &crate::value::ValueBody,
    transition: &DeclaredTransition,
) -> bool {
    let Some(shape) = views.return_shape_of(child) else {
        return false;
    };
    // Load validation refuses an audience mandate on the reserved name.
    let DeclaredTransition::Trust { to, .. } = transition else {
        return false;
    };
    if !shape
        .validate(body.as_str())
        .is_ok_and(|canonical| canonical == body.as_str())
    {
        return false;
    }
    views.fork_seed(child).is_some_and(|seed| seed.trust >= *to)
}

/// The established contribution a bound output sanitizer's first derivation would make, resolved
/// here at dispatch. `None` where the sanitizer is unregistered or does not apply to
/// this output at all. Resolved rather than left to a later registry read because a mandate group
/// resolves at application time, so what the binding promised is fixed now and recorded.
pub(crate) fn bound_contribution(
    registry: &Registry,
    contract: &ToolAnnotation,
    sanitizer: &SanitizerName,
    context: &MembershipContext<'_>,
) -> Result<Option<Label>, MembershipNeeded> {
    let Some(sanitizer) = registry.sanitizer(sanitizer) else {
        return Ok(None);
    };
    sanitizer.derive_output(&contract.output_label(), &contract.tags, context)
}

/// The rulings a block's remedy plan needs gathered: for each authority the block routes to, the
/// gaps its ruling must cover. The mandate routing (which authority covers which gap) stays here
/// in the engine; the runtime only gathers a ruling from each named authority for its gaps and
/// executes the offer that named them. A gap no registered authority covers builds no assignment
/// at all, so the block offers no plan over it rather than offering one that cannot execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredRuling {
    pub authority: AuthorityName,
    pub covers: Vec<Gap>,
}

/// The three-valued cover: does this authority's mandate cover the gap, refutably not, or is
/// the inclusion still awaiting membership answers? Planning offers only a held cover; the
/// load-time planner-cap lint counts everything not refuted, so the runtime count never
/// exceeds what load bounded.
pub(crate) fn gap_cover(
    authority: &Authority,
    gap: &Gap,
    tags: &[TagName],
    context: &MembershipContext<'_>,
) -> Evaluation {
    let mandate = &authority.mandate;
    match gap {
        Gap::TrustFloor { required, .. } => Evaluation::of_exact(
            authority.scope.covers(tags) && mandate.trust_ceiling.is_some_and(|ceiling| ceiling >= *required),
        ),
        Gap::Includes { recipients } => {
            if !authority.scope.covers(tags) {
                return Evaluation::Fails;
            }
            match &mandate.reader_ceiling {
                None => Evaluation::Fails,
                Some(ceiling) => Audience::of_declared(ceiling).includes(recipients, context),
            }
        }
        Gap::NoPrior(kind) => Evaluation::of_exact(authority.scope.covers(tags) && mandate.waivers.contains(kind)),
        // Attention routes by its own currency — the attended mark — never by scope.
        Gap::Attention(mark) => Evaluation::of_exact(mandate.attends.contains(mark)),
        Gap::Prior(_) | Gap::Cap { .. } => Evaluation::Fails,
    }
}

/// The structural cover predicate: held, and nothing else. Load-time counting is its only
/// caller — every runtime path reads [`gap_cover`] three-valued and treats an undecided
/// inclusion as a missing answer, never as a refuted cover.
pub(crate) fn covers_gap(authority: &Authority, gap: &Gap, tags: &[TagName], context: &MembershipContext<'_>) -> bool {
    matches!(gap_cover(authority, gap, tags, context), Evaluation::Holds)
}

/// The atoms the planning of one surfaced block reads, so the operation answers them
/// before [`plan`] runs — the deterministic second gathering stage, a pure function of the
/// block, the policy, and the role. Each read site of the enumeration is mirrored by its
/// gate here: an `includes` gap reads the mandate of every in-scope authority (`gap_cover`)
/// and the transition of every in-scope input sanitizer; a narrowing at a confined result
/// point reads the transition of every in-scope output sanitizer; a `cap` gap reads every
/// tool's delta. The check's own reads — the call's contract — are the check stage's.
pub(crate) fn block_atoms(
    registry: &Registry,
    contract: &ToolAnnotation,
    raw: &RawBlock,
    role: CallRole,
) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    let mut atoms: Vec<SymbolicAtom> = Vec::new();
    let has = |wanted: fn(&Gap) -> bool| raw.requirement_gaps.iter().any(wanted);
    if has(|gap| matches!(gap, Gap::Includes { .. })) {
        for authority in registry.authorities() {
            if authority.scope.covers(&contract.tags) {
                atoms.extend(authority.mandate.needed_atoms(providers));
            }
        }
        if role != CallRole::MarkedSpawn {
            for sanitizer in registry.sanitizers() {
                if sanitizer.on.input && sanitizer.applies_to(&contract.tags) {
                    atoms.extend(sanitizer.needed_atoms(providers));
                }
            }
        }
    }
    if raw.narrowing.is_some() && registry.profile().confines_result(&contract.name) {
        for sanitizer in registry.sanitizers() {
            if sanitizer.on.output && !sanitizer.name.is_attest_schema() && sanitizer.applies_to(&contract.tags) {
                atoms.extend(sanitizer.needed_atoms(providers));
            }
        }
    }
    if has(|gap| matches!(gap, Gap::Cap { .. })) {
        for tool in registry.tools().filter_map(crate::contract::ToolDeclaration::declared) {
            if let Some(audience) = &tool.delta.audience {
                atoms.extend(audience.needed_atoms(providers));
            }
        }
    }
    atoms
}

/// The atoms executing one offered plan reads: the call's own contract, the mandate of
/// every assigned authority as far as the gaps it covers consult it and the transition
/// of every sanitizer a step names.
pub(crate) fn plan_atoms(
    registry: &Registry,
    contract: &ToolAnnotation,
    plan: &ExecutableRemedyPlan,
) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    let mut atoms: Vec<SymbolicAtom> = contract.needed_atoms(providers).collect();
    for required in &plan.required {
        if let Some(authority) = registry.authority(&required.authority) {
            atoms.extend(authority.mandate.reads(&required.covers, providers));
        }
    }
    for step in &plan.steps {
        let sanitizer = match step {
            RemedyStep::Derive(sanitizer) | RemedyStep::Sanitize(sanitizer) => sanitizer,
            RemedyStep::Accept(_) | RemedyStep::Authorize(_) => continue,
        };
        if let Some(sanitizer) = registry.sanitizer(sanitizer) {
            atoms.extend(sanitizer.needed_atoms(providers));
        }
    }
    atoms
}

/// The atoms one confined candidate's stage reads: the transition of every
/// in-scope output sanitizer the chain has not spent.
pub(crate) fn confined_stage_atoms(
    registry: &Registry,
    contract: &ToolAnnotation,
    lineage: &SanitizerLineage,
) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    registry
        .sanitizers()
        .filter(|sanitizer| !lineage.contains(&sanitizer.name))
        .filter(|sanitizer| !sanitizer.name.is_attest_schema())
        .filter(|sanitizer| sanitizer.on.output && sanitizer.applies_to(&contract.tags))
        .flat_map(|sanitizer| sanitizer.needed_atoms(providers))
        .collect()
}

/// The atoms one child return's stage reads: where the deployment
/// confines the return, the transition of every unscoped output sanitizer the chain has not
/// spent.
pub(crate) fn return_stage_atoms(registry: &Registry, lineage: &SanitizerLineage) -> Vec<SymbolicAtom> {
    let providers = registry.audience().providers();
    let mut atoms: Vec<SymbolicAtom> = Vec::new();
    if registry.profile().confines_child_return() {
        for sanitizer in registry.sanitizers() {
            if !lineage.contains(&sanitizer.name) && sanitizer.on.output && sanitizer.applies_to(&[]) {
                atoms.extend(sanitizer.needed_atoms(providers));
            }
        }
    }
    atoms
}

fn direct_redispatches(
    registry: &Registry,
    current: &Label,
    raw: &RawBlock,
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Vec<RedispatchPlan> {
    let mut direct = Vec::new();
    // A redispatch names the tool, not one ordered contract, so it clears a gap only when every
    // variant of that tool clears it.
    for name in registry.tool_names() {
        let variant_clears: Vec<Vec<Gap>> = registry
            .variants(name)
            .map(|declaration| match declaration.declared() {
                Some(tool) => direct_clears(tool, &raw.requirement_gaps, current, context, needs),
                // An Annotated variant's requirements exist only per call: it provably clears
                // nothing at load, so the tool never qualifies as a redispatch.
                None => Vec::new(),
            })
            .collect();
        let Some((first, rest)) = variant_clears.split_first() else {
            continue;
        };
        let clears: Vec<Gap> = first
            .iter()
            .filter(|gap| rest.iter().all(|other| other.contains(gap)))
            .cloned()
            .collect();
        // The constructor is also the emptiness filter: a tool clearing nothing yields `None`.
        direct.extend(RedispatchPlan::new(name.clone(), clears));
    }
    direct
}

/// The gaps a successful call to `tool` clears by itself (RMD-13): a `prior` it emits, or a cap
/// the label it commits from `current` stays within.
pub(crate) fn direct_clears(
    tool: &ToolAnnotation,
    gaps: &[Gap],
    current: &Label,
    context: &MembershipContext<'_>,
    needs: &mut NeededAtoms,
) -> Vec<Gap> {
    let has_cap = gaps.iter().any(|gap| matches!(gap, Gap::Cap { .. }));
    let committed = has_cap.then(|| check::committed_label(tool, current));
    gaps.iter()
        .filter(|gap| match (gap, &committed) {
            (Gap::Prior(kind), _) => tool.emits.contains(kind),
            // An undecided cap comparison is a missing answer, not a refuted clearing: it joins
            // the aggregate refusal so the redispatch list never silently shrinks.
            (Gap::Cap { cap }, Some(committed)) => match committed.within_cap(cap, context) {
                Evaluation::Holds => true,
                Evaluation::Fails => false,
                Evaluation::Needs(needed) => {
                    needs.absorb(needed);
                    false
                }
            },
            _ => false,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Hint, Mandate, Sanitizer, SanitizerPoints, Scope};
    use crate::check::CheckOutcome;
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, PinnedAnnotation, ProducedAnnotation,
        RecipientSpec, Requires, ToolAnnotation, ToolDeclaration,
    };
    use crate::fact::{EffectSet, Fact};
    use crate::label::DeclaredAudience;
    use crate::label::{Audience, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{ToolName, TrajectoryId};
    use proptest::prelude::*;
    use serde_json::json;

    fn exec(plan: &RemedyPlan) -> &ExecutableRemedyPlan {
        plan.executable().expect("an executable plan")
    }

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn build(config: RegistryConfig) -> Registry {
        Registry::build_covered(config).unwrap()
    }

    fn declared(tools: Vec<ToolAnnotation>) -> Vec<ToolDeclaration> {
        tools.into_iter().map(ToolDeclaration::Declared).collect()
    }

    fn opened(label: Label) -> Fact {
        crate::profile::opening_at(traj(), label)
    }

    fn known(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn established(trust: Trust, audience: Audience) -> Label {
        Label::new(trust, audience)
    }

    fn plan_of(registry: &Registry, log: &[Fact], call: &ResolvedCall) -> PlannedBlock {
        let projection = Projection::build(log, log.len() as u64);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let contract = registry
            .annotation_of(call)
            .expect("a test call resolves its annotation");
        let parts = crate::label::TestContext::default();
        let raw = match check::evaluate(&contract, &views, call, &CallStage::default(), &parts.context()) {
            Ok(CheckOutcome::Block(raw)) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        plan(
            registry,
            &views,
            BlockedCall {
                call,
                contract: &contract,
                raw: &raw,
                stage: &CallStage::default(),
                role: CallRole::Ordinary,
            },
            &parts.context(),
        )
        .expect("test planning reads no undecided symbolic audience")
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&args))
    }

    #[test]
    fn an_input_hop_does_not_stand_in_for_the_redispatch_that_clears_a_prior_gap() {
        let emitter = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup")]).unwrap(),
            requires: Requires::default(),
        };
        let partner = Audience::restricted([ReaderId::new("partner")]);
        let wipe = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wipe"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::literal(partner.clone()),
                    ))],
                },
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup"))],
                ..Requires::default()
            },
        };
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![emitter, wipe]),
            authorities: vec![],
            sanitizers: vec![Sanitizer {
                name: SanitizerName::new("redact"),
                on: SanitizerPoints {
                    input: true,
                    output: false,
                },
                transition: DeclaredTransition::Audience {
                    from_includes: DeclaredAudience::literal(internal.clone()),
                    to: DeclaredAudience::restricted([ReaderId::new("insider"), ReaderId::new("partner")]),
                },
                scope: Scope::default(),
                hint: None,
            }],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(Label::new(TRUSTED, internal))];

        let planned = plan_of(&registry, &log, &call("wipe", json!({})));
        assert!(
            planned.plans.iter().any(
                |plan| plan.executable().and_then(ExecutableRemedyPlan::hop) == Some(&SanitizerName::new("redact"))
            ),
            "the substitution clears the recipient gap and is offered"
        );
        assert!(
            planned.plans.iter().any(|plan| matches!(
                plan,
                RemedyPlan::Redispatch(r)
                    if r.tool().as_str() == "backup" && r.clears() == [Gap::Prior(EffectKind::new("backup"))]
            )),
            "the prior gap keeps the redispatch that clears it"
        );
    }

    #[test]
    fn an_annotated_tool_keeps_its_input_substitution_hops() {
        let partner = Audience::restricted([ReaderId::new("partner")]);
        let internal = Audience::restricted([ReaderId::new("insider")]);
        let annotation = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::test_string_argument_schema("to"),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::literal(partner.clone()),
                    ))],
                },
                ..Requires::default()
            },
        };
        let redact = Sanitizer {
            name: SanitizerName::new("redact"),
            on: SanitizerPoints {
                input: true,
                output: false,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(internal.clone()),
                to: DeclaredAudience::restricted([ReaderId::new("insider"), ReaderId::new("partner")]),
            },
            scope: Scope::default(),
            hint: None,
        };
        let log = vec![opened(Label::new(TRUSTED, internal))];
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![ToolDeclaration::Annotated {
                name: ToolName::new("wire"),
                tags: vec![],
                description: Some("A test tool.".to_string()),
                parameters: crate::params::test_string_argument_schema("to"),
                annotator: crate::names::AnnotatorName::new("classifier"),
            }],
            authorities: vec![],
            sanitizers: vec![redact],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![crate::registry::AnnotatorDeclaration {
                name: crate::names::AnnotatorName::new("classifier"),
                trust: None,
                audiences: None,
                marks: None,
                effects: None,
            }],
        });
        let unpinned = call("wire", json!({ "to": "insider" }));
        let call = unpinned.clone().with_annotation(Some(PinnedAnnotation::new(
            crate::names::AnnotatorName::new("classifier"),
            unpinned.digest(),
            ProducedAnnotation {
                delta: annotation.delta,
                emits: annotation.emits,
                requires: annotation.requires,
            },
        )));

        assert!(
            plan_of(&registry, &log, &call).plans.iter().any(|plan| {
                plan.executable().and_then(ExecutableRemedyPlan::hop) == Some(&SanitizerName::new("redact"))
            }),
            "a pinned annotation reads like a declared one, so the input hop is offered"
        );
    }

    fn output_sanitizer(name: &str, transition: DeclaredTransition) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition,
            scope: Scope::default(),
            hint: None,
        }
    }

    fn reader(name: &str, delta: Delta) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("insider")])
    }

    fn sanitize_offers(planned: &PlannedBlock) -> Vec<String> {
        planned
            .plans
            .iter()
            .filter_map(RemedyPlan::executable)
            .filter_map(|plan| Some(plan.sanitizer()?.as_str().to_string()))
            .collect()
    }

    fn any_prebundled_residual(planned: &PlannedBlock) -> bool {
        planned
            .plans
            .iter()
            .filter_map(RemedyPlan::executable)
            .any(|plan| plan.sanitizer().is_some() && plan.narrowing().is_some())
    }

    #[test]
    fn a_narrowing_block_offers_each_applicable_output_sanitizer() {
        let crm = reader(
            "crm",
            Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(internal())),
            },
        );
        let tracker = reader(
            "tracker",
            Delta {
                trust: Some(SUSPICIOUS),
                audience: Some(DeclaredAudience::literal(internal())),
            },
        );
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![crm, tracker]),
            authorities: vec![],
            sanitizers: vec![
                output_sanitizer(
                    "declassify",
                    DeclaredTransition::Audience {
                        from_includes: DeclaredAudience::literal(internal()),
                        to: DeclaredAudience::literal(Audience::public()),
                    },
                ),
                output_sanitizer(
                    "scrub",
                    DeclaredTransition::Trust {
                        from_floor: SUSPICIOUS,
                        to: TRUSTED,
                    },
                ),
            ],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];

        let planned = plan_of(&registry, &log, &call("crm", json!({})));
        assert_eq!(sanitize_offers(&planned), ["declassify".to_string()]);
        assert!(!any_prebundled_residual(&planned));

        let planned = plan_of(&registry, &log, &call("tracker", json!({})));
        assert_eq!(
            sanitize_offers(&planned),
            ["declassify".to_string(), "scrub".to_string()],
            "both mandates apply, and each leaves the dimension it does not transition"
        );
        assert!(!any_prebundled_residual(&planned));
        assert!(
            planned
                .plans
                .iter()
                .filter_map(RemedyPlan::executable)
                .any(|plan| plan.steps == vec![RemedyStep::Accept(planned.raw.narrowing.clone().unwrap())])
        );
    }

    #[test]
    fn a_pinned_annotation_uses_its_audience_for_sanitizer_plans() {
        let mut pinned = reader(
            "lookup",
            Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(internal())),
            },
        );
        pinned.parameters = crate::params::test_string_argument_schema("room");
        let finance = Audience::restricted([ReaderId::new("finance")]);
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![ToolDeclaration::Annotated {
                name: ToolName::new("lookup"),
                tags: vec![],
                description: Some("A test tool.".to_string()),
                parameters: crate::params::test_string_argument_schema("room"),
                annotator: crate::names::AnnotatorName::new("directory"),
            }],
            authorities: vec![],
            sanitizers: vec![
                output_sanitizer(
                    "declassify",
                    DeclaredTransition::Audience {
                        from_includes: DeclaredAudience::literal(internal()),
                        to: DeclaredAudience::literal(Audience::public()),
                    },
                ),
                output_sanitizer(
                    "finance-only",
                    DeclaredTransition::Audience {
                        from_includes: DeclaredAudience::literal(internal()),
                        to: DeclaredAudience::literal(finance.clone()),
                    },
                ),
            ],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![crate::registry::AnnotatorDeclaration {
                name: crate::names::AnnotatorName::new("directory"),
                trust: None,
                audiences: None,
                marks: None,
                effects: None,
            }],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let unpinned = call("lookup", json!({ "room": "insider" }));
        let call = unpinned.clone().with_annotation(Some(PinnedAnnotation::new(
            crate::names::AnnotatorName::new("directory"),
            unpinned.digest(),
            ProducedAnnotation {
                delta: pinned.delta,
                emits: pinned.emits,
                requires: pinned.requires,
            },
        )));

        let planned = plan_of(&registry, &log, &call);
        assert_eq!(
            sanitize_offers(&planned),
            ["declassify".to_string(), "finance-only".to_string()]
        );
        assert!(!any_prebundled_residual(&planned));
    }

    #[test]
    fn a_sanitize_plan_still_carries_every_requirement_ruling() {
        let mut publish = reader(
            "publish",
            Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(internal())),
            },
        );
        publish.requires.attention = vec![MarkName::new("signoff")];
        let steward = Authority {
            name: AuthorityName::new("steward"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![publish]),
            authorities: vec![steward],
            sanitizers: vec![output_sanitizer(
                "declassify",
                DeclaredTransition::Audience {
                    from_includes: DeclaredAudience::literal(internal()),
                    to: DeclaredAudience::literal(Audience::public()),
                },
            )],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("publish", json!({})));

        let executables: Vec<_> = planned.plans.iter().filter_map(RemedyPlan::executable).collect();
        assert_eq!(executables.len(), 2, "one assignment crossed with accept and sanitize");
        for plan in &executables {
            assert_eq!(
                plan.required[0].covers,
                vec![Gap::Attention(MarkName::new("signoff"))],
                "the sanitizer covers no gap: the ruling is still required"
            );
        }
        assert!(
            executables[1]
                .steps
                .contains(&RemedyStep::Authorize(AuthorityName::new("steward")))
        );
        assert_eq!(executables[0].id, PlanId::new(0));
        assert_eq!(executables[1].id, PlanId::new(1));
        assert_eq!(
            executables[0].steps,
            vec![
                RemedyStep::Accept(planned.raw.narrowing.clone().expect("the block narrows")),
                RemedyStep::Authorize(AuthorityName::new("steward")),
            ]
        );
        assert_eq!(
            executables[1].steps,
            vec![
                RemedyStep::Authorize(AuthorityName::new("steward")),
                RemedyStep::Sanitize(SanitizerName::new("declassify")),
            ]
        );
    }

    #[test]
    fn a_direct_emitter_is_offered_without_path_verification() {
        let backup = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Delta {
                trust: Some(SUSPICIOUS),
                audience: None,
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done")]).unwrap(),
            requires: Requires::default(),
        };
        let wipe = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wipe"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let scrub = output_sanitizer(
            "scrub",
            DeclaredTransition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
        );
        let log = vec![opened(known(TRUSTED, Audience::public()))];

        let expected = vec![redispatch("backup", vec![Gap::Prior(EffectKind::new("backup.done"))])];
        let without = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![backup.clone(), wipe.clone()]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let planned = plan_of(&without, &log, &call("wipe", json!({})));
        assert!(
            planned.is_curable(),
            "the direct emitter is named on its own contribution"
        );
        assert_eq!(planned.plans, expected);

        let with = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![backup, wipe]),
            authorities: vec![],
            sanitizers: vec![scrub],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        assert_eq!(
            plan_of(&with, &log, &call("wipe", json!({}))).plans,
            expected,
            "a sanitizer at the prerequisite's block changes nothing about the direct offer"
        );
    }

    #[test]
    fn a_cap_redispatch_claims_only_what_it_actually_clears() {
        let a = || Audience::restricted([ReaderId::new("a")]);
        let ab = || Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let narrowing_tool = |name: &str, to: Audience| ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(to)),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let send = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(a()))],
                },
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![
                send,
                narrowing_tool("narrow_all", a()),
                narrowing_tool("narrow_some", ab()),
            ]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));

        assert_eq!(
            planned.plans,
            vec![redispatch(
                "narrow_all",
                vec![Gap::Cap {
                    cap: DeclaredAudience::literal(a())
                }]
            )]
        );
    }

    #[test]
    fn only_an_established_static_contribution_claims_a_cap_clear() {
        let a = Audience::restricted([ReaderId::new("a")]);
        let send = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(a))],
                },
                ..Requires::default()
            },
        };
        let contract = |name: &str, delta: Delta| ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![
                ToolDeclaration::Declared(send),
                // An Annotated tool's contribution exists only per call: it claims no cap clear.
                ToolDeclaration::Annotated {
                    name: ToolName::new("annotated"),
                    tags: vec![],
                    description: Some("A test tool.".to_string()),
                    parameters: crate::params::test_string_argument_schema("to"),
                    annotator: crate::names::AnnotatorName::new("acl"),
                },
                ToolDeclaration::Declared(contract("neutral", Delta::NONE)),
            ],
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![crate::registry::AnnotatorDeclaration {
                name: crate::names::AnnotatorName::new("acl"),
                trust: None,
                audiences: None,
                marks: None,
                effects: None,
            }],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert!(planned.plans.is_empty());
        assert!(!planned.is_curable());
    }

    #[test]
    fn one_tool_clearing_several_gaps_is_one_offer_with_the_complete_claim() {
        let a = Audience::restricted([ReaderId::new("a")]);
        let send = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(DeclaredAudience::literal(a.clone()))],
                },
                history: vec![
                    HistoryRequirement::Prior(EffectKind::new("backup.done")),
                    HistoryRequirement::Prior(EffectKind::new("receipt")),
                ],
                ..Requires::default()
            },
        };
        let fixer = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("fixer"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::literal(a.clone())),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done"), EffectKind::new("receipt")]).unwrap(),
            requires: Requires::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![send, fixer]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert_eq!(
            planned.plans,
            vec![redispatch(
                "fixer",
                vec![
                    Gap::Cap {
                        cap: DeclaredAudience::literal(a)
                    },
                    Gap::Prior(EffectKind::new("backup.done")),
                    Gap::Prior(EffectKind::new("receipt")),
                ],
            )]
        );
    }

    #[test]
    fn authorize_plan_clears_a_trust_floor_gap() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(planned.is_curable());
        assert_eq!(
            exec(&planned.plans[0]).steps,
            vec![RemedyStep::Authorize(AuthorityName::new("officer"))]
        );
    }

    #[test]
    fn alternative_authorities_yield_one_plan_per_assignment() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let attester = Authority {
            name: AuthorityName::new("attester"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer("officer-a"), officer("officer-b"), attester],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        let floor = Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        };
        let mark = Gap::Attention(MarkName::new("signoff"));
        assert_eq!(planned.plans.len(), 2);
        assert_eq!(exec(&planned.plans[0]).id, PlanId::new(0));
        assert_eq!(
            exec(&planned.plans[0]).required,
            vec![
                RequiredRuling {
                    authority: AuthorityName::new("officer-a"),
                    covers: vec![floor.clone()],
                },
                RequiredRuling {
                    authority: AuthorityName::new("attester"),
                    covers: vec![mark.clone()],
                },
            ]
        );
        assert_eq!(exec(&planned.plans[1]).id, PlanId::new(1));
        assert_eq!(
            exec(&planned.plans[1]).required,
            vec![
                RequiredRuling {
                    authority: AuthorityName::new("officer-b"),
                    covers: vec![floor],
                },
                RequiredRuling {
                    authority: AuthorityName::new("attester"),
                    covers: vec![mark],
                },
            ]
        );
    }

    fn assigned(planned: &PlannedBlock) -> Vec<Vec<&str>> {
        planned
            .plans
            .iter()
            .filter_map(RemedyPlan::executable)
            .map(|plan| plan.required.iter().map(|r| r.authority.as_str()).collect())
            .collect()
    }

    #[test]
    fn a_weaker_authority_registered_later_still_leads_the_menu() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let officer = |name: &str, ceiling: Trust| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(ceiling),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into(), "executive".into()]),
            tools: declared(vec![tool]),
            authorities: vec![officer("executive", Trust::new(2)), officer("officer", TRUSTED)],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(assigned(&planned), vec![vec!["officer"], vec!["executive"]]);
        assert_eq!(exec(&planned.plans[0]).id, PlanId::new(0));
        assert_eq!(exec(&planned.plans[1]).id, PlanId::new(1));
    }

    #[test]
    fn reader_ceilings_order_by_inclusion_and_public_is_maximal() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("hr")]),
                    ))],
                },
                ..Requires::default()
            },
        };
        let desk = |name: &str, ceiling: Audience| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(ceiling)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![
                desk("global", Audience::public()),
                desk(
                    "wide",
                    Audience::restricted([ReaderId::new("hr"), ReaderId::new("finance")]),
                ),
                desk("exact", Audience::restricted([ReaderId::new("hr")])),
            ],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::restricted([ReaderId::new("intern")])))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert_eq!(assigned(&planned), vec![vec!["exact"], vec!["wide"], vec!["global"]]);
    }

    #[test]
    fn a_trust_floor_block_reads_no_group_a_reader_ceiling_writes() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
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
        let desk = |name: &str, trust: Trust| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(trust),
                reader_ceiling: Some(DeclaredAudience::Union(
                    crate::label::Clause::new(
                        [],
                        [crate::label::GroupRef::Named(crate::names::GroupName::new("team"))],
                        [],
                    )
                    .expect("a group clause"),
                )),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![desk("desk", TRUSTED)],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig {
                sources: vec![crate::audience::SourceRegistration {
                    provider: "slack".to_string(),
                    templates: vec![crate::audience::SelectorTemplate::new("user-group/<handle>")],
                }],
                groups: vec![crate::audience::NamedAudience {
                    name: crate::names::GroupName::new("team"),
                    within: None,
                    from: vec![crate::audience::SelectorSpec {
                        provider: "slack".to_string(),
                        selector: "user-group/team".to_string(),
                    }],
                }],
                ..crate::audience::AudienceConfig::default()
            },
            annotators: vec![],
        });
        let log = vec![opened(known(Trust::new(0), Audience::public()))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert_eq!(assigned(&planned), vec![vec!["desk"]]);
        let contract = registry.tool(&ToolName::new("send")).unwrap().declared().unwrap();
        assert!(plan_atoms(&registry, contract, exec(&planned.plans[0])).is_empty());
    }

    /// The four gathering collectors, each against the scope rule its enumeration mirrors:
    /// three named groups, each read by exactly one kind of component, so every set below
    /// names which rule admitted it.
    mod gathering {
        use super::*;
        use crate::authority::DeclaredTransition;
        use crate::candidate::SanitizerLineage;
        use crate::check::{Gap, Narrowing, RawBlock};
        use crate::label::{Clause, GroupRef};
        use crate::names::{GroupName, SanitizerName, TagName};
        use crate::profile::{DeploymentProfile, ProfileDeclaration};
        use crate::registry::PlannerCap;
        use std::collections::BTreeSet;

        fn group(name: &str) -> DeclaredAudience {
            DeclaredAudience::Union(
                Clause::new([], [GroupRef::Named(GroupName::new(name))], []).expect("a group clause"),
            )
        }

        fn atoms(names: &[&str]) -> BTreeSet<SymbolicAtom> {
            names
                .iter()
                .map(|name| SymbolicAtom::Group(GroupRef::Named(GroupName::new(*name))))
                .collect()
        }

        fn tags(tags: &[&str]) -> Vec<TagName> {
            tags.iter().map(|tag| TagName::new(*tag)).collect()
        }

        fn tool(name: &str, tool_tags: &[&str], delta: Delta) -> ToolAnnotation {
            ToolAnnotation {
                description: Some("A test tool.".to_string()),
                name: ToolName::new(name),
                tags: tags(tool_tags),
                delta,
                parameters: crate::params::ToolParameters::open(),
                emits: EffectSet::default(),
                requires: Requires::default(),
            }
        }

        fn authority(name: &str, ceiling: &str, scope: &[&str]) -> Authority {
            Authority {
                name: AuthorityName::new(name),
                mandate: Mandate {
                    reader_ceiling: Some(group(ceiling)),
                    ..Mandate::default()
                },
                scope: Scope { tags: tags(scope) },
                hint: None,
            }
        }

        fn sanitizer(name: &str, on: SanitizerPoints, from: &str, scope: &[&str]) -> Sanitizer {
            Sanitizer {
                name: SanitizerName::new(name),
                on,
                transition: DeclaredTransition::Audience {
                    from_includes: group(from),
                    to: DeclaredAudience::restricted([ReaderId::new("insider")]),
                },
                scope: Scope { tags: tags(scope) },
                hint: None,
            }
        }

        const INPUT: SanitizerPoints = SanitizerPoints {
            input: true,
            output: false,
        };
        const OUTPUT: SanitizerPoints = SanitizerPoints {
            input: false,
            output: true,
        };

        /// `desk` (authority, tag `mail`) reads `team`; `scrub` (input) and `redact` (output,
        /// unscoped) read `legal`; `far` (authority, tag `unrelated`), `aside` (output, tag
        /// `unrelated`) and the `note` tool's delta read `press`.
        fn registry(confined_results: &[&str], confined_child_return: bool) -> Registry {
            let named = |name: &str| crate::audience::NamedAudience {
                name: GroupName::new(name),
                within: None,
                from: vec![crate::audience::SelectorSpec {
                    provider: "slack".to_string(),
                    selector: format!("user-group/{name}"),
                }],
            };
            let config = RegistryConfig {
                trust_chain: chain(),
                tools: declared(vec![
                    tool("send", &["mail"], Delta::NONE),
                    tool(
                        "note",
                        &[],
                        Delta {
                            trust: None,
                            audience: Some(group("press")),
                        },
                    ),
                ]),
                authorities: vec![
                    authority("desk", "team", &["mail"]),
                    authority("far", "press", &["unrelated"]),
                ],
                sanitizers: vec![
                    sanitizer("scrub", INPUT, "legal", &[]),
                    sanitizer("redact", OUTPUT, "legal", &[]),
                    sanitizer("aside", OUTPUT, "press", &["unrelated"]),
                ],
                audience: crate::audience::AudienceConfig {
                    sources: vec![crate::audience::SourceRegistration {
                        provider: "slack".to_string(),
                        templates: vec![crate::audience::SelectorTemplate::new("user-group/<handle>")],
                    }],
                    groups: vec![named("team"), named("legal"), named("press")],
                    ..crate::audience::AudienceConfig::default()
                },
                annotators: vec![],
            };
            let profile = DeploymentProfile::declare(ProfileDeclaration {
                confined_results: confined_results.iter().map(|name| ToolName::new(*name)).collect(),
                confined_child_return,
                ..crate::profile::covering_declaration(&config)
            })
            .expect("the gathering profile declares");
            Registry::build(config, PlannerCap::default(), profile).expect("the gathering registry builds")
        }

        fn send(registry: &Registry) -> ToolAnnotation {
            registry
                .tool(&ToolName::new("send"))
                .and_then(ToolDeclaration::declared)
                .cloned()
                .expect("send is declared")
        }

        fn raw(gaps: Vec<Gap>, narrowing: bool) -> RawBlock {
            RawBlock {
                requirement_gaps: gaps,
                narrowing: narrowing.then(|| Narrowing {
                    from: known(TRUSTED, Audience::public()),
                    to: known(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
                }),
            }
        }

        fn includes() -> Gap {
            Gap::Includes {
                recipients: group("team"),
            }
        }

        fn lineage(names: &[&str]) -> SanitizerLineage {
            SanitizerLineage::try_from(names.iter().map(|name| SanitizerName::new(*name)).collect::<Vec<_>>())
                .expect("a lineage without repeats")
        }

        #[test]
        fn a_block_reads_each_component_the_gap_it_carries_consults() {
            let unconfined = registry(&[], false);
            let confined = registry(&["send"], false);
            let contract = send(&unconfined);
            let collect = |registry: &Registry, raw: &RawBlock, role: CallRole| -> BTreeSet<SymbolicAtom> {
                block_atoms(registry, &contract, raw, role).into_iter().collect()
            };
            assert_eq!(
                collect(&unconfined, &raw(vec![includes()], false), CallRole::Ordinary),
                atoms(&["team", "legal"]),
                "an includes gap reads the in-scope authority and the in-scope input sanitizer"
            );
            assert_eq!(
                collect(&unconfined, &raw(vec![includes()], false), CallRole::MarkedSpawn),
                atoms(&["team"]),
                "a marked spawn never routes through an input sanitizer"
            );
            assert_eq!(
                collect(
                    &unconfined,
                    &raw(vec![Gap::Cap { cap: group("team") }], false),
                    CallRole::Ordinary
                ),
                atoms(&["press"]),
                "a cap gap reads every tool's delta and no authority"
            );
            assert_eq!(
                collect(
                    &unconfined,
                    &raw(
                        vec![Gap::TrustFloor {
                            required: TRUSTED,
                            actual: SUSPICIOUS
                        }],
                        false
                    ),
                    CallRole::Ordinary
                ),
                atoms(&[]),
                "a trust-floor gap consults no reader ceiling"
            );
            assert_eq!(
                collect(&unconfined, &raw(vec![], true), CallRole::Ordinary),
                atoms(&[]),
                "a narrowing at an unconfined result point reads no output sanitizer"
            );
            assert_eq!(
                collect(&confined, &raw(vec![], true), CallRole::Ordinary),
                atoms(&["legal"]),
                "a narrowing at a confined result point reads the in-scope output sanitizers"
            );
        }

        #[test]
        fn a_confined_stage_reads_the_unspent_in_scope_output_sanitizers() {
            let registry = registry(&["send"], false);
            let contract = send(&registry);
            let collect = |lineage: &SanitizerLineage| -> BTreeSet<SymbolicAtom> {
                confined_stage_atoms(&registry, &contract, lineage)
                    .into_iter()
                    .collect()
            };
            assert_eq!(collect(&lineage(&[])), atoms(&["legal"]));
            assert_eq!(
                collect(&lineage(&["redact"])),
                atoms(&[]),
                "a spent sanitizer is not read again"
            );
        }

        #[test]
        fn a_child_return_reads_output_sanitizers_only_where_the_deployment_confines_it() {
            let collect = |registry: &Registry, lineage: &SanitizerLineage| -> BTreeSet<SymbolicAtom> {
                return_stage_atoms(registry, lineage).into_iter().collect()
            };
            assert_eq!(collect(&registry(&[], false), &lineage(&[])), atoms(&[]));
            let confined = registry(&[], true);
            assert_eq!(
                collect(&confined, &lineage(&[])),
                atoms(&["legal"]),
                "only an unscoped output sanitizer applies to a return"
            );
            assert_eq!(collect(&confined, &lineage(&["redact"])), atoms(&[]));
        }

        #[test]
        fn a_plan_reads_its_rulings_as_far_as_their_gaps_consult_the_mandate_and_its_steps() {
            let registry = registry(&[], false);
            let contract = send(&registry);
            let collect = |required: Vec<RequiredRuling>, steps: Vec<RemedyStep>| -> BTreeSet<SymbolicAtom> {
                let plan = ExecutableRemedyPlan {
                    id: PlanId::new(0),
                    steps,
                    required,
                };
                plan_atoms(&registry, &contract, &plan).into_iter().collect()
            };
            let desk = |covers: Vec<Gap>| RequiredRuling {
                authority: AuthorityName::new("desk"),
                covers,
            };
            assert_eq!(
                collect(
                    vec![desk(vec![includes()])],
                    vec![
                        RemedyStep::Authorize(AuthorityName::new("desk")),
                        RemedyStep::Sanitize(SanitizerName::new("redact")),
                    ]
                ),
                atoms(&["team", "legal"])
            );
            assert_eq!(
                collect(
                    vec![desk(vec![Gap::TrustFloor {
                        required: TRUSTED,
                        actual: SUSPICIOUS
                    }])],
                    vec![RemedyStep::Accept(raw(vec![], true).narrowing.expect("a narrowing"))]
                ),
                atoms(&[]),
                "a ruling over no includes gap reads no ceiling; accepting reads nothing"
            );
        }
    }

    #[test]
    fn an_undecided_mandate_refuses_enumeration_with_its_atom_never_a_smaller_menu() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("hr")]),
                    ))],
                },
                ..Requires::default()
            },
        };
        let team_desk = Authority {
            name: AuthorityName::new("desk"),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::Union(
                    crate::label::Clause::new(
                        [],
                        [crate::label::GroupRef::Named(crate::names::GroupName::new("team"))],
                        [],
                    )
                    .expect("a group clause"),
                )),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let global = Authority {
            name: AuthorityName::new("global"),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(Audience::public())),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![team_desk, global],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig {
                sources: vec![crate::audience::SourceRegistration {
                    provider: "slack".to_string(),
                    templates: vec![crate::audience::SelectorTemplate::new("user-group/<handle>")],
                }],
                groups: vec![crate::audience::NamedAudience {
                    name: crate::names::GroupName::new("team"),
                    within: None,
                    from: vec![crate::audience::SelectorSpec {
                        provider: "slack".to_string(),
                        selector: "user-group/team".to_string(),
                    }],
                }],
                ..crate::audience::AudienceConfig::default()
            },
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::restricted([ReaderId::new("intern")])))];
        let proposal = call("send", json!({}));
        let projection = Projection::build(&log, log.len() as u64);
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let contract = registry
            .annotation_of(&proposal)
            .expect("a test call resolves its annotation");
        let atom = SymbolicAtom::Group(crate::label::GroupRef::Named(crate::names::GroupName::new("team")));
        let unanswered = crate::label::TestContext::default();
        let raw = match check::evaluate(
            &contract,
            &views,
            &proposal,
            &CallStage::default(),
            &unanswered.context(),
        ) {
            Ok(CheckOutcome::Block(raw)) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        let stage = CallStage::default();
        let blocked = || BlockedCall {
            call: &proposal,
            contract: &contract,
            raw: &raw,
            stage: &stage,
            role: CallRole::Ordinary,
        };

        // The public-ceiling authority alone could cover the gap, yet the menu is not quietly
        // reconstructed around the undecided mandate: the enumeration refuses with the atom.
        assert_eq!(
            plan(&registry, &views, blocked(), &unanswered.context()),
            Err(MembershipNeeded {
                needed: vec![atom.clone()]
            }),
        );

        let answered = crate::label::TestContext {
            expansions: crate::label::Expansions::new([(atom, BTreeSet::from([ReaderId::new("hr")]))], []),
            ..Default::default()
        };
        let planned = plan(&registry, &views, blocked(), &answered.context()).expect("every atom is answered");
        assert_eq!(assigned(&planned), vec![vec!["desk"], vec!["global"]]);
    }

    #[test]
    fn waiver_sets_order_by_inclusion_ignoring_vector_order_and_duplicates() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("spend"))],
                ..Requires::default()
            },
        };
        let waiver = |name: &str, kinds: Vec<&str>| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                waivers: kinds.into_iter().map(EffectKind::new).collect(),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![
                waiver("broad", vec!["notify", "spend"]),
                waiver("narrow", vec!["spend", "spend"]),
            ],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![
            opened(known(TRUSTED, Audience::public())),
            committed_effect(EffectKind::new("spend")),
        ];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(assigned(&planned), vec![vec!["narrow"], vec!["broad"]]);
    }

    fn open_reservation(tool: &str, kinds: &[&str]) -> Fact {
        let seed = ResolvedCall::new(ToolName::new(tool), crate::params::test_arguments(&json!({})));
        let annotation = ToolAnnotation {
            name: seed.tool().clone(),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            delta: Delta::NONE,
            emits: EffectSet::new(kinds.iter().copied().map(EffectKind::new)).expect("distinct generated effect kinds"),
            requires: Requires::default(),
        };
        Fact::DispatchOpened {
            trajectory: traj(),
            dispatch: crate::value::DispatchId::new(traj(), seed.digest(), 0),
            tool: seed.tool().clone(),
            declaration: seed.declaration_id(),
            arguments: seed.canonical_arguments().clone(),
            proposed_label: established(TRUSTED, Audience::public()),
            receiving: established(TRUSTED, Audience::public()),
            proposed_effects: annotation.emits.clone(),
            annotation: None,
            subject: crate::basis::fixture_subject(&traj()),
            evidence: crate::audience::AudienceEvidence::default(),
        }
    }

    #[test]
    fn a_reservation_caused_no_prior_gap_enumerates_its_waiver_plans() {
        let guard = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("guard"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
        };
        let keeper = Authority {
            name: AuthorityName::new("keeper"),
            mandate: Mandate {
                waivers: vec![EffectKind::new("email.sent")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![guard]),
            authorities: vec![keeper],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![
            opened(known(TRUSTED, Audience::public())),
            open_reservation("send", &["email.sent"]),
        ];
        let planned = plan_of(&registry, &log, &call("guard", json!({})));
        assert!(planned.is_curable());
        assert_eq!(assigned(&planned), vec![vec!["keeper"]]);
    }

    #[test]
    fn a_reservation_at_the_emitters_own_block_does_not_suppress_the_offer() {
        let delete = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::NoPrior(EffectKind::new("lock"))],
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![delete, backup]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let expected = vec![redispatch("backup", vec![Gap::Prior(EffectKind::new("backup.done"))])];
        let clear = vec![opened(known(TRUSTED, Audience::public()))];
        assert_eq!(
            plan_of(&registry, &clear, &call("delete_db", json!({}))).plans,
            expected
        );

        let reserved = vec![
            opened(known(TRUSTED, Audience::public())),
            open_reservation("locker", &["lock"]),
        ];
        assert_eq!(
            plan_of(&registry, &reserved, &call("delete_db", json!({}))).plans,
            expected
        );
    }

    #[test]
    fn crossing_ceilings_are_incomparable_and_keep_enumeration_order() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("hr")]),
                    ))],
                },
                ..Requires::default()
            },
        };
        let desk = |name: &str, ceiling: Audience| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                reader_ceiling: Some(DeclaredAudience::literal(ceiling)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![
                desk(
                    "legal",
                    Audience::restricted([ReaderId::new("hr"), ReaderId::new("legal")]),
                ),
                desk(
                    "audit",
                    Audience::restricted([ReaderId::new("hr"), ReaderId::new("audit")]),
                ),
            ],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::restricted([ReaderId::new("intern")])))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert_eq!(assigned(&planned), vec![vec!["legal"], vec!["audit"]]);
    }

    #[test]
    fn multi_gap_dominance_orders_the_menu_and_crossing_assignments_keep_enumeration_order() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("hr")]),
                    ))],
                },
                ..Requires::default()
            },
        };
        let officer = |name: &str, ceiling: Trust, readers: Audience| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(ceiling),
                reader_ceiling: Some(DeclaredAudience::literal(readers)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: TrustChain::new(vec!["suspicious".into(), "trusted".into(), "executive".into()]),
            tools: declared(vec![tool]),
            authorities: vec![
                officer("strong", Trust::new(2), Audience::public()),
                officer("weak", TRUSTED, Audience::restricted([ReaderId::new("hr")])),
            ],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(
            SUSPICIOUS,
            Audience::restricted([ReaderId::new("intern")]),
        ))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert_eq!(
            assigned(&planned),
            vec![
                vec!["weak"],
                vec!["strong", "weak"],
                vec!["weak", "strong"],
                vec!["strong"],
            ]
        );
    }

    #[test]
    fn a_hint_never_changes_the_presented_order() {
        let tool = || ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let officer = |name: &str, hint: Option<Hint>| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint,
        };
        let plain = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool()]),
            authorities: vec![officer("a", None), officer("b", None)],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let hinted = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool()]),
            authorities: vec![
                officer("a", None),
                officer("b", Some(Hint::new("the fast lane — prefer this desk"))),
            ],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let call = call("wire", json!({}));
        assert_eq!(
            assigned(&plan_of(&plain, &log, &call)),
            assigned(&plan_of(&hinted, &log, &call))
        );
    }

    fn denial(call: &ResolvedCall, authority: &str) -> Fact {
        Fact::Denial {
            trajectory: traj(),
            digest: call.digest(),
            authority: AuthorityName::new(authority),
        }
    }

    fn two_officer_registry() -> Registry {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer("officer-a"), officer("officer-b")],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        })
    }

    #[test]
    fn a_denied_authority_is_excluded_and_the_surviving_sibling_keeps_its_id() {
        let registry = two_officer_registry();
        let wire = call("wire", json!({"amount": 5}));
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let offered = plan_of(&registry, &log, &wire);
        assert_eq!(assigned(&offered), vec![vec!["officer-a"], vec!["officer-b"]]);
        let sibling = exec(&offered.plans[1]).clone();

        let log = vec![
            opened(known(SUSPICIOUS, Audience::public())),
            denial(&wire, "officer-a"),
        ];
        let filtered = plan_of(&registry, &log, &wire);
        assert_eq!(assigned(&filtered), vec![vec!["officer-b"]]);
        assert_eq!(exec(&filtered.plans[0]), &sibling);
        assert_eq!(exec(&filtered.plans[0]).id, PlanId::new(1));
    }

    #[test]
    fn a_denial_binds_the_exact_rendered_call() {
        let registry = two_officer_registry();
        let denied_call = call("wire", json!({"amount": 5}));
        let log = vec![
            opened(known(SUSPICIOUS, Audience::public())),
            denial(&denied_call, "officer-a"),
        ];
        assert_eq!(
            assigned(&plan_of(&registry, &log, &call("wire", json!({"amount": 6})))),
            vec![vec!["officer-a"], vec!["officer-b"]]
        );
        assert_eq!(
            assigned(&plan_of(&registry, &log, &denied_call)),
            vec![vec!["officer-b"]]
        );
    }

    #[test]
    fn a_sole_denied_authority_makes_the_block_terminally_planless() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let wire = call("wire", json!({}));
        let log = vec![opened(known(SUSPICIOUS, Audience::public())), denial(&wire, "officer")];
        let planned = plan_of(&registry, &log, &wire);
        assert!(planned.plans.is_empty());
        assert!(!planned.is_curable());
    }

    #[test]
    fn a_target_denial_does_not_suppress_the_direct_redispatch() {
        let target = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                history: vec![HistoryRequirement::Prior(EffectKind::new("receipt"))],
                ..Requires::default()
            },
        };
        let emitter = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("emitter"),
            tags: vec![],
            delta: crate::contract::Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("receipt")]).unwrap(),
            requires: Requires::default(),
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
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![target, emitter]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let send = call("send", json!({}));
        let expected = vec![redispatch("emitter", vec![Gap::Prior(EffectKind::new("receipt"))])];
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        assert_eq!(plan_of(&registry, &log, &send).plans, expected);

        let log = vec![opened(known(SUSPICIOUS, Audience::public())), denial(&send, "officer")];
        assert_eq!(plan_of(&registry, &log, &send).plans, expected);
    }

    #[test]
    fn a_denial_recorded_for_the_named_tools_own_call_does_not_remove_the_offer() {
        let target = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("receipt"))],
                ..Requires::default()
            },
        };
        let emitter = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("emitter"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("receipt")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
        };
        let gate = Authority {
            name: AuthorityName::new("gate"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![target, emitter]),
            authorities: vec![gate],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let send = call("send", json!({}));
        let null_rendering = ResolvedCall::new(ToolName::new("emitter"), crate::params::test_arguments(&json!({})));
        let log = vec![
            opened(known(SUSPICIOUS, Audience::public())),
            denial(&null_rendering, "gate"),
        ];
        let planned = plan_of(&registry, &log, &send);
        assert!(planned.plans.iter().any(|plan| matches!(
            plan,
            RemedyPlan::Redispatch(r) if r.tool() == &ToolName::new("emitter")
        )));
    }

    #[test]
    fn a_duplicated_requirement_entry_is_one_gap_and_mints_no_permuted_duplicates() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff"), MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let attester = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![attester("a"), attester("b")],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(planned.plans.len(), 2);
        for plan in &planned.plans {
            assert_eq!(exec(plan).required.len(), 1);
            assert_eq!(
                exec(plan).required[0].covers,
                vec![Gap::Attention(MarkName::new("signoff"))]
            );
        }
    }

    #[test]
    fn one_authority_covering_both_gaps_is_one_grouped_ruling() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(planned.plans.len(), 1);
        assert_eq!(exec(&planned.plans[0]).required.len(), 1);
        assert_eq!(
            exec(&planned.plans[0]).required[0].authority,
            AuthorityName::new("officer")
        );
        assert_eq!(exec(&planned.plans[0]).required[0].covers.len(), 2);
    }

    #[test]
    fn no_competent_authority_is_terminal() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(SUSPICIOUS, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(!planned.is_curable());
        assert!(planned.plans.is_empty());
        assert!(planned.fork_advice.is_some());
    }

    #[test]
    fn acceptance_plan_for_pure_narrowing() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("get"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(DeclaredAudience::restricted([ReaderId::new("insider")])),
            },
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("get", json!({})));
        assert!(planned.is_curable());
        assert_eq!(
            exec(&planned.plans[0]).steps,
            vec![RemedyStep::Accept(Narrowing {
                from: established(TRUSTED, Audience::public()),
                to: established(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
            })]
        );
    }

    #[test]
    fn prior_gap_cured_by_a_redispatch() {
        let delete = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("db.deleted")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done")]).unwrap(),
            requires: Requires::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![delete, backup]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(planned.is_curable());
        assert!(matches!(
            planned.plans.as_slice(),
            [RemedyPlan::Redispatch(r)] if r.tool() == &ToolName::new("backup")
        ));
    }

    #[test]
    fn prior_gap_with_multiple_emitters_surfaces_every_curative_redispatch() {
        let delete = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = |name: &str| ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done")]).unwrap(),
            requires: Requires::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![delete, backup("backup_full"), backup("backup_fast")]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(planned.is_curable());
        let curative: Vec<&ToolName> = planned
            .plans
            .iter()
            .filter_map(|plan| match plan {
                RemedyPlan::Redispatch(r) => Some(r.tool()),
                RemedyPlan::Executable(_) => None,
            })
            .collect();
        assert_eq!(
            curative,
            vec![&ToolName::new("backup_fast"), &ToolName::new("backup_full")]
        );
    }

    #[test]
    fn an_emitters_own_unmet_static_includes_does_not_gate_the_offer() {
        let delete = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("backup.done")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        DeclaredAudience::restricted([ReaderId::new("auditor")]),
                    ))],
                },
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![delete, backup]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::restricted([ReaderId::new("insider")])))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert_eq!(
            planned.plans,
            vec![redispatch("backup", vec![Gap::Prior(EffectKind::new("backup.done"))])]
        );
    }

    #[test]
    fn a_placeholder_bearing_emitter_is_advertised_like_any_other() {
        let archive = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("archive"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
        };
        let send = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("send"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::test_string_argument_schema("to"),
            emits: EffectSet::new([EffectKind::new("email.sent")]).unwrap(),
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![archive, send]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("archive", json!({})));
        assert!(planned.is_curable());
        assert!(matches!(
            planned.plans.first(),
            Some(RemedyPlan::Redispatch(r)) if r.tool() == &ToolName::new("send")
        ));
    }

    #[test]
    fn prior_gap_without_emitter_is_terminal() {
        let delete = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![delete]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(!planned.is_curable());
    }

    #[test]
    fn attention_gap_routes_by_mark_not_scope() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![TagName::new("payments")],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope {
                tags: vec![TagName::new("unrelated")],
            },
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(
            exec(&planned.plans[0]).steps,
            vec![RemedyStep::Authorize(AuthorityName::new("officer"))]
        );
    }

    #[test]
    fn attention_with_wrong_mark_is_terminal() {
        let tool = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                attends: vec![MarkName::new("other")],
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![tool]),
            authorities: vec![officer],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(!planned.is_curable());
    }

    #[test]
    fn a_mutual_prerequisite_cycle_does_not_gate_the_direct_offer() {
        let a = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("a"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("ka")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("kb"))],
                ..Requires::default()
            },
        };
        let b = ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new("b"),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::new([EffectKind::new("kb")]).unwrap(),
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("ka"))],
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: declared(vec![a, b]),
            authorities: vec![],
            sanitizers: vec![],
            audience: crate::audience::AudienceConfig::default(),
            annotators: vec![],
        });
        let log = vec![opened(known(TRUSTED, Audience::public()))];
        let planned = plan_of(&registry, &log, &call("a", json!({})));
        assert_eq!(
            planned.plans,
            vec![redispatch("b", vec![Gap::Prior(EffectKind::new("kb"))])]
        );
    }

    mod reference {
        use super::*;

        fn readers(audience: &Audience) -> Option<BTreeSet<ReaderId>> {
            if audience.is_public() {
                return None;
            }
            let mut clauses = audience.clauses();
            let clause = clauses.next().expect("a non-public literal audience holds a clause");
            assert!(clauses.next().is_none(), "reference audiences are literal");
            assert!(clause.chain().is_none() && clause.groups().next().is_none());
            Some(clause.readers().clone())
        }

        fn intersect(a: &Audience, b: &Audience) -> Audience {
            match (readers(a), readers(b)) {
                (None, _) => b.clone(),
                (_, None) => a.clone(),
                (Some(a), Some(b)) => Audience::restricted(a.intersection(&b).cloned()),
            }
        }

        fn within(audience: &Audience, cap: &Audience) -> bool {
            match (readers(audience), readers(cap)) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(a), Some(c)) => a.is_subset(&c),
            }
        }

        pub(super) fn coverable(authorities: &[Authority], gap: &Gap) -> bool {
            authorities.iter().any(|authority| match gap {
                Gap::TrustFloor { required, .. } => authority
                    .mandate
                    .trust_ceiling
                    .is_some_and(|ceiling| ceiling >= *required),
                Gap::Includes { recipients } => {
                    authority.mandate.reader_ceiling.as_ref().is_some_and(|ceiling| {
                        within(&Audience::of_declared(recipients), &Audience::of_declared(ceiling))
                    })
                }
                Gap::NoPrior(kind) => authority.mandate.waivers.contains(kind),
                Gap::Attention(mark) => authority.mandate.attends.contains(mark),
                Gap::Prior(_) | Gap::Cap { .. } => false,
            })
        }

        pub(super) fn direct_set(registry: &Registry, current: &Label, raw: &RawBlock) -> Vec<(ToolName, Vec<Gap>)> {
            let mut expected = Vec::new();
            for tool in registry.tools().filter_map(crate::contract::ToolDeclaration::declared) {
                let narrowed = match &tool.delta {
                    Delta {
                        audience: Some(delta), ..
                    } => Some(intersect(&current.audience, &Audience::of_declared(delta))),
                    _ => None,
                };
                let clears: Vec<Gap> = raw
                    .requirement_gaps
                    .iter()
                    .filter(|gap| match gap {
                        Gap::Prior(kind) => tool.emits.contains(kind),
                        Gap::Cap { cap } => narrowed
                            .as_ref()
                            .is_some_and(|audience| within(audience, &Audience::of_declared(cap))),
                        _ => false,
                    })
                    .cloned()
                    .collect();
                if !clears.is_empty() {
                    expected.push((tool.name.clone(), clears));
                }
            }
            expected
        }
    }

    fn decided(evaluation: crate::label::Evaluation) -> bool {
        match evaluation {
            crate::label::Evaluation::Holds => true,
            crate::label::Evaluation::Fails => false,
            crate::label::Evaluation::Needs(needed) => {
                panic!("literal audiences leave nothing undecided: {needed:?}")
            }
        }
    }

    fn redispatch(tool: &str, clears: Vec<Gap>) -> RemedyPlan {
        let plan = RedispatchPlan::new(ToolName::new(tool), clears).expect("a test claim is a valid redispatch");
        RemedyPlan::Redispatch(plan)
    }

    #[derive(Clone, Debug)]
    struct State {
        label: Label,
        effects: BTreeSet<EffectKind>,
        reservations: BTreeSet<EffectKind>,
    }

    impl State {
        fn partial(&self) -> Label {
            self.label.clone()
        }
    }

    fn effect(name: &str) -> EffectKind {
        EffectKind::new(name)
    }

    fn small_effect() -> impl Strategy<Value = EffectKind> {
        prop_oneof![Just(effect("e0")), Just(effect("e1"))]
    }

    fn small_audience() -> impl Strategy<Value = Audience> {
        prop_oneof![
            Just(Audience::public()),
            Just(Audience::restricted([ReaderId::new("r0")])),
            Just(Audience::restricted([ReaderId::new("r0"), ReaderId::new("r1")])),
        ]
    }

    fn a_delta() -> impl Strategy<Value = Delta> {
        (
            prop::option::of((0u8..2).prop_map(Trust::new)),
            prop::option::of(small_audience().prop_map(DeclaredAudience::literal)),
        )
            .prop_map(|(trust, audience)| Delta { trust, audience })
    }

    fn an_includes() -> impl Strategy<Value = Option<AudienceRequirement>> {
        prop_oneof![
            Just(None),
            small_audience().prop_map(|a| Some(AudienceRequirement::Includes(RecipientSpec::Static(
                DeclaredAudience::literal(a)
            )))),
            Just(Some(AudienceRequirement::Includes(RecipientSpec::Placeholder(
                "to".into()
            )))),
        ]
    }

    fn a_requires() -> impl Strategy<Value = Requires> {
        (
            prop::option::of((0u8..2).prop_map(Trust::new)),
            prop::option::of(small_audience()),
            an_includes(),
            prop::collection::vec(small_effect().prop_map(HistoryRequirement::Prior), 0..2),
            prop::collection::vec(small_effect().prop_map(HistoryRequirement::NoPrior), 0..2),
            prop::bool::ANY,
        )
            .prop_map(|(floor, cap, includes, prior, no_prior, attend)| {
                let mut history = prior;
                history.extend(no_prior);
                let mut audience = Vec::new();
                if let Some(cap) = cap {
                    audience.push(AudienceRequirement::Cap(DeclaredAudience::literal(cap)));
                }
                if let Some(includes) = includes {
                    audience.push(includes);
                }
                Requires {
                    label: LabelRequirements {
                        trust_floor: floor,
                        audience,
                    },
                    history,
                    attention: if attend { vec![MarkName::new("m0")] } else { vec![] },
                }
            })
    }

    fn a_tool(index: usize) -> impl Strategy<Value = ToolAnnotation> {
        let name = ToolName::new(format!("t{index}"));
        (
            a_delta(),
            prop::collection::btree_set(small_effect(), 0..2),
            a_requires(),
        )
            .prop_map(move |(delta, emits, requires)| ToolAnnotation {
                description: Some("A test tool.".to_string()),
                name: name.clone(),
                tags: vec![],
                delta,
                parameters: crate::params::test_string_argument_schema("to"),
                emits: EffectSet::new(emits).expect("a btree_set draw is distinct"),
                requires,
            })
    }

    fn an_authority(index: usize) -> impl Strategy<Value = Authority> {
        let name = AuthorityName::new(format!("a{index}"));
        (
            prop::option::of((0u8..2).prop_map(Trust::new)),
            prop::option::of(small_audience()),
            prop::collection::vec(small_effect(), 0..2),
            prop::bool::ANY,
        )
            .prop_map(move |(trust_ceiling, reader_ceiling, waivers, attends)| Authority {
                name: name.clone(),
                mandate: Mandate {
                    trust_ceiling,
                    reader_ceiling: reader_ceiling.map(DeclaredAudience::literal),
                    waivers,
                    attends: if attends { vec![MarkName::new("m0")] } else { vec![] },
                },
                scope: Scope::default(),
                hint: None,
            })
    }

    fn a_state() -> impl Strategy<Value = State> {
        (
            (0u8..2).prop_map(Trust::new),
            small_audience(),
            prop::collection::btree_set(small_effect(), 0..2),
        )
            .prop_map(|(trust, audience, effects)| State {
                label: established(trust, audience),
                effects,
                reservations: BTreeSet::new(),
            })
    }

    fn synthetic_call(tool: &ToolAnnotation) -> ResolvedCall {
        ResolvedCall::new(tool.name.clone(), crate::params::test_arguments(&json!({})))
    }

    fn a_sanitizer(index: usize) -> impl Strategy<Value = Sanitizer> {
        let name = SanitizerName::new(format!("s{index}"));
        prop_oneof![
            (small_audience(), small_audience()).prop_map(|(from_includes, to)| DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(from_includes),
                to: DeclaredAudience::literal(to)
            }),
            ((0u8..2).prop_map(Trust::new), (0u8..2).prop_map(Trust::new))
                .prop_map(|(from_floor, to)| DeclaredTransition::Trust { from_floor, to }),
        ]
        .prop_map(move |transition| Sanitizer {
            name: name.clone(),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition,
            scope: Scope::default(),
            hint: None,
        })
    }

    proptest! {
        #[test]
        fn planner_agrees_with_reference_oracle(
            tools in prop::collection::vec(a_tool(0), 1..4),
            authorities in prop::collection::vec(an_authority(0), 0..3),
            sanitizers in prop::collection::vec(a_sanitizer(0), 0..2),
            state in a_state(),
            target in 0usize..3,
        ) {
            let tools: Vec<_> = tools.into_iter().enumerate().map(|(i, mut t)| {
                t.name = ToolName::new(format!("t{i}"));
                t
            }).collect();
            let authorities: Vec<_> = authorities.into_iter().enumerate().filter_map(|(i, mut a)| {
                a.name = AuthorityName::new(format!("a{i}"));
                if a.mandate.is_empty() { None } else { Some(a) }
            }).collect();
            let sanitizers: Vec<_> = sanitizers.into_iter().enumerate().map(|(i, mut s)| {
                s.name = SanitizerName::new(format!("s{i}"));
                s
            }).collect();

            let built = Registry::build_covered(RegistryConfig {
                trust_chain: chain(),
                tools: declared(tools),
                authorities,
                sanitizers,
                audience: crate::audience::AudienceConfig::default(),
                annotators: vec![],
            });
            if matches!(built, Err(crate::registry::LoadError::TooManyPlanAlternatives { .. })) {
                return Ok(());
            }
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count").declared().expect("generated tools are declared");
            let call = synthetic_call(contract);

            let has_committed = |kind: &EffectKind| state.effects.contains(kind);
            let has_reserved = |kind: &EffectKind| state.reservations.contains(kind);
            let parts = crate::label::TestContext::default();
            let eval = check::evaluate_state(
                contract,
                &state.partial(),
                &has_committed,
                &has_reserved,
                check::CallReads::Resolved(&call),
                &CallStage::default(),
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");
            if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
                return Ok(());
            }
            let raw = eval;

            let mut log = vec![opened(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, log.len() as u64);
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(
                &registry,
                &views,
                BlockedCall {
                    call: &call,
                    contract,
                    raw: &raw,
                    stage: &CallStage::default(),
                    role: CallRole::Ordinary,
                },
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");

            let coverable = raw
                .requirement_gaps
                .iter()
                .all(|gap| reference::coverable(registry.authorities(), gap));
            let has_executable = planned.plans.iter().any(|plan| plan.executable().is_some());
            prop_assert_eq!(has_executable, coverable);

            let offered: Vec<(ToolName, Vec<Gap>)> = planned
                .plans
                .iter()
                .filter_map(|plan| match plan {
                    RemedyPlan::Redispatch(redispatch) => {
                        Some((redispatch.tool().clone(), redispatch.clears().to_vec()))
                    }
                    RemedyPlan::Executable(_) => None,
                })
                .collect();
            let expected = match coverable || raw.requirement_gaps.is_empty() {
                true => Vec::new(),
                false => reference::direct_set(&registry, &state.partial(), &raw),
            };
            prop_assert_eq!(offered, expected);
        }

        #[test]
        fn every_call_stage_menu_fits_the_recomputed_bound(
            tools in prop::collection::vec(a_tool(0), 1..4),
            authorities in prop::collection::vec(an_authority(0), 0..3),
            sanitizers in prop::collection::vec(a_sanitizer(0), 0..2),
            state in a_state(),
            target in 0usize..3,
        ) {
            let tools: Vec<_> = tools.into_iter().enumerate().map(|(i, mut t)| {
                t.name = ToolName::new(format!("t{i}"));
                t
            }).collect();
            let authorities: Vec<_> = authorities.into_iter().enumerate().filter_map(|(i, mut a)| {
                a.name = AuthorityName::new(format!("a{i}"));
                if a.mandate.is_empty() { None } else { Some(a) }
            }).collect();
            let sanitizers: Vec<_> = sanitizers.into_iter().enumerate().map(|(i, mut s)| {
                s.name = SanitizerName::new(format!("s{i}"));
                s
            }).collect();
            let built = Registry::build_covered(RegistryConfig {
                trust_chain: chain(),
                tools: declared(tools),
                authorities,
                sanitizers,
                audience: crate::audience::AudienceConfig::default(),
                annotators: vec![],
            });
            if matches!(built, Err(crate::registry::LoadError::TooManyPlanAlternatives { .. })) {
                return Ok(());
            }
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count").declared().expect("generated tools are declared");
            let call = synthetic_call(contract);
            let has_committed = |kind: &EffectKind| state.effects.contains(kind);
            let has_reserved = |kind: &EffectKind| state.reservations.contains(kind);
            let parts = crate::label::TestContext::default();
            let eval = check::evaluate_state(
                contract,
                &state.partial(),
                &has_committed,
                &has_reserved,
                check::CallReads::Resolved(&call),
                &CallStage::default(),
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");
            if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
                return Ok(());
            }
            let raw = eval;
            let mut log = vec![opened(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, log.len() as u64);
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(
                &registry,
                &views,
                BlockedCall {
                    call: &call,
                    contract,
                    raw: &raw,
                    stage: &CallStage::default(),
                    role: CallRole::Ordinary,
                },
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");

            let authorities = registry.authorities();
            let mut bound: u128 = 1;
            let mut multiply = |competent: usize| bound = bound.saturating_mul(competent.max(1) as u128);
            if let Some(floor) = contract.requires.trust_floor() {
                multiply(
                    authorities
                        .iter()
                        .filter(|a| a.mandate.trust_ceiling.is_some_and(|ceiling| ceiling >= floor))
                        .count(),
                );
            }
            let mut seen_includes: Vec<&AudienceRequirement> = Vec::new();
            for requirement in contract.requires.audience_requirements() {
                if matches!(requirement, AudienceRequirement::Includes(_)) && !seen_includes.contains(&requirement) {
                    seen_includes.push(requirement);
                }
            }
            let includes_entries = seen_includes.len();
            for _ in 0..includes_entries {
                multiply(
                    authorities
                        .iter()
                        .filter(|a| a.mandate.reader_ceiling.is_some())
                        .count(),
                );
            }
            let no_priors: BTreeSet<&EffectKind> = contract
                .requires
                .history
                .iter()
                .filter_map(|requirement| match requirement {
                    HistoryRequirement::NoPrior(kind) => Some(kind),
                    HistoryRequirement::Prior(_) => None,
                })
                .collect();
            for kind in &no_priors {
                multiply(authorities.iter().filter(|a| a.mandate.waivers.contains(kind)).count());
            }
            let marks: BTreeSet<_> = contract.requires.attention_marks().iter().collect();
            for mark in &marks {
                multiply(authorities.iter().filter(|a| a.mandate.attends.contains(mark)).count());
            }
            let output_sanitizers = registry.sanitizers().filter(|sanitizer| sanitizer.on.output).count();
            bound = bound.saturating_mul(1 + output_sanitizers as u128);
            let priors: BTreeSet<&EffectKind> = contract
                .requires
                .history
                .iter()
                .filter_map(|requirement| match requirement {
                    HistoryRequirement::Prior(kind) => Some(kind),
                    HistoryRequirement::NoPrior(_) => None,
                })
                .collect();
            let has_cap = contract
                .requires.audience_requirements().iter()
                .any(|requirement| matches!(requirement, AudienceRequirement::Cap(DeclaredAudience::Union(_))));
            let redispatches = registry
                .tools()
                .filter_map(ToolDeclaration::declared)
                .filter(|candidate| {
                    candidate.emits.iter().any(|kind| priors.contains(kind))
                        || (has_cap
                            && matches!(candidate.delta.audience.as_ref(), Some(DeclaredAudience::Union(_))))
                })
                .count() as u128;
            bound = bound.saturating_add(redispatches);

            prop_assert!(
                (planned.plans.len() as u128) <= bound,
                "menu of {} exceeds the recomputed bound {}",
                planned.plans.len(),
                bound
            );
        }

        #[test]
        fn planner_enumerates_exactly_the_sound_assignments(
            tools in prop::collection::vec(a_tool(0), 1..3),
            authorities in prop::collection::vec(an_authority(0), 0..3),
            state in a_state(),
            target in 0usize..3,
        ) {
            let tools: Vec<_> = tools.into_iter().enumerate().map(|(i, mut t)| {
                t.name = ToolName::new(format!("t{i}"));
                t
            }).collect();
            let authorities: Vec<_> = authorities.into_iter().enumerate().filter_map(|(i, mut a)| {
                a.name = AuthorityName::new(format!("a{i}"));
                if a.mandate.is_empty() { None } else { Some(a) }
            }).collect();
            let built = Registry::build_covered(RegistryConfig {
                trust_chain: chain(),
                tools: declared(tools),
                authorities: authorities.clone(),
                sanitizers: vec![],
                audience: crate::audience::AudienceConfig::default(),
                annotators: vec![],
            });
            if matches!(built, Err(crate::registry::LoadError::TooManyPlanAlternatives { .. })) {
                return Ok(());
            }
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count").declared().expect("generated tools are declared");
            let call = synthetic_call(contract);
            let has_committed = |kind: &EffectKind| state.effects.contains(kind);
            let has_reserved = |kind: &EffectKind| state.reservations.contains(kind);
            let parts = crate::label::TestContext::default();
            let eval = check::evaluate_state(
                contract,
                &state.partial(),
                &has_committed,
                &has_reserved,
                check::CallReads::Resolved(&call),
                &CallStage::default(),
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");
            if eval.requirement_gaps.is_empty() && eval.narrowing.is_none() {
                return Ok(());
            }
            let raw = eval;

            let mut log = vec![opened(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, log.len() as u64);
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(
                &registry,
                &views,
                BlockedCall {
                    call: &call,
                    contract,
                    raw: &raw,
                    stage: &CallStage::default(),
                    role: CallRole::Ordinary,
                },
                &parts.context(),
            )
            .expect("literal audiences leave nothing undecided");

            let competent = |authority: &Authority, gap: &Gap| -> bool {
                let scoped = authority.scope.covers(&contract.tags);
                match gap {
                    Gap::TrustFloor { required, .. } =>
                        scoped && authority.mandate.trust_ceiling.is_some_and(|c| c >= *required),
                    Gap::Includes { recipients } => scoped && authority.mandate.reader_ceiling.as_ref()
                        .is_some_and(|c| decided(Audience::of_declared(c).includes(recipients, &parts.context()))),
                    Gap::NoPrior(kind) => scoped && authority.mandate.waivers.contains(kind),
                    Gap::Attention(mark) => authority.mandate.attends.contains(mark),
                    Gap::Prior(_) | Gap::Cap { .. } => false,
                }
            };
            let per_gap: Vec<Vec<&Authority>> = raw.requirement_gaps.iter()
                .map(|gap| authorities.iter().filter(|a| competent(a, gap)).collect())
                .collect();
            let enumerated: Vec<Vec<(AuthorityName, Vec<Gap>)>> = if per_gap.iter().any(Vec::is_empty) {
                Vec::new()
            } else {
                let mut combos: Vec<Vec<(AuthorityName, Vec<Gap>)>> = vec![Vec::new()];
                for (gap, options) in raw.requirement_gaps.iter().zip(&per_gap) {
                    let mut next = Vec::new();
                    for combo in &combos {
                        for option in options {
                            let mut grouped = combo.clone();
                            match grouped.iter_mut().find(|(name, _)| name == &option.name) {
                                Some((_, covers)) => covers.push(gap.clone()),
                                None => grouped.push((option.name.clone(), vec![gap.clone()])),
                            }
                            next.push(grouped);
                        }
                    }
                    combos = next;
                }
                let mut unique = Vec::new();
                for combo in combos {
                    if !unique.contains(&combo) {
                        unique.push(combo);
                    }
                }
                unique
            };

            let mandate_of = |name: &AuthorityName| {
                &authorities.iter().find(|a| &a.name == name).expect("assignments name generated authorities").mandate
            };
            let power_cmp = |gap: &Gap, a: &AuthorityName, b: &AuthorityName| -> Option<std::cmp::Ordering> {
                use std::cmp::Ordering as O;
                let inclusion = |a_in_b: bool, b_in_a: bool| match (a_in_b, b_in_a) {
                    (true, true) => Some(O::Equal),
                    (true, false) => Some(O::Less),
                    (false, true) => Some(O::Greater),
                    (false, false) => None,
                };
                let (a, b) = (mandate_of(a), mandate_of(b));
                match gap {
                    Gap::TrustFloor { .. } => Some(a.trust_ceiling.unwrap().cmp(&b.trust_ceiling.unwrap())),
                    Gap::Includes { .. } => {
                        let ceiling = |mandate: &Mandate| mandate.reader_ceiling.clone().unwrap();
                        let (ca, cb) = (ceiling(a), ceiling(b));
                        let covers = |outer: &DeclaredAudience, inner: &DeclaredAudience| {
                            decided(Audience::of_declared(outer).includes(inner, &parts.context()))
                        };
                        inclusion(covers(&cb, &ca), covers(&ca, &cb))
                    }
                    Gap::NoPrior(_) => {
                        let sa: std::collections::BTreeSet<_> = a.waivers.iter().collect();
                        let sb: std::collections::BTreeSet<_> = b.waivers.iter().collect();
                        inclusion(sa.is_subset(&sb), sb.is_subset(&sa))
                    }
                    Gap::Attention(_) | Gap::Prior(_) | Gap::Cap { .. } => Some(O::Equal),
                }
            };
            let precedes = |a: &Vec<(AuthorityName, Vec<Gap>)>, b: &Vec<(AuthorityName, Vec<Gap>)>| -> bool {
                let assigned = |combo: &Vec<(AuthorityName, Vec<Gap>)>, gap: &Gap| {
                    combo.iter().find(|(_, covers)| covers.contains(gap)).expect("every gap is covered").0.clone()
                };
                let mut strictly_less = false;
                for gap in &raw.requirement_gaps {
                    match power_cmp(gap, &assigned(a, gap), &assigned(b, gap)) {
                        Some(std::cmp::Ordering::Less) => strictly_less = true,
                        Some(std::cmp::Ordering::Equal) => {}
                        Some(std::cmp::Ordering::Greater) | None => return false,
                    }
                }
                strictly_less
            };
            let mut expected: Vec<Vec<(AuthorityName, Vec<Gap>)>> = Vec::with_capacity(enumerated.len());
            let mut used = vec![false; enumerated.len()];
            for _ in 0..enumerated.len() {
                let next = (0..enumerated.len())
                    .filter(|&i| !used[i])
                    .find(|&i| (0..enumerated.len())
                        .filter(|&j| !used[j] && j != i)
                        .all(|j| !precedes(&enumerated[j], &enumerated[i])))
                    .expect("a finite strict partial order has a minimal element");
                used[next] = true;
                expected.push(enumerated[next].clone());
            }

            let actual: Vec<Vec<(AuthorityName, Vec<Gap>)>> = planned.plans.iter()
                .filter_map(RemedyPlan::executable)
                .map(|p| p.required.iter().map(|r| (r.authority.clone(), r.covers.clone())).collect())
                .collect();
            for i in 0..actual.len() {
                for j in (i + 1)..actual.len() {
                    prop_assert!(!precedes(&actual[j], &actual[i]),
                        "presented order inverts a dominance edge: {:?} precedes {:?}", actual[j], actual[i]);
                }
            }
            prop_assert_eq!(actual, expected);
        }
    }

    fn committed_effect(kind: EffectKind) -> Fact {
        let dispatch = crate::value::DispatchId::new(
            traj(),
            ResolvedCall::new(
                ToolName::new("seed"),
                crate::params::test_arguments(&json!({ "k": kind.as_str() })),
            )
            .digest(),
            0,
        );
        Fact::DispatchClosed {
            trajectory: traj(),
            dispatch,
            outcome: crate::fact::CloseOutcome::Success {
                effects: EffectSet::new([kind]).unwrap(),
            },
        }
    }
}
