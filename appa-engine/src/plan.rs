//! Remedy planning: turning a raw block into the sound remedies the agent may act on.
//!
//! A [`PlannedBlock`] carries the block as found plus **executable plans** (atomic
//! `Authorize`/`Accept` compositions run through `execute_remedy_plan`) and **prose recommendations**
//! (`Redispatch` — call another tool first, then re-propose; `Fork` — advisory only). The security
//! claim lives here: an **empty** set of executable plans *and* curative recommendations is a *proof*
//! that the block is unliftable — relative to the implemented remedy subset (spec §"Remedy plans":
//! "an empty list is a proof, not a shrug").
//!
//! **Curability is reachability over a finite transition system.** A state is `(committed label,
//! effect history)`; a transition runs a tool that is *directly clearable* at the current state
//! (every gap covered by one atomic ruling, its narrowing accepted), moving to the state that tool's
//! success would produce. A call is curable iff some reachable state clears it directly. The system
//! is finite — labels only descend, effects only grow, both over finite domains — so the search
//! terminates. The production planner is a gap-guarded depth-first search; the completeness proof
//! (tests) checks it against an independently-implemented forward-closure reference planner.
//!
//! **Alternatives.** A clearable block offers **every sound alternative**: one plan per unique
//! grouped authority assignment (per-gap choice among competent authorities), enumeration made
//! total by the registry's load-time bound ([`crate::registry`]'s `MAX_PLAN_ALTERNATIVES`) — no
//! runtime truncation. Curability itself is assignment-independent (any competent authority
//! suffices), so the reachability search and its reference oracle stay on the cheap first-choice
//! form; a separate assignment-set property checks the enumeration set-equal against an
//! independent reference enumerator.
//!
//! **Implemented remedy subset (the honest bound).** `Authorize` (trust floor via `trust_ceiling`,
//! `includes` via `reader_ceiling`, `no_prior` via `waivers`, attention via `attends`), `Accept`
//! (narrowing), and `Redispatch` over `prior(k)` emitters and cap-narrowing tools. A redispatched
//! prerequisite's own **placeholder** `includes($recipient)` is treated as satisfiable (the agent
//! supplies a valid recipient when it actually runs the tool) — an over-approximation, the safe
//! direction for the proof (it never falsely marks a curable block terminal). Its **static**
//! `includes` is a real requirement: the recipients are fixed and the audience only ever narrows,
//! so an *unmet* one is cured by nothing but a covering authority — never advertised without. A
//! **pending-cast** output dimension
//! transitions as identity, the same direction: the resolved label is unknowable statically, so
//! the search may advertise a redispatch whose actual resolution turns out too narrow. Following
//! such a hint is never an unchecked flow — the redispatched call and the retried block are both
//! checked for real — but it is more than wasted turns: the prerequisite's *effects commit* even
//! when its resolution then fails to cure the target. (An unannotated tool transitions as identity
//! for the same reason — its Unknown contribution folds only at admission — with the same caveat.) Those effects are ones the policy allows
//! that call to commit on its own terms, so soundness holds; a deployment for which such a
//! permitted-but-unhelpful side effect is unacceptable should not declare a pending-cast emitter
//! for a `prior(k)` currency (every curative first redispatch is recommended, in name order — the
//! agent picks, and each redispatch is separately checked for real). The pending-cast
//! post-resolution *narrowing* is
//! conversely never counted as a cap cure, which is covered by the cast de-scope below, not a
//! completeness hole. **De-scoped — each spec-marked, so the claim and the spec's enumeration
//! coincide:** sanitizer-backed compiled composites (spec: deferred, confining deployments only)
//! and input-sanitizer argument substitution (spec: design direction, refused at load) and cast
//! resolution of an Unknown (spec: attempted by the harness itself at check and at admission,
//! never surfaced as a plan object). The empty-proof is complete over exactly this subset.
//!
//! Blocked **child returns** are planned separately with their own closed vocabulary
//! ([`crate::branch::ReturnPlan`]: accept, or sanitize with an optional accepted residual) — a
//! return crossing
//! has no dispatch, no gaps, and no authorities, so none of this module's tool-block machinery
//! applies to it, and its sanitizer remedies do not move the de-scope bound above.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authority::Authority;
use crate::check::{self, CheckOutcome, Gap, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::fact::EffectKind;
use crate::label::{Adequacy, Dim, Label};
use crate::names::{AuthorityName, TagName};
use crate::projection::Views;
use crate::registry::Registry;
use crate::value::{ResolvedCall, ToolName};

/// A plan's id within a [`PlannedBlock`]: the token the runtime passes to `execute_remedy_plan`.
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

/// One engine-side act in an executable plan. Both are atomic and change no trajectory label by
/// themselves: `Authorize` records a ruling that admits the dispatch despite a gap; `Accept` records
/// the agent's acceptance of the narrowing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemedyStep {
    /// A ruling by this authority covers one or more of the block's requirement gaps.
    Authorize(AuthorityName),
    /// The agent accepts exactly this narrowing (the frontier loss the delta would commit). The
    /// offered narrowing is embedded so a stale acceptance mismatches by value, like the
    /// child-return plans ([`crate::branch::ReturnPlan`]) — plans re-derive live and match by
    /// value, so a plan minted before the fold moved cannot accept the newly live narrowing.
    Accept(Narrowing),
}

