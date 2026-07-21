use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ToolName;
use crate::approval::{Authority, AuthorityMode};
use crate::audit::AuthorityName;
use crate::contract::{AudienceRule, Fixability, Requirements, Unprovable, Verdict, Violation};
use crate::dimension::{Effect, Effects, KnownTrust};
use crate::plan::NonEmptyVec;
use crate::remedy::{
    Authorization, AuthorizationDelta, AuthorizationScope, DeltaCoordinate, LabelRaise, Lift, PlannedRemedy,
    ReductionTarget,
};
use crate::request::{ArgumentTree, EmissionRequest, ToolRequest};
use crate::revision::{FlowId, ValueId};
use crate::turn::Trajectory;
use crate::value::{TransformerRef, UnknownValue, ValueLabel};

use super::PolicyEngine;
use super::capability::{RESPONSE_SINK, ResponsePolicy, ToolContract};

struct SearchCtx<'a> {
    tree: &'a ArgumentTree<ValueId>,
    flow: FlowId,
}

struct Candidate {
    steps: NonEmptyVec<PlannedRemedy>,
    group: GroupKey,
}

#[derive(Clone, PartialEq, Eq)]
struct GroupKey {
    derives: Vec<(ValueId, TransformerRef)>,
    tool: ToolName,
}

#[derive(Clone)]
struct ReduceState {
    sim: SimFlow,
    steps: Vec<PlannedRemedy>,
    derives: Vec<(ValueId, TransformerRef)>,
    recipient_leaves: BTreeSet<ValueId>,
    path: BTreeSet<StateKey>,
}

/// The semantic identity of a reduce-state: the per-leaf labels and the
/// tool identity. Requirements and recipients are
/// functions of the tool's contract over the fixed request tree, so they
/// need no separate coordinate. Deduplication is deliberately per-route
/// only (a route never revisits its own semantic state): a global
/// visited-set keyed on anything order-insensitive would prune routes whose
/// continuations differ — expansion is path-sensitive, so a state reached
/// by `A,B` and by `B,A` admits different follow-up moves under the cycle
/// check. Order-permuted routes stay distinct plans: only a plan's head is
/// executable and every application rechecks, so the step order is part of
/// the prediction, never a presentation detail to normalize away.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StateKey {
    labels: Vec<(ValueId, ValueLabel)>,
    tool: ToolName,
}

impl StateKey {
    fn of(sim: &SimFlow) -> Self {
        Self {
            labels: sim.leaf_labels.iter().map(|(id, label)| (*id, label.clone())).collect(),
            tool: sim.tool.clone(),
        }
    }
}

impl PolicyEngine {
    pub(super) fn plan_frontier(
        &self,
        trajectory: &Trajectory,
        checked: &ToolRequest,
        contract: Option<&ToolContract>,
        pending: &crate::request::PendingAction,
    ) -> Vec<NonEmptyVec<PlannedRemedy>> {
        let base = match SimFlow::of(trajectory, checked, contract) {
            Ok(base) => base,
            Err(_) => return Vec::new(),
        };
        let ctx = SearchCtx {
            tree: &checked.arguments,
            flow: pending.flow(),
        };
        self.frontier(&base, recipient_leaves_for(contract, ctx.tree), &ctx)
    }

    /// The plan frontier for a pending emission: the same pipeline over the
    /// body tree.
    pub(super) fn emission_plan_frontier(
        &self,
        trajectory: &Trajectory,
        checked: &EmissionRequest,
        flow: FlowId,
    ) -> Vec<NonEmptyVec<PlannedRemedy>> {
        let base = match SimFlow::of_emission(trajectory, checked, self.response_policy.as_ref()) {
            Ok(base) => base,
            Err(_) => return Vec::new(),
        };
        let ctx = SearchCtx {
            tree: &checked.body,
            flow,
        };
        self.frontier(&base, BTreeSet::new(), &ctx)
    }

