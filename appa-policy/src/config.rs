use std::collections::{BTreeMap, BTreeSet};

use appa_engine::audience::SourceRegistration;
use appa_engine::contract::ToolDeclaration;
use appa_engine::engine::Engine;
use appa_engine::label::Label;
use appa_engine::names::AnnotatorName;
use appa_engine::profile::{DeploymentPolicy, PolicyDialectVersion, ProfileDeclaration};
use appa_engine::registry::{AudienceVocabulary, PlannerCap, RegistryConfig, TrustChain};

use crate::annotator::{AnnotatorBinding, compile_annotators, validate_annotator_inputs};
use crate::audience::convert_audience;
use crate::convert::{default_boundary_label, split_selector};
use crate::error::ConfigError;
use crate::raw::{RawConfig, RawDeployment, RawTool};

pub(crate) const SUPPORTED_VERSION: u32 = 2;
const DEFAULT_TRUST_CHAIN: [&str; 2] = ["suspicious", "trusted"];

/// A fully parsed and **fully validated** policy: the opened [`Engine`] — registry, deployment
/// profile, and policy identity behind the one validated constructor — plus the
/// normalized declarations. The runtime owns HTTP and command bindings; an Annotator that
/// carries a stock builtin names it on its own declaration.
#[derive(Clone, Debug)]
pub struct Config {
    engine: Engine,
    registry_config: RegistryConfig,
    boundary_label: Label,
    /// Every registered `[[annotator]]`, with its runtime-owned hint, builtin, and input mapping.
    annotators: BTreeMap<AnnotatorName, AnnotatorBinding>,
    /// Every tool a `[deployment]` field names, as authored, with the field it was named in.
    deployment_tools: Vec<(&'static str, String)>,
}

impl Config {
    /// Parse the policy TOML. HTTP and command bindings remain deployment-owned; a stock
    /// annotator builtin is selected on the declaration that carries it.
    pub fn from_toml_str(s: &str) -> Result<Config, ConfigError> {
        Config::from_toml_str_routed(s, BTreeMap::new(), Vec::new())
    }

    /// [`Config::from_toml_str`] under the deployment's audience bindings. `sources` are the
    /// selector templates each bound provider declares; a `[audience]` selector must match
    /// one, and only providers a selector references enter the policy identity.
    /// `lookup_targets` names, per audience provider whose member lookups the deployment
    /// redirects, the entry that answers them, so every qualified member such a provider
    /// reports is looked up there before it seats. Routing is the deployment's, not the
    /// policy's, and stays out of the policy identity.
    pub fn from_toml_str_routed(
        s: &str,
        lookup_targets: BTreeMap<String, String>,
        sources: Vec<SourceRegistration>,
    ) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(s)?;
        if raw.version != SUPPORTED_VERSION {
            return Err(ConfigError::UnsupportedVersion { found: raw.version });
        }

        let trust_chain = match raw.trust_chain {
            Some(ranks) => TrustChain::new(ranks),
            None => TrustChain::new(DEFAULT_TRUST_CHAIN.iter().map(|r| r.to_string()).collect()),
        };
        // Validate the chain up front so `parse_trust` never truncates a rank index into a u8.
        trust_chain.validate()?;

        let boundary_label = match raw.boundary {
            Some(b) => b.convert(&trust_chain)?,
            None => default_boundary_label(&trust_chain),
        };

        let (annotators, annotator_declarations) = compile_annotators(raw.annotator, &trust_chain)?;
        let (mut audience, mut referenced) = convert_audience(raw.audience, sources)?;
        audience.lookup_targets = lookup_targets;
        referenced.extend(
            annotator_declarations
                .iter()
                .filter_map(|annotator| annotator.audiences.as_ref())
                .flat_map(AudienceVocabulary::referenced_providers),
        );
        let (tools, qualified) = compile_tools(raw.tool, &trust_chain)?;
        referenced.extend(
            tools
                .iter()
                .filter_map(ToolDeclaration::declared)
                .flat_map(|annotation| annotation.referenced_providers()),
        );
        audience.sources.retain(|source| referenced.contains(&source.provider));
        validate_annotator_inputs(&tools, &annotators)?;

