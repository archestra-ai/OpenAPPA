//! What the model is told about each tool.
//!
//! The runtime advertises nothing: a harness owns its tool catalogue,
//! so this host builds it from the policy
//! the visitor submitted. Each registered contract becomes one wire tool
//! whose parameters are the contract's own normalized rendering — the one
//! schema a model may be shown — and whose description states
//! what the contract does to a label. A model that knows `list_customers`
//! narrows the audience can plan around the narrowing instead of
//! discovering it as a block.
//!
//! The corporate demo builds its catalogue the same way and does not share
//! this code. Each host owns what it tells its model, and the shared piece
//! would have to be an engine-typed helper on the harness library, which is
//! the one thing the runtime's API boundary keeps out.

use appa_engine::contract::ToolDeclaration;
use appa_engine::registry::TrustChain;
use appa_example_agent::wire::WireTool;
use appa_policy::Config;

pub fn advertised(config: &Config) -> Vec<WireTool> {
    let registry = config.registry_config();
    registry
        .tools
        .iter()
        .map(|declaration| {
            WireTool::new(
                declaration.name().as_str(),
                describe(declaration, &registry.trust_chain),
                declaration.parameters().normalized(),
            )
        })
        .collect()
}

fn describe(declaration: &ToolDeclaration, chain: &TrustChain) -> String {
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
    if let Some(trust) = &delta.trust {
        clauses.push(format!(
            "output trust={}",
            chain
                .name_of(*trust)
                .expect("validated tool trust rank is in the chain")
        ));
    }
    if let Some(audience) = &delta.audience {
        clauses.push(format!("output audience={audience:?}"));
    }
    if delta.is_none() {
        clauses.push("output label is neutral".to_string());
    }
    if let Some(trust) = contract.requires.label.trust_floor.as_ref() {
        clauses.push(format!(
            "requires trust>={}",
            chain
                .name_of(*trust)
                .expect("validated requirement trust rank is in the chain")
        ));
    }
    if !contract.requires.label.audience.is_empty() {
        clauses.push(format!("audience requirements={:?}", contract.requires.label.audience));
    }
    if !contract.requires.attention.is_empty() {
        clauses.push(format!(
            "requires review marks=[{}]",
            contract
                .requires
                .attention
                .iter()
                .map(|mark| mark.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
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

    use std::collections::BTreeSet;

    use crate::lint::check_policy;
    use crate::systems::System;

    #[test]
    fn every_tool_is_advertised_with_the_contracts_own_parameters() {
        let all: BTreeSet<System> = System::ALL.into_iter().collect();
        let checked = check_policy(include_str!("../policies/default.toml"), &all).expect("the preset loads");
        let advertised = advertised(&checked.config);
        assert_eq!(advertised.len(), checked.tool_count);
        let registry = checked.config.registry_config();
        for tool in &advertised {
            let declaration = registry
                .tools
                .iter()
                .find(|declaration| declaration.name().as_str() == tool.function.name)
                .expect("every advertised tool is registered");
            assert_eq!(tool.function.parameters, Some(declaration.parameters().normalized()));
        }
    }
}
