//! A hosted document's contracts compile from the document alone. This binary sets the
//! operator's Jev endpoint variable to a value no deployment could open with, so it keeps a
//! binary of its own.

use std::time::Duration;

use appa_runtime::config::HostDefaults;
use appa_runtime::contracts::{ContractCall, ContractVerdict, HostedContracts};
use appa_runtime_api::DecisionLabel;

const DOCUMENT: &str = r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"
builtin = "jev"
ranks = ["suspicious", "trusted"]

[[policy.tool]]
name = "mcp/web/fetch"
annotator = "classifier"

[externals.jev]
token_env = "APPA_PROVIDER_JEV_API_KEY"
"#;

#[test]
fn the_operators_environment_does_not_reach_offline_contracts() {
    // SAFETY: the only test of this binary, set before anything reads the environment.
    unsafe { std::env::set_var("APPA_PROVIDER_JEV_API_URL", "not a url") };
    let contracts = HostedContracts::compile(
        DOCUMENT,
        HostDefaults::new(Duration::from_secs(5), 65_536),
        appa_adapter_claude_code::adapter(),
    )
    .unwrap_or_else(|error| panic!("the document compiles from itself: {error}"));
    let call = ContractCall {
        tool: "mcp/web/fetch".to_string(),
        arguments: "{}".to_string(),
        label: DecisionLabel {
            trust: "trusted".to_string(),
            audience: Vec::new(),
        },
        effects: Vec::new(),
    };
    assert_eq!(
        contracts.check(&call),
        Ok(ContractVerdict::NeedsAnnotator {
            annotator: "classifier".to_string(),
        })
    );
}
