//! Remedy planning: turning a raw block into the sound remedies the agent may act on.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::authority::Authority;
use crate::check::{self, CheckOutcome, Gap, RawBlock};
use crate::contract::ToolContract;
use crate::fact::EffectKind;
use crate::label::{Adequacy, Dim, Label};
use crate::names::{AuthorityName, TagName};
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

/// One engine-side act in an executable plan. Both are atomic and change no trajectory label by
/// themselves: `Authorize` records a ruling that admits the dispatch despite a gap; `Accept` records
/// the agent's acceptance of the narrowing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemedyStep {
    Authorize(AuthorityName),
    Accept,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemedyPlan {
    pub id: PlanId,
    pub steps: Vec<RemedyStep>,
}

/// A prose remedy the agent carries out itself as ordinary, separately-checked calls — never atomic
/// with the blocked call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recommendation {
    Redispatch { tool: ToolName, reason: String },
    Fork { reason: String },
}

impl Recommendation {
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    label: Label,
    effects: BTreeSet<EffectKind>,
}

/// Plan the remedies for a raw block. Emits the executable plan when the block clears in one atomic
/// step, and a curative `Redispatch` when only a prior tool call unlocks it; `Fork` is always
/// advisory. See the module docs for the curability model.
pub(crate) fn plan(registry: &Registry, views: &Views, call: &ResolvedCall, raw: &RawBlock) -> PlannedBlock {
    let start = State {
        label: views.current_label(),
        effects: views.present_effects(),
    };

    let mut plans = Vec::new();
    if let Some(steps) = directly_clearable(registry, &start, call) {
        plans.push(RemedyPlan { id: PlanId(0), steps });
    }

    let mut recommendations = Vec::new();
    if plans.is_empty()
        && let Some((tool, reason)) = curative_redispatch(registry, &start, call, raw)
    {
        recommendations.push(Recommendation::Redispatch { tool, reason });
    }
    recommendations.push(Recommendation::Fork {
        reason: "handle in a subagent (advisory: a child begins at the same label, so a fork cures no requirement)"
            .to_string(),
    });

    PlannedBlock {
        raw: raw.clone(),
        plans,
        recommendations,
    }
}

fn directly_clearable(registry: &Registry, state: &State, call: &ResolvedCall) -> Option<Vec<RemedyStep>> {
    let contract = registry.tool(call.tool())?;
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    match check::evaluate_state(registry, contract, &state.label, &has_effect, call) {
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
            if block.narrowing.is_some() {
                steps.push(RemedyStep::Accept);
            }
            Some(steps)
        }
    }
}

fn prerequisite_runnable(registry: &Registry, state: &State, tool: &ToolContract) -> bool {
    let call = synthetic_call(tool);
    let has_effect = |kind: &EffectKind| state.effects.contains(kind);
    match check::evaluate_state(registry, tool, &state.label, &has_effect, &call) {
        CheckOutcome::Allow => true,
        CheckOutcome::Unresolved(_) => false,
        CheckOutcome::Block(block) => block
            .requirement_gaps
            .iter()
            .all(|gap| matches!(gap, Gap::Includes { .. }) || authority_for(registry, gap, &tool.tags).is_some()),
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

pub(crate) fn required_rulings(registry: &Registry, block: &RawBlock, tags: &[TagName]) -> Vec<RequiredRuling> {
    let mut grouped: Vec<RequiredRuling> = Vec::new();
    for gap in &block.requirement_gaps {
        let Some(authority) = authority_for(registry, gap, tags) else {
            continue;
        };
        match grouped.iter_mut().find(|r| &r.authority == authority) {
            Some(existing) => existing.covers.push(gap.clone()),
            None => grouped.push(RequiredRuling {
                authority: authority.clone(),
                covers: vec![gap.clone()],
            }),
        }
    }
    grouped
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

fn transition(registry: &Registry, state: &State, tool: &ToolContract) -> State {
    let mut effects = state.effects.clone();
    effects.extend(tool.emits.iter().cloned());
    State {
        label: check::effective_delta(registry, tool).apply(&state.label),
        effects,
    }
}

fn synthetic_call(tool: &ToolContract) -> ResolvedCall {
    ResolvedCall::new(tool.name.clone(), serde_json::Value::Null, Vec::new())
}

fn curable(registry: &Registry, state: &State, call: &ResolvedCall, visiting: &mut Vec<State>) -> bool {
    if directly_clearable(registry, state, call).is_some() {
        return true;
    }
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
                check::evaluate_state(registry, contract, &state.label, &has_effect, call),
                CheckOutcome::Unresolved(_)
            )
        }
    }
}

