//! Marginal cost of one remedy round — block, approve, release, outcome — on a family log
//! that already holds `N` such rounds, with an older view of the same family held alive.

use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::time::Duration;

use appa_engine::audience::AudienceEvidence;
use appa_engine::authority::{Authority, Mandate, Scope};
use appa_engine::contract::{Delta, LabelRequirements, Requires, ToolAnnotation, ToolDeclaration};
use appa_engine::engine::Engine;
use appa_engine::execute::{AuthorityEvidence, AuthorityReview};
use appa_engine::fact::EffectSet;
use appa_engine::label::{Audience, Label, Trust};
use appa_engine::names::AuthorityName;
use appa_engine::params::ToolParameters;
use appa_engine::profile::{
    BindingMode, DeploymentPolicy, ExecutorClass, PolicyDialectVersion, PolicyFileKey, ProfileDeclaration,
};
use appa_engine::registry::{PlannerCap, RegistryConfig, TrustChain};
use appa_engine::transition::{
    EngineDecision, EngineEvent, EngineView, FollowUp, OfferExecution, OfferOutcome, OutcomeBody, ProposalBatch,
    ProposalBatchId, ProposedCall, ToolOutcome, ToolReport,
};
use appa_engine::value::{OfferNonce, ToolName, TrajectoryId, ValueBody};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

const SUSPICIOUS: Trust = Trust::new(0);
const TRUSTED: Trust = Trust::new(1);
const TOOL: &str = "wire";
const OFFICER: &str = "officer";

#[derive(Clone, Copy)]
enum History {
    DistinctCalls,
    RepeatedCall,
}

impl History {
    fn name(self) -> &'static str {
        match self {
            History::DistinctCalls => "distinct_calls",
            History::RepeatedCall => "repeated_call",
        }
    }

    fn arguments(self, round: usize) -> Vec<u8> {
        match self {
            History::DistinctCalls => format!("{{\"n\":{round}}}").into_bytes(),
            History::RepeatedCall => b"{}".to_vec(),
        }
    }
}

/// A `wire` tool demanding trust the trajectory never has, and one officer who can vouch it.
fn engine() -> Engine {
    let chain = TrustChain::new(vec!["suspicious".into(), "trusted".into()]);
    let wire = ToolAnnotation {
        name: ToolName::new(TOOL),
        tags: vec![],
        description: Some("Sends a wire.".to_string()),
        parameters: ToolParameters::open(),
        delta: Delta::NONE,
        forwards: Delta::NONE,
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
        name: AuthorityName::new(OFFICER),
        mandate: Mandate {
            trust_ceiling: Some(TRUSTED),
            ..Mandate::default()
        },
        scope: Scope::default(),
        hint: None,
    };
    Engine::open(DeploymentPolicy {
        profile: ProfileDeclaration {
            starting_label: starting_label(),
            context_control: false,
            dispatch: ExecutorClass::Enforced,
            executor_exceptions: BTreeMap::new(),
            confined_results: BTreeSet::from([ToolName::new(TOOL)]),
            provider_surfaces: BTreeMap::new(),
            binding: BindingMode::Harness,
        },
        dialect: PolicyDialectVersion::new(1),
        planner_cap: PlannerCap::default(),
        registry: RegistryConfig {
            trust_chain: chain,
            tools: vec![ToolDeclaration::Declared(wire)],
            annotators: vec![],
            authorities: vec![officer],
            sanitizers: vec![],
            audience: Default::default(),
        },
    })
    .expect("the bench policy opens")
}

fn starting_label() -> Label {
    Label::new(SUSPICIOUS, Audience::public())
}

fn trajectory() -> TrajectoryId {
    TrajectoryId::new("bench")
}

fn decide(engine: &Engine, view: &mut EngineView, event: EngineEvent) -> EngineDecision {
    let decision = engine.handle(view, event).expect("the bench event decides");
    view.advance(decision.append.as_ref().expect("every bench decision appends"))
        .expect("the sealed batch advances its view");
    decision
}

