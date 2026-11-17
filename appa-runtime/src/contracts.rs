//! A hosted policy document's tool contracts, compiled as a deployment of the document would
//! compile them and never served: compared with another document's
//! ([`HostedContracts::diff`]), or checked against one call at a given label
//! ([`HostedContracts::check`]). Neither opens a store, reads a secret or consults anything.

use std::collections::BTreeSet;

use appa_engine::check::ContractVerdict as EngineVerdict;
pub use appa_engine::diff::{ChangeKind, ContractChange, Direction, ToolContractDiff};
use appa_engine::fact::EffectKind;
use appa_engine::label::{Audience, Clause, DeclaredAudience, Label};
use appa_engine::value::ToolName;
use appa_runtime_api::{Adapter, DecisionGap, DecisionLabel};

use crate::api::OpenError;
use crate::config::{Config, HostDefaults, IncludeResolution};

/// One hosted document's compiled contracts, and the deployment bindings it writes.
pub struct HostedContracts {
    policy: appa_policy::Config,
    externals: Option<toml::Value>,
}

/// One call as a recorded decision describes it: the canonical tool, its arguments as JSON
/// text, the label the trajectory stood at, and the effects it had committed.
pub struct ContractCall {
    pub tool: String,
    pub arguments: String,
    pub label: DecisionLabel,
    pub effects: Vec<String>,
}

/// What the contracts alone say of a [`ContractCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractVerdict {
    /// No requirement is missed. A narrowing alone is not a denial: the agent accepts it.
    Allow,
    Deny {
        gaps: Vec<DecisionGap>,
    },
    /// An Annotator produces the contract per call, and the floor beside it is met.
    NeedsAnnotator {
        annotator: String,
    },
    /// Deciding an audience comparison needs these audiences' members from their source.
    NeedsMembership {
        audiences: Vec<String>,
    },
    /// The policy refuses the call before any check: no contract covers the tool, or the
    /// arguments do not fit its parameters.
    Refused {
        detail: String,
    },
}

/// Why a [`ContractCall`]'s label does not read under the policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractCallError {
    #[error("trust {0:?} is no rank of the policy's trust chain")]
    Trust(String),
    #[error("audience clause {clause:?} does not read: {reason}")]
    Audience { clause: Vec<String>, reason: String },
}

impl HostedContracts {
    /// Compile a host-composed document, with its includes already resolved, under `adapter`.
    pub fn compile(document: &str, defaults: HostDefaults, adapter: Adapter) -> Result<HostedContracts, OpenError> {
        let config = Config::hosted_included_deferred(document, defaults, |_| Err(IncludeResolution::Unknown))
            .map_err(OpenError::Config)?;
        let policy = crate::api::compile_policy(&config, crate::api::ToolNaming::Canonical { adapter })?;
        let externals = document
            .parse::<toml::Table>()
            .map_err(|error| OpenError::UnsupportedPolicy(error.to_string()))?
            .remove("externals");
        Ok(HostedContracts { policy, externals })
    }

    /// Every tool whose contracts differ from `self` in `after`, and which way. A difference in
    /// how either document binds its external components, or in an Annotator's instruction,
    /// builtin or inputs, is reported on the `*` entry as unknown.
    pub fn diff(&self, after: &HostedContracts) -> Vec<ToolContractDiff> {
        let outside: Vec<ChangeKind> = [
            (self.externals != after.externals).then_some(ChangeKind::Bindings),
            (!self.policy.annotators().eq(after.policy.annotators())).then_some(ChangeKind::Annotator),
        ]
        .into_iter()
        .flatten()
        .collect();
        appa_engine::diff::diff_contracts(
            self.policy.engine().registry(),
            after.policy.engine().registry(),
            &outside,
        )
    }

    /// The verdict the contracts give `call`, deterministic in the document and the call.
    pub fn check(&self, call: &ContractCall) -> Result<ContractVerdict, ContractCallError> {
        let engine = self.policy.engine();
        let chain = engine.registry().trust_chain();
        let current = self.label(&call.label)?;
        let committed: BTreeSet<EffectKind> = call.effects.iter().map(EffectKind::new).collect();
        let verdict = engine.check_contract(
            ToolName::new(call.tool.clone()),
            call.arguments.as_bytes(),
            &current,
            &committed,
        );
        Ok(match verdict {
            Err(refusal) => ContractVerdict::Refused {
                detail: refusal.to_string(),
            },
            Ok(EngineVerdict::Allow) => ContractVerdict::Allow,
            Ok(EngineVerdict::Deny { gaps }) => ContractVerdict::Deny {
                gaps: gaps.iter().map(|gap| crate::engine::decision_gap(gap, chain)).collect(),
            },
            Ok(EngineVerdict::NeedsAnnotator { annotator }) => ContractVerdict::NeedsAnnotator {
                annotator: annotator.as_str().to_string(),
            },
            Ok(EngineVerdict::NeedsMembership { atoms }) => ContractVerdict::NeedsMembership {
                audiences: atoms.iter().map(ToString::to_string).collect(),
            },
        })
    }

    /// A label spelled as decisions record it ([`crate::engine::decision_label`]), read back
    /// under this policy's trust chain.
    fn label(&self, spelled: &DecisionLabel) -> Result<Label, ContractCallError> {
        let chain = self.policy.engine().registry().trust_chain();
        let trust = chain
            .rank_of(&spelled.trust)
            .ok_or_else(|| ContractCallError::Trust(spelled.trust.clone()))?;
        let mut clauses = Vec::with_capacity(spelled.audience.len());
        for entries in &spelled.audience {
            match DeclaredAudience::parse_entries(entries) {
                Ok(DeclaredAudience::Union(clause)) => clauses.push(clause),
                Ok(DeclaredAudience::Public) => {}
                Err(_) if entries.is_empty() => {
                    clauses.push(Clause::new([], [], []).expect("an empty clause names no reader"))
                }
                Err(reason) => {
                    return Err(ContractCallError::Audience {
                        clause: entries.clone(),
                        reason: reason.to_string(),
                    });
                }
            }
        }
        Ok(Label::new(trust, Audience::of_clauses(clauses)))
    }
}
