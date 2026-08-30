//! What the model is told about each tool.
//!
//! The runtime advertises nothing: a harness owns its tool catalogue, and the
//! policy is where this host reads it from. Each registered contract becomes
//! one wire tool whose parameters are the contract's own normalized rendering
//! — the one schema a model may be shown (`CFG-25`) — and whose description
//! states what the contract does to a label. A model that knows `read_hr`
//! narrows the audience can plan around the narrowing instead of discovering
//! it as a block.

use appa_engine::contract::{AudienceDelta, ToolDeclaration};
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
        .filter(|declaration| forking || declaration.name().as_str() != FORK)
        .map(|declaration| schema(declaration, &registry.trust_chain))
        .collect()
}

fn schema(declaration: &ToolDeclaration, chain: &TrustChain) -> WireTool {
    WireTool::new(
        declaration.name().as_str(),
        match declaration.name().as_str() {
            FORK => fork_description(declaration, chain),
            _ => contract_description(declaration, chain),
        },
        declaration.parameters().normalized(),
    )
}

/// The spawn tool carries the one piece of advice the control tool cannot:
/// what to do instead of accepting a narrowing. Only a host that has a spawn
/// tool can offer that escape, so only a host says it.
fn fork_description(declaration: &ToolDeclaration, chain: &TrustChain) -> String {
    format!(
        "Run one self-contained task in a child trajectory. Scope the child to the restrictive \
         read and the work that must sit beside it: every later call in that child runs under the \
         label the read narrowed to, so a write your current label still permits belongs here, \
         issued once the child returns. Prefer this over accepting a narrowing whenever later \
         work needs your current label. A child inherits your label and can never widen it. The \
         child's final message is its return, and it crosses checked — a child that did the work \
         itself should finish by saying nothing. {}",
        contract_description(declaration, chain)
    )
}

/// One contract as clauses: what the output is labelled, what the call
/// demands, and what it commits.
fn contract_description(declaration: &ToolDeclaration, chain: &TrustChain) -> String {
    let contract = match declaration {
        ToolDeclaration::Declared(contract) => contract,
        ToolDeclaration::Annotated { annotator, .. } => {
            return format!(
                "APPA contract: annotator {} produces this call's complete contract — output \
                 labels, requirements, and effects — from the call itself.",
                annotator.as_str()
            );
        }
    };
    let mut clauses = Vec::new();
    let delta = &contract.delta;
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
    match contract.requires.label.trust_floor.as_ref() {
        Some(Dim::Known(trust)) => clauses.push(format!(
            "requires trust>={}",
            chain
                .name_of(*trust)
                .expect("validated requirement trust rank is in the chain")
        )),
        Some(Dim::Unknown) => clauses.push("requires trust=unknown".to_string()),
        None => {}
    }
    match &contract.requires.label.audience {
        Dim::Known(requirements) if requirements.is_empty() => {}
        Dim::Known(requirements) => clauses.push(format!("audience requirements={requirements:?}")),
        Dim::Unknown => clauses.push("audience requirements=unknown".to_string()),
    }
    match &contract.requires.attention {
        Dim::Known(marks) if marks.is_empty() => {}
        Dim::Known(marks) => clauses.push(format!(
            "requires review marks=[{}]",
            marks.iter().map(|mark| mark.as_str()).collect::<Vec<_>>().join(",")
        )),
        Dim::Unknown => clauses.push("requires review marks=unknown".to_string()),
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
                    .find(|declaration| declaration.name().as_str() == "read_hr")
                    .expect("read_hr is registered")
                    .parameters()
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
