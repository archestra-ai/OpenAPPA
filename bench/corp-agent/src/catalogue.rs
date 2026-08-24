//! What the model is told about each tool.
//!
//! The runtime advertises nothing: a harness owns its tool catalogue, and the
//! policy is where this host reads it from. Each registered contract becomes
//! one wire tool whose parameters are the contract's own normalized rendering
//! — the one schema a model may be shown (`CFG-25`) — and whose description
//! states what the contract does to a label. A model that knows `read_hr`
//! narrows the audience can plan around the narrowing instead of discovering
//! it as a block.

use appa_engine::contract::{AudienceDelta, ToolContract};
use appa_engine::label::Dim;
use appa_engine::registry::TrustChain;
use appa_example_agent::wire::WireTool;
use appa_policy::Config;

/// The spawn tool this deployment offers, and the errand argument it reads.
/// It is an ordinary registered contract: the runtime checks it like any
/// call, and only the prose here tells the model what releasing it means.
pub const FORK: &str = "fork";
pub const ERRAND: &str = "task";

/// Every registered tool as the model sees it. `forking` is off for the
/// no-branching arm: a tool the host will not act on must not be advertised,
/// or the model spends rounds proposing a call that can only fail.
pub fn advertised(config: &Config, forking: bool) -> Vec<WireTool> {
    let registry = config.registry_config();
    registry
        .tools
        .iter()
        .filter(|contract| forking || contract.name.as_str() != FORK)
        .map(|contract| schema(contract, &registry.trust_chain))
        .collect()
}

fn schema(contract: &ToolContract, chain: &TrustChain) -> WireTool {
    WireTool::new(
        contract.name.as_str(),
        match contract.name.as_str() {
            FORK => fork_description(contract, chain),
            _ => contract_description(contract, chain),
        },
        contract.parameters.normalized(),
    )
}

/// The spawn tool carries the one piece of advice the control tool cannot:
/// what to do instead of accepting a narrowing. Only a host that has a spawn
/// tool can offer that escape, so only a host says it.
fn fork_description(contract: &ToolContract, chain: &TrustChain) -> String {
    format!(
        "Run one self-contained task in a child trajectory. Scope the child to the restrictive \
         read and the work that must sit beside it: every later call in that child runs under the \
         label the read narrowed to, so a write your current label still permits belongs here, \
         issued once the child returns. Prefer this over accepting a narrowing whenever later \
         work needs your current label. A child inherits your label and can never widen it. The \
         child's final message is its return, and it crosses checked — a child that did the work \
         itself should finish by saying nothing. {}",
        contract_description(contract, chain)
    )
}

/// One contract as clauses: what the output is labelled, what the call
/// demands, and what it commits.
fn contract_description(contract: &ToolContract, chain: &TrustChain) -> String {
    let mut clauses = Vec::new();
    match &contract.delta {
        None => clauses.push("output label is unknown".to_string()),
        Some(delta) => {
            match &delta.trust {
                Some(Dim::Known(trust)) => clauses.push(format!(
                    "output trust={}",
                    chain
                        .name_of(*trust)
                        .expect("validated tool trust rank is in the chain")
                )),
                Some(Dim::Unknown) => clauses.push("output trust=unknown".to_string()),
                None => {}
            }
            match &delta.audience {
                Some(AudienceDelta::Static(audience)) => clauses.push(format!("output audience={audience:?}")),
                Some(AudienceDelta::PendingCast) => clauses.push("output audience=unknown".to_string()),
                None => {}
            }
            if delta.is_none() {
                clauses.push("output label is neutral".to_string());
            }
        }
    }
    for uses in &contract.uses {
        let read = match uses.inputs.is_empty() {
            true => "all arguments".to_string(),
            false => {
                let sources: Vec<String> = uses.inputs.values().map(|source| source.spelling()).collect();
                sources.join(", ")
            }
        };
        let returned: Vec<&str> = uses
            .returns
            .iter()
            .map(|field| match field {
                appa_engine::contract::ResolverReturn::Trust => "output trust",
                appa_engine::contract::ResolverReturn::Audience => "output audience",
                appa_engine::contract::ResolverReturn::RequiredTrust => "a required trust floor",
                appa_engine::contract::ResolverReturn::RequiredAudience => "required recipients",
                appa_engine::contract::ResolverReturn::Attention => "review marks",
            })
            .collect();
        clauses.push(format!(
            "resolver {} classifies {read} into {}",
            uses.resolver.as_str(),
            returned.join(", ")
        ));
    }
    if let Some(trust) = contract.requires.label.trust_floor {
        clauses.push(format!(
            "requires trust>={}",
            chain
                .name_of(trust)
                .expect("validated requirement trust rank is in the chain")
        ));
    }
    if !contract.requires.label.audience.is_empty() {
        clauses.push(format!("audience requirements={:?}", contract.requires.label.audience));
    }
    if !contract.requires.history.is_empty() {
        clauses.push(format!("history requirements={:?}", contract.requires.history));
    }
    if !contract.emits.is_empty() {
        clauses.push(format!(
            "effects=[{}]",
            contract
                .emits
                .iter()
                .map(|effect| effect.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    format!("APPA contract: {}.", clauses.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy proper, as the dialect takes it — the `[policy]` table of a
    /// deployment file, unwrapped.
    const POLICY: &str = r#"
version = 1
trust_chain = ["suspicious", "internal"]

[[tool]]
name = "read_hr"
parameters = { type = "object", properties = { file = { type = "string" } }, required = ["file"], additionalProperties = false }
delta = { audience = ["hr"] }

[[tool]]
name = "fork"
parameters = { type = "object", properties = { task = { type = "string" } }, required = ["task"], additionalProperties = false }
"#;

    fn config() -> Config {
        Config::from_toml_str(POLICY).expect("the fixture policy parses")
    }

    /// The schema a model may be shown is the contract's own (`CFG-25`), so
    /// a host cannot advertise a shape the engine will not accept.
    #[test]
    fn each_tool_carries_the_contracts_own_parameters() {
        let config = config();
        let advertised = advertised(&config, true);
        let read_hr = advertised
            .iter()
            .find(|tool| tool.function.name == "read_hr")
            .expect("read_hr is advertised");
        assert_eq!(
            read_hr.function.parameters,
            Some(
                config
                    .registry_config()
                    .tools
                    .iter()
                    .find(|contract| contract.name.as_str() == "read_hr")
                    .expect("read_hr is registered")
                    .parameters
                    .normalized()
            ),
        );
    }

    /// The no-branching arm runs the same policy; what changes is what the
    /// model is offered.
    #[test]
    fn the_spawn_tool_is_advertised_only_when_the_host_will_act_on_it() {
        let config = config();
        let names = |forking| {
            advertised(&config, forking)
                .into_iter()
                .map(|tool| tool.function.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(true), vec!["read_hr".to_string(), FORK.to_string()]);
        assert_eq!(names(false), vec!["read_hr".to_string()]);
    }
}
