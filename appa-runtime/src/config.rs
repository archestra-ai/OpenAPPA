//! The TOML policy loader: the spec's configuration dialect → the engine's [`RegistryConfig`]
//! plus the outer-layer implementation bindings.
//!
//! Configuration is data (spec §"The configuration surface"): four top-level kinds — tools,
//! authorities, sanitizers, casts — over an ordered trust chain. This module parses that dialect
//! into untrusted `Raw*` mirror structs (every one `deny_unknown_fields`, so a stale
//! `appa-proxy`-era key or a bare operator-less list is a loud load error), then converts each into
//! the engine's native, already-validated types. The split is deliberate: the **declaration** (what
//! an authority may cover, a cast's ceiling) is engine territory and lands in [`RegistryConfig`]; the
//! **implementation** (an HTTP resolver URL, a builtin name) is outer-layer territory and lands in the
//! per-name impl maps this module also returns. Parsing runs **both** the surface lints (unknown
//! rank name, operator-required audience, constant-XOR-resolver, implementation-required, closed
//! builtin names) **and** the engine's algebraic load lints (`Registry::build`: no-empty-mandate,
//! rank bounds, duplicates), so a returned [`Config`] is always loadable.

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

use appa_engine::authority::{
    AudienceTransition, Authority, Cast, CastCeiling, CastResolution, CastTarget, Mandate, Sanitizer, SanitizerPoints,
    Scope,
};
use appa_engine::contract::{
    AudienceRequirement, Delta, HistoryRequirement, LabelRequirements, RecipientSpec, Requires, ToolContract,
};
use appa_engine::fact::EffectKind;
use appa_engine::fact::ReturnPolicy;
use appa_engine::label::{Audience, Dim, DimValue, Label, ReaderId, Trust};
use appa_engine::names::{AuthorityName, CastName, MarkName, SanitizerName, TagName};
use appa_engine::registry::{LoadError, Registry, RegistryConfig, TrustChain};
use appa_engine::value::ToolName;

use crate::external::{BuiltinAuthority, BuiltinSanitizer};

const SUPPORTED_VERSION: u32 = 1;
const DEFAULT_TRUST_CHAIN: [&str; 2] = ["suspicious", "trusted"];
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const TIMEOUT_MS_RANGE: std::ops::RangeInclusive<u64> = 1..=300_000;

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
    #[error("bad sanitizer point {token:?}: expected \"tool_output\"")]
    UnknownSanitizerPoint { token: String },
    #[error(
        "sanitizer {name} registers on \"tool_input\": input-argument substitution is not implemented — an input sanitizer would sit inert, so it is refused, not accepted silently"
    )]
    InputSanitizerPoint { name: String },
    #[error("sanitizer {name} declares no application point (`on` is empty)")]
    NoSanitizerPoint { name: String },
    #[error("{kind} {name}: {reason}")]
    BadImplementation {
        kind: &'static str,
        name: String,
        reason: String,
    },
    #[error("resolver timeout_ms {found} out of range (1..=300000) in {context}")]
    TimeoutOutOfRange { found: u64, context: String },
    #[error("unknown {kind} builtin {name:?} (not a compiled-in implementation)")]
    UnknownBuiltin { kind: &'static str, name: String },
    #[error("tool {tool}: `parameters` must be a JSON-Schema object (a TOML table)")]
    ToolParametersNotAnObject { tool: String },
    #[error("registry rejected: {0}")]
    Registry(#[from] LoadError),
}

/// How an authority's rulings are produced at runtime. The engine holds only the mandate; this is
/// what the router dials to obtain a live decision. A `builtin` name is parsed into the closed
/// [`BuiltinAuthority`] set at load, so an unknown implementation cannot reach runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorityImpl {
    Builtin(BuiltinAuthority),
    HttpResolver { url: String, timeout_ms: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SanitizerImpl {
    Builtin(BuiltinSanitizer),
    HttpResolver { url: String, timeout_ms: u64 },
}

/// How a config-declared tool is executed south. Builtin test tools are registered programmatically;
/// a policy-declared tool binds an HTTP backend here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolImpl {
    Http { url: String, timeout_ms: u64 },
}

/// How a resolver-implemented cast decides a value's Unknown dimension (a constant cast needs no
/// runtime implementation — the engine resolves it directly).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CastImpl {
    HttpResolver { url: String, timeout_ms: u64 },
}

