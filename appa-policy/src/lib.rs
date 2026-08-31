//! The spec's policy-dialect compiler: the configuration dialect (TOML) → the engine's
//! [`RegistryConfig`] for the runtime.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use thiserror::Error;

use appa_engine::audience::{
    AudienceConfig, IdentityImplementation, NamedAudience, SelectorSpec, SelectorTemplate, SourceRegistration,
};
use appa_engine::authority::{Authority, DeclaredTransition, Hint, Mandate, Sanitizer, SanitizerPoints, Scope};
use appa_engine::contract::{
    AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolAnnotation,
    ToolDeclaration,
};
use appa_engine::engine::Engine;
use appa_engine::fact::ReturnPolicy;
use appa_engine::fact::{EffectKind, EffectSet};
use appa_engine::label::{Audience, ChainAudience, Clause, DeclaredAudience, GroupRef, Label, ReaderId, Trust};
use appa_engine::names::{
    AnnotatorName, AuthorityName, GroupName, IdentityImplementationName, MarkName, SanitizerName, SurfaceName, TagName,
};
use appa_engine::params::ToolParameters;
use appa_engine::profile::{
    BindingMode, DeploymentPolicy, ExecutorClass, PolicyDialectVersion, ProfileDeclaration, SurfaceMode,
    neutral_starting_label,
};
use appa_engine::registry::{AnnotatorDeclaration, LoadError, PlannerCap, Registry, RegistryConfig, TrustChain};
use appa_engine::value::ToolName;

const SUPPORTED_VERSION: u32 = 2;
const DEFAULT_TRUST_CHAIN: [&str; 2] = ["suspicious", "trusted"];

/// Why a policy file failed to load. Structural failures (unknown key, missing field, wrong type,
/// bare operator-less list) surface as [`ConfigError::Parse`]; the rest are semantic conversions.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("malformed TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported config version {found} (this build supports {SUPPORTED_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("unknown trust rank {name:?} in {context} (not in the trust chain)")]
    UnknownTrustRank { name: String, context: String },
    #[error("bad reader set in {context}: {reason}")]
    BadAudience { context: String, reason: String },
    #[error("bad sanitizer point {token:?}: expected \"tool_input\" or \"tool_output\"")]
    UnknownSanitizerPoint { token: String },
    #[error("sanitizer {name} declares no application point (`on` is empty)")]
    NoSanitizerPoint { name: String },
    #[error("sanitizer {name} permits: {reason}")]
    SanitizerMandateShape { name: String, reason: &'static str },
    #[error("{kind} {name}: {reason}")]
    BadImplementation {
        kind: &'static str,
        name: String,
        reason: String,
    },
    #[error(
        "{kind} {name} binds an implementation inline; this runtime binds implementations outside the policy, so the policy must not carry the binding"
    )]
    ForbiddenInlineBinding { kind: &'static str, name: String },
    #[error("tool {tool}: {source}")]
    ToolParameters {
        tool: String,
        source: appa_engine::params::ParamsError,
    },
    #[error("tool {tool} declares effect {kind:?} twice — `effects` is a set")]
    DuplicateEffect { tool: String, kind: String },
    #[error("annotator name {0:?} is empty")]
    BadAnnotatorName(String),
    #[error(
        "annotator {name} names unknown builtin {builtin:?}; the stock annotator builtins are \"claude-code\" and \"llm\""
    )]
    UnknownAnnotatorBuiltin { name: String, builtin: String },
    #[error(
        "annotator {annotator} input {input} reads {spelling:?}, which is not a tool-call value: an input reads `$tool_call`, `$tool_call.name`, `$tool_call.description`, `$tool_call.arguments`, or `$tool_call.arguments.<name>`"
    )]
    UnknownCallSource {
        annotator: String,
        input: String,
        spelling: String,
    },
    #[error(
        "tool {tool} names annotator {annotator} and also declares `{field}`; `annotator` replaces the static semantic fields"
    )]
    AnnotatorWithStatics {
        tool: String,
        annotator: String,
        field: &'static str,
    },
    #[error("tool {tool}: annotator {annotator} input {input} reads {reads}, {reason}")]
    AnnotatorInput {
        tool: String,
        annotator: String,
        input: String,
        reads: String,
        reason: String,
    },
    #[error("[limits] planner_cap is 0: a tool's worst case is at least one plan, so a zero cap refuses every tool")]
    ZeroPlannerCap,
    #[error("[deployment] {field}: expected one of {expected}, found {found:?}")]
    BadDeploymentToken {
        field: &'static str,
        expected: &'static str,
        found: String,
    },
    #[error("[deployment] names tool {tool} in both assumed_tools and provider_run_tools")]
    ConflictingExecutorException { tool: String },
    #[error("bad audience source in {context}: {selector:?} {reason}")]
    BadAudienceSource {
        context: String,
        selector: String,
        reason: String,
    },
    #[error("bad named audience {name:?}: {reason}")]
    BadNamedAudience { name: String, reason: String },
    #[error("[identity] implementation {name:?}: {reason}")]
    BadIdentity { name: String, reason: String },
    #[error("registry rejected: {0}")]
    Registry(#[from] LoadError),
}

/// The audience-source catalog the stock batteries register: one provider per battery, its
/// selector templates fixed by this build. A policy's `from` selectors pick collections out
/// of it, and only providers the policy references enter its identity. `viewer` names the
/// requesting principal and feeds `self`; the members collections can feed `internal`; the
/// named collections (and members collections) can feed `[[audience.group]]`.
pub fn stock_audience_sources() -> Vec<SourceRegistration> {
    let source = |provider: &str, templates: &[&str]| SourceRegistration {
        provider: provider.to_string(),
        templates: templates
            .iter()
            .map(|template| SelectorTemplate::new(*template))
            .collect(),
    };
    vec![
        source("google-workspace", &["viewer", "full-members", "group/<group-address>"]),
        source("slack", &["viewer", "full-members", "user-group/<handle>"]),
        source("github", &["viewer", "org/<org>/members", "org/<org>/team/<team>"]),
    ]
}

/// What one catalog collection may feed: `self` (the requesting principal), `internal`
/// (a provider's full membership, or one explicitly selected GitHub organization), or a
/// named audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionRole {
    Viewer,
    Members,
    Named,
}

fn collection_role(spec: &SelectorSpec) -> Option<CollectionRole> {
    let template = stock_audience_sources()
        .into_iter()
        .find(|source| source.provider == spec.provider)?
        .templates
        .into_iter()
        .find(|template| template.matches(&spec.selector))?;
    Some(match template.as_str() {
        "viewer" => CollectionRole::Viewer,
        "full-members" | "org/<org>/members" => CollectionRole::Members,
        _ => CollectionRole::Named,
    })
}

/// The stock model transports an `[[annotator]]` may name on its declaration with `builtin`.
/// Closed: the runtime compiles both in. An Annotator that names one takes no deployment
/// binding; every other Annotator is bound by the deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotatorBuiltin {
    ClaudeCode,
    Llm,
}

impl AnnotatorBuiltin {
    pub const ALL: [AnnotatorBuiltin; 2] = [AnnotatorBuiltin::ClaudeCode, AnnotatorBuiltin::Llm];

    /// The name a policy writes: `claude-code` or `llm`.
    pub fn wire_name(self) -> &'static str {
        match self {
            AnnotatorBuiltin::ClaudeCode => "claude-code",
            AnnotatorBuiltin::Llm => "llm",
        }
    }

    fn parse(name: &str) -> Option<AnnotatorBuiltin> {
        AnnotatorBuiltin::ALL
            .into_iter()
            .find(|builtin| builtin.wire_name() == name)
    }
}

/// `$tool_call` is the only input source an `[[annotator]]` reads, and these are its five
/// forms. The mapping is policy syntax the runtime executes when it builds a consult
/// artifact; the engine never sees it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolCallSource {
    /// `$tool_call` — the complete call: its name, its description when the tool declares one,
    /// and its arguments.
    Call,
    /// `$tool_call.name`
    Name,
    /// `$tool_call.description`
    Description,
    /// `$tool_call.arguments` — the complete argument object.
    Arguments,
    /// `$tool_call.arguments.<name>` — one top-level argument.
    Argument(String),
}

impl ToolCallSource {
    pub fn parse(spelling: &str) -> Option<ToolCallSource> {
        match spelling {
            "$tool_call" => Some(ToolCallSource::Call),
            "$tool_call.name" => Some(ToolCallSource::Name),
            "$tool_call.description" => Some(ToolCallSource::Description),
            "$tool_call.arguments" => Some(ToolCallSource::Arguments),
            // One top-level argument only: an empty name and a nested path are both outside
            // the five forms, and neither has a value the schema can pin.
            _ => spelling
                .strip_prefix("$tool_call.arguments.")
                .filter(|name| !name.is_empty() && !name.contains('.'))
                .map(|name| ToolCallSource::Argument(name.to_string())),
        }
    }

    pub fn spelling(&self) -> String {
        match self {
            ToolCallSource::Call => "$tool_call".to_string(),
            ToolCallSource::Name => "$tool_call.name".to_string(),
            ToolCallSource::Description => "$tool_call.description".to_string(),
            ToolCallSource::Arguments => "$tool_call.arguments".to_string(),
            ToolCallSource::Argument(argument) => format!("$tool_call.arguments.{argument}"),
        }
    }
}