/// An executable remedy plan: an atomic composition of steps that clears the **whole** block.
/// The plan value *is* its authority assignment: `required` carries, per authority, the exact gaps
/// its one ruling must cover, so execution validates the supplied rulings against precisely the
/// grouping that was offered — overlapping mandates cannot silently reroute it, and a stale handle
/// cannot retarget a different assignment (plans re-derive and match by value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemedyPlan {
    pub id: PlanId,
    pub steps: Vec<RemedyStep>,
    pub required: Vec<RequiredRuling>,
}

/// A prose remedy the agent carries out itself as ordinary, separately-checked calls — never atomic
/// with the blocked call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recommendation {
    /// Run `tool` first (satisfying a `prior(k)` or narrowing within a cap), then re-propose. Emitted
    /// **only when curative**: the named tool is itself curable and running it makes the call curable.
    Redispatch { tool: ToolName, reason: String },
    /// Handle the work in a subagent. Advisory only — a child begins at the same label, so a fork
    /// cures no requirement. **Never counts toward curability.**
    Fork { reason: String },
}

impl Recommendation {
    /// Does this recommendation, if followed, actually lift the block? `Fork` never does.
    pub fn is_curative(&self) -> bool {
        matches!(self, Recommendation::Redispatch { .. })
    }
}

/// A block with its remedies attached: the raw gaps/narrowing, the executable plans, and the prose
/// recommendations. [`PlannedBlock::is_curable`] is the security-relevant verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedBlock {
    pub raw: RawBlock,
    pub plans: Vec<RemedyPlan>,
    pub recommendations: Vec<Recommendation>,
}

impl PlannedBlock {
    /// Is any remedy available? An executable plan, or a curative recommendation. **Empty is a proof
    /// the block is unliftable** over the implemented remedy subset — the agent should not spend
    /// turns on it.
    pub fn is_curable(&self) -> bool {
        !self.plans.is_empty() || self.recommendations.iter().any(Recommendation::is_curative)
    }
}

/// A node in the remedy transition system: the branch's committed label and the family's effects.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    label: Label,
    effects: BTreeSet<EffectKind>,
}

