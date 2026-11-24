use appa_engine::audience::AudienceConfig;
use appa_engine::engine::Engine;
use appa_engine::fact::{Fact, RootForkOrigin};
use appa_engine::label::ReaderId;
use appa_engine::profile::{
    DeploymentName, DeploymentPolicy, DeploymentProfile, PolicyFileKey, PolicyRevision, ProfileDeclaration,
};
use appa_engine::registry::{PlannerCap, RegistryConfig, TrustChain};
use appa_engine::value::TrajectoryId;
use serde::Serialize;

// Pin the struct-variant wire format used by openings already stored in family logs.
#[derive(Serialize)]
enum StoredFact<'a> {
    TrajectoryOpened {
        trajectory: &'a TrajectoryId,
        profile: &'a DeploymentProfile,
        policy_file_key: &'a PolicyFileKey,
        deployment: &'a DeploymentName,
        revision: PolicyRevision,
        #[serde(skip_serializing_if = "Option::is_none")]
        forked_from: Option<&'a RootForkOrigin>,
    },
}

/// An engine of the test deployment line, at its first revision.
fn placed(policy: DeploymentPolicy) -> Engine {
    Engine::open(policy)
        .expect("the policy opens")
        .at(DeploymentName::new("default"), PolicyRevision::FIRST)
}