fn proposal(history: History, round: usize, stage: &str) -> EngineEvent {
    EngineEvent::Proposals(ProposalBatch {
        id: ProposalBatchId::new(format!("r{round}-{stage}")),
        trajectory: trajectory(),
        provider_results: vec![],
        proposals: vec![ProposedCall {
            tool: ToolName::new(TOOL),
            arguments: history.arguments(round),
            annotation: None,
        }],
        spawn: None,
        offer_nonce: OfferNonce::new([7; 32]),
        evidence: vec![],
        audience: AudienceEvidence::default(),
    })
}

/// One remedy round: the call blocks, the officer's plan executes into an approval, the
/// re-proposed call consumes it and releases, and the tool's success is admitted.
fn round(engine: &Engine, view: &mut EngineView, history: History, round: usize) {
    let blocked = match decide(engine, view, proposal(history, round, "block")).follow_up {
        FollowUp::Proposals { mut blocked, .. } if blocked.len() == 1 => blocked.remove(0),
        other => panic!("the wire blocks, got {other:?}"),
    };
    let (offer, plan) = blocked
        .block
        .plans
        .iter()
        .filter_map(|plan| plan.executable())
        .find(|plan| !plan.required.is_empty())
        .and_then(|plan| {
            blocked
                .offers
                .iter()
                .find(|(_, offered)| *offered == plan.id)
                .map(|(offer, _)| (*offer, plan))
        })
        .expect("the officer's plan is offered");
    let evidence = plan
        .required
        .iter()
        .map(|ruling| AuthorityEvidence {
            offer,
            authority: ruling.authority.clone(),
            covers: ruling.covers.clone(),
            reviewed: AuthorityReview {
                tool: ToolName::new(TOOL),
                trajectory_label: starting_label(),
            },
        })
        .collect();
    decide(
        engine,
        view,
        EngineEvent::ExecuteOffer(OfferExecution {
            trajectory: trajectory(),
            offer,
            outcome: OfferOutcome::Approved(evidence),
            return_policy: None,
            offer_nonce: OfferNonce::new([11; 32]),
            audience: AudienceEvidence::default(),
        }),
    );
    let dispatch = match decide(engine, view, proposal(history, round, "release")).follow_up {
        FollowUp::Proposals { mut released, .. } if released.len() == 1 => released.remove(0).dispatch,
        other => panic!("the approved wire releases, got {other:?}"),
    };
    decide(
        engine,
        view,
        EngineEvent::Outcome(ToolReport {
            dispatch,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(ValueBody::new("sent")),
            },
            evidence: vec![],
            offer_nonce: OfferNonce::new([13; 32]),
            audience: AudienceEvidence::default(),
        }),
    );
}

/// The view after `rounds` rounds, and the view as it stood halfway there.
fn history_of(engine: &Engine, history: History, rounds: usize) -> (EngineView, EngineView) {
    let opening = engine
        .open_trajectory(&trajectory(), PolicyFileKey::of(b"bench policy"), None)
        .expect("the bench trajectory opens")
        .into_unsealed();
    let mut view = engine.view(&trajectory(), opening, 1).expect("the opening replays");
    let mut held = view.clone();
    for index in 0..rounds {
        if index == rounds / 2 {
            held = view.clone();
        }
        round(engine, &mut view, history, index);
    }
    (view, held)
}

fn decisions(c: &mut Criterion) {
    let engine = engine();
    let mut group = c.benchmark_group("remedy_round");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(20);
    for history in [History::DistinctCalls, History::RepeatedCall] {
        for rounds in [50, 200, 800] {
            let (built, held) = history_of(&engine, history, rounds);
            group.bench_with_input(BenchmarkId::new(history.name(), rounds), &rounds, |b, &rounds| {
                b.iter_batched(
                    || built.clone(),
                    |mut view| {
                        round(&engine, &mut view, history, rounds);
                        view
                    },
                    BatchSize::LargeInput,
                )
            });
            black_box(&held);
        }
    }
    group.finish();
}

criterion_group!(benches, decisions);
criterion_main!(benches);
