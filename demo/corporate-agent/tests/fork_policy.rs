//! The shipped fork policy actually assembles: it parses, every registered
//! tool gets a backend, and the registered sanitizer resolves to a live
//! builtin — the exact wiring `appa-corp-agent` performs at startup.

use std::collections::BTreeMap;

use appa_engine::names::SanitizerName;
use appa_runtime::tool::BuiltinTool;
use appa_runtime::{Config, Mediator};

const FORK_POLICY: &str = include_str!("../../../bench-corp/policies/appa.toml");
const FORK_OPEN_POLICY: &str = include_str!("../../../bench-corp/policies/open.toml");

fn assemble(policy: &str) -> Mediator {
    let config = Config::from_toml_str(policy).expect("the policy parses");
    let backends: BTreeMap<_, _> = config
        .registry_config()
        .tools
        .iter()
        .map(|contract| (contract.name.clone(), BuiltinTool::Echo(String::new())))
        .collect();
    assert_eq!(backends.len(), 13, "the 13-tool corp surface");
    Mediator::new(config, backends).expect("the mediator assembles")
}

#[test]
fn fork_policy_assembles_with_a_backend_per_tool_and_a_live_sanitizer() {
    let mediator = assemble(FORK_POLICY);
    assert!(
        mediator
            .sanitizer_backend(&SanitizerName::new("pii-redactor"))
            .is_some(),
        "the registered sanitizer must resolve to an implementation, or child-return \
         derivations fail closed at runtime"
    );
}

#[test]
fn fork_open_policy_assembles_over_the_same_surface() {
    assemble(FORK_OPEN_POLICY);
}