/// A fully parsed and **fully validated** policy: the immutable engine [`Registry`] plus the
/// outer layer's per-name implementation bindings. Parsing runs both the surface lints and the engine's
/// algebraic load lints, so a returned `Config` is always loadable.
#[derive(Clone, Debug)]
pub struct Config {
    registry: Registry,
    registry_config: RegistryConfig,
    boundary_label: Label,
    authority_impls: BTreeMap<AuthorityName, AuthorityImpl>,
    sanitizer_impls: BTreeMap<SanitizerName, SanitizerImpl>,
    cast_impls: BTreeMap<CastName, CastImpl>,
    tool_impls: BTreeMap<ToolName, ToolImpl>,
    tool_parameters: BTreeMap<ToolName, serde_json::Value>,
    child_return: ReturnPolicy,
}

impl Config {
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

        let mut tools = Vec::new();
        let mut tool_impls = BTreeMap::new();
        let mut tool_parameters = BTreeMap::new();
        for t in raw.tool {
            let (tool, imp, parameters) = t.convert(&trust_chain)?;
            if let Some(imp) = imp {
                tool_impls.insert(tool.name.clone(), imp);
            }
            if let Some(parameters) = parameters {
                tool_parameters.insert(tool.name.clone(), parameters);
            }
            tools.push(tool);
        }

        let mut authorities = Vec::new();
        let mut authority_impls = BTreeMap::new();
        for a in raw.authority {
            let (authority, imp) = a.convert(&trust_chain)?;
            authority_impls.insert(authority.name.clone(), imp);
            authorities.push(authority);
        }

        let mut sanitizers = Vec::new();
        let mut sanitizer_impls = BTreeMap::new();
        for s in raw.sanitizer {
            let (sanitizer, imp) = s.convert()?;
            sanitizer_impls.insert(sanitizer.name.clone(), imp);
            sanitizers.push(sanitizer);
        }

        let mut casts = Vec::new();
        let mut cast_impls = BTreeMap::new();
        for c in raw.cast {
            let (cast, imp) = c.convert(&trust_chain)?;
            if let Some(imp) = imp {
                cast_impls.insert(cast.name.clone(), imp);
            }
            casts.push(cast);
        }