        let mut authorities = Vec::new();
        for a in raw.authority {
            authorities.push(a.convert(&trust_chain)?);
        }

        let mut sanitizers = Vec::new();
        for s in raw.sanitizer {
            sanitizers.push(s.convert(&trust_chain)?);
        }

        let planner_cap = match raw.limits.as_ref().and_then(|l| l.planner_cap) {
            None => PlannerCap::default(),
            Some(cap) => PlannerCap::new(cap).ok_or(ConfigError::ZeroPlannerCap)?,
        };

        let (profile, deployment_tools) = match raw.deployment {
            Some(mut deployment) => {
                qualify_deployment_names(&mut deployment, &qualified);
                let named = deployment.tool_names();
                (deployment.convert(&trust_chain)?, named)
            }
            None => (ProfileDeclaration::no_coverage(&trust_chain), Vec::new()),
        };

        let registry_config = RegistryConfig {
            trust_chain,
            tools,
            annotators: annotator_declarations,
            authorities,
            sanitizers,
            audience,
        };
        let engine = Engine::open(DeploymentPolicy {
            registry: registry_config.clone(),
            planner_cap,
            dialect: PolicyDialectVersion::new(SUPPORTED_VERSION),
            profile,
        })?;

        let boundary_atoms: Vec<_> = boundary_label.audience.symbolic_atoms().into_iter().collect();
        engine
            .registry()
            .audience()
            .needed_primitives(&boundary_atoms, None)
            .map_err(|fault| ConfigError::BadAudience {
                context: "boundary audience".to_string(),
                reason: fault.to_string(),
            })?;

        Ok(Config {
            engine,
            registry_config,
            boundary_label,
            annotators,
            deployment_tools,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The label assigned to every north user turn (RP1) — a server policy default, never client
    /// supplied. Defaults to the neutral `L0`: top trust (the user is the trust boundary), public.
    pub fn boundary_label(&self) -> &Label {
        &self.boundary_label
    }

    pub fn registry_config(&self) -> &RegistryConfig {
        &self.registry_config
    }

    /// Every tool a `[deployment]` field names, as authored, paired with the field's
    /// spelling. Each name is matched against a tool identity exactly, so a deployment
    /// that requires a naming convention checks these beside the contracts.
    pub fn deployment_tool_names(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.deployment_tools
            .iter()
            .map(|(field, name)| (*field, name.as_str()))
    }

    /// Every `[[annotator]]` with its runtime-owned binding: its hint, the stock builtin it names
    /// on its declaration, if any, and its consult input mapping. An Annotator naming a builtin
    /// takes no deployment binding; every other Annotator is bound by name under
    /// `[externals.annotators]`.
    pub fn annotators(&self) -> impl Iterator<Item = (&AnnotatorName, &AnnotatorBinding)> {
        self.annotators.iter()
    }
}

/// Every bare tool identity each authored bare name qualifies to, so a `[deployment]` field
/// can name a tool as the policy wrote it.
type QualifiedNames = BTreeMap<String, BTreeSet<String>>;

fn compile_tools(
    raw: Vec<RawTool>,
    trust_chain: &TrustChain,
) -> Result<(Vec<ToolDeclaration>, QualifiedNames), ConfigError> {
    let mut tools = Vec::new();
    let mut qualified = QualifiedNames::new();
    for t in raw {
        let authored = split_selector(&t.name).0.to_string();
        let tool = t.convert(trust_chain)?;
        qualified
            .entry(authored)
            .or_default()
            .insert(split_selector(tool.name().as_str()).0.to_string());
        tools.push(tool);
    }
    Ok((tools, qualified))
}

fn qualify_deployment_names(deployment: &mut RawDeployment, qualified: &QualifiedNames) {
    for names in [
        &mut deployment.confined_results,
        &mut deployment.assumed_tools,
        &mut deployment.provider_run_tools,
    ] {
        *names = std::mem::take(names)
            .into_iter()
            .flat_map(|name| {
                qualified
                    .get(&name)
                    .map(|names| names.iter().cloned().collect())
                    .unwrap_or_else(|| vec![name])
            })
            .collect();
    }
}
