use appa_engine::audience::AudienceConfig;
use appa_engine::engine::Engine;
use appa_engine::fact::{Fact, RootForkOrigin};
use appa_engine::profile::{
    DeploymentPolicy, DeploymentProfile, OpenVector, PolicyDialectVersion, PolicyFileKey, PolicyIdentityV1,
    ProfileDeclaration,
};
use appa_engine::registry::{PlannerCap, RegistryConfig, TrustChain};
use appa_engine::value::TrajectoryId;
use serde::Serialize;

// Pin the struct-variant wire format used by openings already stored in family logs.
#[derive(Serialize)]
enum StoredFact<'a> {
    TrajectoryOpened {
        trajectory: &'a TrajectoryId,
        dialect: PolicyDialectVersion,
        profile: &'a DeploymentProfile,
        policy_digest: PolicyIdentityV1,
        policy_file_key: &'a PolicyFileKey,
        open_vectors: Vec<OpenVector>,
        #[serde(skip_serializing_if = "Option::is_none")]
        forked_from: Option<&'a RootForkOrigin>,
    },
}

#[test]
fn ordinary_and_forked_openings_preserve_stored_bytes_and_replay() {
    let chain = TrustChain::new(vec!["untrusted".into(), "trusted".into()]);
    let engine = Engine::open(DeploymentPolicy {
        profile: ProfileDeclaration::no_coverage(&chain),
        dialect: PolicyDialectVersion::new(1),
        planner_cap: PlannerCap::default(),
        registry: RegistryConfig {
            trust_chain: chain,
            tools: vec![],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: AudienceConfig::default(),
        },
    })
    .expect("the policy opens");
    let parent = TrajectoryId::new("parent");
    let fork = TrajectoryId::new("fork");
    let key = PolicyFileKey::of(b"stored opening policy");
    let ordinary = engine
        .open_trajectory(&parent, key.clone())
        .expect("the parent opens")
        .into_unsealed();
    let view = engine.view(&parent, ordinary.clone(), 1).expect("the parent replays");
    let origin = view.root_fork_origin(&parent).expect("the parent can be forked");
    let forked = engine
        .open_root_fork(&fork, key.clone(), origin.clone())
        .expect("the independent root opens")
        .into_unsealed();

    for (id, facts, source) in [(&parent, &ordinary, None), (&fork, &forked, Some(&origin))] {
        let stored = StoredFact::TrajectoryOpened {
            trajectory: id,
            dialect: PolicyDialectVersion::new(1),
            profile: engine.profile(),
            policy_digest: engine.identity(),
            policy_file_key: &key,
            open_vectors: engine.open_vectors(),
            forked_from: source,
        };
        let bytes = serde_json::to_vec(&stored).expect("the stored form serializes");
        assert_eq!(serde_json::to_vec(&facts[0]).expect("the opening serializes"), bytes);
        let restored: Fact = serde_json::from_slice(&bytes).expect("the stored opening still reads");
        assert_eq!(restored, facts[0]);
        engine
            .view(id, vec![restored], 1)
            .expect("the stored opening still validates");

        if source.is_none() {
            let mut explicit_null = serde_json::to_value(&stored).expect("the stored form is JSON");
            explicit_null["TrajectoryOpened"]["forked_from"] = serde_json::Value::Null;
            assert_eq!(
                serde_json::from_value::<Fact>(explicit_null).expect("an explicit absent origin still reads"),
                facts[0]
            );
        }
    }
}