/// Plan the remedies for a raw block. Emits the executable plans when the block clears in one atomic
/// step, and every curative `Redispatch` when only a prior tool call unlocks it; `Fork` is always
/// advisory. See the module docs for the curability model.
pub(crate) fn plan(registry: &Registry, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> PlannedBlock {
    let start = State {
        label: views.current_label(),
        effects: views.present_effects(),
    };

    let plans = enumerate_plans(registry, &start, call);

    let mut recommendations = Vec::new();
    // Only when the block does not clear atomically do we look for curative first redispatches — the
    // first edge of a curative path is a tool directly clearable *at the start state*, so running it
    // skips no prerequisite (this is what keeps the planner's verdict identical to the oracle's).
    if plans.is_empty() {
        for (tool, reason) in curative_redispatches(registry, &start, call, raw) {
            recommendations.push(Recommendation::Redispatch { tool, reason });
        }
    }
    // Fork advice is context-sensitive. Whenever the call narrows it is genuinely actionable: the
    // child begins at this label, accepts the narrowing itself, and the parent's label stays —
    // requirement gaps follow the child unchanged and must still be remedied there. On a gap-only
    // block it cures nothing — a child begins at the same label, so every gap follows.
    let fork_reason = match (&raw.narrowing, raw.requirement_gaps.is_empty()) {
        (Some(_), true) => {
            "delegate the restricting work to a child session: the child accepts this narrowing and this session's label stays"
        }
        (Some(_), false) => {
            "delegate the restricting work to a child session: the child accepts this narrowing and remedies the requirement gaps there, and this session's label stays"
        }
        (None, _) => {
            "handle in a subagent (advisory: a child begins at the same label, so a fork cures no requirement)"
        }
    };
    recommendations.push(Recommendation::Fork {
        reason: fork_reason.to_string(),
    });

    PlannedBlock {
        raw: raw.clone(),
        plans,
        recommendations,
    }
}

/// Is `call` clearable at `state` by one atomic plan? `Some(steps)` when every requirement gap has a
/// covering authority and the narrowing (if any) is accepted; `None` when a gap is a redispatch
/// species (`prior`/`cap`), has no covering authority, or the committed label is still Unknown.
/// First-registered routing only — curability does not depend on *which* competent authority rules,
/// so the reachability search and the reference oracle stay on this cheap form.
fn directly_clearable(registry: &Registry, state: &State, call: &ResolvedCall) -> Option<Vec<RemedyStep>> {
    let contract = registry.tool(call.tool())?;
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    match check::evaluate_state(
        registry,
        contract,
        &state.label,
        &has_effect,
        call,
        check::PlaceholderGaps::FailClosed,
    ) {
        CheckOutcome::Allow => Some(Vec::new()),
        CheckOutcome::Unresolved(_) => None,
        CheckOutcome::Block(block) => {
            let mut steps = Vec::new();
            for gap in &block.requirement_gaps {
                // One ruling by an authority covers one or more gaps — emit each authority once.
                let step = RemedyStep::Authorize(authority_for(registry, gap, &contract.tags)?.clone());
                if !steps.contains(&step) {
                    steps.push(step);
                }
            }
            if let Some(narrowing) = block.narrowing {
                steps.push(RemedyStep::Accept(narrowing));
            }
            Some(steps)
        }
    }
}

/// Every sound plan for `call` at `state`: one per **unique grouped authority assignment**. Each
/// requirement gap independently chooses among its competent authorities (registration order, so
/// plan 0 is the all-first-choices assignment); a choice combination groups into per-authority
/// covers, and combinations whose groupings coincide are one plan. Enumeration is total — the
/// registry's load lint bounds the worst-case assignment count, so there is no runtime truncation
/// and "every sound alternative" holds literally. Empty when a gap has no competent authority, the
/// state is unresolved, or the block needs no atomic plan.
fn enumerate_plans(registry: &Registry, state: &State, call: &ResolvedCall) -> Vec<RemedyPlan> {
    let Some(contract) = registry.tool(call.tool()) else {
        return Vec::new();
    };
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    let block = match check::evaluate_state(
        registry,
        contract,
        &state.label,
        &has_effect,
        call,
        check::PlaceholderGaps::FailClosed,
    ) {
        CheckOutcome::Block(block) => block,
        CheckOutcome::Allow | CheckOutcome::Unresolved(_) => return Vec::new(),
    };

    // Per gap, all competent authorities. Any gap with none makes the block plan-free (a
    // prior/cap gap has no covering mandate by construction).
    let mut choices: Vec<Vec<&AuthorityName>> = Vec::with_capacity(block.requirement_gaps.len());
    for gap in &block.requirement_gaps {
        let competent: Vec<&AuthorityName> = registry
            .authorities()
            .iter()
            .filter(|authority| covers_gap(authority, gap, &contract.tags))
            .map(|authority| &authority.name)
            .collect();
        if competent.is_empty() {
            return Vec::new();
        }
        choices.push(competent);
    }

    let mut plans: Vec<RemedyPlan> = Vec::new();
    let mut assignment = vec![0usize; choices.len()];
    loop {
        // Group this combination's per-gap choices into per-authority covers, in gap order.
        let mut required: Vec<RequiredRuling> = Vec::new();
        for (index, gap) in block.requirement_gaps.iter().enumerate() {
            let authority = choices[index][assignment[index]].clone();
            match required.iter_mut().find(|r| r.authority == authority) {
                Some(existing) => existing.covers.push(gap.clone()),
                None => required.push(RequiredRuling {
                    authority,
                    covers: vec![gap.clone()],
                }),
            }
        }
        if !plans.iter().any(|plan| plan.required == required) {
            let mut steps: Vec<RemedyStep> = required
                .iter()
                .map(|r| RemedyStep::Authorize(r.authority.clone()))
                .collect();
            if let Some(narrowing) = &block.narrowing {
                steps.push(RemedyStep::Accept(narrowing.clone()));
            }
            plans.push(RemedyPlan {
                id: PlanId(plans.len() as u32),
                steps,
                required,
            });
        }
        // Odometer over the per-gap choice indices.
        let mut position = choices.len();
        loop {
            if position == 0 {
                return plans;
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

/// Is `tool` runnable as a **redispatch prerequisite** at `state`? Like [`directly_clearable`], but a
/// *placeholder* `includes($recipient)` is waived — the agent supplies a recipient the trajectory
/// already covers when it actually redispatches (a synthetic no-argument call cannot know it). The
/// waiver is structural ([`check::PlaceholderGaps::Waived`] skips the unresolvable spec inside the
/// gap logic itself), never inferred from a gap's recipient value — a static contract could legally
/// declare a sentinel-shaped reader. It over-approximates the transition relation, the *safe*
/// direction for the empty-proof: it can only add curative paths, never falsely mark a curable
/// block terminal. An **unmet static** `includes` gap gets no such pass: its recipients are fixed
/// in the contract and the audience only ever narrows, so it is cured by nothing but a covering
/// authority at the prerequisite's own dispatch — waving it through would advertise a redispatch
/// that can never actually run.
fn prerequisite_runnable(registry: &Registry, state: &State, tool: &ToolContract) -> bool {
    let call = synthetic_call(tool);
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    match check::evaluate_state(
        registry,
        tool,
        &state.label,
        &has_effect,
        &call,
        check::PlaceholderGaps::Waived,
    ) {
        CheckOutcome::Allow => true,
        CheckOutcome::Unresolved(_) => false,
        CheckOutcome::Block(block) => block
            .requirement_gaps
            .iter()
            .all(|gap| authority_for(registry, gap, &tool.tags).is_some()),
    }
}

/// The rulings a block's remedy plan needs gathered: for each authority the block routes to, the gaps
/// its ruling must cover. The mandate routing (which authority covers which gap) stays here in the
/// engine; the runtime only gathers a ruling from each named authority for its gaps and hands them to
/// `execute_plan`. A gap with no covering authority is omitted — the plan is then not executable and
/// `execute_plan` reports the gap uncovered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredRuling {
    pub authority: AuthorityName,
    pub covers: Vec<Gap>,
}

/// The first registered authority whose mandate reaches `gap` (and whose scope covers the call's
/// tags, except attention which routes by its mark alone). `prior`/`cap` have no covering mandate —
/// no ruling raises history or narrows the label.
fn authority_for<'r>(registry: &'r Registry, gap: &Gap, tags: &[TagName]) -> Option<&'r AuthorityName> {
    registry
        .authorities()
        .iter()
        .find(|authority| covers_gap(authority, gap, tags))
        .map(|authority| &authority.name)
}

pub(crate) fn covers_gap(authority: &Authority, gap: &Gap, tags: &[TagName]) -> bool {
    let mandate = &authority.mandate;
    match gap {
        Gap::TrustFloor { required, .. } => {
            authority.scope.covers(tags) && mandate.trust_ceiling.is_some_and(|ceiling| ceiling >= *required)
        }
        Gap::Includes { recipients } => {
            authority.scope.covers(tags)
                && mandate
                    .reader_ceiling
                    .as_ref()
                    .is_some_and(|ceiling| Dim::Known(ceiling.clone()).covers(recipients) == Adequacy::Holds)
        }
        Gap::NoPrior(kind) => authority.scope.covers(tags) && mandate.waivers.contains(kind),
        // Attention routes by its own currency — the attended mark — never by scope.
        Gap::Attention(mark) => mandate.attends.contains(mark),
        Gap::Prior(_) | Gap::Cap { .. } => false,
    }
}

/// The state a tool's success would produce: its effects added, its **effective** contribution
/// folded in (a sanitizer-bound tool folds its bound derivation's label, matching the check; a
/// pending-cast dimension or an unannotated tool folds identity — the module-doc
/// over-approximation).
fn transition(registry: &Registry, state: &State, tool: &ToolContract) -> State {
    let mut effects = state.effects.clone();
    effects.extend(tool.emits.iter().cloned());
    State {
        label: check::committed_label(registry, tool, &state.label),
        effects,
    }
}

/// A no-argument call standing in for a redispatched tool. It can resolve no `includes`
/// placeholder, so [`prerequisite_runnable`] evaluates it under
/// [`check::PlaceholderGaps::Waived`] — the unresolvable placeholder specs are skipped inside the
/// gap logic itself, while static `includes` gaps keep needing a covering authority. The planner
/// and the reference oracle share this convention, so they agree.
fn synthetic_call(tool: &ToolContract) -> ResolvedCall {
    ResolvedCall::new(tool.name.clone(), serde_json::Value::Null, Vec::new())
}

/// Is `call` curable at `state` — directly, or after a sequence of redispatches? Depth-first over the
/// transition system, transitioning only on tools directly clearable at the current state; the
/// `visiting` stack breaks cycles (a revisited state offers no new progress on this path).
fn curable(registry: &Registry, state: &State, call: &ResolvedCall, visiting: &mut Vec<State>) -> bool {
    if directly_clearable(registry, state, call).is_some() {
        return true;
    }
    // An Unknown committed label is never resolved by a redispatch (that is the cast path); treat it
    // as terminal so the search does not chase states that cannot clear this call.
    if is_unresolved(registry, state, call) {
        return false;
    }
    if visiting.contains(state) {
        return false;
    }
    visiting.push(state.clone());
    let cured = registry.tools().any(|tool| {
        if !prerequisite_runnable(registry, state, tool) {
            return false;
        }
        let next = transition(registry, state, tool);
        next != *state && curable(registry, &next, call, visiting)
    });
    visiting.pop();
    cured
}

fn is_unresolved(registry: &Registry, state: &State, call: &ResolvedCall) -> bool {
    match registry.tool(call.tool()) {
        None => true,
        Some(contract) => {
            let has_effect = |kind: &EffectKind| state.effects.contains(kind);
            matches!(
                check::evaluate_state(
                    registry,
                    contract,
                    &state.label,
                    &has_effect,
                    call,
                    check::PlaceholderGaps::FailClosed
                ),
                CheckOutcome::Unresolved(_)
            )
        }
    }
}

/// Find every curative first redispatch: each tool directly clearable at `start` whose success makes
/// `call` curable, in registry (name) order — "every sound alternative" holds for redispatch
/// recommendations as it does for executable plans. Ties each recommendation's prose to the gap the
/// tool addresses.
fn curative_redispatches(
    registry: &Registry,
    start: &State,
    call: &ResolvedCall,
    raw: &RawBlock,
) -> Vec<(ToolName, String)> {
    let mut curative = Vec::new();
    for tool in registry.tools() {
        if !prerequisite_runnable(registry, start, tool) {
            continue;
        }
        let next = transition(registry, start, tool);
        if next == *start {
            continue;
        }
        let mut visiting = Vec::new();
        if curable(registry, &next, call, &mut visiting) {
            curative.push((tool.name.clone(), redispatch_reason(tool, raw)));
        }
    }
    curative
}

fn redispatch_reason(tool: &ToolContract, raw: &RawBlock) -> String {
    let name = tool.name.as_str();
    for gap in &raw.requirement_gaps {
        match gap {
            Gap::Prior(kind) if tool.emits.contains(kind) => {
                return format!("run {name} first to satisfy prior({})", kind.as_str());
            }
            // Only an established audience delta is a narrowing the redispatch can promise; a
            // pending-cast or unannotated one contributes nothing until resolved.
            Gap::Cap { .. }
                if tool
                    .delta
                    .as_ref()
                    .is_some_and(|d| matches!(d.audience, Some(Dim::Known(_)))) =>
            {
                return format!("run {name} first to narrow the audience within the cap");
            }
            _ => {}
        }
    }
    format!("run {name} first, then re-propose")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Mandate, Scope};
    use crate::contract::{
        AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
    };
    use crate::fact::{Fact, Revision};
    use crate::label::{Audience, ReaderId, Trust};
    use crate::names::MarkName;
    use crate::projection::Projection;
    use crate::registry::{RegistryConfig, TrustChain};
    use crate::value::{LabeledValue, Provenance, ToolName, TrajectoryId, ValueBody};
    use proptest::prelude::*;
    use serde_json::json;

    const SUSPICIOUS: Trust = Trust::new(0);
    const TRUSTED: Trust = Trust::new(1);

    fn traj() -> TrajectoryId {
        TrajectoryId::new("t")
    }

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn build(config: RegistryConfig) -> Registry {
        Registry::build(config).unwrap()
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

    /// Run the real check + planner over a log, returning the planned block (call must be a block).
    fn plan_of(registry: &Registry, log: &[Fact], call: &ResolvedCall) -> PlannedBlock {
        let projection = Projection::build(log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let contract = registry.tool(call.tool()).unwrap();
        let raw = match check::evaluate(registry, contract, &views, call) {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        plan(registry, &views, call, &raw)
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args, vec![])
    }

    #[test]
    fn authorize_plan_clears_a_trust_floor_gap() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(planned.is_curable());
        assert_eq!(
            planned.plans[0].steps,
            vec![RemedyStep::Authorize(AuthorityName::new("officer"))]
        );
    }

    #[test]
    fn alternative_authorities_yield_one_plan_per_assignment() {
        // Two officers can cover the trust floor; only the attester attends the mark. The gaps
        // [floor, mark] choose independently → two plans, first-registered assignment first, each
        // carrying its exact grouped covers.
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let attester = Authority {
            name: AuthorityName::new("attester"),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![officer("officer-a"), officer("officer-b"), attester],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        let floor = Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        };
        let mark = Gap::Attention(MarkName::new("signoff"));
        assert_eq!(planned.plans.len(), 2);
        assert_eq!(planned.plans[0].id, PlanId::new(0));
        assert_eq!(
            planned.plans[0].required,
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
        assert_eq!(planned.plans[1].id, PlanId::new(1));
        assert_eq!(
            planned.plans[1].required,
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

    #[test]
    fn a_duplicated_requirement_entry_is_one_gap_and_mints_no_permuted_duplicates() {
        // `attention = ["signoff", "signoff"]` is one obligation: the check canonicalizes the gap
        // set, so two competent authorities yield exactly two plans (one per authority), never the
        // order-permuted pair a duplicate gap would mint.
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                attention: vec![MarkName::new("signoff"), MarkName::new("signoff")],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let attester = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![attester("a"), attester("b")],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(planned.plans.len(), 2);
        for plan in &planned.plans {
            assert_eq!(plan.required.len(), 1);
            assert_eq!(plan.required[0].covers, vec![Gap::Attention(MarkName::new("signoff"))]);
        }
    }

    #[test]
    fn one_authority_covering_both_gaps_is_one_grouped_ruling() {
        // A single authority legitimately attending the mark AND holding the trust ceiling covers
        // both gaps with one ruling — one plan, one required entry, both covers (the sound
        // single-Ruling assignment; distinct-issuer constraints are policy-language future work).
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(TRUSTED),
                attends: vec![MarkName::new("signoff")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(planned.plans.len(), 1);
        assert_eq!(planned.plans[0].required.len(), 1);
        assert_eq!(planned.plans[0].required[0].authority, AuthorityName::new("officer"));
        assert_eq!(planned.plans[0].required[0].covers.len(), 2);
    }

    #[test]
    fn no_competent_authority_is_terminal() {
        // A trust floor no mandate can reach: nothing raises the trajectory label → unliftable.
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(!planned.is_curable());
        assert!(planned.plans.is_empty());
        // Fork is present but excluded from curability.
        assert!(planned.recommendations.iter().all(|r| !r.is_curative()));
    }

    #[test]
    fn acceptance_plan_for_pure_narrowing() {
        let tool = ToolContract {
            name: ToolName::new("get"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")]))),
            }),
            emits: vec![],
            requires: Requires::default(),
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("get", json!({})));
        // A narrowing is never terminal: the always-available acceptance plan clears it — and the
        // plan embeds exactly the offered narrowing, so a stale acceptance mismatches by value.
        assert!(planned.is_curable());
        assert_eq!(
            planned.plans[0].steps,
            vec![RemedyStep::Accept(Narrowing {
                from: known(TRUSTED, Audience::Public),
                to: known(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
            })]
        );
    }

    #[test]
    fn prior_gap_cured_by_a_redispatch() {
        // delete_db needs prior(backup.done); backup emits it and is itself unblocked → curative.
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("db.deleted")],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let backup = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires::default(),
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![delete, backup],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(planned.is_curable());
        assert!(planned.plans.is_empty()); // a prior gap has no engine-side step
        assert!(matches!(
            planned.recommendations.iter().find(|r| r.is_curative()),
            Some(Recommendation::Redispatch { tool, .. }) if tool == &ToolName::new("backup")
        ));
    }

    #[test]
    fn prior_gap_with_multiple_emitters_surfaces_every_curative_redispatch() {
        // Two independent emitters of the required effect: both are curative first steps, and both
        // must be recommended — "every sound alternative", not the first in name order.
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let backup = |name: &str| ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires::default(),
            output_sanitizer: None,
        };
        // Registered in reverse name order, so the assertion below pins name-order iteration —
        // not an accident of registration order.
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![delete, backup("backup_full"), backup("backup_fast")],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(planned.is_curable());
        let curative: Vec<&ToolName> = planned
            .recommendations
            .iter()
            .filter_map(|r| match r {
                Recommendation::Redispatch { tool, .. } => Some(tool),
                Recommendation::Fork { .. } => None,
            })
            .collect();
        assert_eq!(
            curative,
            vec![&ToolName::new("backup_fast"), &ToolName::new("backup_full")]
        );
    }

    #[test]
    fn static_includes_prerequisite_without_covering_authority_is_not_advertised() {
        // The only emitter of the required effect itself requires a static includes the trajectory
        // cannot meet (the audience only narrows) and no authority covers. Advertising it would be
        // a redispatch that can never run — the block is terminal.
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let backup = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        Audience::restricted([ReaderId::new("auditor")]),
                    ))],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![delete, backup],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(
            TRUSTED,
            Audience::restricted([ReaderId::new("internal")]),
        ))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(!planned.is_curable());
        assert!(planned.recommendations.iter().all(|r| !r.is_curative()));
    }

    #[test]
    fn static_includes_prerequisite_with_covering_authority_is_advertised() {
        // Same registry, plus an authority whose reader ceiling reaches the prerequisite's static
        // recipients: the prerequisite can actually run (under that ruling), so it is curative.
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let backup = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Static(
                        Audience::restricted([ReaderId::new("auditor")]),
                    ))],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let voucher = Authority {
            name: AuthorityName::new("voucher"),
            mandate: Mandate {
                reader_ceiling: Some(Audience::restricted([ReaderId::new("auditor")])),
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![delete, backup],
            authorities: vec![voucher],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(
            TRUSTED,
            Audience::restricted([ReaderId::new("internal")]),
        ))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(planned.is_curable());
        assert!(matches!(
            planned.recommendations.iter().find(|r| r.is_curative()),
            Some(Recommendation::Redispatch { tool, .. }) if tool == &ToolName::new("backup")
        ));
    }

    #[test]
    fn a_sentinel_shaped_static_recipient_is_not_mistaken_for_a_placeholder() {
        // A static requirement legally naming the reader "<unresolved:to>" beside a placeholder
        // "to" must keep needing a covering authority: the placeholder waiver is structural
        // (the spec is skipped inside the gap logic), never inferred from the recipient value,
        // so the collision cannot wave the static obligation through.
        let archive = ToolContract {
            name: ToolName::new("archive"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let send = ToolContract {
            name: ToolName::new("send"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("email.sent")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![
                        AudienceRequirement::Includes(RecipientSpec::Static(Audience::restricted([ReaderId::new(
                            "<unresolved:to>",
                        )]))),
                        AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into())),
                    ],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![archive, send],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(
            TRUSTED,
            Audience::restricted([ReaderId::new("internal")]),
        ))];
        let planned = plan_of(&registry, &log, &call("archive", json!({})));
        assert!(!planned.is_curable());
    }

    #[test]
    fn placeholder_includes_prerequisite_is_still_advertised() {
        // Deterministic positive pin: the proptest oracle shares `prerequisite_runnable`, so it
        // cannot catch a regression that turns placeholder-bearing prerequisites terminal. The only
        // emitter's includes($recipient) cannot resolve on the synthetic call and there is no
        // reader-ceiling authority — it must still be advertised: the agent supplies the recipient
        // at real dispatch.
        let archive = ToolContract {
            name: ToolName::new("archive"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let send = ToolContract {
            name: ToolName::new("send"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("email.sent")],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![archive, send],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("archive", json!({})));
        assert!(planned.is_curable());
        assert!(matches!(
            planned.recommendations.iter().find(|r| r.is_curative()),
            Some(Recommendation::Redispatch { tool, .. }) if tool == &ToolName::new("send")
        ));
    }

    #[test]
    fn prior_gap_without_emitter_is_terminal() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![delete],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("delete_db", json!({})));
        assert!(!planned.is_curable());
    }

    #[test]
    fn attention_gap_routes_by_mark_not_scope() {
        // The authority has a foreign scope tag but attends the mark: attention ignores scope.
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![TagName::new("payments")],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
            output_sanitizer: None,
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
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(
            planned.plans[0].steps,
            vec![RemedyStep::Authorize(AuthorityName::new("officer"))]
        );
    }

    #[test]
    fn attention_with_wrong_mark_is_terminal() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                attention: vec![MarkName::new("signoff")],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let officer = Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                attends: vec![MarkName::new("other")],
                ..Mandate::default()
            },
            scope: Scope::default(),
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(!planned.is_curable());
    }

    #[test]
    fn cyclic_prerequisites_terminate_and_are_uncurable() {
        // a needs prior(kb) (emitted only by b); b needs prior(ka) (emitted only by a). Neither can
        // go first — the search must terminate and report the block uncurable.
        let a = ToolContract {
            name: ToolName::new("a"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("ka")],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("kb"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let b = ToolContract {
            name: ToolName::new("b"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("kb")],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("ka"))],
                ..Requires::default()
            },
            output_sanitizer: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![a, b],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("a", json!({})));
        assert!(!planned.is_curable());
    }

    // ---- Reference planner: an independent forward-closure search over the same finite system. ----
    mod reference {
        use super::*;

        /// The closure of states reachable from `start` by chaining tools directly clearable at each
        /// reached state — computed by naive fixed-point iteration (no gap-guided pruning).
        fn reachable(registry: &Registry, start: &State) -> Vec<State> {
            let mut states = vec![start.clone()];
            loop {
                let mut grew = false;
                for state in states.clone() {
                    for tool in registry.tools() {
                        if prerequisite_runnable(registry, &state, tool) {
                            let next = transition(registry, &state, tool);
                            if !states.contains(&next) {
                                states.push(next);
                                grew = true;
                            }
                        }
                    }
                }
                if !grew {
                    break;
                }
            }
            states
        }

        /// The call is curable iff some reachable state clears it directly.
        pub(super) fn curable(registry: &Registry, start: &State, call: &ResolvedCall) -> bool {
            reachable(registry, start)
                .iter()
                .any(|state| directly_clearable(registry, state, call).is_some())
        }
    }

    // ---- Generators for the planner-vs-oracle completeness proptest. ----

    fn effect(name: &str) -> EffectKind {
        EffectKind::new(name)
    }

    fn small_effect() -> impl Strategy<Value = EffectKind> {
        prop_oneof![Just(effect("e0")), Just(effect("e1"))]
    }

    fn small_audience() -> impl Strategy<Value = Audience> {
        prop_oneof![
            Just(Audience::Public),
            Just(Audience::restricted([ReaderId::new("r0")])),
            Just(Audience::restricted([ReaderId::new("r0"), ReaderId::new("r1")])),
        ]
    }

    /// Declared deltas (possibly partial or neutral) and the unannotated tool alike — the
    /// planner-vs-oracle law must hold over both.
    fn a_delta() -> impl Strategy<Value = Option<Delta>> {
        prop_oneof![
            Just(None),
            (
                prop::option::of((0u8..2).prop_map(|t| Dim::Known(Trust::new(t)))),
                prop::option::of(small_audience().prop_map(Dim::Known)),
            )
                .prop_map(|(trust, audience)| Some(Delta { trust, audience })),
        ]
    }

    fn an_includes() -> impl Strategy<Value = Option<AudienceRequirement>> {
        prop_oneof![
            Just(None),
            small_audience().prop_map(|a| Some(AudienceRequirement::Includes(RecipientSpec::Static(a)))),
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
                    audience.push(AudienceRequirement::Cap(cap));
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

    fn a_tool(index: usize) -> impl Strategy<Value = ToolContract> {
        let name = ToolName::new(format!("t{index}"));
        (a_delta(), prop::collection::vec(small_effect(), 0..2), a_requires()).prop_map(
            move |(delta, emits, mut requires)| {
                // The load lint refuses label requirements on an unannotated tool; generated
                // configs must load, so an undrawn delta strips them.
                if delta.is_none() {
                    requires.label = LabelRequirements::default();
                }
                ToolContract {
                    name: name.clone(),
                    tags: vec![],
                    delta,
                    emits,
                    requires,
                    output_sanitizer: None,
                }
            },
        )
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
                    reader_ceiling,
                    waivers,
                    attends: if attends { vec![MarkName::new("m0")] } else { vec![] },
                },
                scope: Scope::default(),
            })
    }

    fn a_state() -> impl Strategy<Value = State> {
        (
            (0u8..2).prop_map(Trust::new),
            small_audience(),
            prop::collection::btree_set(small_effect(), 0..2),
        )
            .prop_map(|(trust, audience, effects)| State {
                label: known(trust, audience),
                effects,
            })
    }

    proptest! {
        /// The production planner's curability verdict matches the independent reference oracle on
        /// every generated block — the empty-list completeness proof (spec §"Remedy plans").
        #[test]
        fn planner_agrees_with_reference_oracle(
            tools in prop::collection::vec(a_tool(0), 1..4),
            authorities in prop::collection::vec(an_authority(0), 0..3),
            state in a_state(),
            target in 0usize..3,
        ) {
            // Re-key the generated tools/authorities to distinct names.
            let tools: Vec<_> = tools.into_iter().enumerate().map(|(i, mut t)| {
                t.name = ToolName::new(format!("t{i}"));
                t
            }).collect();
            let authorities: Vec<_> = authorities.into_iter().enumerate().filter_map(|(i, mut a)| {
                a.name = AuthorityName::new(format!("a{i}"));
                // A mandate that grants nothing would be a load error — drop those.
                if a.mandate.is_empty() { None } else { Some(a) }
            }).collect();

            // The generators produce valid-by-construction configs (ranks within the chain,
            // re-keyed names, empty mandates dropped), so a build failure is a broken generator or
            // a validation change that silently shrank this property's coverage — fail loudly,
            // never skip. The one legitimate refusal is the alternative-count lint: a generated
            // config multiplying interchangeable authorities past the bound is a genuine scope
            // filter (such configs are unloadable by design), not lost coverage.
            let built = Registry::build(RegistryConfig {
                trust_chain: chain(),
                tools,
                authorities,
                sanitizers: vec![],
                casts: vec![],
            });
            if matches!(built, Err(crate::registry::LoadError::TooManyPlanAlternatives { .. })) {
                return Ok(());
            }
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count");
            let call = synthetic_call(contract);

            // Only blocks carry a planned remedy set; passing/unresolved calls are a genuine scope
            // filter for this property, not lost coverage (their behavior is pinned elsewhere).
            let has_effect = |kind: &EffectKind| state.effects.contains(kind);
            let raw = match check::evaluate_state(&registry, contract, &state.label, &has_effect, &call, check::PlaceholderGaps::FailClosed) {
                CheckOutcome::Block(raw) => raw,
                _ => return Ok(()),
            };

            // Drive the planner through the same public state (build a synthetic branch log).
            let mut log = vec![user_value(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, Revision::new(log.len() as u64));
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(&registry, &views, &call, &raw);

            let oracle = reference::curable(&registry, &state, &call);
            prop_assert_eq!(planned.is_curable(), oracle);
        }

        /// The planner's plan set is exactly the reference's assignment set — every sound grouped
        /// authority assignment, no more, no fewer, in the same order. The reference re-implements
        /// competence and grouping independently (the duplication is the point: the curability
        /// boolean above cannot see missing, duplicated, or mis-grouped alternatives).
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
            let built = Registry::build(RegistryConfig {
                trust_chain: chain(),
                tools,
                authorities: authorities.clone(),
                sanitizers: vec![],
                casts: vec![],
            });
            if matches!(built, Err(crate::registry::LoadError::TooManyPlanAlternatives { .. })) {
                return Ok(());
            }
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count");
            let call = synthetic_call(contract);
            let has_effect = |kind: &EffectKind| state.effects.contains(kind);
            let raw = match check::evaluate_state(&registry, contract, &state.label, &has_effect, &call, check::PlaceholderGaps::FailClosed) {
                CheckOutcome::Block(raw) => raw,
                _ => return Ok(()),
            };

            let mut log = vec![user_value(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, Revision::new(log.len() as u64));
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(&registry, &views, &call, &raw);

            // Independent competence: a deliberate re-statement of the mandate semantics.
            let competent = |authority: &Authority, gap: &Gap| -> bool {
                let scoped = authority.scope.covers(&contract.tags);
                match gap {
                    Gap::TrustFloor { required, .. } =>
                        scoped && authority.mandate.trust_ceiling.is_some_and(|c| c >= *required),
                    Gap::Includes { recipients } => scoped && authority.mandate.reader_ceiling.as_ref()
                        .is_some_and(|c| Dim::Known(c.clone()).covers(recipients) == Adequacy::Holds),
                    Gap::NoPrior(kind) => scoped && authority.mandate.waivers.contains(kind),
                    Gap::Attention(mark) => authority.mandate.attends.contains(mark),
                    Gap::Prior(_) | Gap::Cap { .. } => false,
                }
            };
            let per_gap: Vec<Vec<&Authority>> = raw.requirement_gaps.iter()
                .map(|gap| authorities.iter().filter(|a| competent(a, gap)).collect())
                .collect();
            let expected: Vec<Vec<(AuthorityName, Vec<Gap>)>> = if per_gap.iter().any(Vec::is_empty) {
                Vec::new()
            } else {
                // Cartesian product in registration order, grouped in gap order.
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
            let actual: Vec<Vec<(AuthorityName, Vec<Gap>)>> = planned.plans.iter()
                .map(|p| p.required.iter().map(|r| (r.authority.clone(), r.covers.clone())).collect())
                .collect();
            prop_assert_eq!(actual, expected);
        }
    }

    /// A closed dispatch that committed `kind` — the minimal way to seed a present family effect.
    fn committed_effect(kind: EffectKind) -> Fact {
        let dispatch = crate::value::DispatchId::new(
            traj(),
            ResolvedCall::new(ToolName::new("seed"), json!({ "k": kind.as_str() }), vec![]).digest(),
            0,
        );
        Fact::DispatchClosed {
            trajectory: traj(),
            dispatch,
            outcome: crate::fact::CloseOutcome::Success { effects: vec![kind] },
        }
    }
}