#[test]
fn ordinary_and_forked_openings_preserve_stored_bytes_and_replay() {
    let chain = TrustChain::new(vec!["untrusted".into(), "trusted".into()]);
    let engine = placed(DeploymentPolicy {
        profile: ProfileDeclaration::no_coverage(&chain),
        planner_cap: PlannerCap::default(),
        registry: RegistryConfig {
            recorders: Default::default(),
            trust_chain: chain,
            tools: vec![],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: AudienceConfig::default(),
        },
    });
    let parent = TrajectoryId::new("parent");
    let fork = TrajectoryId::new("fork");
    let key = PolicyFileKey::of(b"stored opening policy");
    let ordinary = engine
        .open_trajectory(&parent, key.clone(), None, Default::default())
        .expect("the parent opens")
        .into_unsealed();
    let view = engine.view(&parent, ordinary.clone(), 1).expect("the parent replays");
    let origin = engine
        .root_fork_origin(&view, &parent)
        .expect("the parent can be forked");
    let forked = engine
        .open_root_fork(&fork, key.clone(), origin.clone(), Default::default())
        .expect("the independent root opens")
        .into_unsealed();

    for (id, facts, source) in [(&parent, &ordinary, None), (&fork, &forked, Some(&origin))] {
        let stored = StoredFact::TrajectoryOpened {
            trajectory: id,
            profile: engine.profile(),
            policy_file_key: &key,
            deployment: engine.deployment().expect("the engine is placed"),
            revision: engine.revision().expect("the engine is placed"),
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

        // The opening fields an earlier release also wrote are read past: its families stay
        // readable. An opening that names no deployment does not read at all.
        let mut earlier = serde_json::to_value(&stored).expect("the stored form is JSON");
        earlier["TrajectoryOpened"]["dialect"] = 1.into();
        earlier["TrajectoryOpened"]["policy_digest"] = "00".repeat(32).into();
        earlier["TrajectoryOpened"]["open_vectors"] = serde_json::json!([]);
        assert_eq!(
            serde_json::from_value::<Fact>(earlier.clone()).expect("an earlier release's opening reads"),
            facts[0]
        );
        earlier["TrajectoryOpened"]
            .as_object_mut()
            .expect("the opening is an object")
            .remove("deployment");
        assert!(serde_json::from_value::<Fact>(earlier).is_err());
    }
}

/// The principal a family acts for rides its opening and a root fork's origin; replay refuses
/// one that is not an address, or a fork whose principal is not its origin's.
#[test]
fn a_principal_opening_forks_with_its_principal_and_replay_holds_it_to_shape() {
    let chain = TrustChain::new(vec!["untrusted".into(), "trusted".into()]);
    let engine = placed(DeploymentPolicy {
        profile: ProfileDeclaration::no_coverage(&chain),
        planner_cap: PlannerCap::default(),
        registry: RegistryConfig {
            recorders: Default::default(),
            trust_chain: chain,
            tools: vec![],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: AudienceConfig::default(),
        },
    });
    let parent = TrajectoryId::new("parent");
    let fork = TrajectoryId::new("fork");
    let key = PolicyFileKey::of(b"stored opening policy");
    let alice = ReaderId::new("alice@corp.example");
    let opened = engine
        .open_trajectory(&parent, key.clone(), Some(alice.clone()), Default::default())
        .expect("the parent opens")
        .into_unsealed();
    let view = engine.view(&parent, opened.clone(), 1).expect("the parent replays");
    assert_eq!(view.principal(), Some(&alice));

    let origin = engine
        .root_fork_origin(&view, &parent)
        .expect("the parent can be forked");
    let forked = engine
        .open_root_fork(&fork, key, origin, Default::default())
        .expect("the independent root opens")
        .into_unsealed();
    let fork_view = engine.view(&fork, forked.clone(), 1).expect("the fork replays");
    assert_eq!(fork_view.principal(), Some(&alice));

    let tampered = |facts: &[Fact], id: &TrajectoryId, principal: &str| {
        let mut json = serde_json::to_value(&facts[0]).expect("the opening is JSON");
        json["TrajectoryOpened"]["principal"] = principal.into();
        let fact: Fact = serde_json::from_value(json).expect("the tampered opening reads");
        engine.view(id, vec![fact], 1).is_err()
    };
    assert!(
        tampered(&opened, &parent, "alice"),
        "a principal that is not an address"
    );
    assert!(
        tampered(&forked, &fork, "bob@corp.example"),
        "a fork acting for another principal"
    );
}

/// The settings a host pins ride the opening they were pinned at: a root fork records its
/// own, never its parent's. A pinned opening reads back as written and replays; one whose line
/// variant is no token does not read at all.
#[test]
fn pinned_settings_ride_their_opening_and_read_back_as_written() {
    use appa_runtime_api::{AutoApply, LineVariant, SessionSettings};
    let chain = TrustChain::new(vec!["untrusted".into(), "trusted".into()]);
    let engine = placed(DeploymentPolicy {
        profile: ProfileDeclaration::no_coverage(&chain),
        planner_cap: PlannerCap::default(),
        registry: RegistryConfig {
            recorders: Default::default(),
            trust_chain: chain,
            tools: vec![],
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            audience: AudienceConfig::default(),
        },
    });
    let parent = TrajectoryId::new("parent");
    let fork = TrajectoryId::new("fork");
    let key = PolicyFileKey::of(b"stored opening policy");
    let observe = SessionSettings {
        auto_apply: AutoApply::RulingsAndAccept,
        line_variant: Some(LineVariant::parse("observe").expect("a token")),
    };
    let opened = engine
        .open_trajectory(&parent, key.clone(), None, observe.clone())
        .expect("the parent opens")
        .into_unsealed();
    let settings_of = |fact: &Fact| match fact {
        Fact::TrajectoryOpened(opening) => opening.settings.clone(),
        other => panic!("not an opening: {other:?}"),
    };
    assert_eq!(settings_of(&opened[0]), observe);
    let view = engine.view(&parent, opened.clone(), 1).expect("the parent replays");

    let origin = engine
        .root_fork_origin(&view, &parent)
        .expect("the parent can be forked");
    let forked = engine
        .open_root_fork(&fork, key, origin, SessionSettings::default())
        .expect("the independent root opens")
        .into_unsealed();
    assert!(settings_of(&forked[0]).is_default());

    let bytes = serde_json::to_vec(&opened[0]).expect("the opening serializes");
    let restored: Fact = serde_json::from_slice(&bytes).expect("the pinned opening reads");
    assert_eq!(restored, opened[0]);
    assert_eq!(serde_json::to_vec(&restored).expect("the opening serializes"), bytes);
    engine
        .view(&parent, vec![restored], 1)
        .expect("the pinned opening validates");

    let mut json = serde_json::to_value(&opened[0]).expect("the opening is JSON");
    json["TrajectoryOpened"]["settings"]["line_variant"] = "no spaces allowed".into();
    assert!(serde_json::from_value::<Fact>(json).is_err());
}

/// A close's stored forms read and write back byte for byte; an abandoned success is a tag of
/// its own, never one an earlier release wrote.
#[test]
fn stored_closes_keep_their_bytes_beside_an_abandoned_success() {
    use appa_engine::fact::{CloseOutcome, EffectKind, EffectSet};
    use appa_engine::value::RawResultDigest;

    let observed = RawResultDigest::of(b"raw");
    let stored = [
        (
            r#"{"Success":{"effects":["read"]}}"#.to_string(),
            CloseOutcome::Success {
                effects: EffectSet::new([EffectKind::new("read")]).expect("one effect is a set"),
            },
        ),
        (
            r#"{"Success":{"effects":[]}}"#.to_string(),
            CloseOutcome::Success {
                effects: EffectSet::default(),
            },
        ),
        (r#""Failure""#.to_string(), CloseOutcome::Failure),
        (
            format!(
                r#"{{"FailureWithBody":{{"observed":{}}}}}"#,
                serde_json::to_string(&observed).expect("a digest serializes")
            ),
            CloseOutcome::FailureWithBody { observed },
        ),
        (r#""Indeterminate""#.to_string(), CloseOutcome::Indeterminate),
    ];
    for (bytes, outcome) in &stored {
        assert_eq!(
            &serde_json::from_str::<CloseOutcome>(bytes).expect("a stored close reads"),
            outcome
        );
        assert_eq!(&serde_json::to_string(outcome).expect("a close serializes"), bytes);
    }
    let abandoned = serde_json::to_string(&CloseOutcome::SuccessAbandoned).expect("a close serializes");
    assert_eq!(abandoned, r#""SuccessAbandoned""#);
    assert_eq!(
        serde_json::from_str::<CloseOutcome>(&abandoned).expect("an abandoned close reads"),
        CloseOutcome::SuccessAbandoned
    );
}
