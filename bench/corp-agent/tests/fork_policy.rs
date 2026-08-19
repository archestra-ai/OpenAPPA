//! The shipped bench policies actually deploy: they compile through the
//! dialect, every tool the agent will advertise is registered, and the
//! deployment opens with the redactor bound to the shim that hosts it — the
//! exact wiring `appa-corp-agent` performs at startup.

use appa_runtime::api::Runtime;
use appa_runtime::config::{Config, Implementation};
use corp_systems::systems::System;
use corporate_agent_demo::catalogue;
use corporate_agent_demo::shim::{self, CorpWorld};

const FORK_POLICY: &str = include_str!("../../../bench/corp/policies/appa.toml");
const FORK_OPEN_POLICY: &str = include_str!("../../../bench/corp/policies/open.toml");

/// The corp surface plus the spawn tool: seventeen tools the world implements
/// and `fork`, which the harness acts on instead of dispatching.
const SURFACE: usize = 18;

fn deployment(policy: &str, dir: &tempfile::TempDir) -> Config {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy).expect("the policy writes");
    Config::load(&path).expect("the policy is a loadable deployment")
}

fn compiled(config: &Config) -> appa_policy::Config {
    appa_policy::Config::from_toml_str(&toml::to_string(config.policy_file().value()).expect("the policy re-renders"))
        .expect("the policy compiles through the dialect")
}

/// Serve the shim on a real port and bind what it hosts, as the binary does.
async fn hosted(dir: &tempfile::TempDir, config: &mut Config) -> String {
    let address = shim::serve(CorpWorld {
        data_root: dir.path().join("data"),
        sink_root: dir.path().join("sink"),
        enabled: System::ALL.into_iter().collect(),
    })
    .await
    .expect("the shim binds");
    let origin = format!("http://{address}");
    for implementation in config.externals.sanitizers.values_mut() {
        if let Implementation::Resolver(endpoint) = implementation
            && let Some(path) = endpoint.url.strip_prefix("http://127.0.0.1:0")
            && shim::serves(path)
        {
            endpoint.url = format!("{origin}{path}");
        }
    }
    origin
}

#[tokio::test]
async fn the_guarded_policy_opens_with_its_redactor_hosted_by_the_agents_own_shim() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let mut config = deployment(FORK_POLICY, &dir);
    assert!(
        config.externals.sanitizers.contains_key("pii-redactor"),
        "the guarded policy registers the declassifier its child returns depend on",
    );
    let origin = hosted(&dir, &mut config).await;
    match config.externals.sanitizers.get("pii-redactor") {
        Some(Implementation::Resolver(endpoint)) => assert_eq!(
            endpoint.url,
            format!("{origin}{}", shim::REDACTOR_PATH),
            "the shim's origin replaces the unbound one and the path survives",
        ),
        other => panic!("the redactor must be a hosted resolver, not {other:?}"),
    }

    // Opening is where an unhostable component would be caught: the runtime
    // refuses a deployment it cannot honor before anything runs.
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the guarded deployment opens");
}

#[tokio::test]
async fn the_open_policy_opens_over_the_same_surface() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let config = deployment(FORK_OPEN_POLICY, &dir);
    assert!(
        config.externals.sanitizers.is_empty(),
        "the undefended baseline registers no declassifier",
    );
    Runtime::open(config, dir.path().join("appa.db"), None).expect("the open deployment opens");
}

/// Both arms show the model the same tools, so a score difference is the
/// policy's doing and never a difference in what the model was offered.
#[test]
fn both_arms_advertise_the_same_surface_and_the_ablation_drops_only_the_spawn_tool() {
    for policy in [FORK_POLICY, FORK_OPEN_POLICY] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let config = compiled(&deployment(policy, &dir));
        let branching: Vec<String> = catalogue::advertised(&config, true)
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(branching.len(), SURFACE);
        assert!(branching.contains(&catalogue::FORK.to_string()));

        let ablation: Vec<String> = catalogue::advertised(&config, false)
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(ablation.len(), SURFACE - 1);
        assert!(!ablation.contains(&catalogue::FORK.to_string()));
    }
}