/// One registered `[[annotator]]` as the runtime consumes it: the stock builtin it names, if
/// any, and the input mapping its consult artifacts carry. An empty mapping sends the
/// complete call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatorBinding {
    pub builtin: Option<AnnotatorBuiltin>,
    pub inputs: BTreeMap<String, ToolCallSource>,
}

/// A fully parsed and **fully validated** policy: the opened [`Engine`] — registry, deployment
/// profile, and policy identity behind the one validated constructor — plus the
/// normalized declarations. The runtime owns HTTP and command bindings; an Annotator that
/// carries a stock builtin names it on its own declaration.
#[derive(Clone, Debug)]
pub struct Config {
    engine: Engine,
    registry_config: RegistryConfig,
    boundary_label: Label,
    /// Every registered `[[annotator]]`, with its runtime-owned builtin and input mapping.
    annotators: BTreeMap<AnnotatorName, AnnotatorBinding>,
}

impl Config {
    /// Parse the policy TOML. HTTP and command bindings remain deployment-owned; a stock
    /// annotator builtin is selected on the declaration that carries it.
    pub fn from_toml_str(s: &str) -> Result<Config, ConfigError> {
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

        let mut annotators: BTreeMap<AnnotatorName, AnnotatorBinding> = BTreeMap::new();
        // A duplicate name reaches the registry (the Vec keeps both) and is refused there.
        let mut annotator_declarations = Vec::new();
        for annotator in raw.annotator {
            if annotator.name.is_empty() {
                return Err(ConfigError::BadAnnotatorName(annotator.name));
            }
            let name = AnnotatorName::new(annotator.name);
            if annotator.implementation.is_some() {
                return Err(ConfigError::ForbiddenInlineBinding {
                    kind: "annotator",
                    name: name.as_str().to_string(),
                });
            }
            let builtin = match annotator.builtin {
                Some(builtin) => match AnnotatorBuiltin::parse(&builtin) {
                    Some(builtin) => Some(builtin),
                    None => {
                        return Err(ConfigError::UnknownAnnotatorBuiltin {
                            name: name.as_str().to_string(),
                            builtin,
                        });
                    }
                },
                None => None,
            };
            let mut inputs = BTreeMap::new();
            for (input, spelling) in annotator.inputs.unwrap_or_default() {
                let Some(source) = ToolCallSource::parse(&spelling) else {
                    return Err(ConfigError::UnknownCallSource {
                        annotator: name.as_str().to_string(),
                        input,
                        spelling,
                    });
                };
                inputs.insert(input, source);
            }
            let ctx = || format!("annotator {}", name.as_str());
            annotator_declarations.push(AnnotatorDeclaration {
                name: name.clone(),
                trust: annotator
                    .ranks
                    .map(|ranks| {
                        ranks
                            .iter()
                            .map(|rank| parse_trust(rank, &trust_chain, &ctx()))
                            .collect()
                    })
                    .transpose()?,
                audiences: annotator
                    .audiences
                    .map(|readers| parse_annotator_readers(&readers, &ctx()))
                    .transpose()?,
                marks: annotator
                    .marks
                    .map(|marks| marks.into_iter().map(MarkName::new).collect()),
                effects: annotator
                    .effects
                    .map(|effects| effects.into_iter().map(EffectKind::new).collect()),
            });
            annotators.insert(name, AnnotatorBinding { builtin, inputs });
        }
        let audience = convert_audience(raw.audience, raw.identity)?;
        let mut tools = Vec::new();
        for t in raw.tool {
            tools.push(t.convert(&trust_chain)?);
        }
        // An input mapping is validated against every tool that routes through its Annotator:
        // a mapped argument must be a required top-level property of that tool's schema, and a
        // description read needs a declared description. A tool naming an unregistered
        // annotator is skipped here — the registry refuses it.
        for tool in &tools {
            if let ToolDeclaration::Annotated {
                name,
                description,
                parameters,
                annotator,
                ..
            } = tool
                && let Some(binding) = annotators.get(annotator)
            {
                for (input, source) in &binding.inputs {
                    let refused = match source {
                        ToolCallSource::Argument(argument) => parameters
                            .required_property(argument)
                            .err()
                            .map(|fault| format!("which {fault}")),
                        ToolCallSource::Description if description.is_none() => {
                            Some("but the tool declares no description".to_string())
                        }
                        _ => None,
                    };
                    if let Some(reason) = refused {
                        return Err(ConfigError::AnnotatorInput {
                            tool: name.as_str().to_string(),
                            annotator: annotator.as_str().to_string(),
                            input: input.clone(),
                            reads: format!("{:?}", source.spelling()),
                            reason,
                        });
                    }
                }
            }
        }

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

        let child_return = match raw.child {
            None => ReturnPolicy::Raw,
            Some(RawChild { return_sanitizer: None }) => {
                return Err(ConfigError::BadImplementation {
                    kind: "child",
                    name: "return binding".to_string(),
                    reason: "an empty [child] table binds nothing — configure return_sanitizer".to_string(),
                });
            }
            Some(RawChild {
                return_sanitizer: Some(sanitizer),
            }) => {
                let name = SanitizerName::new(sanitizer);
                match sanitizers.iter().find(|s| s.name == name) {
                    Some(s) if s.on.output => ReturnPolicy::Sanitized(name),
                    Some(_) => {
                        return Err(ConfigError::BadImplementation {
                            kind: "child return_sanitizer",
                            name: name.as_str().to_string(),
                            reason: "not registered for tool output".to_string(),
                        });
                    }
                    None => {
                        return Err(ConfigError::BadImplementation {
                            kind: "child return_sanitizer",
                            name: name.as_str().to_string(),
                            reason: "no such sanitizer".to_string(),
                        });
                    }
                }
            }
        };

        let profile = match raw.deployment {
            Some(deployment) => deployment.convert(&trust_chain)?,
            None => ProfileDeclaration::no_coverage(&trust_chain),
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
            child_return,
            profile,
        })?;

        let boundary_atoms: Vec<_> = boundary_label.audience.symbolic_atoms().into_iter().collect();
        engine
            .registry()
            .audience()
            .needed_primitives(&boundary_atoms)
            .map_err(|fault| ConfigError::BadAudience {
                context: "boundary audience".to_string(),
                reason: fault.to_string(),
            })?;

