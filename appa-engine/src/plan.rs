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
//! (every gap covered by one atomic ruling, its narrowing accepted), moving to a state that tool's
//! success could produce — the raw crossing, or the relabel of any output sanitizer the agent could
//! bind at that tool's own block ([`transitions`]). A call is curable iff some reachable state
//! clears it directly. The system is finite — labels only descend, effects only grow, both over
//! finite domains — so the search terminates. The production planner is a gap-guarded depth-first
//! search; the completeness proof (tests) checks it against an independently-implemented
//! forward-closure reference planner.
//!
//! **Alternatives.** A clearable block offers **every sound alternative**: each unique grouped
//! authority assignment (per-gap choice among competent authorities) crossed with each way of
//! settling the narrowing — acceptance, or an applicable output sanitizer with the residual it
//! cannot shed. Enumeration is made total by the registry's load-time bound ([`crate::registry`]'s
//! `MAX_PLAN_ALTERNATIVES`), which spans both factors — no runtime truncation. Curability itself is
//! assignment-independent (any competent authority suffices) and sanitizer-independent at the
//! blocked call (acceptance is always available, `RMD-11`), so the reachability search and its
//! reference oracle stay on the cheap first-choice form; a separate assignment-set property checks
//! the enumeration set-equal against an independent reference enumerator.
//!
//! **Implemented remedy subset (the honest bound).** `Authorize` (trust floor via `trust_ceiling`,
//! `includes` via `reader_ceiling`, `no_prior` via `waivers`, attention via `attends`), `Accept`
//! (narrowing), `Sanitize` (an output sanitizer's relabel standing in for the raw crossing,
//! `SAN-2`/`RMD-18`), and `Redispatch` over `prior(k)` emitters and cap-narrowing tools. A redispatched
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
//! coincide:** input-sanitizer argument substitution (spec: design direction, refused at load) and cast
//! resolution of an Unknown (spec: attempted by the harness itself at check and at admission,
//! never surfaced as a plan object). The empty-proof is complete over exactly this subset.
//!
//! **A sanitize step settles a narrowing and never a requirement gap**. The gaps are
//! evaluated on the raw committed label even in a plan that sanitizes: a requirement gates the
//! dispatch, and the dispatch happens before any derivation exists, so a promise to clean the
//! result afterwards cannot justify the release. That is the fail-closed direction, and it is why
//! adding the step leaves `is_curable` unchanged — acceptance was already always available for a
//! narrowing, so the sanitizer adds alternatives, never reachability, *at the blocked
//! call*. It does add reachability one hop out, which is what [`transitions`] accounts for.
//!
//! Blocked **child returns** are planned separately with their own closed vocabulary
//! ([`crate::branch::ReturnPlan`]: accept, or sanitize with an optional accepted residual) — a
//! return crossing
//! has no dispatch, no gaps, and no authorities, so none of this module's tool-block machinery
//! applies to it. The tool-output plans here mirror its shape deliberately.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authority::{Authority, Sanitizer};
use crate::check::{self, Gap, Narrowing, RawBlock};
use crate::contract::ToolContract;
use crate::fact::EffectKind;
use crate::label::{Adequacy, Dim, Label};
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemedyStep {
    Authorize(AuthorityName),
    Accept(Narrowing),
    Sanitize(SanitizerName),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableRemedyPlan {
    pub id: PlanId,
    pub steps: Vec<RemedyStep>,
    pub required: Vec<RequiredRuling>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedispatchEffect {
    Clears(Vec<Gap>),
    EnablesPath,
}

/// One way out of a block. A plan with an engine-side step is an executable object with
/// an id, run through `execute_remedy_plan`; one without names a call the agent makes for itself
/// and carries no id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemedyPlan {
    Executable(ExecutableRemedyPlan),
    Redispatch { tool: ToolName, effect: RedispatchEffect },
}

impl RemedyPlan {
    pub fn executable(&self) -> Option<&ExecutableRemedyPlan> {
        match self {
            RemedyPlan::Executable(plan) => Some(plan),
            RemedyPlan::Redispatch { .. } => None,
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
    /// subset — the assertion concerns requirement gaps and narrowing: an
    /// unestablished-only block is plan-free *by design*, cleared by a fact landing rather than
    /// by anything the agent executes, so its emptiness is not unliftability. Fork advice is not
    /// a remedy and never enters this verdict.
    pub fn is_curable(&self) -> bool {
        !self.plans.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    label: Label,
    effects: BTreeSet<EffectKind>,
}

/// Plan the remedies for a raw block. Emits the executable plans when the block clears in one
/// atomic step, and every curative redispatch when only a prior tool call unlocks it. Both land in
/// the one `plans` list; fork advice is separate and never a remedy. See the module docs
/// for the curability model.
pub(crate) fn plan(registry: &Registry, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> PlannedBlock {
    let start = State {
        label: views.current_label(),
        effects: views.present_effects(),
    };

    let mut plans: Vec<RemedyPlan> = enumerate_plans(registry, &start, call)
        .into_iter()
        .map(RemedyPlan::Executable)
        .collect();

    if plans.is_empty() && !raw.requirement_gaps.is_empty() {
        for (tool, effect) in curative_redispatches(registry, &start, call, raw) {
            plans.push(RemedyPlan::Redispatch { tool, effect });
        }
    }
    let fork_reason = match (&raw.narrowing, raw.requirement_gaps.is_empty()) {
        (Some(_), true) => {
            "delegate the restricting work to a child session: the child accepts this narrowing and this session's label stays. A value the child returns still crosses checked at the merge — a raw restricted return costs this session the same narrowing, so have the child finish the work and submit_result null, or return a sanitized derivation"
        }
        (Some(_), false) => {
            "delegate the restricting work to a child session: the child accepts this narrowing and remedies the requirement gaps there, and this session's label stays. A value the child returns still crosses checked at the merge — a raw restricted return costs this session the same narrowing, so have the child finish the work and submit_result null, or return a sanitized derivation"
        }
        (None, _) => {
            "handle in a subagent (advisory: a child begins at the same label, so a fork cures no requirement)"
        }
    };
    PlannedBlock {
        raw: raw.clone(),
        plans,
        fork_advice: Some(fork_reason.to_string()),
    }
}

fn directly_clearable(registry: &Registry, state: &State, call: &ResolvedCall) -> Option<Vec<RemedyStep>> {
    let contract = registry.tool(call.tool())?;
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    let eval = check::evaluate_state(
        contract,
        &state.label,
        &has_effect,
        call,
        check::PlaceholderGaps::FailClosed,
    );
    // `consumed` is deliberately not consulted: the search asks whether the *gaps* clear, per
    // the gap-scoped plan semantics — a persisting unestablished dimension gates execution and
    // dispatch, never the offer. Masking keeps a consumed requirement out of the gap
    // set, so no step below ever claims to cure it.
    let mut steps = Vec::new();
    for gap in &eval.requirement_gaps {
        // One ruling by an authority covers one or more gaps — emit each authority once.
        let step = RemedyStep::Authorize(authority_for(registry, gap, &contract.tags)?.clone());
        if !steps.contains(&step) {
            steps.push(step);
        }
    }
    if let Some(narrowing) = eval.narrowing {
        steps.push(RemedyStep::Accept(narrowing));
    }
    Some(steps)
}

fn enumerate_plans(registry: &Registry, state: &State, call: &ResolvedCall) -> Vec<ExecutableRemedyPlan> {
    let Some(contract) = registry.tool(call.tool()) else {
        return Vec::new();
    };
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    let block = check::evaluate_state(
        contract,
        &state.label,
        &has_effect,
        call,
        check::PlaceholderGaps::FailClosed,
    );
    if block.requirement_gaps.is_empty() && block.narrowing.is_none() {
        return Vec::new();
    }

    let Some(assignments) = enumerate_assignments(registry, &block.requirement_gaps, &contract.tags) else {
        return Vec::new();
    };
    let tails = narrowing_remedies(registry, &state.label, contract, block.narrowing.as_ref());

    let mut plans: Vec<ExecutableRemedyPlan> = Vec::new();
    for required in assignments {
        for tail in &tails {
            let mut steps: Vec<RemedyStep> = required
                .iter()
                .map(|r| RemedyStep::Authorize(r.authority.clone()))
                .collect();
            steps.extend(tail.iter().cloned());
            plans.push(ExecutableRemedyPlan {
                id: PlanId(plans.len() as u32),
                steps,
                required: required.clone(),
            });
        }
    }
    plans
}

/// Every unique grouped authority assignment covering `gaps`, in registration order (so the first
/// is all-first-choices). `None` when some gap has no competent authority — a prior/cap gap has
/// none by construction, which is what makes the two plan kinds mutually exclusive. A gapless block
/// yields exactly one empty assignment, so the narrowing remedies still cross with something.
fn enumerate_assignments(registry: &Registry, gaps: &[Gap], tags: &[TagName]) -> Option<Vec<Vec<RequiredRuling>>> {
    let mut choices: Vec<Vec<&AuthorityName>> = Vec::with_capacity(gaps.len());
    for gap in gaps {
        let competent: Vec<&AuthorityName> = registry
            .authorities()
            .iter()
            .filter(|authority| covers_gap(authority, gap, tags))
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

fn narrowing_remedies(
    registry: &Registry,
    current: &Label,
    contract: &ToolContract,
    narrowing: Option<&Narrowing>,
) -> Vec<Vec<RemedyStep>> {
    let Some(narrowing) = narrowing else {
        return vec![Vec::new()];
    };
    let mut tails = vec![vec![RemedyStep::Accept(narrowing.clone())]];
    for sanitizer in applicable_output_sanitizers(registry, contract) {
        let Some(sanitized) = sanitized_commit(current, contract, sanitizer) else {
            continue;
        };
        let mut tail = vec![RemedyStep::Sanitize(sanitizer.name.clone())];
        if &sanitized != current {
            tail.push(RemedyStep::Accept(Narrowing {
                from: current.clone(),
                to: sanitized,
            }));
        }
        tails.push(tail);
    }
    tails
}

fn applicable_output_sanitizers<'r>(registry: &'r Registry, contract: &ToolContract) -> Vec<&'r Sanitizer> {
    if contract.pending_cast_dim().is_some() {
        return Vec::new();
    }
    let output = contract.output_label();
    registry
        .sanitizers()
        .filter(|sanitizer| sanitizer.on.output && sanitizer.transition.admits(&output) == Adequacy::Holds)
        .collect()
}

fn sanitized_commit(current: &Label, contract: &ToolContract, sanitizer: &Sanitizer) -> Option<Label> {
    let raw = check::committed_label(contract, current);
    if &raw == current {
        return None;
    }
    let sanitized = current.combine(&sanitizer.transition.derive(&contract.output_label()));
    (sanitized != raw).then_some(sanitized)
}

fn prerequisite_runnable(registry: &Registry, state: &State, tool: &ToolContract) -> bool {
    let call = synthetic_call(tool);
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    let eval = check::evaluate_state(tool, &state.label, &has_effect, &call, check::PlaceholderGaps::Waived);
    if !eval.consumed.is_empty() {
        return false;
    }
    eval.requirement_gaps
        .iter()
        .all(|gap| authority_for(registry, gap, &tool.tags).is_some())
}

/// The rulings a block's remedy plan needs gathered: for each authority the block routes to, the gaps
/// its ruling must cover. The mandate routing (which authority covers which gap) stays here in the
/// engine; the runtime only gathers a ruling from each named authority for its gaps and hands them to
/// `execute_remedy_plan`. A gap with no covering authority is omitted — the plan is then not executable and
/// `execute_remedy_plan` reports the gap uncovered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredRuling {
    pub authority: AuthorityName,
    pub covers: Vec<Gap>,
}

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

fn transitions(registry: &Registry, state: &State, tool: &ToolContract) -> Vec<State> {
    let mut effects = state.effects.clone();
    effects.extend(tool.emits.iter().cloned());
    let mut states = vec![State {
        label: check::committed_label(tool, &state.label),
        effects: effects.clone(),
    }];
    for sanitizer in applicable_output_sanitizers(registry, tool) {
        if let Some(label) = sanitized_commit(&state.label, tool, sanitizer) {
            let next = State {
                label,
                effects: effects.clone(),
            };
            if !states.contains(&next) {
                states.push(next);
            }
        }
    }
    states
}

fn synthetic_call(tool: &ToolContract) -> ResolvedCall {
    ResolvedCall::new(tool.name.clone(), serde_json::Value::Null, Vec::new())
}

fn curable(registry: &Registry, state: &State, call: &ResolvedCall, visiting: &mut Vec<State>) -> bool {
    if directly_clearable(registry, state, call).is_some() {
        return true;
    }
    if visiting.contains(state) {
        return false;
    }
    visiting.push(state.clone());
    let cured = registry.tools().any(|tool| {
        if !prerequisite_runnable(registry, state, tool) {
            return false;
        }
        transitions(registry, state, tool)
            .into_iter()
            .any(|next| next != *state && curable(registry, &next, call, visiting))
    });
    visiting.pop();
    cured
}

fn curative_redispatches(
    registry: &Registry,
    start: &State,
    call: &ResolvedCall,
    raw: &RawBlock,
) -> Vec<(ToolName, RedispatchEffect)> {
    let mut curative = Vec::new();
    for tool in registry.tools() {
        if !prerequisite_runnable(registry, start, tool) {
            continue;
        }
        let successors = transitions(registry, start, tool);
        let reaches = successors.iter().any(|next| {
            let mut visiting = Vec::new();
            next != start && curable(registry, next, call, &mut visiting)
        });
        if !reaches {
            continue;
        }
        curative.push((
            tool.name.clone(),
            redispatch_effect(registry, tool, call, &successors[0], raw),
        ));
    }
    curative
}

fn redispatch_effect(
    registry: &Registry,
    tool: &ToolContract,
    call: &ResolvedCall,
    next: &State,
    raw: &RawBlock,
) -> RedispatchEffect {
    let retried = registry
        .tool(call.tool())
        .map(|target| check::committed_label(target, &next.label));
    let cleared: Vec<Gap> = raw
        .requirement_gaps
        .iter()
        .filter(|gap| match gap {
            Gap::Prior(kind) => tool.emits.contains(kind),
            Gap::Cap { cap } => retried
                .as_ref()
                .is_some_and(|label| label.audience.within_cap(cap) == Adequacy::Holds),
            _ => false,
        })
        .cloned()
        .collect();
    match cleared.is_empty() {
        true => RedispatchEffect::EnablesPath,
        false => RedispatchEffect::Clears(cleared),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Mandate, Sanitizer, SanitizerPoints, Scope, Transition};
    use crate::check::CheckOutcome;
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

    fn plan_of(registry: &Registry, log: &[Fact], call: &ResolvedCall) -> PlannedBlock {
        let projection = Projection::build(log, Revision::new(log.len() as u64));
        let trajectory = traj();
        let views = projection.view(&trajectory);
        let contract = registry.tool(call.tool()).unwrap();
        let raw = match check::evaluate(contract, &views, call) {
            CheckOutcome::Block(raw) => raw,
            other => panic!("expected a block, got {other:?}"),
        };
        plan(registry, &views, call, &raw)
    }

    fn call(tool: &str, args: serde_json::Value) -> ResolvedCall {
        ResolvedCall::new(ToolName::new(tool), args, vec![])
    }

    #[test]
    fn an_unestablished_only_block_mints_no_plan_and_a_mixed_block_keeps_its_offers() {
        let gate = ToolContract {
            name: ToolName::new("gate"),
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
        };
        let mut vault = gate.clone();
        vault.name = ToolName::new("vault");
        vault.requires.attention = vec![MarkName::new("signoff")];
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
            tools: vec![gate, vault],
            authorities: vec![steward],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];

        let planned = plan_of(&registry, &log, &call("gate", json!({})));
        assert!(planned.plans.is_empty(), "an unestablished-only block offers nothing");

        let planned = plan_of(&registry, &log, &call("vault", json!({})));
        let executables: Vec<_> = planned.plans.iter().filter_map(RemedyPlan::executable).collect();
        assert_eq!(executables.len(), 1, "the mixed block keeps its attention offer");
        assert_eq!(
            executables[0].required[0].covers,
            vec![Gap::Attention(MarkName::new("signoff"))]
        );
        assert!(
            executables[0]
                .steps
                .iter()
                .all(|step| !matches!(step, RemedyStep::Accept(_))),
            "no acceptance step: the block carries no narrowing"
        );
    }

    fn output_sanitizer(name: &str, transition: Transition) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition,
            hint: None,
        }
    }

    fn reader(name: &str, delta: Delta) -> ToolContract {
        ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(delta),
            emits: vec![],
            requires: Requires::default(),
        }
    }

    fn internal() -> Audience {
        Audience::restricted([ReaderId::new("internal")])
    }

    fn sanitize_offers(planned: &PlannedBlock) -> Vec<(String, bool)> {
        planned
            .plans
            .iter()
            .filter_map(RemedyPlan::executable)
            .filter_map(|plan| {
                let name = plan.steps.iter().find_map(|step| match step {
                    RemedyStep::Sanitize(name) => Some(name.as_str().to_string()),
                    _ => None,
                })?;
                let residual = plan.steps.iter().any(|step| matches!(step, RemedyStep::Accept(_)));
                Some((name, residual))
            })
            .collect()
    }

    #[test]
    fn a_narrowing_block_offers_each_applicable_output_sanitizer() {
        let crm = reader(
            "crm",
            Delta {
                trust: None,
                audience: Some(Dim::Known(internal())),
            },
        );
        let tracker = reader(
            "tracker",
            Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: Some(Dim::Known(internal())),
            },
        );
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![crm, tracker],
            authorities: vec![],
            sanitizers: vec![
                output_sanitizer(
                    "declassify",
                    Transition::Audience {
                        from_includes: internal(),
                        to: Audience::Public,
                    },
                ),
                output_sanitizer(
                    "scrub",
                    Transition::Trust {
                        from_floor: SUSPICIOUS,
                        to: TRUSTED,
                    },
                ),
            ],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];

        let planned = plan_of(&registry, &log, &call("crm", json!({})));
        assert_eq!(sanitize_offers(&planned), [("declassify".to_string(), false)]);

        let planned = plan_of(&registry, &log, &call("tracker", json!({})));
        assert_eq!(
            sanitize_offers(&planned),
            [("declassify".to_string(), true), ("scrub".to_string(), true)],
            "both mandates apply, and each leaves the dimension it does not transition"
        );
        assert!(
            planned
                .plans
                .iter()
                .filter_map(RemedyPlan::executable)
                .any(|plan| plan.steps == vec![RemedyStep::Accept(planned.raw.narrowing.clone().unwrap())])
        );
    }

    #[test]
    fn a_sanitize_plan_still_carries_every_requirement_ruling() {
        let mut publish = reader(
            "publish",
            Delta {
                trust: None,
                audience: Some(Dim::Known(internal())),
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
            tools: vec![publish],
            authorities: vec![steward],
            sanitizers: vec![output_sanitizer(
                "declassify",
                Transition::Audience {
                    from_includes: internal(),
                    to: Audience::Public,
                },
            )],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
    }

    #[test]
    fn a_prerequisites_sanitized_crossing_counts_as_a_curative_path() {
        let backup = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta {
                trust: Some(Dim::Known(SUSPICIOUS)),
                audience: None,
            }),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires::default(),
        };
        let wipe = ToolContract {
            name: ToolName::new("wipe"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
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
            Transition::Trust {
                from_floor: SUSPICIOUS,
                to: TRUSTED,
            },
        );
        let log = vec![user_value(known(TRUSTED, Audience::Public))];

        let without = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![backup.clone(), wipe.clone()],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        assert!(
            !plan_of(&without, &log, &call("wipe", json!({}))).is_curable(),
            "raw only: running backup breaks the trust floor it would satisfy"
        );

        let with = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![backup, wipe],
            authorities: vec![],
            sanitizers: vec![scrub],
            casts: vec![],
        });
        let planned = plan_of(&with, &log, &call("wipe", json!({})));
        assert!(
            planned.is_curable(),
            "the sanitized crossing keeps the trust and the effect"
        );
        assert_eq!(
            planned.plans,
            vec![RemedyPlan::Redispatch {
                tool: ToolName::new("backup"),
                effect: RedispatchEffect::Clears(vec![Gap::Prior(EffectKind::new("backup.done"))]),
            }]
        );
    }

    #[test]
    fn mixed_blocks_keep_their_prior_and_cap_redispatches_while_a_fact_is_missing() {
        let emitter = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup")],
            requires: Requires::default(),
        };
        let prior_target = ToolContract {
            name: ToolName::new("wipe"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![],
                },
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup"))],
                ..Requires::default()
            },
        };
        let a = Audience::restricted([ReaderId::new("a")]);
        let narrower = ToolContract {
            name: ToolName::new("narrow"),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(a.clone())),
            }),
            emits: vec![],
            requires: Requires::default(),
        };
        let cap_target = ToolContract {
            name: ToolName::new("send"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: Some(TRUSTED),
                    audience: vec![AudienceRequirement::Cap(a)],
                },
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![emitter, prior_target, narrower, cap_target],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(Label::new(Dim::Unknown, Dim::Known(Audience::Public)))];

        let planned = plan_of(&registry, &log, &call("wipe", json!({})));
        assert!(planned.plans.iter().any(|plan| matches!(
            plan,
            RemedyPlan::Redispatch {
                tool,
                effect: RedispatchEffect::Clears(gaps),
            } if tool.as_str() == "backup" && gaps == &vec![Gap::Prior(EffectKind::new("backup"))]
        )));

        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert!(planned.plans.iter().any(|plan| matches!(
            plan,
            RemedyPlan::Redispatch {
                tool,
                effect: RedispatchEffect::Clears(gaps),
            } if tool.as_str() == "narrow" && matches!(gaps.as_slice(), [Gap::Cap { .. }])
        )));
    }

    #[test]
    fn a_reader_ceiling_authority_cannot_cover_the_masked_sentinel() {
        let send = ToolContract {
            name: ToolName::new("send"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
                },
                ..Requires::default()
            },
        };
        let generous = Authority {
            name: AuthorityName::new("generous"),
            mandate: Mandate {
                reader_ceiling: Some(Audience::Public),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![send],
            authorities: vec![generous],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(Label::new(Dim::Known(TRUSTED), Dim::Unknown))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));
        assert!(
            planned.plans.is_empty(),
            "nothing for the covering authority to rule on"
        );
    }

    #[test]
    fn a_cap_redispatch_claims_only_what_it_actually_clears() {
        let a = || Audience::restricted([ReaderId::new("a")]);
        let ab = || Audience::restricted([ReaderId::new("a"), ReaderId::new("b")]);
        let narrowing_tool = |name: &str, to: Audience| ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta {
                trust: None,
                audience: Some(Dim::Known(to)),
            }),
            emits: vec![],
            requires: Requires::default(),
        };
        let send = ToolContract {
            name: ToolName::new("send"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                label: LabelRequirements {
                    trust_floor: None,
                    audience: vec![AudienceRequirement::Cap(a())],
                },
                ..Requires::default()
            },
        };
        let registry = build(RegistryConfig {
            trust_chain: chain(),
            tools: vec![
                send,
                narrowing_tool("narrow_all", a()),
                narrowing_tool("narrow_some", ab()),
            ],
            authorities: vec![],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("send", json!({})));

        assert!(planned.plans.iter().all(|plan| plan.executable().is_none()));
        let effect_of = |tool: &str| {
            planned
                .plans
                .iter()
                .find_map(|plan| match plan {
                    RemedyPlan::Redispatch { tool: name, effect } if name == &ToolName::new(tool) => Some(effect),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{tool} is offered"))
        };
        assert_eq!(
            effect_of("narrow_all"),
            &RedispatchEffect::Clears(vec![Gap::Cap { cap: a() }])
        );
        assert_eq!(effect_of("narrow_some"), &RedispatchEffect::EnablesPath);
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
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert!(planned.is_curable());
        assert_eq!(
            exec(&planned.plans[0]).steps,
            vec![RemedyStep::Authorize(AuthorityName::new("officer"))]
        );
    }

    #[test]
    fn alternative_authorities_yield_one_plan_per_assignment() {
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

    #[test]
    fn a_duplicated_requirement_entry_is_one_gap_and_mints_no_permuted_duplicates() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
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
            tools: vec![tool],
            authorities: vec![attester("a"), attester("b")],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
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
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(SUSPICIOUS, Audience::Public))];
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
        assert!(planned.fork_advice.is_some());
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
        assert!(planned.is_curable());
        assert_eq!(
            exec(&planned.plans[0]).steps,
            vec![RemedyStep::Accept(Narrowing {
                from: known(TRUSTED, Audience::Public),
                to: known(TRUSTED, Audience::restricted([ReaderId::new("internal")])),
            })]
        );
    }

    #[test]
    fn prior_gap_cured_by_a_redispatch() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("db.deleted")],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = ToolContract {
            name: ToolName::new("backup"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires::default(),
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
        assert!(matches!(
            planned.plans.as_slice(),
            [RemedyPlan::Redispatch { tool, .. }] if tool == &ToolName::new("backup")
        ));
    }

    #[test]
    fn prior_gap_with_multiple_emitters_surfaces_every_curative_redispatch() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
        };
        let backup = |name: &str| ToolContract {
            name: ToolName::new(name),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("backup.done")],
            requires: Requires::default(),
        };
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
            .plans
            .iter()
            .filter_map(|plan| match plan {
                RemedyPlan::Redispatch { tool, .. } => Some(tool),
                RemedyPlan::Executable(_) => None,
            })
            .collect();
        assert_eq!(
            curative,
            vec![&ToolName::new("backup_fast"), &ToolName::new("backup_full")]
        );
    }

    #[test]
    fn static_includes_prerequisite_without_covering_authority_is_not_advertised() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
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
        assert!(planned.plans.is_empty());
    }

    #[test]
    fn static_includes_prerequisite_with_covering_authority_is_advertised() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("backup.done"))],
                ..Requires::default()
            },
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
        };
        let voucher = Authority {
            name: AuthorityName::new("voucher"),
            mandate: Mandate {
                reader_ceiling: Some(Audience::restricted([ReaderId::new("auditor")])),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
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
            planned.plans.first(),
            Some(RemedyPlan::Redispatch { tool, .. }) if tool == &ToolName::new("backup")
        ));
    }

    #[test]
    fn a_sentinel_shaped_static_recipient_is_not_mistaken_for_a_placeholder() {
        let archive = ToolContract {
            name: ToolName::new("archive"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
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
        let archive = ToolContract {
            name: ToolName::new("archive"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("email.sent"))],
                ..Requires::default()
            },
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
            planned.plans.first(),
            Some(RemedyPlan::Redispatch { tool, .. }) if tool == &ToolName::new("send")
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
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![TagName::new("payments")],
            delta: Some(Delta::NONE),
            emits: vec![],
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
            tools: vec![tool],
            authorities: vec![officer],
            sanitizers: vec![],
            casts: vec![],
        });
        let log = vec![user_value(known(TRUSTED, Audience::Public))];
        let planned = plan_of(&registry, &log, &call("wire", json!({})));
        assert_eq!(
            exec(&planned.plans[0]).steps,
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
        let a = ToolContract {
            name: ToolName::new("a"),
            tags: vec![],
            delta: Some(Delta::NONE),
            emits: vec![EffectKind::new("ka")],
            requires: Requires {
                history: vec![HistoryRequirement::Prior(EffectKind::new("kb"))],
                ..Requires::default()
            },
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

    mod reference {
        use super::*;

        fn reachable(registry: &Registry, start: &State) -> Vec<State> {
            let mut states = vec![start.clone()];
            loop {
                let mut grew = false;
                for state in states.clone() {
                    for tool in registry.tools() {
                        if prerequisite_runnable(registry, &state, tool) {
                            for next in transitions(registry, &state, tool) {
                                if !states.contains(&next) {
                                    states.push(next);
                                    grew = true;
                                }
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

        pub(super) fn curable(registry: &Registry, start: &State, call: &ResolvedCall) -> bool {
            reachable(registry, start)
                .iter()
                .any(|state| directly_clearable(registry, state, call).is_some())
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
            Just(Audience::Public),
            Just(Audience::restricted([ReaderId::new("r0")])),
            Just(Audience::restricted([ReaderId::new("r0"), ReaderId::new("r1")])),
        ]
    }

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
                if delta.is_none() {
                    requires.label = LabelRequirements::default();
                }
                ToolContract {
                    name: name.clone(),
                    tags: vec![],
                    delta,
                    emits,
                    requires,
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
                label: known(trust, audience),
                effects,
            })
    }

    fn a_sanitizer(index: usize) -> impl Strategy<Value = Sanitizer> {
        let name = SanitizerName::new(format!("s{index}"));
        prop_oneof![
            (small_audience(), small_audience())
                .prop_map(|(from_includes, to)| Transition::Audience { from_includes, to }),
            ((0u8..2).prop_map(Trust::new), (0u8..2).prop_map(Trust::new))
                .prop_map(|(from_floor, to)| Transition::Trust { from_floor, to }),
        ]
        .prop_map(move |transition| Sanitizer {
            name: name.clone(),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition,
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

            let built = Registry::build(RegistryConfig {
                trust_chain: chain(),
                tools,
                authorities,
                sanitizers,
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
            let eval = check::evaluate_state(contract, &state.label, &has_effect, &call, check::PlaceholderGaps::FailClosed);
            if !eval.consumed.is_empty() || (eval.requirement_gaps.is_empty() && eval.narrowing.is_none()) {
                return Ok(());
            }
            let raw = RawBlock {
                requirement_gaps: eval.requirement_gaps,
                narrowing: eval.narrowing,
                unestablished: Vec::new(),
            };

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
            let eval = check::evaluate_state(contract, &state.label, &has_effect, &call, check::PlaceholderGaps::FailClosed);
            if !eval.consumed.is_empty() || (eval.requirement_gaps.is_empty() && eval.narrowing.is_none()) {
                return Ok(());
            }
            let raw = RawBlock {
                requirement_gaps: eval.requirement_gaps,
                narrowing: eval.narrowing,
                unestablished: Vec::new(),
            };

            let mut log = vec![user_value(state.label.clone())];
            for kind in &state.effects {
                log.push(committed_effect(kind.clone()));
            }
            let projection = Projection::build(&log, Revision::new(log.len() as u64));
            let trajectory = traj();
            let views = projection.view(&trajectory);
            let planned = plan(&registry, &views, &call, &raw);

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
                .filter_map(RemedyPlan::executable)
                .map(|p| p.required.iter().map(|r| (r.authority.clone(), r.covers.clone())).collect())
                .collect();
            prop_assert_eq!(actual, expected);
        }
    }

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
