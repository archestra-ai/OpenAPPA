//! The spec's policy-dialect compiler: the configuration dialect (TOML) → the engine's
//! [`RegistryConfig`] for the runtime.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use thiserror::Error;

use appa_engine::authority::{
    Authority, Cast, CastCeiling, CastResolution, DeclaredLabel, DeclaredTransition, Hint, Mandate, Sanitizer,
    SanitizerPoints, Scope,
};
use appa_engine::contract::{
    AudienceDelta, AudienceRequirement, Delta, DynamicAudienceBinding, HistoryRequirement, LabelRequirements,
    RecipientSpec, Requires, ToolContract,
};
use appa_engine::engine::Engine;
use appa_engine::fact::ReturnPolicy;
use appa_engine::fact::{EffectKind, EffectSet};
use appa_engine::groups::DeclaredAudience;
use appa_engine::label::{Audience, Dim, Label, ReaderId, Trust};
use appa_engine::names::{
    AuthorityName, CastName, DynamicResolverName, GroupName, MarkName, MembershipResolverName, SanitizerName,
    SurfaceName, TagName,
};
use appa_engine::params::ToolParameters;
use appa_engine::profile::{
    BindingMode, DeploymentPolicy, ExecutorClass, PolicyDialectVersion, ProfileDeclaration, SurfaceMode,
    neutral_starting_label,
};
use appa_engine::registry::{LoadError, PlannerCap, Registry, RegistryConfig, TrustChain};
use appa_engine::value::ToolName;

const SUPPORTED_VERSION: u32 = 1;
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
    #[error("trust rank name \"unknown\" is reserved: it declares a pending-cast delta dimension")]
    ReservedRankName,
    #[error("bad reader set in {context}: {reason}")]
    BadAudience { context: String, reason: String },
    #[error("bad sanitizer point {token:?}: expected \"tool_input\" or \"tool_output\"")]
    UnknownSanitizerPoint { token: String },
    #[error("sanitizer {name} declares no application point (`on` is empty)")]
    NoSanitizerPoint { name: String },
    #[error("sanitizer {name} mandate: {reason}")]
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
    #[error("duplicate dynamic resolver {0}")]
    DuplicateDynamicResolver(String),
    #[error("tool {tool} dynamic binding names unregistered resolver {resolver}")]
    UnregisteredDynamicResolver { tool: String, resolver: String },
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
    #[error("registry rejected: {0}")]
    Registry(#[from] LoadError),
}

/// A fully parsed and **fully validated** policy: the opened [`Engine`] — registry, deployment
/// profile, and policy identity behind the one validated constructor — plus the
/// normalized declarations. The runtime owns implementation bindings.
#[derive(Clone, Debug)]
pub struct Config {
    engine: Engine,
    registry_config: RegistryConfig,
    boundary_label: Label,
}

impl Config {
    /// Parse the runtime's declaration-only policy TOML. Inline implementation bindings are a
    /// policy error because the deployment binds them in `[externals]`.
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
        // "unknown" in a trust position is the pending-cast token, never a rank name.
        if trust_chain.rank_of(UNKNOWN_TOKEN).is_some() {
            return Err(ConfigError::ReservedRankName);
        }

        let boundary_label = match raw.boundary {
            Some(b) => b.convert(&trust_chain)?,
            None => default_boundary_label(&trust_chain),
        };