    fn frontier(
        &self,
        base: &SimFlow,
        base_recipient_leaves: BTreeSet<ValueId>,
        ctx: &SearchCtx<'_>,
    ) -> Vec<NonEmptyVec<PlannedRemedy>> {
        let mut candidates: Vec<Candidate> = Vec::new();
        for state in self.reduce_states(base, base_recipient_leaves) {
            self.peel_state(&state, ctx, &mut candidates);
        }
        candidates.extend(self.rescue_candidates(base, ctx));

        // Structural dedup: exactly the same ordered remedy sequence
        // (ignoring the violation vectors shown to authorities) generated
        // twice keeps its first occurrence. Equal-multiset, different-order
        // plans are deliberately NOT collapsed: their asks are equal, but
        // the executable head and the recheck sequence differ — the
        // head-only contract makes order observable, so both orderings are
        // distinct predictions in the frontier (mutually non-dominating:
        // same group, equal ask vectors). Execution converges regardless,
        // because every applied head re-plans.
        let mut deduped: Vec<Candidate> = Vec::new();
        for candidate in candidates {
            if !deduped
                .iter()
                .any(|kept| kept.group == candidate.group && same_step_sequence(&kept.steps, &candidate.steps))
            {
                deduped.push(candidate);
            }
        }

        deduped.retain(|candidate| {
            let steps: Vec<&PlannedRemedy> = candidate.steps.iter().collect();
            debug_assert!(
                self.replay_unlocks(base, &steps),
                "every generated plan must predict a clean flow"
            );
            (0..steps.len()).all(|removed| {
                let reduced: Vec<&PlannedRemedy> = steps
                    .iter()
                    .enumerate()
                    .filter_map(|(i, step)| (i != removed).then_some(*step))
                    .collect();
                !self.replay_unlocks(base, &reduced)
            })
        });

        let asks: Vec<AskVector> = deduped.iter().map(|c| AskVector::of(&c.steps)).collect();
        let mut keep = vec![true; deduped.len()];
        for i in 0..deduped.len() {
            for j in 0..deduped.len() {
                if i != j && deduped[i].group == deduped[j].group && ask_cmp(&asks[j], &asks[i]) == Some(Ordering::Less)
                {
                    keep[i] = false;
                    break;
                }
            }
        }
        let mut plans: Vec<NonEmptyVec<PlannedRemedy>> = deduped
            .into_iter()
            .zip(keep)
            .filter_map(|(candidate, keep)| keep.then_some(candidate.steps))
            .collect();
        plans.sort_by_key(NonEmptyVec::len);
        plans
    }

    fn reduce_states(&self, base: &SimFlow, base_recipient_leaves: BTreeSet<ValueId>) -> Vec<ReduceState> {
        let base_key = StateKey::of(base);
        let mut queue = VecDeque::from([ReduceState {
            sim: base.clone(),
            steps: Vec::new(),
            derives: Vec::new(),
            recipient_leaves: base_recipient_leaves,
            path: BTreeSet::from([base_key]),
        }]);
        let mut states = Vec::new();
        while let Some(mut state) = queue.pop_front() {
            let leaves: Vec<ValueId> = state.sim.leaf_labels.keys().copied().collect();
            for leaf in leaves {
                if state.recipient_leaves.contains(&leaf) {
                    continue;
                }
                for transformer in &self.transformers {
                    let label = &state.sim.leaf_labels[&leaf];
                    if !transformer.descriptor.precondition.matches(label) || transformer.descriptor.output == *label {
                        continue;
                    }
                    let mut next = state.clone();
                    next.sim.leaf_labels.insert(leaf, transformer.descriptor.output.clone());
                    if !Self::enter(&mut next) {
                        continue;
                    }
                    next.steps.push(PlannedRemedy::Reduce(ReductionTarget::DeriveValue {
                        source: leaf,
                        transformer: transformer.descriptor.transformer.clone(),
                    }));
                    next.derives.push((leaf, transformer.descriptor.transformer.clone()));
                    queue.push_back(next);
                }
            }
            state.derives.sort_by_key(|(leaf, _)| *leaf);
            states.push(state);
        }
        states
    }

    fn enter(next: &mut ReduceState) -> bool {
        let key = StateKey::of(&next.sim);
        if next.path.contains(&key) {
            return false;
        }
        next.path.insert(key);
        true
    }

    fn peel_state(&self, state: &ReduceState, ctx: &SearchCtx<'_>, out: &mut Vec<Candidate>) {
        let group = GroupKey {
            derives: state.derives.clone(),
            tool: state.sim.tool.clone(),
        };
        let mut sim = state.sim.clone();
        let mut steps = state.steps.clone();
        let mut remaining = sim.violations(None);

        let endorse = endorse_steps(&sim, &remaining);
        let raised_state: Option<(SimFlow, Vec<Violation>, Vec<PlannedRemedy>)> = {
            let mut probe = sim.clone();
            let mut residual = remaining.clone();
            endorse
                .iter()
                .map(|(leaf, delta)| {
                    let step = self.authorize_step(raise_authorization(*leaf, delta), residual.clone())?;
                    let raised = delta.raise(&probe.leaf_labels[leaf]);
                    probe.leaf_labels.insert(*leaf, raised);
                    residual = probe.violations(None);
                    Some(step)
                })
                .collect::<Option<Vec<_>>>()
                .map(|raise_steps| (probe, residual, raise_steps))
        };
        if let Some((raised, residual, raise_steps)) = raised_state {
            sim = raised;
            remaining = residual;
            steps.extend(raise_steps);
        }

        if remaining.is_empty() {
            if let Some(steps) = NonEmptyVec::from_vec(steps) {
                out.push(Candidate { steps, group });
            }
            return;
        }

        for delta in self.waiver_candidates(&sim, &remaining) {
            if !sim.violations(Some(&delta)).is_empty() {
                continue;
            }
            let grant = authorization_for(&delta, &remaining, ctx.flow);
            let Some(step) = self.authorize_step(grant, remaining.clone()) else {
                continue;
            };
            let mut lift_steps = steps.clone();
            lift_steps.push(step);
            let steps = NonEmptyVec::from_vec(lift_steps).expect("lift step just pushed");
            out.push(Candidate {
                steps,
                group: group.clone(),
            });
        }
    }