        let registry_config = RegistryConfig {
            trust_chain,
            tools,
            authorities,
            sanitizers,
            casts,
        };
        // Run the engine's algebraic load lints now, so a returned Config is always loadable.
        let registry = Registry::build(registry_config.clone())?;

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
                match registry.sanitizer(&name) {
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

        Ok(Config {
            registry,
            registry_config,
            boundary_label,
            authority_impls,
            sanitizer_impls,
            cast_impls,
            tool_impls,
            tool_parameters,
            child_return,
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The label assigned to every north user turn (RP1) — a server policy default, never client
    /// supplied. Defaults to the neutral `L0`: top trust (the user is the trust boundary), public.
    pub fn boundary_label(&self) -> &Label {
        &self.boundary_label
    }

    pub fn registry_config(&self) -> &RegistryConfig {
        &self.registry_config
    }

    pub fn authority_impl(&self, name: &AuthorityName) -> Option<&AuthorityImpl> {
        self.authority_impls.get(name)
    }

    pub fn sanitizer_impl(&self, name: &SanitizerName) -> Option<&SanitizerImpl> {
        self.sanitizer_impls.get(name)
    }

    /// The fork return policy this configuration binds to every child (RP6): the `[child]` static
    /// binding when one is declared, else raw returns under the narrowing check.
    pub fn child_return_policy(&self) -> ReturnPolicy {
        self.child_return.clone()
    }

    pub fn cast_impl(&self, name: &CastName) -> Option<&CastImpl> {
        self.cast_impls.get(name)
    }

    pub fn tool_impl(&self, name: &ToolName) -> Option<&ToolImpl> {
        self.tool_impls.get(name)
    }

    /// The tool's declared argument schema (`parameters`), advertised verbatim in its wire schema.
    /// Advisory to the model only — the engine checks flows, never argument shapes.
    pub fn tool_parameters(&self, name: &ToolName) -> Option<&serde_json::Value> {
        self.tool_parameters.get(name)
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
    child: Option<RawChild>,
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
    implementation: Option<RawToolImpl>,
    parameters: Option<serde_json::Value>,
}

impl RawTool {
    fn convert(
        self,
        chain: &TrustChain,
    ) -> Result<(ToolContract, Option<ToolImpl>, Option<serde_json::Value>), ConfigError> {
        let ctx = || format!("tool {}", self.name);
        // No `delta` key at all = unannotated (results admitted at Unknown/Unknown, fail-closed);
        // `delta = {}` = the deliberate neutral annotation. The distinction is the whole point —
        // never collapse an omitted delta into the neutral one.
        let delta = self.delta.map(|d| d.convert(chain, &ctx())).transpose()?;
        let requires = match self.requires {
            Some(r) => r.convert(chain, &ctx())?,
            None => Requires::default(),
        };
        let imp = self.implementation.map(|i| i.convert(&self.name)).transpose()?;
        if let Some(parameters) = &self.parameters
            && !parameters.is_object()
        {
            return Err(ConfigError::ToolParametersNotAnObject { tool: self.name });
        }
        Ok((
            ToolContract {
                name: ToolName::new(self.name),
                tags: self.tags.into_iter().map(TagName::new).collect(),
                delta,
                emits: self.effects.into_iter().map(EffectKind::new).collect(),
                requires,
            },
            imp,
            self.parameters,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolImpl {
    http: RawHttp,
}

impl RawToolImpl {
    fn convert(self, name: &str) -> Result<ToolImpl, ConfigError> {
        Ok(ToolImpl::Http {
            url: self.http.url,
            timeout_ms: check_timeout(self.http.timeout_ms, &format!("tool {name}"))?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHttp {
    url: String,
    timeout_ms: Option<u64>,
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
            Some(RawDeltaAudience::Token(token)) if token == UNKNOWN_TOKEN => Some(Dim::Unknown),
            Some(RawDeltaAudience::Token(token)) => {
                return Err(ConfigError::BadAudience {
                    context: format!("{ctx} delta audience"),
                    reason: format!("expected {{ exactly = [...] }} or \"unknown\", found {token:?}"),
                });
            }
            Some(RawDeltaAudience::Exactly(a)) => Some(Dim::Known(parse_audience(
                &a.exactly,
                &format!("{ctx} delta audience"),
            )?)),
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
                audience.push(AudienceRequirement::Includes(parse_recipient_spec(
                    &inc,
                    &format!("{ctx} requires includes"),
                )?));
            }
            if let Some(cap) = a.cap {
                audience.push(AudienceRequirement::Cap(parse_audience(
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
    includes: Option<Vec<String>>,
    cap: Option<Vec<String>>,
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
    mandate: RawMandate,
    #[serde(default)]
    scope: RawScope,
    implementation: RawAuthorityImpl,
}

impl RawAuthority {
    fn convert(self, chain: &TrustChain) -> Result<(Authority, AuthorityImpl), ConfigError> {
        let ctx = format!("authority {}", self.name);
        let mandate = self.mandate.convert(chain, &ctx)?;
        let imp = self.implementation.convert(&self.name)?;
        Ok((
            Authority {
                name: AuthorityName::new(self.name),
                mandate,
                scope: Scope {
                    tags: self.scope.tags.into_iter().map(TagName::new).collect(),
                },
            },
            imp,
        ))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMandate {
    can_raise_trust_to: Option<String>,
    can_add_readers: Option<RawMayAdd>,
    #[serde(default)]
    can_waive: Vec<String>,
    #[serde(default)]
    attends: Vec<String>,
}

impl RawMandate {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<Mandate, ConfigError> {
        Ok(Mandate {
            trust_ceiling: self
                .can_raise_trust_to
                .map(|t| parse_trust(&t, chain, ctx))
                .transpose()?,
            reader_ceiling: self
                .can_add_readers
                .map(|r| parse_audience(&r.may_add, &format!("{ctx} can_add_readers")))
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
struct RawAuthorityImpl {
    builtin: Option<String>,
    resolver: Option<RawResolver>,
}

impl RawAuthorityImpl {
    fn convert(self, name: &str) -> Result<AuthorityImpl, ConfigError> {
        match (self.builtin, self.resolver) {
            (Some(builtin), None) => {
                let builtin = BuiltinAuthority::from_name(&builtin).ok_or(ConfigError::UnknownBuiltin {
                    kind: "authority",
                    name: builtin,
                })?;
                Ok(AuthorityImpl::Builtin(builtin))
            }
            (None, Some(resolver)) => resolver.convert_authority(name),
            (Some(_), Some(_)) => Err(bad_impl("authority", name, "declares both builtin and resolver")),
            (None, None) => Err(bad_impl("authority", name, "declares no implementation")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResolver {
    url: Option<String>,
    timeout_ms: Option<u64>,
}

impl RawResolver {
    fn convert_authority(self, name: &str) -> Result<AuthorityImpl, ConfigError> {
        let (url, timeout_ms) = self.convert_transform("authority", name)?;
        Ok(AuthorityImpl::HttpResolver { url, timeout_ms })
    }

    fn convert_transform(self, kind: &'static str, name: &str) -> Result<(String, u64), ConfigError> {
        match self.url {
            Some(url) => Ok((url, check_timeout(self.timeout_ms, &format!("{kind} {name}"))?)),
            None => Err(bad_impl(kind, name, "resolver needs `url`")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizer {
    name: String,
    on: Vec<String>,
    mandate: RawSanitizerMandate,
    implementation: RawTransformImpl,
}

impl RawSanitizer {
    fn convert(self) -> Result<(Sanitizer, SanitizerImpl), ConfigError> {
        let on = parse_points(&self.on, &self.name)?;
        let can_reduce = self.mandate.convert(&self.name)?;
        let imp = match (self.implementation.builtin, self.implementation.resolver) {
            (Some(builtin), None) => SanitizerImpl::Builtin(BuiltinSanitizer::from_name(&builtin).ok_or(
                ConfigError::UnknownBuiltin {
                    kind: "sanitizer",
                    name: builtin,
                },
            )?),
            (None, Some(resolver)) => {
                let (url, timeout_ms) = resolver.convert_transform("sanitizer", &self.name)?;
                SanitizerImpl::HttpResolver { url, timeout_ms }
            }
            (Some(_), Some(_)) => return Err(bad_impl("sanitizer", &self.name, "declares both builtin and resolver")),
            (None, None) => return Err(bad_impl("sanitizer", &self.name, "declares no implementation")),
        };
        Ok((
            Sanitizer {
                name: SanitizerName::new(self.name),
                on,
                can_reduce,
            },
            imp,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSanitizerMandate {
    audience: RawTransition,
}

impl RawSanitizerMandate {
    fn convert(self, name: &str) -> Result<AudienceTransition, ConfigError> {
        Ok(AudienceTransition {
            from_includes: parse_audience(&self.audience.from.includes, &format!("sanitizer {name} from"))?,
            to: parse_audience(&self.audience.to.exactly, &format!("sanitizer {name} to"))?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransition {
    from: RawIncludes,
    to: RawExactly,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIncludes {
    includes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransformImpl {
    builtin: Option<String>,
    resolver: Option<RawResolver>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCast {
    name: String,
    constant: Option<RawDimValue>,
    resolver: Option<RawCastResolver>,
}

impl RawCast {
    fn convert(self, chain: &TrustChain) -> Result<(Cast, Option<CastImpl>), ConfigError> {
        let ctx = format!("cast {}", self.name);
        match (self.constant, self.resolver) {
            (Some(dv), None) => Ok((
                Cast {
                    name: CastName::new(self.name),
                    resolution: CastResolution::Constant(dv.convert(chain, &ctx)?),
                },
                None,
            )),
            (None, Some(r)) => {
                let may_cast = r.may_cast.convert(chain, &ctx)?;
                let imp = CastImpl::HttpResolver {
                    url: r.url,
                    timeout_ms: check_timeout(r.timeout_ms, &ctx)?,
                };
                Ok((
                    Cast {
                        name: CastName::new(self.name),
                        resolution: CastResolution::Resolver { may_cast },
                    },
                    Some(imp),
                ))
            }
            (Some(_), Some(_)) => Err(bad_impl("cast", &self.name, "declares both constant and resolver")),
            (None, None) => Err(bad_impl("cast", &self.name, "declares neither constant nor resolver")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDimValue {
    trust: Option<String>,
    audience: Option<RawExactly>,
}

impl RawDimValue {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<CastTarget, ConfigError> {
        match (self.trust, self.audience) {
            (Some(t), None) => Ok(DimValue::Trust(parse_trust(&t, chain, ctx)?)),
            (None, Some(a)) => Ok(DimValue::Audience(parse_audience(&a.exactly, ctx)?)),
            _ => Err(ConfigError::BadAudience {
                context: ctx.to_string(),
                reason: "a dimension value names exactly one of `trust` or `audience`".to_string(),
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCastResolver {
    url: String,
    timeout_ms: Option<u64>,
    may_cast: RawCastCeiling,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCastCeiling {
    #[serde(default)]
    trust: Vec<String>,
    /// Each entry names a target audience with its operator (`{ exactly = [...] }`) — a bare list
    /// would be an operator-less set mention, which the dialect forbids.
    #[serde(default)]
    audience: Vec<RawExactly>,
}

impl RawCastCeiling {
    fn convert(self, chain: &TrustChain, ctx: &str) -> Result<CastCeiling, ConfigError> {
        Ok(CastCeiling {
            trust: self
                .trust
                .iter()
                .map(|t| parse_trust(t, chain, ctx))
                .collect::<Result<_, _>>()?,
            audience: self
                .audience
                .iter()
                .map(|set| parse_audience(&set.exactly, ctx))
                .collect::<Result<_, _>>()?,
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

fn check_timeout(timeout_ms: Option<u64>, context: &str) -> Result<u64, ConfigError> {
    let ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if TIMEOUT_MS_RANGE.contains(&ms) {
        Ok(ms)
    } else {
        Err(ConfigError::TimeoutOutOfRange {
            found: ms,
            context: context.to_string(),
        })
    }
}

fn parse_trust(name: &str, chain: &TrustChain, context: &str) -> Result<Trust, ConfigError> {
    chain.rank_of(name).ok_or_else(|| ConfigError::UnknownTrustRank {
        name: name.to_string(),
        context: context.to_string(),
    })
}

fn parse_audience(list: &[String], context: &str) -> Result<Audience, ConfigError> {
    if list.iter().any(|r| r == "public") {
        return if list.len() == 1 {
            Ok(Audience::Public)
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
    Ok(Audience::restricted(list.iter().map(ReaderId::new)))
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
    Ok(RecipientSpec::Static(parse_audience(list, context)?))
}

fn parse_points(tokens: &[String], name: &str) -> Result<SanitizerPoints, ConfigError> {
    let mut points = SanitizerPoints {
        input: false,
        output: false,
    };
    for token in tokens {
        match token.as_str() {
            "tool_input" => {
                return Err(ConfigError::InputSanitizerPoint { name: name.to_string() });
            }
            "tool_output" => points.output = true,
            other => {
                return Err(ConfigError::UnknownSanitizerPoint {
                    token: other.to_string(),
                });
            }
        }
    }
    if !points.output {
        return Err(ConfigError::NoSanitizerPoint { name: name.to_string() });
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKED: &str = r#"
version = 1

[[tool]]
name     = "get_ticket_from_crm"
requires = { trust = "trusted" }
delta    = { audience = { exactly = ["internal"] } }

[[tool]]
name     = "send_email"
requires = { trust = "trusted", audience = { includes = ["$recipient"] } }
effects  = ["egress"]
delta    = {}   # deliberately neutral: a delivery receipt carries nothing

[[tool]]
name     = "file_github_ticket"
requires = { trust = "trusted", audience = { includes = ["public"] } }
effects  = ["egress", "mutation"]
delta    = {}

[[sanitizer]]
name = "remove_pii"
on   = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"

[[authority]]
name = "human_in_the_loop_approver"
[authority.mandate]
can_add_readers = { may_add = ["public"] }
[authority.implementation]
builtin = "hitl"
"#;

    fn config(s: &str) -> Config {
        Config::from_toml_str(s).expect("fixture parses")
    }

    #[test]
    fn worked_example_builds_a_registry() {
        let cfg = config(WORKED);
        let reg = cfg.registry();

        let get = reg.tool(&ToolName::new("get_ticket_from_crm")).expect("tool present");
        assert_eq!(
            get.delta.as_ref().expect("declared delta").audience,
            Some(Dim::Known(Audience::restricted([ReaderId::new("internal")])))
        );
        assert_eq!(get.requires.label.trust_floor, Some(Trust::new(1)));

        let send = reg.tool(&ToolName::new("send_email")).expect("tool present");
        assert_eq!(send.emits, vec![EffectKind::new("egress")]);
        assert_eq!(
            send.requires.label.audience,
            vec![AudienceRequirement::Includes(RecipientSpec::Placeholder(
                "recipient".to_string()
            ))]
        );

        let file = reg.tool(&ToolName::new("file_github_ticket")).expect("tool present");
        assert_eq!(
            file.requires.label.audience,
            vec![AudienceRequirement::Includes(RecipientSpec::Static(Audience::Public))]
        );
    }

    #[test]
    fn a_sanitizer_registers_on_tool_output_only() {
        let cfg = config(WORKED);
        let reg = cfg.registry();
        let san = reg
            .sanitizer(&SanitizerName::new("remove_pii"))
            .expect("sanitizer present");
        assert!(!san.on.input && san.on.output);
        assert_eq!(san.can_reduce.to, Audience::Public);
        assert_eq!(
            cfg.sanitizer_impl(&SanitizerName::new("remove_pii")),
            Some(&SanitizerImpl::Builtin(BuiltinSanitizer::RedactEmail))
        );
    }

    #[test]
    fn a_tool_input_sanitizer_point_is_refused_not_inert() {
        assert!(matches!(
            err(
                "version = 1\n[[sanitizer]]\nname = \"pii\"\non = [\"tool_input\", \"tool_output\"]\n\
                 [sanitizer.mandate]\naudience = { from = { includes = [\"internal\"] }, to = { exactly = [\"public\"] } }\n\
                 [sanitizer.implementation]\nbuiltin = \"redact-email\"\n"
            ),
            ConfigError::InputSanitizerPoint { name } if name == "pii"
        ));
    }

    #[test]
    fn hitl_authority_impl_and_reader_ceiling() {
        let cfg = config(WORKED);
        let name = AuthorityName::new("human_in_the_loop_approver");
        assert_eq!(
            cfg.authority_impl(&name),
            Some(&AuthorityImpl::Builtin(BuiltinAuthority::Hitl))
        );
        let reg = cfg.registry();
        assert_eq!(
            reg.authority(&name).unwrap().mandate.reader_ceiling,
            Some(Audience::Public)
        );
    }

    #[test]
    fn omitted_delta_is_unannotated_and_empty_delta_is_neutral() {
        let cfg = config(
            r#"
version = 1
[[tool]]
name = "ping"
"#,
        );
        let reg = cfg.registry();
        let ping = reg.tool(&ToolName::new("ping")).unwrap();
        assert!(ping.delta.is_none());
        assert_eq!(ping.requires, Requires::default());

        let cfg = config(
            r#"
version = 1
[[tool]]
name = "ping"
delta = {}
"#,
        );
        let reg = cfg.registry();
        let ping = reg.tool(&ToolName::new("ping")).unwrap();
        assert_eq!(ping.delta, Some(Delta::NONE));
    }

    #[test]
    fn default_trust_chain_is_suspicious_then_trusted() {
        let cfg = config("version = 1\n");
        let chain = &cfg.registry_config().trust_chain;
        assert_eq!(chain.rank_of("suspicious"), Some(Trust::new(0)));
        assert_eq!(chain.rank_of("trusted"), Some(Trust::new(1)));
    }

    #[test]
    fn custom_trust_chain_orders_ranks() {
        let cfg = config(
            r#"
version = 1
trust_chain = ["unvetted", "vendor", "internal"]
[[tool]]
name = "t"
requires = { trust = "internal" }
delta = {}
"#,
        );
        let reg = cfg.registry();
        assert_eq!(
            reg.tool(&ToolName::new("t")).unwrap().requires.label.trust_floor,
            Some(Trust::new(2))
        );
    }

    #[test]
    fn constant_cast_needs_no_runtime_impl() {
        let cfg = config(
            r#"
version = 1
[[cast]]
name = "paranoid"
constant = { trust = "suspicious" }
"#,
        );
        assert_eq!(cfg.cast_impl(&CastName::new("paranoid")), None);
        assert!(cfg.registry().cast(&CastName::new("paranoid")).is_some());
    }

    #[test]
    fn resolver_cast_carries_ceiling_and_impl() {
        let cfg = config(
            r#"
version = 1
[[cast]]
name = "classifier"
resolver = { url = "https://c/resolve", timeout_ms = 10000, may_cast = { trust = ["suspicious"] } }
"#,
        );
        assert_eq!(
            cfg.cast_impl(&CastName::new("classifier")),
            Some(&CastImpl::HttpResolver {
                url: "https://c/resolve".to_string(),
                timeout_ms: 10_000
            })
        );
    }

    fn err(s: &str) -> ConfigError {
        Config::from_toml_str(s).expect_err("must reject")
    }

    #[test]
    fn legacy_proxy_dialect_is_rejected() {
        for legacy in [
            "upstream_base_url = \"https://x\"\nversion = 1\n",
            "version = 1\n[contracts.trajectory]\ntrust = \"trusted\"\n",
            "version = 1\n[[contracts.tool]]\nname = \"t\"\n",
            "version = 1\n[[tool]]\nname = \"t\"\noutput = { trust = \"trusted\" }\n",
            "version = 1\n[[authority]]\nname = \"a\"\nrule = \"allow\"\n",
        ] {
            assert!(matches!(err(legacy), ConfigError::Parse(_)), "should reject: {legacy}");
        }
    }

    #[test]
    fn bare_operatorless_audience_list_is_rejected() {
        assert!(matches!(
            err("version = 1\n[[tool]]\nname = \"t\"\nrequires = { audience = [\"finance\"] }\n"),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    fn unknown_trust_rank_is_rejected() {
        assert!(matches!(
            err("version = 1\n[[tool]]\nname = \"t\"\nrequires = { trust = \"godmode\" }\n"),
            ConfigError::UnknownTrustRank { name, .. } if name == "godmode"
        ));
    }

    #[test]
    fn unknown_token_declares_a_pending_cast_dimension() {
        let cfg = Config::from_toml_str(
            "version = 1\n[[tool]]\nname = \"scan\"\ndelta = { trust = \"unknown\" }\n\
             [[tool]]\nname = \"probe\"\ndelta = { audience = \"unknown\" }\n",
        )
        .unwrap();
        let reg = cfg.registry();
        let scan = reg.tool(&ToolName::new("scan")).unwrap();
        assert_eq!(scan.delta.as_ref().expect("declared delta").trust, Some(Dim::Unknown));
        let probe = reg.tool(&ToolName::new("probe")).unwrap();
        assert_eq!(
            probe.delta.as_ref().expect("declared delta").audience,
            Some(Dim::Unknown)
        );
    }

    const PII: &str = r#"
[[sanitizer]]
name = "pii"
on   = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#;

    #[test]
    fn child_return_sanitizer_must_be_a_registered_output_sanitizer() {
        let cfg = Config::from_toml_str(&format!("version = 1\n[child]\nreturn_sanitizer = \"pii\"\n{PII}")).unwrap();
        assert!(matches!(
            cfg.child_return_policy(),
            ReturnPolicy::Sanitized(name) if name == SanitizerName::new("pii")
        ));

        assert!(matches!(
            err("version = 1\n[child]\nreturn_sanitizer = \"ghost\"\n"),
            ConfigError::BadImplementation {
                kind: "child return_sanitizer",
                ..
            }
        ));
    }

    #[test]
    fn child_return_binding_is_declared_or_absent_never_empty() {
        assert!(matches!(
            err("version = 1\n[child]\n"),
            ConfigError::BadImplementation { kind: "child", .. }
        ));

        let cfg = Config::from_toml_str("version = 1\n").unwrap();
        assert!(matches!(cfg.child_return_policy(), ReturnPolicy::Raw));
    }

    #[test]
    fn a_trust_rank_named_unknown_is_reserved() {
        assert!(matches!(
            err("version = 1\ntrust_chain = [\"unknown\", \"trusted\"]\n"),
            ConfigError::ReservedRankName
        ));
    }

    #[test]
    fn a_bare_delta_audience_string_other_than_unknown_is_rejected() {
        assert!(matches!(
            err("version = 1\n[[tool]]\nname = \"t\"\ndelta = { audience = \"internal\" }\n"),
            ConfigError::BadAudience { .. }
        ));
    }

    #[test]
    fn public_cannot_mix_with_named_readers() {
        assert!(matches!(
            err(
                "version = 1\n[[tool]]\nname = \"t\"\ndelta = { audience = { exactly = [\"public\", \"finance\"] } }\n"
            ),
            ConfigError::BadAudience { .. }
        ));
    }

    #[test]
    fn placeholder_only_in_includes() {
        assert!(matches!(
            err("version = 1\n[[tool]]\nname = \"t\"\ndelta = { audience = { exactly = [\"$recipient\"] } }\n"),
            ConfigError::BadAudience { .. }
        ));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        assert!(matches!(
            err("version = 2\n"),
            ConfigError::UnsupportedVersion { found: 2 }
        ));
    }

    #[test]
    fn cast_must_be_constant_xor_resolver() {
        assert!(matches!(
            err("version = 1\n[[cast]]\nname = \"c\"\n"),
            ConfigError::BadImplementation { .. }
        ));
        assert!(matches!(
            err(r#"version = 1
[[cast]]
name = "c"
constant = { trust = "suspicious" }
resolver = { url = "x", may_cast = { trust = ["suspicious"] } }
"#),
            ConfigError::BadImplementation { .. }
        ));
    }

    #[test]
    fn builtin_approve_may_back_a_cover_bearing_mandate() {
        let cfg = config(
            "version = 1\n[[authority]]\nname = \"self\"\n[authority.mandate]\ncan_raise_trust_to = \"trusted\"\n[authority.implementation]\nbuiltin = \"approve\"\n",
        );
        assert_eq!(
            cfg.authority_impl(&AuthorityName::new("self")),
            Some(&AuthorityImpl::Builtin(BuiltinAuthority::Approve))
        );
    }

    #[test]
    fn authority_without_implementation_is_rejected() {
        assert!(matches!(
            err("version = 1\n[[authority]]\nname = \"a\"\n[authority.mandate]\ncan_waive = [\"x\"]\n"),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    fn unknown_builtin_names_are_rejected() {
        assert!(matches!(
            err("version = 1\n[[authority]]\nname = \"a\"\n[authority.mandate]\ncan_waive = [\"x\"]\n[authority.implementation]\nbuiltin = \"yolo-approve-all\"\n"),
            ConfigError::UnknownBuiltin { kind: "authority", name } if name == "yolo-approve-all"
        ));
        assert!(matches!(
            err(r#"version = 1
[[sanitizer]]
name = "s"
on = ["tool_output"]
[sanitizer.mandate]
audience = { from = { includes = ["a"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "scrub-everything"
"#),
            ConfigError::UnknownBuiltin { kind: "sanitizer", name } if name == "scrub-everything"
        ));
    }

    #[test]
    fn a_resolver_without_an_endpoint_is_rejected() {
        assert!(matches!(
            err(
                "version = 1\n[[authority]]\nname = \"a\"\n[authority.mandate]\ncan_waive = [\"x\"]\n[authority.implementation]\nresolver = { timeout_ms = 5000 }\n"
            ),
            ConfigError::BadImplementation { kind: "authority", .. }
        ));
    }

    #[test]
    fn builtin_hitl_may_back_a_cover_bearing_mandate() {
        let cfg = config(
            "version = 1\n[[authority]]\nname = \"a\"\n[authority.mandate]\ncan_raise_trust_to = \"trusted\"\n[authority.implementation]\nbuiltin = \"hitl\"\n",
        );
        assert_eq!(
            cfg.authority_impl(&AuthorityName::new("a")),
            Some(&AuthorityImpl::Builtin(BuiltinAuthority::Hitl))
        );
    }

    #[test]
    fn may_cast_audience_requires_an_operator() {
        assert!(matches!(
            err(r#"version = 1
[[cast]]
name = "c"
resolver = { url = "https://c", may_cast = { audience = [["public"]] } }
"#),
            ConfigError::Parse(_)
        ));
        let cfg = config(
            r#"
version = 1
[[cast]]
name = "c"
resolver = { url = "https://c", may_cast = { audience = [{ exactly = ["public"] }] } }
"#,
        );
        assert!(cfg.cast_impl(&CastName::new("c")).is_some());
    }

    #[test]
    fn algebraic_registry_lints_run_at_parse() {
        assert!(matches!(
            err(
                "version = 1\n[[authority]]\nname = \"noop\"\n[authority.mandate]\n[authority.implementation]\nbuiltin = \"approve\"\n"
            ),
            ConfigError::Registry(_)
        ));
    }

    #[test]
    fn boundary_label_defaults_to_top_trust_public() {
        let cfg = config("version = 1\n");
        assert_eq!(
            cfg.boundary_label(),
            &Label::new(Dim::Known(Trust::new(1)), Dim::Known(Audience::Public))
        );
    }

    #[test]
    fn boundary_label_reads_a_configured_team_private_default() {
        let cfg = config(
            r#"
version = 1
[boundary]
trust = "trusted"
audience = { exactly = ["operator", "sre-team"] }
"#,
        );
        assert_eq!(
            cfg.boundary_label(),
            &Label::new(
                Dim::Known(Trust::new(1)),
                Dim::Known(Audience::restricted([
                    ReaderId::new("operator"),
                    ReaderId::new("sre-team")
                ]))
            )
        );
    }

    #[test]
    fn tool_http_implementation_binds_a_backend() {
        let cfg = config(
            r#"
version = 1
[[tool]]
name = "fetch"
requires = { trust = "trusted" }
delta = {}
[tool.implementation]
http = { url = "https://tools/fetch", timeout_ms = 5000 }
"#,
        );
        assert_eq!(
            cfg.tool_impl(&ToolName::new("fetch")),
            Some(&ToolImpl::Http {
                url: "https://tools/fetch".to_string(),
                timeout_ms: 5000
            })
        );
    }

    #[test]
    fn sanitizer_point_token_must_be_exact() {
        assert!(matches!(
            err(r#"version = 1
[[sanitizer]]
name = "s"
on = ["tool_stdin"]
[sanitizer.mandate]
audience = { from = { includes = ["a"] }, to = { exactly = ["public"] } }
[sanitizer.implementation]
builtin = "redact-email"
"#),
            ConfigError::UnknownSanitizerPoint { token } if token == "tool_stdin"
        ));
    }

    #[test]
    fn resolver_timeout_out_of_range_is_rejected() {
        assert!(matches!(
            err(r#"version = 1
[[authority]]
name = "a"
[authority.mandate]
can_waive = ["x"]
[authority.implementation]
resolver = { url = "https://a", timeout_ms = 999999 }
"#),
            ConfigError::TimeoutOutOfRange { found: 999999, .. }
        ));
    }

    #[test]
    fn documented_configs_parse() {
        let doc = include_str!("../../website/content/docs/contracts.md");
        let mut blocks = 0;
        for block in doc.split("```toml").skip(1) {
            let toml = block.split("```").next().unwrap_or("");
            if toml.contains("version = 1") {
                Config::from_toml_str(toml).unwrap_or_else(|e| panic!("documented config fails to parse: {e}\n{toml}"));
                blocks += 1;
            }
        }
        assert!(blocks >= 2, "expected the worked + k8s configs, found {blocks}");
    }
}