        let mut dynamic_resolver_names = BTreeSet::new();
        for resolver in raw.dynamic_resolver {
            let name = DynamicResolverName::new(resolver.name);
            if resolver.resolver.is_some() {
                return Err(ConfigError::ForbiddenInlineBinding {
                    kind: "dynamic resolver",
                    name: name.as_str().to_string(),
                });
            }
            if !dynamic_resolver_names.insert(name.clone()) {
                return Err(ConfigError::DuplicateDynamicResolver(name.as_str().to_string()));
            }
        }
        let membership = match raw.membership {
            Some(membership) => {
                if membership.url.is_some() || membership.builtin.is_some() {
                    return Err(ConfigError::ForbiddenInlineBinding {
                        kind: "membership resolver",
                        name: membership.name,
                    });
                }
                Some(MembershipResolverName::new(membership.name))
            }
            None => None,
        };
        let mut tools = Vec::new();
        for t in raw.tool {
            tools.push(t.convert(&trust_chain)?);
        }
        for tool in &tools {
            for binding in appa_engine::check::dynamic_reads(tool) {
                if !dynamic_resolver_names.contains(&binding.resolver) {
                    return Err(ConfigError::UnregisteredDynamicResolver {
                        tool: tool.name.as_str().into(),
                        resolver: binding.resolver.as_str().into(),
                    });
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

        let mut casts = Vec::new();
        for c in raw.cast {
            casts.push(c.convert(&trust_chain)?);
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
            authorities,
            sanitizers,
            casts,
            membership,
        };
        let engine = Engine::open(DeploymentPolicy {
            registry: registry_config.clone(),
            planner_cap,
            dialect: PolicyDialectVersion::new(SUPPORTED_VERSION),
            child_return,
            profile,
        })?;

        Ok(Config {
            engine,
            registry_config,
            boundary_label,
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
    cast: Vec<RawCast>,
    #[serde(default)]
    dynamic_resolver: Vec<RawDynamicResolver>,
    membership: Option<RawMembership>,
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
    Exactly(RawExactly),
}

impl RawDeployment {
    fn convert(self, chain: &TrustChain) -> Result<ProfileDeclaration, ConfigError> {
        let neutral = neutral_starting_label(chain);
        let starting_label = match self.starting_label {
            Some(label) => {
                let trust = match label.trust {
                    Some(name) => parse_trust(&name, chain, "deployment starting_label")?,
                    None => match neutral.trust {
                        Dim::Known(top) => top,
                        Dim::Unknown => unreachable!("the neutral starting label is established"),
                    },
                };
                let audience = match label.audience {
                    None => Audience::Public,
                    Some(RawStartingAudience::Token(token)) if token == "public" => Audience::Public,
                    Some(RawStartingAudience::Token(token)) => {
                        return Err(ConfigError::BadDeploymentToken {
                            field: "starting_label audience",
                            expected: r#""public" or { exactly = [...] }"#,
                            found: token,
                        });
                    }
                    Some(RawStartingAudience::Exactly(a)) => {
                        parse_audience(&a.exactly, "deployment starting_label audience")?
                    }
                };
                Label::new(Dim::Known(trust), Dim::Known(audience))
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
struct RawDynamicResolver {
    name: String,
    resolver: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMembership {
    name: String,
    url: Option<toml::Value>,
    builtin: Option<toml::Value>,
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
    audience: Option<RawExactly>,
}

impl RawBoundary {
    fn convert(self, chain: &TrustChain) -> Result<Label, ConfigError> {
        let trust = match self.trust {
            Some(name) => parse_trust(&name, chain, "boundary")?,
            None => top_trust(chain),
        };
        let audience = match self.audience {
            Some(a) => parse_audience(&a.exactly, "boundary audience")?,
            None => Audience::Public,
        };
        Ok(Label::new(Dim::Known(trust), Dim::Known(audience)))
    }
}

fn default_boundary_label(chain: &TrustChain) -> Label {
    Label::new(Dim::Known(top_trust(chain)), Dim::Known(Audience::Public))
}

fn top_trust(chain: &TrustChain) -> Trust {
    Trust::new((chain.len() - 1) as u8)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    name: String,
    #[serde(default)]
    tags: Vec<String>,
    delta: Option<RawDelta>,
    requires: Option<RawRequires>,
    #[serde(default)]
    effects: Vec<String>,
    implementation: Option<toml::Value>,
    parameters: Option<serde_json::Value>,
}

impl RawTool {
    fn convert(self, chain: &TrustChain) -> Result<ToolContract, ConfigError> {
        let ctx = || format!("tool {}", self.name);
        // No `delta` key at all = unannotated (results admitted at Unknown/Unknown, fail-closed);
        // `delta = {}` = the deliberate neutral annotation. The distinction is the whole point —
        // never collapse an omitted delta into the neutral one.
        let delta = self.delta.map(|d| d.convert(chain, &ctx())).transpose()?;
        let requires = match self.requires {
            Some(r) => r.convert(chain, &ctx())?,
            None => Requires::default(),
        };
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
        let emits = EffectSet::new(self.effects.into_iter().map(EffectKind::new)).map_err(|duplicate| {
            ConfigError::DuplicateEffect {
                tool: self.name.clone(),
                kind: duplicate.0.as_str().to_string(),
            }
        })?;
        Ok(ToolContract {
            name: ToolName::new(self.name),
            tags: self.tags.into_iter().map(TagName::new).collect(),
            parameters,
            delta,
            emits,
            requires,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDelta {
    trust: Option<String>,
    audience: Option<RawDeltaAudience>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDeltaAudience {
    Token(String),
    Exactly(RawExactly),
    Dynamic(RawDynamicBinding),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDynamicBinding {
    resolver: String,
    argument: String,
}

const UNKNOWN_TOKEN: &str = "unknown";

impl RawDelta {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Delta, ConfigError> {
        let trust = match self.trust.as_deref() {
            Some(UNKNOWN_TOKEN) => Some(Dim::Unknown),
            Some(name) => Some(Dim::Known(parse_trust(name, chain, ctx)?)),
            None => None,
        };
        let audience = match self.audience {
            Some(RawDeltaAudience::Token(token)) if token == UNKNOWN_TOKEN => Some(AudienceDelta::PendingCast),
            Some(RawDeltaAudience::Token(token)) => {
                return Err(ConfigError::BadAudience {
                    context: format!("{ctx} delta audience"),
                    reason: format!("expected {{ exactly = [...] }} or \"unknown\", found {token:?}"),
                });
            }
            Some(RawDeltaAudience::Exactly(a)) => Some(AudienceDelta::Static(parse_declared_audience(
                &a.exactly,
                &format!("{ctx} delta audience"),
            )?)),
            Some(RawDeltaAudience::Dynamic(b)) => Some(AudienceDelta::Dynamic(b.into_binding())),
            None => None,
        };
        Ok(Delta { trust, audience })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExactly {
    exactly: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRequires {
    trust: Option<String>,
    audience: Option<RawRequiresAudience>,
    effects: Option<RawHistory>,
    #[serde(default)]
    attention: Vec<String>,
}

impl RawRequires {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Requires, ConfigError> {
        let mut audience = Vec::new();
        if let Some(a) = self.audience {
            if let Some(inc) = a.includes {
                audience.push(AudienceRequirement::Includes(match inc {
                    RawRecipientSpec::Static(list) => parse_recipient_spec(&list, &format!("{ctx} requires includes"))?,
                    RawRecipientSpec::Dynamic(binding) => RecipientSpec::Dynamic(binding.into_binding()),
                }));
            }
            if let Some(cap) = a.cap {
                audience.push(AudienceRequirement::Cap(parse_declared_audience(
                    &cap,
                    &format!("{ctx} requires cap"),
                )?));
            }
        }
        let mut history = Vec::new();
        if let Some(e) = self.effects {
            history.extend(e.has.into_iter().map(|k| HistoryRequirement::Prior(EffectKind::new(k))));
            history.extend(
                e.has_no
                    .into_iter()
                    .map(|k| HistoryRequirement::NoPrior(EffectKind::new(k))),
            );
        }
        Ok(Requires {
            label: LabelRequirements {
                trust_floor: self.trust.map(|t| parse_trust(&t, chain, ctx)).transpose()?,
                audience,
            },
            history,
            attention: self.attention.into_iter().map(MarkName::new).collect(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequiresAudience {
    includes: Option<RawRecipientSpec>,
    cap: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawRecipientSpec {
    Static(Vec<String>),
    Dynamic(RawDynamicBinding),
}

impl RawDynamicBinding {
    fn into_binding(self) -> DynamicAudienceBinding {
        DynamicAudienceBinding {
            resolver: DynamicResolverName::new(self.resolver),
            argument: self.argument,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHistory {
    #[serde(default)]
    has: Vec<String>,
    #[serde(default)]
    has_no: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuthority {
    name: String,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    mandate: RawMandate,
    #[serde(default)]
    scope: RawScope,
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
        let mandate = self.mandate.convert(chain, &ctx)?;
        Ok(Authority {
            name: AuthorityName::new(self.name),
            mandate,
            scope: Scope {
                tags: self.scope.tags.into_iter().map(TagName::new).collect(),
            },
            hint: self.hint.map(Hint::new),
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMandate {
    can_cover_trust_to: Option<String>,
    can_cover_readers: Option<RawMayAdd>,
    #[serde(default)]
    can_waive: Vec<String>,
    #[serde(default)]
    attends: Vec<String>,
}

impl RawMandate {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Mandate, ConfigError> {
        Ok(Mandate {
            trust_ceiling: self
                .can_cover_trust_to
                .map(|t| parse_trust(&t, chain, ctx))
                .transpose()?,
            reader_ceiling: self
                .can_cover_readers
                .map(|r| parse_declared_audience(&r.may_add, &format!("{ctx} can_cover_readers")))
                .transpose()?,
            waivers: self.can_waive.into_iter().map(EffectKind::new).collect(),
            attends: self.attends.into_iter().map(MarkName::new).collect(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMayAdd {
    may_add: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawScope {
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizer {
    name: String,
    on: Vec<String>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    scope: RawScope,
    mandate: RawSanitizerMandate,
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
        let transition = self.mandate.convert(chain, &self.name)?;
        Ok(Sanitizer {
            name: SanitizerName::new(self.name),
            on,
            transition,
            scope: Scope {
                tags: self.scope.tags.into_iter().map(TagName::new).collect(),
            },
            hint: self.hint.map(Hint::new),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizerMandate {
    #[serde(default)]
    audience: Option<RawAudienceTransition>,
    #[serde(default)]
    trust: Option<RawTrustTransition>,
}

impl RawSanitizerMandate {
    fn convert(self, chain: &TrustChain, name: &str) -> Result<DeclaredTransition, ConfigError> {
        match (self.audience, self.trust) {
            (Some(audience), None) => Ok(DeclaredTransition::Audience {
                from_includes: parse_declared_audience(&audience.from.includes, &format!("sanitizer {name} from"))?,
                to: parse_declared_audience(&audience.to.exactly, &format!("sanitizer {name} to"))?,
            }),
            (None, Some(trust)) => Ok(DeclaredTransition::Trust {
                from_floor: parse_trust(&trust.from, chain, &format!("sanitizer {name} from"))?,
                to: parse_trust(&trust.to, chain, &format!("sanitizer {name} to"))?,
            }),
            (Some(_), Some(_)) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares both an audience and a trust transition — a mandate binds one dimension",
            }),
            (None, None) => Err(ConfigError::SanitizerMandateShape {
                name: name.to_string(),
                reason: "declares no transition — give the mandate an `audience` or a `trust` key",
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudienceTransition {
    from: RawIncludes,
    to: RawExactly,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrustTransition {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIncludes {
    includes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCast {
    name: String,
    constant: Option<RawConstantLabel>,
    resolver: Option<RawCastResolver>,
    #[serde(default)]
    scope: RawScope,
}

impl RawCast {
    fn convert(self, chain: &TrustChain) -> Result<Cast, ConfigError> {
        let ctx = format!("cast {}", self.name);
        let scope = Scope {
            tags: self.scope.tags.into_iter().map(TagName::new).collect(),
        };
        let resolution = match (self.constant, self.resolver) {
            (Some(_), Some(_)) => {
                return Err(bad_impl(
                    "cast",
                    &self.name,
                    "declares both a constant and a resolver — a cast resolves one way or the other",
                ));
            }
            (Some(constant), None) => CastResolution::Constant(constant.convert(chain, &ctx)?),
            (None, Some(resolver)) => CastResolution::Resolver {
                may_cast: resolver.convert(chain, &self.name)?,
            },
            (None, None) => {
                return Err(bad_impl(
                    "cast",
                    &self.name,
                    "declares neither a constant nor a resolver",
                ));
            }
        };
        Ok(Cast {
            name: CastName::new(self.name),
            resolution,
            scope,
        })
    }
}

/// A resolver-backed cast as the policy writes it: the ceiling only. The endpoint binds
/// at the deployment, so any binding key here is refused like every other inline binding.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCastResolver {
    may_cast: RawMayCast,
    url: Option<toml::Value>,
    builtin: Option<toml::Value>,
    token_env: Option<toml::Value>,
}

impl RawCastResolver {
    fn convert(self, chain: &TrustChain, name: &str) -> Result<CastCeiling, ConfigError> {
        if self.url.is_some() || self.builtin.is_some() || self.token_env.is_some() {
            return Err(ConfigError::ForbiddenInlineBinding {
                kind: "cast",
                name: name.to_string(),
            });
        }
        let ctx = format!("cast {name} may_cast");
        Ok(CastCeiling {
            trust: self
                .may_cast
                .trust
                .iter()
                .map(|rank| parse_trust(rank, chain, &ctx))
                .collect::<Result<_, _>>()?,
            audience: parse_declared_audience(&self.may_cast.audience.cap, &ctx)?,
        })
    }
}

/// The complete product ceiling: the trust ranks a resolver may grant, and the cap its
/// resolved audience must stay within. An empty `trust` admits no unresolved trust at all.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMayCast {
    #[serde(default)]
    trust: Vec<String>,
    audience: RawCap,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCap {
    cap: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConstantLabel {
    trust: String,
    audience: RawExactly,
}

impl RawConstantLabel {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<DeclaredLabel, ConfigError> {
        Ok(DeclaredLabel {
            trust: parse_trust(&self.trust, chain, ctx)?,
            audience: parse_declared_audience(&self.audience.exactly, ctx)?,
        })
    }
}

// --- shared conversion helpers -------------------------------------------------

fn bad_impl(kind: &'static str, name: &str, reason: &str) -> ConfigError {
    ConfigError::BadImplementation {
        kind,
        name: name.to_string(),
        reason: reason.to_string(),
    }
}

fn parse_trust(name: &str, chain: &TrustChain, context: &str) -> Result<Trust, ConfigError> {
    chain.rank_of(name).ok_or_else(|| ConfigError::UnknownTrustRank {
        name: name.to_string(),
        context: context.to_string(),
    })
}

fn parse_audience(list: &[String], context: &str) -> Result<Audience, ConfigError> {
    match parse_declared_audience(list, context)? {
        DeclaredAudience::Public => Ok(Audience::Public),
        DeclaredAudience::Restricted { readers, groups } => match groups.into_iter().next() {
            None => Ok(Audience::Restricted(readers)),
            Some(group) => Err(ConfigError::BadAudience {
                context: context.to_string(),
                reason: format!(
                    "{group} is a group mention: a label the algebra holds directly names literal readers only"
                ),
            }),
        },
    }
}

fn parse_declared_audience(list: &[String], context: &str) -> Result<DeclaredAudience, ConfigError> {
    if list.iter().any(|r| r == "public") {
        return if list.len() == 1 {
            Ok(DeclaredAudience::Public)
        } else {
            Err(ConfigError::BadAudience {
                context: context.to_string(),
                reason: "`public` is the whole universe and cannot be combined with named readers".to_string(),
            })
        };
    }
    if list.is_empty() {
        return Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: "empty reader set".to_string(),
        });
    }
    if let Some(ph) = list.iter().find(|r| r.starts_with('$')) {
        return Err(ConfigError::BadAudience {
            context: context.to_string(),
            reason: format!("argument placeholder {ph:?} is only valid in an `includes`"),
        });
    }
    let mut readers = Vec::new();
    let mut groups = Vec::new();
    for entry in list {
        match entry.strip_prefix('@') {
            Some("") => {
                return Err(ConfigError::BadAudience {
                    context: context.to_string(),
                    reason: "`@` names no group".to_string(),
                });
            }
            Some(group) => groups.push(GroupName::new(group)),
            None => readers.push(ReaderId::new(entry)),
        }
    }
    DeclaredAudience::declared(readers, groups).map_err(|error| ConfigError::BadAudience {
        context: context.to_string(),
        reason: error.to_string(),
    })
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
version = 1

[[dynamic_resolver]]
name = "crm-acl"

[[tool]]
name = "lookup"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
delta = { audience = { resolver = "crm-acl", argument = "customer_id" } }

[[tool]]
name = "send"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
requires = { audience = { includes = { resolver = "crm-acl", argument = "customer_id" } } }
delta = {}

[[authority]]
name = "approver"
[authority.mandate]
can_cover_trust_to = "trusted"

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[deployment]
dispatch = "enforced"
confined_results = ["lookup"]
"#;

    #[test]
    fn declaration_only_policy_builds_the_engine_registry() {
        let config = Config::from_toml_str(DECLARATIONS).expect("the policy compiles");
        assert!(config.registry().tool(&ToolName::new("lookup")).is_some());
        assert!(config.registry().tool(&ToolName::new("send")).is_some());
        assert!(config.registry().authority(&AuthorityName::new("approver")).is_some());
        assert!(config.registry().sanitizer(&SanitizerName::new("pii")).is_some());
        assert_eq!(config.registry_config().tools.len(), 2);
    }

    #[test]
    fn every_inline_implementation_site_is_refused() {
        let cases = [
            (
                "tool",
                "version = 1\n[[tool]]\nname = \"t\"\nimplementation = { http = { url = \"https://tool.invalid\" } }\n",
            ),
            (
                "authority",
                "version = 1\n[[authority]]\nname = \"a\"\nimplementation = { builtin = \"approve\" }\n[authority.mandate]\ncan_cover_trust_to = \"trusted\"\n",
            ),
            (
                "sanitizer",
                "version = 1\n[[sanitizer]]\nname = \"s\"\non = [\"tool_output\"]\nimplementation = { builtin = \"hosted\" }\n[sanitizer.mandate]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n",
            ),
            (
                "dynamic resolver",
                "version = 1\n[[dynamic_resolver]]\nname = \"d\"\nresolver = { url = \"https://resolver.invalid\" }\n",
            ),
            (
                "cast",
                "version = 1\n[[cast]]\nname = \"c\"\nresolver = { url = \"https://cast.invalid\", may_cast = { trust = [\"trusted\"], audience = { cap = [\"public\"] } } }\n",
            ),
            (
                "membership resolver",
                "version = 1\n[membership]\nname = \"directory\"\nurl = \"https://directory.invalid\"\n",
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
    fn a_resolver_cast_registers_the_ceiling_the_policy_declares() {
        let policy = "version = 1\n\
             [[tool]]\nname = \"fetch\"\ntags = [\"web\"]\n\
             [[cast]]\nname = \"content-classifier\"\n\
             resolver = { may_cast = { trust = [\"suspicious\"], audience = { cap = [\"public\"] } } }\n\
             [cast.scope]\ntags = [\"web\"]\n";
        let config = Config::from_toml_str(policy).expect("a resolver cast loads");
        let cast = config
            .registry()
            .cast(&CastName::new("content-classifier"))
            .expect("content-classifier registers");
        match &cast.resolution {
            CastResolution::Resolver { may_cast } => {
                assert_eq!(may_cast.trust, vec![Trust::new(0)]);
                assert_eq!(may_cast.audience, DeclaredAudience::Public);
            }
            other => panic!("expected a resolver ceiling, got {other:?}"),
        }
    }

    #[test]
    fn a_may_cast_omitting_trust_admits_no_unresolved_trust() {
        let policy = "version = 1\n\
             [[tool]]\nname = \"fetch\"\ndelta = { audience = \"unknown\" }\n\
             [[cast]]\nname = \"audience-only\"\n\
             resolver = { may_cast = { audience = { cap = [\"public\"] } } }\n\
             [deployment]\nconfined_results = [\"fetch\"]\n";
        let config = Config::from_toml_str(policy).expect("an audience-only ceiling loads");
        assert!(matches!(
            &config.registry().cast(&CastName::new("audience-only")).expect("it registers").resolution,
            CastResolution::Resolver { may_cast } if may_cast.trust.is_empty()
        ));
    }

    #[test]
    fn a_cast_declaring_both_forms_or_neither_is_refused() {
        let both = "version = 1\n[[cast]]\nname = \"c\"\n\
             constant = { trust = \"suspicious\", audience = { exactly = [\"public\"] } }\n\
             resolver = { may_cast = { trust = [\"trusted\"], audience = { cap = [\"public\"] } } }\n";
        let neither = "version = 1\n[[cast]]\nname = \"c\"\n";
        for (case, policy) in [("both", both), ("neither", neither)] {
            assert!(
                matches!(
                    Config::from_toml_str(policy),
                    Err(ConfigError::BadImplementation { kind: "cast", .. })
                ),
                "a cast declaring {case} must be refused"
            );
        }
    }

    #[test]
    fn an_inline_binding_on_the_reserved_attest_schema_is_refused() {
        let policy = "version = 1\n\
             [[sanitizer]]\nname = \"attest-schema\"\non = [\"tool_output\"]\n\
             implementation = { url = \"https://attest.invalid\" }\n\
             [sanitizer.mandate]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n";
        assert!(matches!(
            Config::from_toml_str(policy),
            Err(ConfigError::ForbiddenInlineBinding { kind: "sanitizer", name }) if name == "attest-schema"
        ));
    }

    #[test]
    fn an_input_sanitizer_registers_with_its_scope_and_refuses_a_trust_mandate() {
        let policy = |mandate: &str| {
            format!(
                "version = 1\n\
                 [[tool]]\nname = \"post\"\ntags = [\"outbound\"]\ndelta = {{}}\n\
                 [[sanitizer]]\nname = \"redact\"\non = [\"tool_input\"]\n\
                 scope = {{ tags = [\"outbound\"] }}\n\
                 [sanitizer.mandate]\n{mandate}\n"
            )
        };
        let config = Config::from_toml_str(&policy(
            "audience = { from = { includes = [\"internal\"] }, to = { exactly = [\"partner\"] } }",
        ))
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
                &policy("audience = { from = { includes = [\"internal\"] }, to = { exactly = [\"partner\"] } }")
                    .replace("on = [\"tool_input\"]", "on = []")
            ),
            Err(ConfigError::NoSanitizerPoint { name }) if name == "redact"
        ));
    }

    #[test]
    fn a_membership_table_registers_the_resolver_by_name() {
        let with = Config::from_toml_str("version = 1\n[membership]\nname = \"directory\"\n").unwrap();
        assert_eq!(
            with.registry_config().membership,
            Some(MembershipResolverName::new("directory"))
        );
        let without = Config::from_toml_str("version = 1\n").unwrap();
        assert_eq!(without.registry_config().membership, None);
    }

    #[test]
    fn dynamic_bindings_require_a_registered_name() {
        let missing_name = DECLARATIONS.replace("[[dynamic_resolver]]\nname = \"crm-acl\"\n", "");
        assert!(matches!(
            Config::from_toml_str(&missing_name),
            Err(ConfigError::UnregisteredDynamicResolver { .. })
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
        let bindings = [
            "requires = { audience = { includes = [\"$to\"] } }\ndelta = {}",
            "requires = { audience = { includes = { resolver = \"directory\", argument = \"to\" } } }\ndelta = {}",
            "delta = { audience = { resolver = \"directory\", argument = \"to\" } }",
        ];
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
            format!(
                "version = 1\n[[dynamic_resolver]]\nname = \"directory\"\n[[tool]]\nname = \"send\"\n{parameters}\n{binding}\n"
            )
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
        let static_recipients = "version = 1\n[[tool]]\nname = \"send\"\nrequires = { audience = { includes = [\"finance\"] } }\ndelta = {}\n";
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
        let plain = Config::from_toml_str("version = 1\n[[tool]]\nname = \"t\"\ndelta = {}\n").expect("loads");
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
        let base = "version = 1\n[[tool]]\nname = \"t\"\ndelta = {}\n";
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
            "version = 1\n[[tool]]\nname = \"t\"\nparameters = { type = \"object\", properties = { value = { type = \"string\" } } }\n",
        )
        .expect("the schema compiles");
        let tool = config
            .registry()
            .tool(&ToolName::new("t"))
            .expect("the tool is registered");
        assert_eq!(
            tool.parameters.normalized(),
            serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": [],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn a_group_written_in_a_declaration_registers_and_labels_refuse_it() {
        let policy = r#"
version = 1

[membership]
name = "directory"

[[tool]]
name = "read"
delta = { audience = { exactly = ["auditor", "@team"] } }

[[tool]]
name = "send"
requires = { audience = { cap = ["@team"], includes = ["@board"] } }
delta = {}

[[authority]]
name = "officer"
[authority.mandate]
can_cover_readers = { may_add = ["@officers"] }

[[sanitizer]]
name = "declassify"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["@team"] } }

[[cast]]
name = "paranoid"
constant = { trust = "suspicious", audience = { exactly = ["@team"] } }

[deployment]
dispatch = "enforced"
confined_results = ["read", "send"]
"#;
        let config = Config::from_toml_str(policy).expect("group-writing declarations load");
        let registry = config.registry();
        assert_eq!(
            registry.groups(),
            [
                GroupName::new("board"),
                GroupName::new("officers"),
                GroupName::new("team")
            ]
        );
        let read = registry.tool(&ToolName::new("read")).expect("read registers");
        assert_eq!(
            read.delta.as_ref().and_then(|delta| delta.audience.as_ref()),
            Some(&AudienceDelta::Static(
                DeclaredAudience::declared([ReaderId::new("auditor")], [GroupName::new("team")]).unwrap()
            ))
        );
        let officer = registry
            .authority(&AuthorityName::new("officer"))
            .expect("officer registers");
        assert_eq!(
            officer.mandate.reader_ceiling,
            Some(DeclaredAudience::declared([], [GroupName::new("officers")]).unwrap())
        );
        assert!(matches!(
            &registry.sanitizer(&SanitizerName::new("declassify")).expect("declassify registers").transition,
            DeclaredTransition::Audience { to, .. } if to.groups().count() == 1
        ));
        assert!(matches!(
            &registry.cast(&CastName::new("paranoid")).expect("paranoid registers").resolution,
            CastResolution::Constant(constant) if constant.audience.groups().count() == 1
        ));

        let unregistered = policy.replace("[membership]\nname = \"directory\"\n", "");
        assert!(matches!(
            Config::from_toml_str(&unregistered),
            Err(ConfigError::Registry(LoadError::GroupWithoutResolver { .. }))
        ));

        for (case, replacement) in [
            ("bare @", "cap = [\"@\"]"),
            ("public beside a group", "cap = [\"public\", \"@team\"]"),
        ] {
            let malformed = policy.replace("cap = [\"@team\"]", replacement);
            assert!(
                matches!(Config::from_toml_str(&malformed), Err(ConfigError::BadAudience { .. })),
                "{case} loads"
            );
        }

        let base = "version = 1\n[membership]\nname = \"directory\"\n[[tool]]\nname = \"t\"\ndelta = {}\n";
        for site in [
            "[deployment]\nstarting_label = { audience = { exactly = [\"@team\"] } }\n",
            "[boundary]\naudience = { exactly = [\"@team\"] }\n",
        ] {
            assert!(
                matches!(
                    Config::from_toml_str(&format!("{base}{site}")),
                    Err(ConfigError::BadAudience { .. })
                ),
                "{site} loads"
            );
        }
    }
}