fn curative_redispatch(
    registry: &Registry,
    start: &State,
    call: &ResolvedCall,
    raw: &RawBlock,
) -> Option<(ToolName, String)> {
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
            return Some((tool.name.clone(), redispatch_reason(tool, raw)));
        }
    }
    None
}

fn redispatch_reason(tool: &ToolContract, raw: &RawBlock) -> String {
    let name = tool.name.as_str();
    for gap in &raw.requirement_gaps {
        match gap {
            Gap::Prior(kind) if tool.emits.contains(kind) => {
                return format!("run {name} first to satisfy prior({})", kind.as_str());
            }
            Gap::Cap { .. } if matches!(tool.delta.audience, Some(Dim::Known(_))) => {
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
            delta: Delta::NONE,
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
    fn no_competent_authority_is_terminal() {
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![],
            delta: Delta::NONE,
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
        assert!(planned.recommendations.iter().all(|r| !r.is_curative()));
    }

    #[test]
    fn acceptance_plan_for_pure_narrowing() {
        let tool = ToolContract {
            name: ToolName::new("get"),
            tags: vec![],
            delta: Delta {
                trust: None,
                audience: Some(Dim::Known(Audience::restricted([ReaderId::new("internal")]))),
            },
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
        assert!(planned.is_curable());
        assert_eq!(planned.plans[0].steps, vec![RemedyStep::Accept]);
    }

    #[test]
    fn prior_gap_cured_by_a_redispatch() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
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
            delta: Delta::NONE,
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
    fn prior_gap_without_emitter_is_terminal() {
        let delete = ToolContract {
            name: ToolName::new("delete_db"),
            tags: vec![],
            delta: Delta::NONE,
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
        let tool = ToolContract {
            name: ToolName::new("wire"),
            tags: vec![TagName::new("payments")],
            delta: Delta::NONE,
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
            delta: Delta::NONE,
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
        let a = ToolContract {
            name: ToolName::new("a"),
            tags: vec![],
            delta: Delta::NONE,
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
            delta: Delta::NONE,
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

    mod reference {
        use super::*;

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

    fn a_delta() -> impl Strategy<Value = Delta> {
        (
            prop::option::of((0u8..2).prop_map(|t| Dim::Known(Trust::new(t)))),
            prop::option::of(small_audience().prop_map(Dim::Known)),
        )
            .prop_map(|(trust, audience)| Delta { trust, audience })
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
            move |(delta, emits, requires)| ToolContract {
                name: name.clone(),
                tags: vec![],
                delta,
                emits,
                requires,
                output_sanitizer: None,
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
        #[test]
        fn planner_agrees_with_reference_oracle(
            tools in prop::collection::vec(a_tool(0), 1..4),
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
                authorities,
                sanitizers: vec![],
                casts: vec![],
            });
            prop_assert!(built.is_ok(), "generated config must load: {:?}", built.err());
            let registry = built.unwrap();

            let target = ToolName::new(format!("t{}", target % registry.tools().count().max(1)));
            let contract = registry.tool(&target).expect("target is modulo the re-keyed tool count");
            let call = synthetic_call(contract);

            let has_effect = |kind: &EffectKind| state.effects.contains(kind);
            let raw = match check::evaluate_state(&registry, contract, &state.label, &has_effect, &call) {
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

            let oracle = reference::curable(&registry, &state, &call);
            prop_assert_eq!(planned.is_curable(), oracle);
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