    fn authorize_step(&self, authorization: Authorization, targets: Vec<Violation>) -> Option<PlannedRemedy> {
        let routes: Vec<AuthorityName> = self
            .competent_authorities(&authorization)
            .map(|authority| authority.name.clone())
            .collect();
        NonEmptyVec::from_vec(routes).map(|routes| PlannedRemedy::Authorize {
            authorization,
            routes,
            targets,
        })
    }

    fn rescue_candidates(&self, base: &SimFlow, ctx: &SearchCtx<'_>) -> Vec<Candidate> {
        if base.control_labels.is_empty() {
            return Vec::new();
        }
        let ids: Vec<ValueId> = base.control_labels.keys().copied().collect();
        let group = GroupKey {
            derives: Vec::new(),
            tool: base.tool.clone(),
        };
        self.minimal_joint_releases(base, &ids, ctx.flow)
            .into_iter()
            .map(|steps| Candidate {
                steps,
                group: group.clone(),
            })
            .collect()
    }

    /// Every minimum-cardinality release whose joint composition clears the
    /// projection: sizes ascending through a streaming lexicographic
    /// combination generator, collecting *all* successes of the first
    /// successful size. Size-first semantics, not the full inclusion-minimal
    /// antichain: each returned set is inclusion-minimal (every proper
    /// subset was probed at a smaller size and failed) and same-size sets
    /// are mutually incomparable, but cleanability is non-monotone, so a
    /// *larger* inclusion-minimal release with no successful proper subset
    /// is deliberately not enumerated — doing so would forfeit the early
    /// exit and sweep the lattice even when a one-value release exists.
    /// The complete sweep still happens whenever *nothing* succeeds, which
    /// is exactly the `Terminal` proof. There is
    /// no width or count bound: a flow with no successful release at any size
    /// sweeps the full subset lattice — exponential in the request's own
    /// control-set size, and exactly the proof behind a `Terminal` claim
    /// (accepted prototype trade; a silent bound would turn "no remedy
    /// exists" into "none was found where we looked"). The empty release is
    /// not probed: an unreleased endorse-plus-waiver solve is the ordinary
    /// peels' domain, and its candidates are already in the pool.
    fn minimal_joint_releases(&self, base: &SimFlow, ids: &[ValueId], flow: FlowId) -> Vec<NonEmptyVec<PlannedRemedy>> {
        for size in 1..=ids.len() {
            let hits: Vec<NonEmptyVec<PlannedRemedy>> = Combinations::new(ids.len(), size)
                .filter_map(|combo| {
                    let release: BTreeSet<ValueId> = combo.iter().map(|&i| ids[i]).collect();
                    self.joint_rescue(base, &release, flow)
                })
                .collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        Vec::new()
    }

    fn joint_rescue(
        &self,
        base: &SimFlow,
        release: &BTreeSet<ValueId>,
        flow: FlowId,
    ) -> Option<NonEmptyVec<PlannedRemedy>> {
        let mut projected = base.clone();
        projected.control_labels.retain(|id, _| !release.contains(id));
        let mut actual = base.clone();
        let mut steps = Vec::new();
        let mut residual = projected.violations(None);
        while let Some((leaf, delta)) = endorse_steps(&projected, &residual).into_iter().next() {
            let step = self.authorize_step(raise_authorization(leaf, &delta), residual)?;
            let raised = delta.raise(&projected.leaf_labels[&leaf]);
            projected.leaf_labels.insert(leaf, raised.clone());
            actual.leaf_labels.insert(leaf, raised);
            steps.push(step);
            residual = projected.violations(None);
        }
        let remaining = actual.violations(None);
        let mut delta = needed_delta(&residual);
        delta.control_release = release.clone();
        if !projected.violations(Some(&delta)).is_empty() {
            return None;
        }
        let step = self.authorize_step(authorization_for(&delta, &remaining, flow), remaining)?;
        steps.push(step);
        NonEmptyVec::from_vec(steps)
    }

    fn replay_unlocks(&self, base: &SimFlow, steps: &[&PlannedRemedy]) -> bool {
        let mut sim = base.clone();
        let mut lift: Option<Lift> = None;
        for step in steps {
            match step {
                PlannedRemedy::Reduce(ReductionTarget::DeriveValue { source, transformer }) => {
                    let Some(registered) = self
                        .transformers
                        .iter()
                        .find(|t| t.descriptor.transformer == *transformer)
                    else {
                        return false;
                    };
                    let Some(label) = sim.leaf_labels.get(source) else {
                        return false;
                    };
                    if !registered.descriptor.precondition.matches(label) {
                        return false;
                    }
                    sim.leaf_labels.insert(*source, registered.descriptor.output.clone());
                }
                PlannedRemedy::Authorize { authorization, .. } => {
                    if let AuthorizationScope::DerivedValue { source } = authorization.scope() {
                        let raise = authorization
                            .delta()
                            .coordinates()
                            .find_map(|coordinate| match coordinate {
                                DeltaCoordinate::RaiseLabel(raise) => Some(raise),
                                _ => None,
                            })
                            .expect("a derived-value authorization carries a raise");
                        let Some(label) = sim.leaf_labels.get(source) else {
                            return false;
                        };
                        let raised = raise.raise(label);
                        sim.leaf_labels.insert(*source, raised);
                    }
                    if Lift::lifts(authorization.delta()) {
                        lift.get_or_insert_with(Lift::empty).absorb(authorization.delta());
                    }
                }
            }
        }
        sim.violations(lift.as_ref()).is_empty()
    }

    fn waiver_candidates(&self, sim: &SimFlow, remaining: &[Violation]) -> Vec<Lift> {
        let mut candidates = Vec::new();
        if let Some(release) = self.minimal_control_release(sim) {
            let after = sim.violations(Some(&Lift {
                control_release: release.clone(),
                ..Lift::empty()
            }));
            let mut delta = needed_delta(&after);
            delta.control_release = release;
            candidates.push(delta);
        }
        let plain = needed_delta(remaining);
        if !candidates.contains(&plain) {
            candidates.push(plain);
        }
        candidates
    }

    fn minimal_control_release(&self, sim: &SimFlow) -> Option<BTreeSet<ValueId>> {
        let ids: Vec<ValueId> = sim.control_labels.keys().copied().collect();
        if ids.is_empty() {
            return None;
        }
        let residual = |set: &BTreeSet<ValueId>| -> Vec<Violation> {
            sim.violations(Some(&Lift {
                control_release: set.clone(),
                ..Lift::empty()
            }))
        };
        let none = residual(&BTreeSet::new());
        let all: BTreeSet<ValueId> = ids.iter().copied().collect();
        let full = residual(&all);
        if full == none {
            return None;
        }
        let mut minimal = all;
        loop {
            let mut progressed = false;
            for id in &ids {
                if !minimal.contains(id) {
                    continue;
                }
                let mut candidate = minimal.clone();
                candidate.remove(id);
                if residual(&candidate) == full {
                    minimal = candidate;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        Some(minimal)
    }

    /// Authorities competent for `grant`, in routing order: inline before
    /// external (a deterministic answer beats a round-trip to a human), each in
    /// registration order. An inline authority may still abstain at ruling
    /// time, which falls through to the next authority in this order.
    pub(super) fn competent_authorities<'a>(&'a self, ask: &'a Authorization) -> impl Iterator<Item = &'a Authority> {
        let inline = self
            .authorities
            .iter()
            .filter(move |a| matches!(a.mode, AuthorityMode::Inline(_)) && a.mandate.authorizes(ask));
        let external = self
            .authorities
            .iter()
            .filter(move |a| matches!(a.mode, AuthorityMode::External) && a.mandate.authorizes(ask));
        inline.chain(external)
    }
}

fn recipient_leaves_for(contract: Option<&ToolContract>, tree: &ArgumentTree<ValueId>) -> BTreeSet<ValueId> {
    contract
        .and_then(|c| c.arguments.recipients.as_ref().and_then(|role| tree.top_level(role)))
        .map(|subtree| subtree.leaves())
        .unwrap_or_default()
}

struct Combinations {
    indices: Vec<usize>,
    n: usize,
    done: bool,
}

impl Combinations {
    fn new(n: usize, k: usize) -> Self {
        Self {
            indices: (0..k).collect(),
            n,
            done: k == 0 || k > n,
        }
    }
}

impl Iterator for Combinations {
    type Item = Vec<usize>;

    fn next(&mut self) -> Option<Vec<usize>> {
        if self.done {
            return None;
        }
        let current = self.indices.clone();
        let k = self.indices.len();
        let mut i = k;
        loop {
            if i == 0 {
                self.done = true;
                break;
            }
            i -= 1;
            if self.indices[i] != i + self.n - k {
                self.indices[i] += 1;
                for j in i + 1..k {
                    self.indices[j] = self.indices[j - 1] + 1;
                }
                break;
            }
        }
        Some(current)
    }
}

/// Structural identity of two step sequences: the same remedies in the same
/// order, ignoring the violation vectors shown to authorities (`targets`) —
/// prediction metadata that differs by generation path, not by what the plan
/// asks or does. Routes are a deterministic function of the authorization, so
/// they never differ between shape-identical steps. Deliberately
/// order-sensitive: a permuted sequence is a different prediction (different
/// executable head, different recheck sequence) and stays in the frontier.
fn same_step_sequence(a: &NonEmptyVec<PlannedRemedy>, b: &NonEmptyVec<PlannedRemedy>) -> bool {
    fn step_eq(x: &PlannedRemedy, y: &PlannedRemedy) -> bool {
        match (x, y) {
            (PlannedRemedy::Reduce(t1), PlannedRemedy::Reduce(t2)) => t1 == t2,
            (
                PlannedRemedy::Authorize { authorization: a1, .. },
                PlannedRemedy::Authorize { authorization: a2, .. },
            ) => a1 == a2,
            _ => false,
        }
    }
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| step_eq(x, y))
}

/// The total authorization a plan asks for, folded into one comparable
/// vector of atomic coordinates. Scope targets are pinned by validated
/// construction (a raise lives at its derived value, lifts at the one
/// policy check), so the vector keys on coordinate kind — plus the raised
/// leaf for raises.
#[derive(Default)]
pub(super) struct AskVector {
    raises: BTreeMap<ValueId, LabelRaise>,
    except: Option<BTreeSet<Effect>>,
    confirm: bool,
    release: Option<BTreeSet<ValueId>>,
    acknowledged: Option<Vec<Unprovable>>,
}

impl AskVector {
    pub(super) fn of(steps: &NonEmptyVec<PlannedRemedy>) -> Self {
        let mut vector = Self::default();
        for step in steps.iter() {
            let PlannedRemedy::Authorize { authorization, .. } = step else {
                continue;
            };
            for coordinate in authorization.delta().coordinates() {
                match (coordinate, authorization.scope()) {
                    (DeltaCoordinate::RaiseLabel(raise), AuthorizationScope::DerivedValue { source }) => {
                        let entry = vector.raises.entry(*source).or_default();
                        entry.trust = entry.trust.max(raise.trust);
                        if let Some(readers) = &raise.audience {
                            entry
                                .audience
                                .get_or_insert_with(BTreeSet::new)
                                .extend(readers.iter().cloned());
                        }
                    }
                    (DeltaCoordinate::RaiseLabel(_), _) => unreachable!("validated construction pins raise scope"),
                    (DeltaCoordinate::ExceptPriorEffects(effects), _) => {
                        vector
                            .except
                            .get_or_insert_with(BTreeSet::new)
                            .extend(effects.iter().copied());
                    }
                    (DeltaCoordinate::StandInConfirmation, _) => vector.confirm = true,
                    (DeltaCoordinate::ReleaseControl(deps), _) => {
                        vector
                            .release
                            .get_or_insert_with(BTreeSet::new)
                            .extend(deps.iter().copied());
                    }
                    (DeltaCoordinate::AcknowledgeUnknown(facts), _) => {
                        vector
                            .acknowledged
                            .get_or_insert_with(Vec::new)
                            .extend(facts.iter().cloned());
                    }
                }
            }
        }
        vector
    }
}

pub(super) fn ask_cmp(a: &AskVector, b: &AskVector) -> Option<Ordering> {
    let mut acc = Ordering::Equal;
    let leaves: BTreeSet<ValueId> = a.raises.keys().chain(b.raises.keys()).copied().collect();
    for leaf in leaves {
        let step = match (a.raises.get(&leaf), b.raises.get(&leaf)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => {
                let trust = x.trust.cmp(&y.trust);
                let audience = option_set_cmp(&x.audience, &y.audience)?;
                combine_orders(trust, audience)?
            }
        };
        acc = combine_orders(acc, step)?;
    }
    acc = combine_orders(acc, option_set_cmp(&a.except, &b.except)?)?;
    acc = combine_orders(acc, a.confirm.cmp(&b.confirm))?;
    acc = combine_orders(acc, option_set_cmp(&a.release, &b.release)?)?;
    acc = combine_orders(acc, acknowledged_cmp(&a.acknowledged, &b.acknowledged)?)?;
    Some(acc)
}

fn combine_orders(acc: Ordering, next: Ordering) -> Option<Ordering> {
    match (acc, next) {
        (Ordering::Equal, next) => Some(next),
        (acc, Ordering::Equal) => Some(acc),
        (acc, next) if acc == next => Some(acc),
        _ => None,
    }
}

fn option_set_cmp<T: Ord>(a: &Option<BTreeSet<T>>, b: &Option<BTreeSet<T>>) -> Option<Ordering> {
    match (a, b) {
        (None, None) => Some(Ordering::Equal),
        (None, Some(_)) => Some(Ordering::Less),
        (Some(_), None) => Some(Ordering::Greater),
        (Some(x), Some(y)) => set_cmp(x, y),
    }
}

fn set_cmp<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> Option<Ordering> {
    match (a == b, a.is_subset(b), b.is_subset(a)) {
        (true, ..) => Some(Ordering::Equal),
        (false, true, _) => Some(Ordering::Less),
        (false, _, true) => Some(Ordering::Greater),
        _ => None,
    }
}

fn acknowledged_cmp(a: &Option<Vec<Unprovable>>, b: &Option<Vec<Unprovable>>) -> Option<Ordering> {
    let subset = |x: &Vec<Unprovable>, y: &Vec<Unprovable>| x.iter().all(|fact| y.contains(fact));
    match (a, b) {
        (None, None) => Some(Ordering::Equal),
        (None, Some(_)) => Some(Ordering::Less),
        (Some(_), None) => Some(Ordering::Greater),
        (Some(x), Some(y)) => match (subset(x, y), subset(y, x)) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            (false, false) => None,
        },
    }
}

/// The pure simulation state of one flow's check: per-leaf argument labels
/// (so a transform can be predicted by swapping one), the control fold, and
/// the sink parameters. Prediction (planning) and validation (application)
/// share this so a plan's predictions mean exactly what the recheck
/// computes.
#[derive(Debug, Clone)]
pub(crate) struct SimFlow {
    pub(crate) leaf_labels: BTreeMap<ValueId, ValueLabel>,
    /// Control dependencies kept individually (not pre-folded) so a scoped
    /// `control_release` can exclude exactly the named deps and attribution can
    /// ask which single dep carries a breach dimension.
    pub(crate) control_labels: BTreeMap<ValueId, ValueLabel>,
    pub(crate) tool: ToolName,
    pub(crate) requires: Requirements,
    pub(crate) recipients: BTreeSet<crate::dimension::UserId>,
    pub(crate) past_effects: Effects,
    pub(crate) extra: Vec<Violation>,
}

impl SimFlow {
    pub(crate) fn of(
        trajectory: &Trajectory,
        checked: &ToolRequest,
        contract: Option<&ToolContract>,
    ) -> Result<Self, UnknownValue> {
        let view = trajectory.view();
        let mut leaf_labels = BTreeMap::new();
        for leaf in checked.arguments.leaves() {
            leaf_labels.insert(leaf, view.fold_labels([&leaf])?);
        }
        let mut control_labels = BTreeMap::new();
        for id in checked.control.iter() {
            control_labels.insert(*id, view.fold_labels([id])?);
        }
        let (recipients, extra) = match contract {
            Some(c) => (
                c.arguments.resolve_recipients(&checked.arguments, trajectory.store())?,
                Vec::new(),
            ),
            None => (
                BTreeSet::new(),
                vec![Violation::Unprovable(Unprovable::NoContract {
                    tool: checked.tool.clone(),
                })],
            ),
        };
        let mut sim = Self {
            leaf_labels,
            control_labels,
            tool: checked.tool.clone(),
            requires: Requirements::default(),
            recipients,
            past_effects: trajectory.past_effects().clone(),
            extra,
        };
        if let Some(c) = contract {
            sim.adopt_requires(&c.requires);
        }
        Ok(sim)
    }

    /// Adopt a contract's requirement declaration: known requirements are
    /// checked; unknown ones (None) contribute the RequirementsUnknown fact
    /// instead.
    pub(crate) fn adopt_requires(&mut self, requires: &Option<Requirements>) {
        self.extra
            .retain(|v| !matches!(v, Violation::Unprovable(Unprovable::RequirementsUnknown)));
        match requires {
            Some(requires) => self.requires = requires.clone(),
            None => {
                self.requires = Requirements::default();
                self.extra.push(Violation::Unprovable(Unprovable::RequirementsUnknown));
            }
        }
    }

    /// The simulation state of one emission flow's check, under the reserved
    /// response sink and the registered [`ResponsePolicy`]. An emission
    /// proposes no effects; its recipients are the policy's declared
    /// readers. Only an authority's check-scoped confirmation stand-in can
    /// satisfy the response sink's attention rule.
    pub(crate) fn of_emission(
        trajectory: &Trajectory,
        checked: &EmissionRequest,
        policy: Option<&ResponsePolicy>,
    ) -> Result<Self, UnknownValue> {
        let view = trajectory.view();
        let mut leaf_labels = BTreeMap::new();
        for leaf in checked.body.leaves() {
            leaf_labels.insert(leaf, view.fold_labels([&leaf])?);
        }
        let mut control_labels = BTreeMap::new();
        for id in checked.control.iter() {
            control_labels.insert(*id, view.fold_labels([id])?);
        }
        let (requires, recipients, extra) = match policy {
            Some(policy) => (policy.requires.clone(), policy.readers.clone(), Vec::new()),
            None => (
                Requirements::default(),
                BTreeSet::new(),
                vec![Violation::Unprovable(Unprovable::NoContract {
                    tool: ToolName::new(RESPONSE_SINK),
                })],
            ),
        };
        Ok(Self {
            leaf_labels,
            control_labels,
            tool: ToolName::new(RESPONSE_SINK),
            requires,
            recipients,
            past_effects: trajectory.past_effects().clone(),
            extra,
        })
    }

    pub(super) fn flow_label(&self) -> ValueLabel {
        ValueLabel::fold(self.leaf_labels.values().cloned())
            .combine(ValueLabel::fold(self.control_labels.values().cloned()))
    }

    /// The violations this flow would report, optionally under a
    /// check-transient lift. A lift loosens exactly its declared dimensions
    /// and acknowledges acknowledge-only facts on the record.
    pub(crate) fn violations(&self, waiver: Option<&Lift>) -> Vec<Violation> {
        let released = waiver.map(|w| &w.control_release);
        let control = ValueLabel::fold(self.control_labels.iter().filter_map(|(id, label)| {
            if released.is_some_and(|set| set.contains(id)) {
                None
            } else {
                Some(label.clone())
            }
        }));
        let flow = ValueLabel::fold(self.leaf_labels.values().cloned()).combine(control);
        let mut past = self.past_effects.clone();
        let mut confirmed = false;
        if let Some(w) = waiver {
            if let Some(waived) = &w.prior_effects {
                past = past.waiving(waived);
            }
            confirmed = w.confirms;
        }
        let mut remaining = self.extra.clone();
        match self
            .requires
            .check_flow(&flow, &past, confirmed, &self.tool, &self.recipients)
        {
            Verdict::Allow => {}
            Verdict::Escalate(violations) => remaining.extend(violations),
        }
        if waiver.is_some() {
            remaining.retain(|v| v.fixability() != Fixability::AcknowledgeOnly);
        }
        remaining
    }
}

pub(super) fn authorization_for(delta: &Lift, resolved: &[Violation], flow: FlowId) -> Authorization {
    let acknowledged: Vec<Unprovable> = resolved
        .iter()
        .filter(|violation| violation.fixability() == Fixability::AcknowledgeOnly)
        .filter_map(|violation| match violation {
            Violation::Unprovable(fact) => Some(fact.clone()),
            Violation::Breach(_) => None,
        })
        .collect();
    let mut coordinates = Vec::new();
    if let Some(effects) = &delta.prior_effects {
        coordinates.push(DeltaCoordinate::ExceptPriorEffects(effects.clone()));
    }
    if delta.confirms {
        coordinates.push(DeltaCoordinate::StandInConfirmation);
    }
    if !delta.control_release.is_empty() {
        coordinates.push(DeltaCoordinate::ReleaseControl(delta.control_release.clone()));
    }
    if !acknowledged.is_empty() || coordinates.is_empty() {
        coordinates.push(DeltaCoordinate::AcknowledgeUnknown(acknowledged));
    }
    Authorization::new(
        AuthorizationDelta::product(coordinates).expect("at least one coordinate by construction"),
        AuthorizationScope::PolicyCheck { flow },
    )
    .expect("the planner lifts only non-empty coordinates at their check scope")
}

pub(super) fn raise_authorization(source: ValueId, delta: &LabelRaise) -> Authorization {
    Authorization::new(
        AuthorizationDelta::single(DeltaCoordinate::RaiseLabel(delta.clone())),
        AuthorizationScope::DerivedValue { source },
    )
    .expect("the planner raises only non-empty deltas at their derived-value scope")
}

fn needed_delta(violations: &[Violation]) -> Lift {
    use crate::contract::Breach;
    let mut delta = Lift::empty();
    for violation in violations {
        match violation {
            Violation::Breach(Breach::ForbiddenPriorEffects { effects }) => {
                delta
                    .prior_effects
                    .get_or_insert_with(BTreeSet::new)
                    .extend(effects.iter().copied());
            }
            Violation::Breach(Breach::ConfirmationMissing { .. }) => {
                delta.confirms = true;
            }
            Violation::Breach(
                Breach::TrustBelow { .. }
                | Breach::AudienceExceeds { .. }
                | Breach::AudienceNotPublic { .. }
                | Breach::UndeclaredRecipients,
            )
            | Violation::Unprovable(
                Unprovable::TrustUnknown
                | Unprovable::AudienceUnknown
                | Unprovable::EffectsUnknown
                | Unprovable::RequirementsUnknown
                | Unprovable::NoContract { .. },
            ) => {}
        }
    }
    delta
}

fn endorse_steps(sim: &SimFlow, violations: &[Violation]) -> Vec<(ValueId, LabelRaise)> {
    use crate::contract::Breach;
    let trust_req: Option<KnownTrust> = violations.iter().find_map(|v| match v {
        Violation::Breach(Breach::TrustBelow { required, .. }) => Some(*required),
        Violation::Unprovable(Unprovable::TrustUnknown) => sim.requires.trust,
        _ => None,
    });
    let mut readers = BTreeSet::new();
    for v in violations {
        match v {
            Violation::Breach(Breach::AudienceExceeds { outside }) => readers.extend(outside.iter().cloned()),
            Violation::Unprovable(Unprovable::AudienceUnknown) => match &sim.requires.audience {
                AudienceRule::FromRecipients => readers.extend(sim.recipients.iter().cloned()),
                AudienceRule::Readers(declared) => readers.extend(declared.iter().cloned()),
                AudienceRule::Public | AudienceRule::Unrestricted => {}
            },
            _ => {}
        }
    }
    let audience_req = if readers.is_empty() { None } else { Some(readers) };
    if trust_req.is_none() && audience_req.is_none() {
        return Vec::new();
    }
    let full = LabelRaise {
        trust: trust_req,
        audience: audience_req,
    };
    let mut steps = Vec::new();
    for (leaf, label) in &sim.leaf_labels {
        let audience = full
            .audience
            .as_ref()
            .map(|readers| label.audience.missing_readers(readers));
        let delta = LabelRaise {
            trust: full.trust.filter(|req| label.trust.raised_to(*req) != label.trust),
            audience: audience.filter(|deficit| !deficit.is_empty()),
        };
        if !delta.is_empty() {
            steps.push((*leaf, delta));
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{Authority, Ruling, TrajectoryView};
    use crate::contract::{AttentionRule, Breach};
    use crate::dimension::Trust;
    use crate::transition::AuthorityMandate;

    fn approve_all(_: &Authorization, _: &[Violation], _: &TrajectoryView) -> Option<Ruling> {
        Some(Ruling::Approve {
            reason: "approved".to_owned(),
        })
    }

    fn engine_with_waiver(mandate: AuthorityMandate) -> PolicyEngine {
        let mut engine = PolicyEngine::new();
        engine
            .register_authority(Authority::inline("waiver", mandate, approve_all))
            .unwrap();
        engine
    }

    fn probe_sim(requires: Requirements, controls: &[(ValueId, ValueLabel)]) -> SimFlow {
        SimFlow {
            leaf_labels: BTreeMap::new(),
            control_labels: controls.iter().cloned().collect(),
            tool: ToolName::new("probe.sink"),
            requires,
            recipients: BTreeSet::new(),
            past_effects: Effects::none(),
            extra: Vec::new(),
        }
    }

    fn release_of(steps: &NonEmptyVec<PlannedRemedy>) -> BTreeSet<ValueId> {
        let PlannedRemedy::Authorize { authorization, .. } = steps.first() else {
            panic!("a rescue candidate ends in an authorize step");
        };
        authorization
            .delta()
            .coordinates()
            .find_map(|c| match c {
                DeltaCoordinate::ReleaseControl(deps) => Some(deps.clone()),
                _ => None,
            })
            .expect("a rescue waiver carries its release")
    }

    #[test]
    fn joint_release_sweep_collects_every_first_cardinality_success() {
        let engine = engine_with_waiver(AuthorityMandate {
            confirms: true,
            may_release_control: true,
            ..AuthorityMandate::none()
        });
        let (s1, s2) = (ValueId::new(1), ValueId::new(2));
        let sim = probe_sim(
            Requirements {
                attention: AttentionRule::ExplicitConfirmation,
                ..Requirements::default()
            },
            &[(s1, ValueLabel::identity()), (s2, ValueLabel::identity())],
        );

        let hits = engine.minimal_joint_releases(&sim, &[s1, s2], FlowId::new(0));

        assert_eq!(hits.len(), 2);
        assert_eq!(release_of(&hits[0]), BTreeSet::from([s1]));
        assert_eq!(release_of(&hits[1]), BTreeSet::from([s2]));
        for hit in &hits {
            assert_eq!(hit.len(), 1);
            let PlannedRemedy::Authorize { routes, targets, .. } = hit.first() else {
                panic!("the sole step is the waiver");
            };
            assert_eq!(routes.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["waiver"]);
            assert_eq!(
                targets,
                &[Violation::Breach(Breach::ConfirmationMissing {
                    tool: ToolName::new("probe.sink"),
                })]
            );
        }
    }

    #[test]
    fn joint_release_sweep_ascends_past_a_failed_cardinality() {
        let engine = engine_with_waiver(AuthorityMandate {
            may_release_control: true,
            ..AuthorityMandate::none()
        });
        let (s1, s2) = (ValueId::new(1), ValueId::new(2));
        let suspicious = ValueLabel {
            trust: Trust::SUSPICIOUS,
            ..ValueLabel::identity()
        };
        let sim = probe_sim(
            Requirements {
                trust: Some(KnownTrust::Trusted),
                ..Requirements::default()
            },
            &[(s1, suspicious.clone()), (s2, suspicious)],
        );

        let hits = engine.minimal_joint_releases(&sim, &[s1, s2], FlowId::new(0));
        assert_eq!(hits.len(), 1);
        assert_eq!(release_of(&hits[0]), BTreeSet::from([s1, s2]));

        let incompetent = engine_with_waiver(AuthorityMandate::none());
        let sim = probe_sim(
            Requirements {
                trust: Some(KnownTrust::Trusted),
                ..Requirements::default()
            },
            &[
                (
                    s1,
                    ValueLabel {
                        trust: Trust::SUSPICIOUS,
                        ..ValueLabel::identity()
                    },
                ),
                (
                    s2,
                    ValueLabel {
                        trust: Trust::SUSPICIOUS,
                        ..ValueLabel::identity()
                    },
                ),
            ],
        );
        assert!(
            incompetent
                .minimal_joint_releases(&sim, &[s1, s2], FlowId::new(0))
                .is_empty()
        );
    }
}