        Ok(Config {
            engine,
            registry_config,
            boundary_label,
            annotators,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn registry(&self) -> &Registry {
        self.engine.registry()
    }

    /// The label assigned to every north user turn (RP1) — a server policy default, never client
    /// supplied. Defaults to the neutral `L0`: top trust (the user is the trust boundary), public.
    pub fn boundary_label(&self) -> &Label {
        &self.boundary_label
    }

    pub fn registry_config(&self) -> &RegistryConfig {
        &self.registry_config
    }

    /// Every `[[annotator]]` the policy registers — the validated superset of every annotator
    /// name a tool declaration routes through.
    pub fn annotator_names(&self) -> impl Iterator<Item = &AnnotatorName> {
        self.annotators.keys()
    }

    /// Every `[[annotator]]` with its runtime-owned binding: the stock builtin it names on its
    /// declaration, if any, and its consult input mapping. An Annotator naming a builtin takes
    /// no deployment binding; every other Annotator is bound by name under
    /// `[externals.annotators]`.
    pub fn annotators(&self) -> impl Iterator<Item = (&AnnotatorName, &AnnotatorBinding)> {
        self.annotators.iter()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    trust_chain: Option<Vec<String>>,
    boundary: Option<RawBoundary>,
    #[serde(default)]
    tool: Vec<RawTool>,
    #[serde(default)]
    authority: Vec<RawAuthority>,
    #[serde(default)]
    sanitizer: Vec<RawSanitizer>,
    #[serde(default)]
    annotator: Vec<RawAnnotator>,
    audience: Option<RawAudience>,
    identity: Option<RawIdentity>,
    child: Option<RawChild>,
    limits: Option<RawLimits>,
    deployment: Option<RawDeployment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeployment {
    starting_label: Option<RawStartingLabel>,
    binding: Option<BindingMode>,
    context_control: Option<bool>,
    dispatch: Option<ExecutorClass>,
    #[serde(default)]
    assumed_tools: Vec<String>,
    #[serde(default)]
    provider_run_tools: Vec<String>,
    #[serde(default)]
    confined_results: Vec<String>,
    confined_child_return: Option<bool>,
    #[serde(default)]
    provider_surfaces: BTreeMap<String, SurfaceMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStartingLabel {
    trust: Option<String>,
    audience: Option<RawStartingAudience>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawStartingAudience {
    Token(String),
    List(Vec<String>),
}

impl RawDeployment {
    fn convert(self, chain: &TrustChain) -> Result<ProfileDeclaration, ConfigError> {
        let neutral = neutral_starting_label(chain);
        let starting_label = match self.starting_label {
            Some(label) => {
                let trust = match label.trust {
                    Some(name) => parse_trust(&name, chain, "deployment starting_label")?,
                    None => neutral.trust,
                };
                let audience = match label.audience {
                    None => Audience::public(),
                    Some(RawStartingAudience::Token(token)) if token == "public" => Audience::public(),
                    Some(RawStartingAudience::Token(token)) => {
                        return Err(ConfigError::BadDeploymentToken {
                            field: "starting_label audience",
                            expected: r#""public" or [...]"#,
                            found: token,
                        });
                    }
                    Some(RawStartingAudience::List(a)) => parse_audience(&a, "deployment starting_label audience")?,
                };
                Label::new(trust, audience)
            }
            None => neutral,
        };

        let mut executor_exceptions = BTreeMap::new();
        for name in self.assumed_tools {
            executor_exceptions.insert(ToolName::new(name), ExecutorClass::Assumed);
        }
        for name in self.provider_run_tools {
            let tool = ToolName::new(name);
            if executor_exceptions
                .insert(tool.clone(), ExecutorClass::ProviderRun)
                .is_some_and(|previous| previous != ExecutorClass::ProviderRun)
            {
                return Err(ConfigError::ConflictingExecutorException {
                    tool: tool.as_str().to_string(),
                });
            }
        }

        Ok(ProfileDeclaration {
            starting_label,
            context_control: self.context_control.unwrap_or(false),
            dispatch: self.dispatch.unwrap_or(ExecutorClass::Assumed),
            executor_exceptions,
            confined_results: self.confined_results.into_iter().map(ToolName::new).collect(),
            confined_child_return: self.confined_child_return.unwrap_or(false),
            provider_surfaces: self
                .provider_surfaces
                .into_iter()
                .map(|(surface, mode)| (SurfaceName::new(surface), mode))
                .collect(),
            binding: self.binding.unwrap_or(BindingMode::Harness),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    planner_cap: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnnotator {
    name: String,
    implementation: Option<toml::Value>,
    /// The stock model transport this Annotator carries: `"claude-code"` or `"llm"`. An
    /// Annotator without it is bound by the deployment under `[externals.annotators]`.
    builtin: Option<String>,
    /// The consult inputs, each a `$tool_call` source. Omitted means the consult artifact is
    /// the complete tool call.
    inputs: Option<BTreeMap<String, String>>,
    /// The trust ranks a produced annotation may write. Omitted admits every chain rank.
    ranks: Option<Vec<String>>,
    /// The literal readers a produced annotation may name. Omitted admits every reader the
    /// policy writes; `public` is always admissible.
    audiences: Option<Vec<String>>,
    /// The attention marks a produced annotation may require. Omitted admits every declared
    /// mark.
    marks: Option<Vec<String>>,
    /// The effect kinds a produced annotation may emit or require. Omitted admits every
    /// declared kind.
    effects: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudience {
    #[serde(rename = "self")]
    self_from: Option<RawAudienceLevel>,
    internal: Option<RawAudienceLevel>,
    #[serde(default)]
    group: Vec<RawAudienceGroup>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudienceLevel {
    from: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudienceGroup {
    name: String,
    within: Option<String>,
    from: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentity {
    implementation: String,
    url: Option<toml::Value>,
    command: Option<toml::Value>,
}

/// Compile `[audience.*]` and `[identity]` into the engine's audience configuration. Each
/// `from` selector must name a stock collection whose role fits its level; a provider enters
/// the registered sources — and so the policy identity — exactly when some selector picks
/// from it.
fn convert_audience(
    audience: Option<RawAudience>,
    identity: Option<RawIdentity>,
) -> Result<AudienceConfig, ConfigError> {
    let mut config = AudienceConfig::default();
    let mut providers: Vec<String> = Vec::new();
    let mut selectors = |list: &[String],
                         context: &str,
                         admits: &dyn Fn(CollectionRole) -> bool,
                         expected: &str|
     -> Result<Vec<SelectorSpec>, ConfigError> {
        let refused = |selector: &str, reason: String| ConfigError::BadAudienceSource {
            context: context.to_string(),
            selector: selector.to_string(),
            reason,
        };
        let mut specs = Vec::new();
        for entry in list {
            let spec = SelectorSpec::parse(entry)
                .ok_or_else(|| refused(entry, "is not a `<provider>:<selector>` source".to_string()))?;
            let role = collection_role(&spec)
                .ok_or_else(|| refused(entry, "names no collection a stock battery registers".to_string()))?;
            if !admits(role) {
                return Err(refused(entry, format!("cannot feed this audience — {expected}")));
            }
            if !providers.contains(&spec.provider) {
                providers.push(spec.provider.clone());
            }
            specs.push(spec);
        }
        Ok(specs)
    };
    if let Some(audience) = audience {
        if let Some(level) = audience.self_from {
            config.self_from = selectors(
                &level.from,
                "[audience.self]",
                &|role| role == CollectionRole::Viewer,
                "`self` reads only each provider's `viewer`",
            )?;
        }
        if let Some(level) = audience.internal {
            config.internal_from = selectors(
                &level.from,
                "[audience.internal]",
                &|role| role == CollectionRole::Members,
                "`internal` reads only full-membership collections and explicitly selected GitHub organizations",
            )?;
        }
        for group in audience.group {
            let bad = |reason: &str| ConfigError::BadNamedAudience {
                name: group.name.clone(),
                reason: reason.to_string(),
            };
            if group.name.is_empty() {
                return Err(bad("a named audience needs a name"));
            }
            if group.name.starts_with('@') {
                return Err(bad("the name is written bare here; `@` marks a mention"));
            }
            if !ReaderId::new(group.name.clone()).is_literal() || SelectorSpec::parse(&group.name).is_some() {
                return Err(bad("this spelling is reserved"));
            }
            let within =
                match &group.within {
                    None => None,
                    Some(target) => Some(ChainAudience::parse(target).ok_or_else(|| {
                        bad("`within` asserts containment in a built-in audience: `self` or `internal`")
                    })?),
                };
            let from = selectors(
                &group.from,
                &format!("[[audience.group]] {}", group.name),
                &|role| role != CollectionRole::Viewer,
                "a named audience reads collections, and `viewer` names the requesting principal",
            )?;
            config.groups.push(NamedAudience {
                name: GroupName::new(group.name),
                within,
                from,
            });
        }
    }
    if let Some(identity) = identity {
        if identity.url.is_some() || identity.command.is_some() {
            return Err(ConfigError::ForbiddenInlineBinding {
                kind: "identity implementation",
                name: identity.implementation,
            });
        }
        config.identity = Some(match identity.implementation.as_str() {
            IdentityImplementation::VERIFIED_EMAIL => IdentityImplementation::VerifiedEmail,
            "" => {
                return Err(ConfigError::BadIdentity {
                    name: identity.implementation,
                    reason: "names no implementation".to_string(),
                });
            }
            custom => IdentityImplementation::Custom(IdentityImplementationName::new(custom)),
        });
    }
    config.sources = stock_audience_sources()
        .into_iter()
        .filter(|source| providers.contains(&source.provider))
        .collect();
    Ok(config)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChild {
    return_sanitizer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoundary {
    trust: Option<String>,
    audience: Option<Vec<String>>,
}

impl RawBoundary {
    fn convert(self, chain: &TrustChain) -> Result<Label, ConfigError> {
        let trust = match self.trust {
            Some(name) => parse_trust(&name, chain, "boundary")?,
            None => top_trust(chain),
        };
        let audience = match self.audience {
            Some(a) => parse_audience(&a, "boundary audience")?,
            None => Audience::public(),
        };
        Ok(Label::new(trust, audience))
    }
}

fn default_boundary_label(chain: &TrustChain) -> Label {
    Label::new(top_trust(chain), Audience::public())
}

fn top_trust(chain: &TrustChain) -> Trust {
    Trust::new((chain.len() - 1) as u8)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    name: String,
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    delta: Option<RawDelta>,
    requires: Option<RawRequires>,
    #[serde(default)]
    effects: Vec<String>,
    implementation: Option<toml::Value>,
    parameters: Option<serde_json::Value>,
    /// The registered `[[annotator]]` that produces this tool's semantics per call. Replaces
    /// the static `delta`/`requires`/`effects` — a declaration carries one recipe.
    annotator: Option<String>,
}

impl RawTool {
    fn convert(self, chain: &TrustChain) -> Result<ToolDeclaration, ConfigError> {
        let ctx = || format!("tool {}", self.name);
        if self.implementation.is_some() {
            return Err(ConfigError::ForbiddenInlineBinding {
                kind: "tool",
                name: self.name,
            });
        }
        let parameters = match &self.parameters {
            Some(authored) => ToolParameters::compile(authored).map_err(|source| ConfigError::ToolParameters {
                tool: self.name.clone(),
                source,
            })?,
            None => ToolParameters::open(),
        };
        if let Some(annotator) = self.annotator {
            let statics = match (&self.delta, &self.requires, self.effects.is_empty()) {
                (Some(_), _, _) => Some("delta"),
                (None, Some(_), _) => Some("requires"),
                (None, None, false) => Some("effects"),
                (None, None, true) => None,
            };
            if let Some(field) = statics {
                return Err(ConfigError::AnnotatorWithStatics {
                    tool: self.name,
                    annotator,
                    field,
                });
            }
            return Ok(ToolDeclaration::Annotated {
                name: ToolName::new(self.name),
                tags: self.tags.into_iter().map(TagName::new).collect(),
                description: self.description,
                parameters,
                annotator: AnnotatorName::new(annotator),
            });
        }
        // Declaring the tool is the deployment saying it knows it, so an omitted `delta` and
        // `delta = {}` say the same thing: the dimensions this annotation does not describe
        // restrict nothing.
        let delta = match self.delta {
            Some(d) => d.convert(chain, &ctx())?,
            None => Delta::default(),
        };
        let requires = match self.requires {
            Some(r) => r.convert(chain, &ctx())?,
            None => Requires::default(),
        };
        let emits = EffectSet::new(self.effects.into_iter().map(EffectKind::new)).map_err(|duplicate| {
            ConfigError::DuplicateEffect {
                tool: self.name.clone(),
                kind: duplicate.0.as_str().to_string(),
            }
        })?;
        Ok(ToolDeclaration::Declared(ToolAnnotation {
            name: ToolName::new(self.name),
            description: self.description,
            tags: self.tags.into_iter().map(TagName::new).collect(),
            parameters,
            delta,
            emits,
            requires,
        }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDelta {
    trust: Option<String>,
    audience: Option<Vec<String>>,
}

impl RawDelta {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Delta, ConfigError> {
        let trust = match self.trust.as_deref() {
            Some(value) => Some(parse_trust(value, chain, ctx)?),
            None => None,
        };
        let audience = match self.audience {
            Some(a) => Some(parse_declared_audience(&a, &format!("{ctx} delta audience"))?),
            None => None,
        };
        Ok(Delta { trust, audience })
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRequires {
    trust: Option<String>,
    audience: Option<RawRequiresAudience>,
    effects: Option<RawHistory>,
    #[serde(default)]
    attention: Option<Vec<String>>,
}

/// The `requires` table as the policy writes it; a tool an Annotator covers receives its
/// `requires` in the per-call annotation instead.
impl RawRequires {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Requires, ConfigError> {
        let mut audience = Vec::new();
        if let Some(a) = self.audience {
            if let Some(inc) = a.contains {
                audience.push(AudienceRequirement::Includes(parse_recipient_spec(
                    &inc,
                    &format!("{ctx} requires contains"),
                )?));
            }
            if let Some(cap) = a.within {
                audience.push(AudienceRequirement::Cap(parse_declared_audience(
                    &cap,
                    &format!("{ctx} requires within"),
                )?));
            }
        }
        let mut history = Vec::new();
        if let Some(e) = self.effects {
            history.extend(
                e.contains
                    .into_iter()
                    .map(|k| HistoryRequirement::Prior(EffectKind::new(k))),
            );
            history.extend(
                e.excludes
                    .into_iter()
                    .map(|k| HistoryRequirement::NoPrior(EffectKind::new(k))),
            );
        }
        let trust_floor = match self.trust.as_deref() {
            Some(value) => Some(parse_trust(value, chain, ctx)?),
            None => None,
        };
        let attention = match self.attention {
            Some(marks) => marks.into_iter().map(MarkName::new).collect(),
            None => Vec::new(),
        };
        Ok(Requires {
            label: LabelRequirements { trust_floor, audience },
            history,
            attention,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequiresAudience {
    contains: Option<Vec<String>>,
    within: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHistory {
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuthority {
    name: String,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    permits: RawPermits,
    #[serde(default)]
    tags: Vec<String>,
    implementation: Option<toml::Value>,
}

impl RawAuthority {
    fn convert(self, chain: &TrustChain) -> Result<Authority, ConfigError> {
        if self.implementation.is_some() {
            return Err(ConfigError::ForbiddenInlineBinding {
                kind: "authority",
                name: self.name,
            });
        }
        let ctx = format!("authority {}", self.name);
        let mandate = self.permits.convert(chain, &ctx)?;
        Ok(Authority {
            name: AuthorityName::new(self.name),
            mandate,
            scope: Scope {
                tags: self.tags.into_iter().map(TagName::new).collect(),
            },
            hint: self.hint.map(Hint::new),
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawPermits {
    trust_below: Option<String>,
    audience_missing: Option<Vec<String>>,
    #[serde(default)]
    effects_containing: Vec<String>,
    #[serde(default)]
    attention: Vec<String>,
}

impl RawPermits {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Mandate, ConfigError> {
        Ok(Mandate {
            trust_ceiling: self.trust_below.map(|t| parse_trust(&t, chain, ctx)).transpose()?,
            reader_ceiling: self
                .audience_missing
                .map(|r| parse_declared_audience(&r, &format!("{ctx} audience_missing")))
                .transpose()?,
            waivers: self.effects_containing.into_iter().map(EffectKind::new).collect(),
            attends: self.attention.into_iter().map(MarkName::new).collect(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizer {
    name: String,
    on: Vec<String>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    permits: RawSanitizerPermits,
    implementation: Option<toml::Value>,
}

impl RawSanitizer {
    fn convert(self, chain: &TrustChain) -> Result<Sanitizer, ConfigError> {
        if self.implementation.is_some() {
            return Err(ConfigError::ForbiddenInlineBinding {
                kind: "sanitizer",
                name: self.name,
            });
        }
        let on = parse_points(&self.on, &self.name)?;
        let transition = self.permits.convert(chain, &self.name)?;
        Ok(Sanitizer {
            name: SanitizerName::new(self.name),
            on,
            transition,
            scope: Scope {
                tags: self.tags.into_iter().map(TagName::new).collect(),
            },
            hint: self.hint.map(Hint::new),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizerPermits {
    #[serde(default)]
    audience: Option<RawAudienceTransition>,
    #[serde(default)]
    trust: Option<RawTrustTransition>,
}

impl RawSanitizerPermits {
    fn convert(self, chain: &TrustChain, name: &str) -> Result<DeclaredTransition, ConfigError> {
        match (self.audience, self.trust) {
            (Some(audience), None) => Ok(DeclaredTransition::Audience {
                from_includes: parse_declared_audience(&audience.from, &format!("sanitizer {name} from"))?,
                to: parse_declared_audience(&audience.to, &format!("sanitizer {name} to"))?,
            }),
            (None, Some(trust)) => Ok(DeclaredTransition::Trust {
                from_floor: parse_trust(&trust.from, chain, &format!("sanitizer {name} from"))?,
                to: parse_trust(&trust.to, chain, &format!("sanitizer {name} to"))?,
            }),
            (Some(_), Some(_)) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares both an audience and a trust transition — a sanitizer permits one dimension",
            }),
            (None, None) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares no transition — give `permits` an `audience` or a `trust` key",
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudienceTransition {
    from: Vec<String>,
    to: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrustTransition {
    from: String,
    to: String,
}

// --- shared conversion helpers -------------------------------------------------

fn parse_trust(name: &str, chain: &TrustChain, context: &str) -> Result<Trust, ConfigError> {
    chain.rank_of(name).ok_or_else(|| ConfigError::UnknownTrustRank {
        name: name.to_string(),
        context: context.to_string(),
    })
}

fn parse_audience(list: &[String], context: &str) -> Result<Audience, ConfigError> {
    Ok(Audience::of_declared(&parse_declared_audience(list, context)?))
}

/// One written audience list: the union of its entries. `public` stands alone; `self` and
/// `internal` are the built-in symbolic audiences (at most one — the union of two chain
/// levels is the outer one, so writing both is a mistake); `@name` mentions a configured
/// named audience and `@provider:selector` a source collection directly; everything else is
/// a literal reader.
fn parse_declared_audience(list: &[String], context: &str) -> Result<DeclaredAudience, ConfigError> {
    let refused = |reason: String| ConfigError::BadAudience {
        context: context.to_string(),
        reason,
    };
    if list.iter().any(|r| r == "public") {
        return if list.len() == 1 {
            Ok(DeclaredAudience::Public)
        } else {
            Err(refused(
                "`public` is the whole universe and cannot be combined with other entries".to_string(),
            ))
        };
    }
    if list.is_empty() {
        return Err(refused("empty reader set".to_string()));
    }
    if let Some(ph) = list.iter().find(|r| r.starts_with('$')) {
        return Err(refused(format!(
            "argument placeholder {ph:?} is only valid in a `contains`"
        )));
    }
    let mut chain = None;
    let mut groups = Vec::new();
    let mut readers = Vec::new();
    for entry in list {
        if let Some(level) = ChainAudience::parse(entry) {
            if chain.replace(level).is_some() {
                return Err(refused(
                    "two built-in audiences in one union: the outer one already contains the inner".to_string(),
                ));
            }
            continue;
        }
        match entry.strip_prefix('@') {
            Some(mention) => match GroupRef::parse(mention) {
                Some(group) => groups.push(group),
                None => return Err(refused(format!("`@{mention}` names no audience"))),
            },
            None => readers.push(ReaderId::new(entry)),
        }
    }
    Clause::new(chain, groups, readers)
        .map(DeclaredAudience::Union)
        .map_err(|error| refused(error.to_string()))
}

/// The literal readers an `[[annotator]]` mandate's `audiences` may name. An annotation is
/// literal, so a group mention has no place here, and `public` — always admissible — is never
/// listed.
fn parse_annotator_readers(list: &[String], context: &str) -> Result<BTreeSet<ReaderId>, ConfigError> {
    // `audiences = []` closes the mandate to `public` answers only — the one spelling of
    // that bound, distinct from an omitted mandate, which admits every reader the policy
    // names. (An ordinary declared audience still refuses the empty list: a value some
    // sink reads must name somebody.)
    if list.is_empty() {
        return Ok(BTreeSet::new());
    }
    match parse_declared_audience(list, context)? {
        DeclaredAudience::Public => Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: "`public` is always an admissible annotation audience and is never listed as a reader".to_string(),
        }),
        DeclaredAudience::Union(clause) => {
            if clause.chain().is_some() || clause.groups().next().is_some() {
                return Err(ConfigError::BadAudience {
                    context: context.to_string(),
                    reason: "an annotation names literal readers only — no symbolic audience".to_string(),
                });
            }
            Ok(clause.readers().clone())
        }
    }
}

fn parse_recipient_spec(list: &[String], context: &str) -> Result<RecipientSpec, ConfigError> {
    if list.len() == 1
        && let Some(arg) = list[0].strip_prefix('$')
    {
        return Ok(RecipientSpec::Placeholder(arg.to_string()));
    }
    if let Some(ph) = list.iter().find(|r| r.starts_with('$')) {
        return Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: format!("placeholder {ph:?} must be the sole recipient"),
        });
    }
    Ok(RecipientSpec::Static(parse_declared_audience(list, context)?))
}

fn parse_points(tokens: &[String], name: &str) -> Result<SanitizerPoints, ConfigError> {
    let mut points = SanitizerPoints {
        input: false,
        output: false,
    };
    for token in tokens {
        match token.as_str() {
            "tool_input" => points.input = true,
            "tool_output" => points.output = true,
            other => {
                return Err(ConfigError::UnknownSanitizerPoint {
                    token: other.to_string(),
                });
            }
        }
    }
    if !points.input && !points.output {
        return Err(ConfigError::NoSanitizerPoint { name: name.to_string() });
    }
    Ok(points)
}
#[cfg(test)]
mod tests {
    use super::*;

    const DECLARATIONS: &str = r#"
version = 2

[[annotator]]
name = "crm-acl"
inputs = { customer_id = "$tool_call.arguments.customer_id" }
audiences = ["finance", "insider"]

[[tool]]
name = "lookup"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator = "crm-acl"

[[tool]]
name = "send"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator = "crm-acl"

[[authority]]
name = "approver"
[authority.permits]
trust_below = "trusted"

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[deployment]
dispatch = "enforced"
confined_results = ["lookup"]
"#;

    #[test]
    fn declaration_only_policy_builds_the_engine_registry() {
        let config = Config::from_toml_str(DECLARATIONS).expect("the policy compiles");
        assert!(config.registry().variants(&ToolName::new("lookup")).next().is_some());
        assert!(config.registry().variants(&ToolName::new("send")).next().is_some());
        assert!(config.registry().authority(&AuthorityName::new("approver")).is_some());
        assert!(config.registry().sanitizer(&SanitizerName::new("pii")).is_some());
        assert_eq!(config.registry_config().tools.len(), 2);
    }

    #[test]
    fn every_inline_implementation_site_is_refused() {
        let cases = [
            (
                "tool",
                "version = 2\n[[tool]]\nname = \"t\"\nimplementation = { http = { url = \"https://tool.invalid\" } }\n",
            ),
            (
                "authority",
                "version = 2\n[[authority]]\nname = \"a\"\nimplementation = { builtin = \"approve\" }\n[authority.permits]\ntrust_below = \"trusted\"\n",
            ),
            (
                "sanitizer",
                "version = 2\n[[sanitizer]]\nname = \"s\"\non = [\"tool_output\"]\nimplementation = { builtin = \"hosted\" }\n[sanitizer.permits]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n",
            ),
            (
                "annotator",
                "version = 2\n[[annotator]]\nname = \"d\"\nimplementation = { url = \"https://annotator.invalid\" }\n",
            ),
            (
                "identity implementation",
                "version = 2\n[identity]\nimplementation = \"corp-identity\"\nurl = \"https://identity.invalid\"\n",
            ),
        ];
        for (kind, policy) in cases {
            assert!(
                matches!(
                    Config::from_toml_str(policy),
                    Err(ConfigError::ForbiddenInlineBinding { kind: found, .. }) if found == kind
                ),
                "{kind} inline binding was accepted"
            );
        }
    }

    #[test]
    fn an_inline_binding_on_the_reserved_attest_schema_is_refused() {
        let policy = "version = 2\n\
             [[sanitizer]]\nname = \"attest-schema\"\non = [\"tool_output\"]\n\
             implementation = { url = \"https://attest.invalid\" }\n\
             [sanitizer.permits]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n";
        assert!(matches!(
            Config::from_toml_str(policy),
            Err(ConfigError::ForbiddenInlineBinding { kind: "sanitizer", name }) if name == "attest-schema"
        ));
    }

    #[test]
    fn an_input_sanitizer_registers_with_its_scope_and_refuses_a_trust_mandate() {
        let policy = |mandate: &str| {
            format!(
                "version = 2\n\
                 [[tool]]\nname = \"post\"\ntags = [\"outbound\"]\ndelta = {{}}\n\
                 [[sanitizer]]\nname = \"redact\"\non = [\"tool_input\"]\n\
                 tags = [\"outbound\"]\n\
                 [sanitizer.permits]\n{mandate}\n"
            )
        };
        let config = Config::from_toml_str(&policy("audience = { from = [\"insider\"], to = [\"partner\"] }"))
            .expect("an input substitution compiles");
        let sanitizer = config
            .registry()
            .sanitizer(&SanitizerName::new("redact"))
            .expect("the input point registers the sanitizer");
        assert!(sanitizer.on.input && !sanitizer.on.output);
        assert!(sanitizer.scope.covers(&[TagName::new("outbound")]));
        assert!(!sanitizer.scope.covers(&[TagName::new("inbound")]));

        assert!(matches!(
            Config::from_toml_str(&policy("trust = { from = \"suspicious\", to = \"trusted\" }")),
            Err(ConfigError::Registry(LoadError::InputSanitizerTrust(name))) if name == "redact"
        ));
        assert!(matches!(
            Config::from_toml_str(
                &policy("audience = { from = [\"insider\"], to = [\"partner\"] }")
                    .replace("on = [\"tool_input\"]", "on = []")
            ),
            Err(ConfigError::NoSanitizerPoint { name }) if name == "redact"
        ));
    }

    #[test]
    fn the_audience_tables_compile_into_the_registered_configuration() {
        let policy = "version = 2\n\
             [audience.self]\nfrom = [\"slack:viewer\"]\n\
             [audience.internal]\nfrom = [\"slack:full-members\", \"github:org/corp/members\"]\n\
             [[audience.group]]\nname = \"finance\"\nwithin = \"internal\"\n\
             from = [\"google-workspace:group/finance@corp.com\"]\n\
             [identity]\nimplementation = \"verified-email\"\n";
        let config = Config::from_toml_str(policy).expect("the audience tables load");
        let audience = &config.registry_config().audience;
        let spec = |spelled: &str| SelectorSpec::parse(spelled).expect("a stock selector parses");
        assert_eq!(
            audience.sources.iter().map(|s| s.provider.as_str()).collect::<Vec<_>>(),
            ["google-workspace", "slack", "github"],
            "exactly the referenced providers register, in catalog order"
        );
        assert_eq!(audience.self_from, [spec("slack:viewer")]);
        assert_eq!(
            audience.internal_from,
            [spec("slack:full-members"), spec("github:org/corp/members")]
        );
        assert_eq!(
            audience.groups,
            [NamedAudience {
                name: GroupName::new("finance"),
                within: Some(ChainAudience::Internal),
                from: vec![spec("google-workspace:group/finance@corp.com")],
            }]
        );
        assert_eq!(audience.identity, Some(IdentityImplementation::VerifiedEmail));

        let custom = Config::from_toml_str("version = 2\n[identity]\nimplementation = \"corp-identity\"\n")
            .expect("a custom identity name loads without an inline binding");
        assert_eq!(
            custom.registry_config().audience.identity,
            Some(IdentityImplementation::Custom(IdentityImplementationName::new(
                "corp-identity"
            )))
        );

        let bare = Config::from_toml_str("version = 2\n").unwrap();
        assert_eq!(bare.registry_config().audience, AudienceConfig::default());
    }

    #[test]
    fn an_audience_source_selector_must_fit_its_level() {
        for (case, table, expected) in [
            (
                "self reads only viewers",
                "[audience.self]\nfrom = [\"slack:full-members\"]\n",
                "source",
            ),
            (
                "internal reads only full memberships",
                "[audience.internal]\nfrom = [\"slack:viewer\"]\n",
                "source",
            ),
            (
                "a team is not a full membership",
                "[audience.internal]\nfrom = [\"github:org/corp/team/x\"]\n",
                "source",
            ),
            (
                "a group never reads a viewer",
                "[[audience.group]]\nname = \"g\"\nfrom = [\"slack:viewer\"]\n",
                "source",
            ),
            (
                "an uncatalogued collection",
                "[audience.self]\nfrom = [\"slack:banana\"]\n",
                "source",
            ),
            (
                "a bare word is no selector",
                "[audience.self]\nfrom = [\"banana\"]\n",
                "source",
            ),
            (
                "a group name never carries the @ mark",
                "[[audience.group]]\nname = \"@g\"\nfrom = [\"slack:user-group/g\"]\n",
                "group",
            ),
            (
                "a reserved spelling is no group name",
                "[[audience.group]]\nname = \"internal\"\nfrom = [\"slack:user-group/g\"]\n",
                "group",
            ),
            (
                "within targets the built-in chain only",
                "[[audience.group]]\nname = \"g\"\nwithin = \"public\"\nfrom = [\"slack:user-group/g\"]\n",
                "group",
            ),
            (
                "identity names an implementation",
                "[identity]\nimplementation = \"\"\n",
                "identity",
            ),
        ] {
            let refusal = Config::from_toml_str(&format!("version = 2\n{table}")).expect_err(case);
            let fits = match expected {
                "source" => matches!(refusal, ConfigError::BadAudienceSource { .. }),
                "group" => matches!(refusal, ConfigError::BadNamedAudience { .. }),
                _ => matches!(refusal, ConfigError::BadIdentity { .. }),
            };
            assert!(fits, "{case}: got {refusal:?}");
        }
    }

    #[test]
    fn an_annotator_carries_the_stock_builtin_on_its_declaration() {
        let policy = |builtin: &str| {
            format!(
                "version = 2\n[[annotator]]\nname = \"classify\"\nbuiltin = \"{builtin}\"\n\
                 [[annotator]]\nname = \"bound\"\n"
            )
        };
        for expected in AnnotatorBuiltin::ALL {
            let config = Config::from_toml_str(&policy(expected.wire_name())).expect("the stock builtin loads");
            let annotators: Vec<_> = config
                .annotators()
                .map(|(name, binding)| (name.as_str(), binding.builtin))
                .collect();
            assert_eq!(
                annotators,
                vec![("bound", None), ("classify", Some(expected))],
                "a bound annotator still registers"
            );
        }

        assert!(matches!(
            Config::from_toml_str(&policy("no-such")),
            Err(ConfigError::UnknownAnnotatorBuiltin { name, builtin }) if name == "classify" && builtin == "no-such"
        ));
    }

    #[test]
    fn a_tool_requires_a_registered_annotator() {
        assert!(matches!(
            Config::from_toml_str("version = 2\n[[tool]]\nname = \"lookup\"\nannotator = \"classifier\"\n"),
            Err(ConfigError::Registry(LoadError::UnknownAnnotator { tool, annotator }))
                if tool == "lookup" && annotator == "classifier"
        ));
    }

    #[test]
    fn a_duplicate_annotator_is_refused() {
        assert!(matches!(
            Config::from_toml_str("version = 2\n[[annotator]]\nname = \"a\"\n[[annotator]]\nname = \"a\"\n"),
            Err(ConfigError::Registry(LoadError::DuplicateAnnotator(name))) if name == "a"
        ));
    }

    #[test]
    fn a_tool_names_an_annotator_or_declares_static_semantics_never_both() {
        let with = |statics: &str| {
            format!(
                "version = 2\n[[annotator]]\nname = \"acl\"\n\
                 [[tool]]\nname = \"send\"\nannotator = \"acl\"\n{statics}\n"
            )
        };
        assert!(Config::from_toml_str(&with("")).is_ok());
        // Metadata is not a recipe: it stays legal beside `annotator`.
        assert!(Config::from_toml_str(&with("description = \"Sends one message.\"\ntags = [\"outbound\"]")).is_ok());
        for (field, statics) in [
            ("delta", "delta = {}"),
            ("requires", "requires = { trust = \"trusted\" }"),
            ("effects", "effects = [\"email.sent\"]"),
        ] {
            assert!(
                matches!(
                    Config::from_toml_str(&with(statics)),
                    Err(ConfigError::AnnotatorWithStatics { tool, annotator, field: found })
                        if tool == "send" && annotator == "acl" && found == field
                ),
                "`{field}` beside `annotator` must be refused"
            );
        }
    }

    #[test]
    fn the_wildcard_tool_loads_with_an_annotator_and_nothing_else() {
        let policy = "version = 2\n[[annotator]]\nname = \"any\"\n\
                      [[tool]]\nname = \"*\"\nannotator = \"any\"\n";
        let config = Config::from_toml_str(policy).expect("the wildcard loads");
        assert_eq!(
            config.registry().classify(&appa_engine::value::ToolName::new("ghost")),
            Some(appa_engine::registry::ToolKind::Wildcard)
        );
    }

    #[test]
    fn a_second_wildcard_tool_is_refused() {
        let policy = "version = 2\n[[annotator]]\nname = \"any\"\n\
                      [[tool]]\nname = \"*\"\nannotator = \"any\"\n\
                      [[tool]]\nname = \"*\"\nannotator = \"any\"\n";
        assert!(matches!(
            Config::from_toml_str(policy),
            Err(ConfigError::Registry(LoadError::DuplicateWildcard))
        ));
    }

    #[test]
    fn a_wildcard_tool_with_static_semantics_is_refused() {
        for statics in ["", "delta = {}"] {
            let policy = format!("version = 2\n[[tool]]\nname = \"*\"\n{statics}\n");
            assert!(
                matches!(
                    Config::from_toml_str(&policy),
                    Err(ConfigError::Registry(LoadError::WildcardStatic))
                ),
                "a wildcard without an annotator (statics: {statics:?}) must be refused"
            );
        }
    }

    #[test]
    fn a_wildcard_tool_with_metadata_is_refused() {
        for metadata in [
            "description = \"Anything.\"",
            "tags = [\"web\"]",
            "parameters = { type = \"object\", properties = { path = { type = \"string\" } } }",
        ] {
            let policy = format!(
                "version = 2\n[[annotator]]\nname = \"any\"\n\
                 [[tool]]\nname = \"*\"\nannotator = \"any\"\n{metadata}\n"
            );
            assert!(
                matches!(
                    Config::from_toml_str(&policy),
                    Err(ConfigError::Registry(LoadError::WildcardMetadata))
                ),
                "wildcard metadata {metadata:?} must be refused"
            );
        }
    }

    #[test]
    fn an_annotator_mandate_resolves_omitted_bounds_to_the_whole_vocabulary() {
        let policy = r#"
version = 2

[[annotator]]
name = "bounded"
ranks = ["suspicious"]
audiences = ["alice"]
marks = ["operator-signoff"]
effects = ["email.sent"]

[[annotator]]
name = "open"

[[tool]]
name = "post"
delta = { audience = ["alice", "bob"] }
effects = ["email.sent", "backup.completed"]

[[tool]]
name = "fetch"
annotator = "open"

[[tool]]
name = "classify"
annotator = "bounded"

[[authority]]
name = "reviewer"
[authority.permits]
attention = ["operator-signoff", "legal-review"]
"#;
        let config = Config::from_toml_str(policy).expect("annotator bounds load");
        let registry = config.registry();

        let open = registry
            .annotator_mandate(&AnnotatorName::new("open"))
            .expect("open registers");
        assert_eq!(
            open.trust_ranks().collect::<Vec<_>>(),
            vec![Trust::new(0), Trust::new(1)]
        );
        assert_eq!(
            open.audiences().map(|reader| reader.as_str()).collect::<Vec<_>>(),
            ["alice", "bob"]
        );
        assert_eq!(
            open.marks().map(|mark| mark.as_str()).collect::<Vec<_>>(),
            ["legal-review", "operator-signoff"]
        );
        assert_eq!(
            open.effects().map(|kind| kind.as_str()).collect::<Vec<_>>(),
            ["backup.completed", "email.sent"]
        );

        let bounded = registry
            .annotator_mandate(&AnnotatorName::new("bounded"))
            .expect("bounded registers");
        assert_eq!(bounded.trust_ranks().collect::<Vec<_>>(), vec![Trust::new(0)]);
        assert_eq!(bounded.audiences().map(|r| r.as_str()).collect::<Vec<_>>(), ["alice"]);
        assert_eq!(
            bounded.marks().map(|m| m.as_str()).collect::<Vec<_>>(),
            ["operator-signoff"]
        );
        assert_eq!(
            bounded.effects().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["email.sent"]
        );

        assert!(matches!(
            Config::from_toml_str("version = 2\n[[annotator]]\nname = \"a\"\nranks = [\"nope\"]\n"),
            Err(ConfigError::UnknownTrustRank { .. })
        ));
    }

    #[test]
    fn an_empty_annotator_audience_mandate_closes_it_to_public_answers() {
        let policy = r#"
version = 2

[[annotator]]
name = "acl"
audiences = []

[[tool]]
name = "post"
delta = { audience = ["alice"] }

[[tool]]
name = "fetch"
annotator = "acl"
"#;
        let config = Config::from_toml_str(policy).expect("the public-only mandate loads");
        let mandate = config
            .registry()
            .annotator_mandate(&AnnotatorName::new("acl"))
            .expect("acl registers");
        assert_eq!(mandate.audiences().count(), 0, "no named reader is admissible");
    }

    #[test]
    fn a_wildcard_covers_a_confined_result_the_policy_never_names() {
        let policy = r#"
version = 2

[deployment]
dispatch = "enforced"
confined_results = ["unwritten"]

[[annotator]]
name = "acl"

[[tool]]
name = "*"
annotator = "acl"
"#;
        Config::from_toml_str(policy).expect("the wildcard covers the confined tool");
    }

    #[test]
    fn an_annotator_audience_bound_names_literal_readers_only() {
        let with = |audiences: &str| format!("version = 2\n[[annotator]]\nname = \"acl\"\naudiences = {audiences}\n");
        assert!(Config::from_toml_str(&with("[\"alice\", \"bob\"]")).is_ok());
        for (case, audiences) in [("`public`", "[\"public\"]"), ("a group mention", "[\"@team\"]")] {
            assert!(
                matches!(
                    Config::from_toml_str(&with(audiences)),
                    Err(ConfigError::BadAudience { .. })
                ),
                "{case} must be refused in an annotator's `audiences`"
            );
        }
    }

    #[test]
    fn an_input_reads_one_of_the_five_tool_call_values() {
        let policy = |spelling: &str| {
            format!(
                "version = 2\n[[annotator]]\nname = \"r\"\ninputs = {{ subject = \"{spelling}\" }}\n\
                 [[tool]]\nname = \"lookup\"\ndescription = \"d\"\n\
                 parameters = {{ type = \"object\", properties = {{ id = {{ type = \"string\" }} }}, required = [\"id\"] }}\n\
                 annotator = \"r\"\n"
            )
        };
        for supported in [
            "$tool_call",
            "$tool_call.name",
            "$tool_call.description",
            "$tool_call.arguments",
            "$tool_call.arguments.id",
        ] {
            assert!(
                Config::from_toml_str(&policy(supported)).is_ok(),
                "{supported} is a tool-call value"
            );
        }
        for unsupported in [
            "$tool",
            "$tool_call.foo",
            "$tool_call.arguments.id.deep",
            "$tool_call.arguments.",
            "id",
        ] {
            assert!(
                matches!(
                    Config::from_toml_str(&policy(unsupported)),
                    Err(ConfigError::UnknownCallSource { annotator, input, spelling })
                        if annotator == "r" && input == "subject" && spelling == unsupported
                ),
                "{unsupported} is not a tool-call value"
            );
        }
    }

    #[test]
    fn a_mapped_argument_input_needs_a_required_top_level_property() {
        let policy = |parameters: &str| {
            format!(
                "version = 2\n[[annotator]]\nname = \"acl\"\ninputs = {{ subject = \"$tool_call.arguments.id\" }}\n\
                 [[tool]]\nname = \"lookup\"\nannotator = \"acl\"\n{parameters}\n"
            )
        };
        assert!(
            Config::from_toml_str(&policy(
                "parameters = { type = \"object\", properties = { id = { type = \"string\" } }, required = [\"id\"] }"
            ))
            .is_ok()
        );
        for (case, parameters) in [
            ("an open schema", ""),
            (
                "an optional property",
                "parameters = { type = \"object\", properties = { id = { type = \"string\" } } }",
            ),
            (
                "another property",
                "parameters = { type = \"object\", properties = { cc = { type = \"string\" } }, required = [\"cc\"] }",
            ),
        ] {
            assert!(
                matches!(
                    Config::from_toml_str(&policy(parameters)),
                    Err(ConfigError::AnnotatorInput { tool, annotator, input, .. })
                        if tool == "lookup" && annotator == "acl" && input == "subject"
                ),
                "{case} must refuse the argument input"
            );
        }
    }

    #[test]
    fn a_description_input_needs_a_declared_description() {
        let policy = |description: &str| {
            format!(
                "version = 2\n[[annotator]]\nname = \"acl\"\ninputs = {{ what = \"$tool_call.description\" }}\n\
                 [[tool]]\nname = \"lookup\"\nannotator = \"acl\"\n{description}\n"
            )
        };
        assert!(Config::from_toml_str(&policy("description = \"Looks one customer up.\"")).is_ok());
        assert!(matches!(
            Config::from_toml_str(&policy("")),
            Err(ConfigError::AnnotatorInput { tool, input, .. }) if tool == "lookup" && input == "what"
        ));
    }

    #[test]
    fn an_annotator_name_is_an_opaque_non_empty_string() {
        let policy = "version = 2\n[[annotator]]\nname = \"a.b\"\n\
            [[tool]]\nname = \"lookup\"\ndescription = \"Looks up a value.\"\nannotator = \"a.b\"\n";
        assert!(Config::from_toml_str(policy).is_ok());
        assert!(matches!(
            Config::from_toml_str("version = 2\n[[annotator]]\nname = \"\"\n"),
            Err(ConfigError::BadAnnotatorName(name)) if name.is_empty()
        ));
    }

    #[test]
    fn every_audience_argument_binding_needs_a_required_top_level_string_in_parameters() {
        use appa_engine::params::PropertyFault;
        let refused = |policy: &str, expected: PropertyFault| match Config::from_toml_str(policy) {
            Err(ConfigError::Registry(LoadError::AudienceBindingSchema { argument, fault, .. })) => {
                assert_eq!(argument, "to");
                assert_eq!(fault, expected, "policy:\n{policy}");
            }
            other => panic!("expected an audience-binding refusal with {expected:?}, got {other:?} for:\n{policy}"),
        };
        let bindings = ["requires = { audience = { contains = [\"$to\"] } }\ndelta = {}"];
        let parameters = [
            ("", PropertyFault::Undeclared),
            (
                "parameters = { type = \"object\", properties = { cc = { type = \"string\" } }, required = [\"cc\"] }",
                PropertyFault::Undeclared,
            ),
            (
                "parameters = { type = \"object\", properties = { envelope = { type = \"object\", properties = { to = { type = \"string\" } }, required = [\"to\"] } }, required = [\"envelope\"] }",
                PropertyFault::Undeclared,
            ),
            (
                "parameters = { type = \"object\", properties = { to = { type = \"string\" } } }",
                PropertyFault::Optional,
            ),
            (
                "parameters = { type = \"object\", properties = { to = { type = \"integer\" } }, required = [\"to\"] }",
                PropertyFault::NotString,
            ),
        ];
        let policy = |binding: &str, parameters: &str| {
            format!("version = 2\n[[tool]]\nname = \"send\"\n{parameters}\n{binding}\n")
        };
        for binding in bindings {
            for (parameters, expected) in parameters {
                refused(&policy(binding, parameters), expected);
            }
            let ok = policy(
                binding,
                "parameters = { type = \"object\", properties = { to = { type = \"string\" }, body = { type = \"string\" } }, required = [\"to\"] }",
            );
            assert!(Config::from_toml_str(&ok).is_ok(), "must load:\n{ok}");
        }
        let static_recipients = "version = 2\n[[tool]]\nname = \"send\"\nrequires = { audience = { contains = [\"finance\"] } }\ndelta = {}\n";
        assert!(Config::from_toml_str(static_recipients).is_ok());
    }

    #[test]
    fn the_deployment_table_compiles_into_the_validated_profile() {
        let config = Config::from_toml_str(DECLARATIONS).expect("the policy compiles");
        let profile = config.engine().profile();
        assert_eq!(
            profile.executor_class(&ToolName::new("lookup")),
            ExecutorClass::Enforced
        );
        assert!(profile.confines_result(&ToolName::new("lookup")));
        assert!(!profile.confines_result(&ToolName::new("send")));
        assert_eq!(profile.binding(), BindingMode::Harness);
        assert!(!profile.context_control());
        assert_eq!(
            profile.starting_label(),
            &neutral_starting_label(config.registry().trust_chain())
        );
    }

    #[test]
    fn an_absent_deployment_table_is_the_no_coverage_default_and_refuses_covered_constructs() {
        let plain = Config::from_toml_str("version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n").expect("loads");
        assert_eq!(
            plain.engine().profile().executor_class(&ToolName::new("t")),
            ExecutorClass::Assumed
        );
        assert_eq!(plain.engine().open_vectors().len(), 1);
        let uncovered = DECLARATIONS.replace(
            "[deployment]\ndispatch = \"enforced\"\nconfined_results = [\"lookup\"]\n",
            "",
        );
        assert!(matches!(
            Config::from_toml_str(&uncovered),
            Err(ConfigError::Registry(LoadError::OutputSanitizerUncovered { .. }))
        ));
    }

    #[test]
    fn deployment_tokens_and_exception_conflicts_are_load_errors() {
        let base = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
        let with = |table: &str| format!("{base}\n[deployment]\n{table}\n");
        for bad_token in [
            "dispatch = \"trusted\"",
            "binding = \"content\"",
            "provider_surfaces = { web_search = \"proxied\" }",
        ] {
            assert!(matches!(
                Config::from_toml_str(&with(bad_token)),
                Err(ConfigError::Parse(_))
            ));
        }
        assert!(matches!(
            Config::from_toml_str(&with("starting_label = { audience = \"everyone\" }")),
            Err(ConfigError::BadDeploymentToken {
                field: "starting_label audience",
                ..
            })
        ));
        assert!(matches!(
            Config::from_toml_str(&with("assumed_tools = [\"t\"]\nprovider_run_tools = [\"t\"]")),
            Err(ConfigError::ConflictingExecutorException { tool }) if tool == "t"
        ));
        assert!(matches!(
            Config::from_toml_str(&with("confined_results = [\"ghost\"]")),
            Err(ConfigError::Registry(LoadError::UnknownDeploymentTool { .. }))
        ));
    }

    #[test]
    fn the_wildcard_is_part_of_the_policy_identity() {
        let identity = |source: &str| Config::from_toml_str(source).expect("loads").engine().identity();
        let base = "version = 2\n\n[[annotator]]\nname = \"acl\"\n\n[[annotator]]\nname = \"other\"\n\n[[tool]]\nname = \"post\"\ndelta = {}\n";
        let with_wildcard = format!("{base}\n[[tool]]\nname = \"*\"\nannotator = \"acl\"\n");
        let with_other = format!("{base}\n[[tool]]\nname = \"*\"\nannotator = \"other\"\n");
        assert_ne!(
            identity(base),
            identity(&with_wildcard),
            "adding the wildcard changes what an unwritten tool call does"
        );
        assert_ne!(
            identity(&with_wildcard),
            identity(&with_other),
            "rerouting the wildcard changes who annotates the long tail"
        );
    }

    #[test]
    fn hints_and_limits_never_move_the_policy_identity() {
        let identity = |source: &str| Config::from_toml_str(source).expect("loads").engine().identity();
        let base = identity(DECLARATIONS);
        let hinted = DECLARATIONS.replace(
            "name = \"approver\"",
            "name = \"approver\"\nhint = \"the wire-approval desk\"",
        );
        assert_eq!(identity(&hinted), base);
        let capped = format!("{DECLARATIONS}\n[limits]\nplanner_cap = 7\n");
        assert_eq!(identity(&capped), base);
        let weakened = DECLARATIONS.replace("dispatch = \"enforced\"", "dispatch = \"assumed\"");
        assert_ne!(identity(&weakened), base);
    }

    #[test]
    fn compiled_tool_parameters_are_normalized_in_policy_identity() {
        let config = Config::from_toml_str(
            "version = 2\n[[tool]]\nname = \"t\"\nparameters = { type = \"object\", properties = { value = { type = \"string\" } } }\n",
        )
        .expect("the schema compiles");
        let tool = config
            .registry()
            .variants(&ToolName::new("t"))
            .next()
            .expect("the tool is registered");
        assert_eq!(
            tool.parameters().normalized(),
            serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": [],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn a_declared_group_mention_loads_when_routed_and_refuses_at_load_when_not() {
        let policy = r#"
version = 2

[audience.internal]
from = ["slack:full-members"]

[[audience.group]]
name = "team"
within = "internal"
from = ["slack:user-group/team"]

[[audience.group]]
name = "board"
from = ["slack:user-group/board"]

[[audience.group]]
name = "officers"
from = ["slack:user-group/officers"]

[[tool]]
name = "read"
delta = { audience = ["auditor", "@team"] }

[[tool]]
name = "send"
requires = { audience = { within = ["@team"], contains = ["@board"] } }
delta = {}

[[authority]]
name = "officer"
[authority.permits]
audience_missing = ["@officers"]

[[sanitizer]]
name = "declassify"
on = ["tool_output"]
[sanitizer.permits]
audience = { from = ["internal"], to = ["@team"] }

[deployment]
dispatch = "enforced"
confined_results = ["read", "send"]
"#;
        let mention = |name: &str, readers: &[&str]| {
            DeclaredAudience::Union(
                Clause::new(
                    [],
                    [GroupRef::Named(GroupName::new(name))],
                    readers.iter().map(|reader| ReaderId::new(*reader)),
                )
                .expect("the fixture readers are literal"),
            )
        };
        let config = Config::from_toml_str(policy).expect("routed group mentions load");
        let registry = config.registry();
        assert_eq!(
            registry
                .audience()
                .groups()
                .map(|group| group.name.as_str())
                .collect::<Vec<_>>(),
            ["board", "officers", "team"]
        );
        let read = registry
            .variants(&ToolName::new("read"))
            .next()
            .expect("read registers")
            .declared()
            .expect("read is declared");
        assert_eq!(read.delta.audience.as_ref(), Some(&mention("team", &["auditor"])));
        let officer = registry
            .authority(&AuthorityName::new("officer"))
            .expect("officer registers");
        assert_eq!(officer.mandate.reader_ceiling, Some(mention("officers", &[])));
        match &registry
            .sanitizer(&SanitizerName::new("declassify"))
            .expect("declassify registers")
            .transition
        {
            DeclaredTransition::Audience { from_includes, to } => {
                assert_eq!(
                    from_includes,
                    &DeclaredAudience::Union(Clause::new([ChainAudience::Internal], [], []).expect("a chain clause"))
                );
                assert_eq!(to, &mention("team", &[]));
            }
            other => panic!("expected an audience transition, got {other:?}"),
        }

        let unrouted = policy.replace(
            "[[audience.group]]\nname = \"board\"\nfrom = [\"slack:user-group/board\"]\n",
            "",
        );
        assert!(matches!(
            Config::from_toml_str(&unrouted),
            Err(ConfigError::Registry(LoadError::UnroutableAudience { .. }))
        ));

        for (case, replacement) in [
            ("bare @", "within = [\"@\"]"),
            ("public beside a group", "within = [\"public\", \"@team\"]"),
        ] {
            let malformed = policy.replace("within = [\"@team\"]", replacement);
            assert!(
                matches!(Config::from_toml_str(&malformed), Err(ConfigError::BadAudience { .. })),
                "{case} loads"
            );
        }

        let routed = "version = 2\n\
             [[audience.group]]\nname = \"team\"\nfrom = [\"slack:user-group/team\"]\n\
             [[tool]]\nname = \"t\"\ndelta = {}\n";
        let bare = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
        let symbolic = Config::from_toml_str(&format!(
            "{routed}[deployment]\nstarting_label = {{ audience = [\"@team\"] }}\n"
        ))
        .expect("a routed symbolic starting label loads");
        assert_eq!(
            symbolic.engine().profile().starting_label().audience,
            Audience::of_declared(&mention("team", &[]))
        );
        Config::from_toml_str(&format!("{routed}[boundary]\naudience = [\"@team\"]\n"))
            .expect("a routed symbolic boundary label loads");
        assert!(matches!(
            Config::from_toml_str(&format!(
                "{bare}[deployment]\nstarting_label = {{ audience = [\"@team\"] }}\n"
            )),
            Err(ConfigError::Registry(LoadError::UnroutableAudience { .. }))
        ));
        assert!(matches!(
            Config::from_toml_str(&format!("{bare}[boundary]\naudience = [\"@team\"]\n")),
            Err(ConfigError::BadAudience { .. })
        ));
    }
    #[test]
    fn a_string_token_and_a_reader_list_are_different_audience_shapes() {
        let base = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
        let starting = |audience: &str| {
            let policy = format!("{base}[deployment]\nstarting_label = {{ audience = {audience} }}\n");
            Config::from_toml_str(&policy)
                .expect("a public starting label loads")
                .engine()
                .profile()
                .starting_label()
                .clone()
        };
        assert_eq!(starting("\"public\""), starting("[\"public\"]"));

        let delta = |audience: &str| {
            let policy = format!(
                "version = 2\n[[tool]]\nname = \"t\"\ndelta = {{ audience = {audience} }}\n\
                 [deployment]\ndispatch = \"enforced\"\nconfined_results = [\"t\"]\n"
            );
            Config::from_toml_str(&policy)
        };
        assert!(
            delta("\"public\"").is_err(),
            "a tool delta audience is a reader list, never a token"
        );
        assert!(matches!(delta("[]"), Err(ConfigError::BadAudience { .. })));
    }

    #[test]
    fn a_sanitizer_transition_and_a_component_tag_list_keep_every_member() {
        let policy = "version = 2\n\
             [[audience.group]]\nname = \"auditors\"\nfrom = [\"slack:user-group/auditors\"]\n\
             [[audience.group]]\nname = \"reviewers\"\nfrom = [\"slack:user-group/reviewers\"]\n\
             [[tool]]\nname = \"read\"\ntags = [\"hr\", \"crm\"]\ndelta = { audience = [\"alice\", \"bob\"] }\n\
             [[sanitizer]]\nname = \"redact\"\non = [\"tool_output\"]\ntags = [\"hr\", \"crm\"]\n\
             [sanitizer.permits]\naudience = { from = [\"alice\", \"@auditors\"], to = [\"alice\", \"@reviewers\"] }\n\
             [[authority]]\nname = \"officer\"\ntags = [\"hr\", \"crm\"]\n\
             [authority.permits]\naudience_missing = [\"alice\", \"bob\"]\n\
             [deployment]\ndispatch = \"enforced\"\nconfined_results = [\"read\"]\n";
        let config = Config::from_toml_str(policy).expect("lists of more than one member load");
        let registry = config.registry();
        let redact = registry
            .sanitizer(&SanitizerName::new("redact"))
            .expect("redact registers");
        assert_eq!(redact.scope.tags.len(), 2);
        match &redact.transition {
            DeclaredTransition::Audience { from_includes, to } => {
                let expected = |group: &str| {
                    DeclaredAudience::Union(
                        Clause::new([], [GroupRef::Named(GroupName::new(group))], [ReaderId::new("alice")]).unwrap(),
                    )
                };
                assert_eq!(from_includes, &expected("auditors"));
                assert_eq!(to, &expected("reviewers"));
            }
            other => panic!("expected an audience transition, got {other:?}"),
        }
        let officer = registry
            .authority(&AuthorityName::new("officer"))
            .expect("officer registers");
        assert_eq!(officer.scope.tags.len(), 2);
        assert_eq!(
            officer.mandate.reader_ceiling,
            Some(DeclaredAudience::restricted([
                ReaderId::new("alice"),
                ReaderId::new("bob")
            ]))
        );
    }
}
