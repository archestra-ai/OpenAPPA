//! Configuration: one immutable `appa.toml` with the policy and the
//! external bindings. The harness adapter is a CLI flag on the
//! binary, not configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    policy: PolicyFile,
    pub externals: Externals,
    included_batteries: Vec<String>,
}

/// The runtime's own environment namespace: its wiring (`APPA_CONFIG`, `APPA_DB`,
/// `APPA_GATE`, …) and every secret a `token_env` names. Nothing in it reaches a child
/// process, so a bearer token this runtime sends and a gate variable that would recurse a
/// consult both stay in this process.
pub(crate) const RUNTIME_VARIABLE_PREFIX: &str = "APPA_";

/// The namespace a `command` external's own credential lives in — a battery's provider
/// token, which the runtime never reads and never sends. A child inherits nothing of the
/// runtime's namespace by default; the one variable its binding names is put back, and only
/// from this namespace, so a credential reaches the one command that reads it and no other.
/// A `url` binding's `token_env` may not name a variable here, so the passthrough cannot
/// become a way to hand a subprocess a secret this runtime holds.
pub(crate) const PROVIDER_CREDENTIAL_PREFIX: &str = "APPA_PROVIDER_";

/// The effective policy file: deterministic TOML bytes after includes
/// compose, and the policy value parsed from those bytes. The stored
/// value and bytes always describe the same deployment.
#[derive(Debug, Clone)]
pub struct PolicyFile {
    bytes: Vec<u8>,
    value: toml::Value,
}

impl PolicyFile {
    fn new(bytes: Vec<u8>, value: toml::Value) -> PolicyFile {
        PolicyFile { bytes, value }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn value(&self) -> &toml::Value {
        &self.value
    }
}

/// The declared bindings, as a deployment writes them: one entry per registered
/// component under its kind's section, and the deployment-wide tables. This is the one
/// input shape both load paths share — `appa.toml` composes into it, and an embedded host
/// hands it in — and it carries no secret: a token is named by its `APPA_*` variable and
/// resolved from the environment when the deployment validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalBindings {
    pub timeout_ms: u64,
    pub review_timeout_ms: u64,
    pub max_body_bytes: usize,
    pub authorities: BTreeMap<String, Binding>,
    pub sanitizers: BTreeMap<String, Binding>,
    pub annotators: BTreeMap<String, Binding>,
    pub audience: BTreeMap<String, Binding>,
    pub identity: BTreeMap<String, Binding>,
    pub claude_code: ClaudeCode,
    pub llm: Option<LlmBinding>,
}

impl ExternalBindings {
    /// No bindings and the default review window; the two mandatory settings are the
    /// caller's.
    pub fn new(timeout: Duration, max_body_bytes: usize) -> ExternalBindings {
        ExternalBindings {
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            review_timeout_ms: default_review_timeout_ms(),
            max_body_bytes,
            authorities: BTreeMap::new(),
            sanitizers: BTreeMap::new(),
            annotators: BTreeMap::new(),
            audience: BTreeMap::new(),
            identity: BTreeMap::new(),
            claude_code: ClaudeCode::default(),
            llm: None,
        }
    }
}

/// How one registered component is served, as declared: exactly one of an HTTP endpoint,
/// a local command, or a builtin name. A command's `cwd` is the directory of the file that
/// declared it — an embedded host supplies an absolute one itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Url {
        url: String,
        token_env: Option<String>,
    },
    Command {
        argv: Vec<String>,
        cwd: PathBuf,
        token_env: Option<String>,
    },
    Builtin(String),
}

/// The `[externals.llm]` table as declared: the one API-key model profile every
/// `builtin = "llm"` entry in the deployment consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmBinding {
    pub provider: LlmProvider,
    pub model: String,
    pub url: Option<String>,
    pub token_env: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_concurrent: Option<u32>,
}

/// The API providers the `llm` builtin speaks to. Closed: the transport is compiled in
/// per provider, so a name outside this set is a configuration refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Anthropic,
    OpenAi,
    Gemini,
    Ollama,
}

impl LlmProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            LlmProvider::Anthropic => "anthropic",
            LlmProvider::OpenAi => "openai",
            LlmProvider::Gemini => "gemini",
            LlmProvider::Ollama => "ollama",
        }
    }

    fn parse(name: &str) -> Option<LlmProvider> {
        match name {
            "anthropic" => Some(LlmProvider::Anthropic),
            "openai" => Some(LlmProvider::OpenAi),
            "gemini" => Some(LlmProvider::Gemini),
            "ollama" => Some(LlmProvider::Ollama),
            _ => None,
        }
    }
}

/// The registered externals, validated and resolved: the implementation every
/// bound component runs on, and the deployment-wide tables. Every machine consult
/// carries an explicit timeout and byte cap and fails closed.
#[derive(Debug, Clone)]
pub struct Externals {
    pub timeout: Duration,
    /// How long a human review may stay open before the runtime treats
    /// it as no answer. Deliberately separate from
    /// `timeout`, which bounds a machine consult: a person reads the
    /// arguments and thinks. It also bounds the MCP session's
    /// keep-alive, because a review outliving its session cannot be
    /// answered.
    pub review_timeout: Duration,
    pub max_body_bytes: usize,
    pub authorities: BTreeMap<String, Implementation>,
    pub sanitizers: BTreeMap<String, Implementation>,
    /// One implementation per policy-declared `[[annotator]]` that names no `builtin` on
    /// its declaration. An Annotator that carries a stock builtin takes no entry here.
    pub annotators: BTreeMap<String, AnnotatorImplementation>,
    /// One implementation per audience source provider the policy's `[audience.*]` tables
    /// reference, under the provider's name.
    pub audience: BTreeMap<String, Implementation>,
    /// The one custom identity implementation the policy's `[identity]` selects, under its
    /// name. The shipped `verified-email` implementation is deterministic and takes no entry.
    pub identity: BTreeMap<String, Implementation>,
    /// Deployment knobs for the stock `claude-code` builtin.
    pub claude_code: ClaudeCode,
    /// The profile the stock `llm` builtin consults, where the deployment declares one.
    pub llm: Option<LlmProfile>,
}

impl Externals {
    /// How many `llm` consults this deployment lets run at once: `max_concurrent` of its
    /// profile, none without one.
    pub(crate) fn llm_bound(&self) -> usize {
        self.llm.as_ref().map_or(0, |profile| profile.max_concurrent)
    }
}

/// How this deployment runs the stock `claude-code` builtin. `command` overrides the
/// executable (a service environment often strips `PATH`); `model` pins the model the
/// consult runs on; `timeout` bounds one consult.
///
/// A model consult runs for tens of seconds, so it owns its budget. `externals.timeout_ms`
/// bounds an HTTP round trip and never applies here: a deployment that names no
/// `timeout_ms` gets a default sized for a model call, not the shared one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCode {
    pub command: PathBuf,
    pub model: String,
    pub timeout: Duration,
}

/// The budget one `claude-code` consult gets when the deployment names none.
const DEFAULT_CLAUDE_CODE_TIMEOUT: Duration = Duration::from_secs(60);

impl Default for ClaudeCode {
    /// The usable defaults every construction path shares — an embedded host building
    /// its bindings by hand gets the same `claude` on `PATH`, `sonnet` alias, and consult
    /// budget the file loader fills in, never an empty command.
    fn default() -> ClaudeCode {
        ClaudeCode {
            command: "claude".into(),
            model: "sonnet".to_string(),
            timeout: DEFAULT_CLAUDE_CODE_TIMEOUT,
        }
    }
}

/// The `[externals.llm]` profile, validated: its endpoint rules are a `url` binding's
/// (`https` anywhere, cleartext `http` only to loopback, no credentials in the URL, the
/// token from an `APPA_*` variable). `url` is `None` where the provider's own API host
/// serves; `timeout` is the profile's own consult budget, `None` meaning the shared one.
#[derive(Debug, Clone)]
pub struct LlmProfile {
    pub provider: LlmProvider,
    pub model: String,
    pub url: Option<String>,
    pub token: Option<Token>,
    pub timeout: Option<Duration>,
    pub max_concurrent: usize,
}

const DEFAULT_LLM_CONCURRENCY: usize = 4;

/// How one bound component is served — an HTTP endpoint, a local command, or a builtin
/// name — a closed choice per entry, the same for every kind.
#[derive(Debug, Clone)]
pub enum Implementation {
    Resolver(Endpoint),
    Command(ResolverCommand),
    Builtin(String),
}

/// How one deployment-bound Annotator runs: an HTTP endpoint or a local command. A stock
/// builtin is named on the policy declaration, never bound here.
#[derive(Debug, Clone)]
pub enum AnnotatorImplementation {
    Resolver(Endpoint),
    Command(ResolverCommand),
}

/// A command binding's argv, the directory of the config that declared it, and the one
/// `APPA_PROVIDER_*` variable its child inherits. The name is carried, never the value: the
/// runtime forwards the variable at spawn and never reads the credential itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub token_env: Option<String>,
}

pub const CLAUDE_CODE_BUILTIN: &str = "claude-code";
pub const LLM_BUILTIN: &str = "llm";

/// One external endpoint: a validated URL plus its bearer token, if
/// the service needs one. `https` reaches anywhere; `http` only
/// loopback — a bearer token MUST NOT cross the network in the clear.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub token: Option<Token>,
    host: EndpointHost,
}

/// Where an endpoint's host is. A request to `Loopback` must not leave this
/// machine, so it never goes through a proxy: cleartext is permitted only
/// there, and the bearer token it carries stays on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointHost {
    Loopback,
    Remote,
}

impl Endpoint {
    /// The endpoint at `url`. The host is derived here rather than taken from the
    /// caller, so no endpoint can name a reach that disagrees with its own URL.
    pub fn new(url: String, token: Option<Token>) -> Endpoint {
        // An unparsable URL never reaches this far — `validated_url` refuses it — and a
        // request to one fails anyway. Withholding the proxy is the safe reading of it.
        let remote = reqwest::Url::parse(&url).is_ok_and(|parsed| !is_loopback(&parsed));
        let host = match remote {
            true => EndpointHost::Remote,
            false => EndpointHost::Loopback,
        };
        Endpoint { url, token, host }
    }

    pub(crate) fn host(&self) -> EndpointHost {
        self.host
    }
}

/// A bearer token resolved from an `APPA_*` environment variable.
/// The value never appears in logs: `Debug` redacts it.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub(crate) fn new(value: String) -> Token {
        Token(value)
    }

    pub(crate) fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Unreadable { path: String, source: std::io::Error },
    #[error("cannot parse {path}: {source}")]
    Unparsable { path: String, source: toml::de::Error },
    #[error("cannot parse the composed policy: {source}")]
    UnparsablePolicy { source: toml::de::Error },
    #[error("the composed policy does not render as a policy file: {source}")]
    UnrenderablePolicy { source: toml::ser::Error },
    #[error("root policy.version must be an integer")]
    InvalidPolicyVersion,
    #[error("include path {path:?} must be relative to the root config")]
    AbsoluteInclude { path: String },
    #[error("the root config includes {path:?} more than once")]
    DuplicateInclude { path: String },
    #[error("included config {path} has unsupported top-level field {field:?}")]
    IncludedTopLevel { path: String, field: String },
    #[error("included config {path} cannot set policy field {field:?}")]
    IncludedPolicyField { path: String, field: String },
    #[error("included config {path} cannot set externals field {field:?}")]
    IncludedExternalsField { path: String, field: String },
    #[error("included config {path} uses policy version {found}, but the root uses {root}")]
    IncludedVersion { path: String, root: i64, found: i64 },
    #[error("included config {path} repeats [externals.{section}] entry {name:?}")]
    DuplicateExternal {
        path: String,
        section: String,
        name: String,
    },
    #[error("invalid internal composition metadata: {reason}")]
    InvalidComposedMetadata { reason: String },
    #[error("the {section} endpoint {name:?} has an invalid url: {url}")]
    InvalidEndpoint {
        section: &'static str,
        name: String,
        url: String,
    },
    #[error("the {section} endpoint {name:?} uses cleartext http to a non-loopback host: {url}")]
    CleartextEndpoint {
        section: &'static str,
        name: String,
        url: String,
    },
    #[error("the {section} endpoint {name:?} embeds credentials in its url")]
    CredentialsInUrl { section: &'static str, name: String },
    #[error("the {section} endpoint {name:?} names {var}, which must start with APPA_")]
    ForeignSecretVariable {
        section: &'static str,
        name: String,
        var: String,
    },
    #[error(
        "the {section} endpoint {name:?} names {var}, which a command child can inherit: a token this runtime sends itself needs a variable outside {prefix}"
    )]
    ChildCredentialVariable {
        section: &'static str,
        name: String,
        var: String,
        prefix: &'static str,
    },
    #[error("the {section} command {name:?} names {var}, and only a {prefix} variable reaches its child")]
    CommandCredentialVariable {
        section: &'static str,
        name: String,
        var: String,
        prefix: &'static str,
    },
    #[error("the {section} endpoint {name:?} names {var}, which is not set")]
    MissingSecret {
        section: &'static str,
        name: String,
        var: String,
    },
    #[error("externals.timeout_ms must be greater than zero")]
    ZeroTimeout,
    #[error("externals.review_timeout_ms must be greater than zero")]
    ZeroReviewTimeout,
    #[error("externals.max_body_bytes must be greater than zero")]
    ZeroByteCap,
    #[error("externals.llm.max_concurrent must be greater than zero")]
    ZeroConcurrency,
    #[error("externals.llm.provider {provider:?} is not one of anthropic, openai, gemini, ollama")]
    InvalidLlmProvider { provider: String },
    #[error("externals.llm.provider {provider} needs a token_env: only ollama runs without a key")]
    LlmTokenRequired { provider: &'static str },
    #[error("the {section} entry {name:?} must name exactly one implementation, and only url takes token_env")]
    ImplementationChoice { section: &'static str, name: String },
    #[error("the {section} entry {name:?} cannot be builtin")]
    BuiltinNotAllowed { section: &'static str, name: String },
    #[error("the {section} entry {name:?} names the builtin {builtin:?}, which is not a valid implementation name")]
    InvalidBuiltinName {
        section: &'static str,
        name: String,
        builtin: String,
    },
    #[error("the {section} entry {name:?} names the builtin \"llm\", but the deployment declares no [externals.llm]")]
    LlmNotConfigured { section: &'static str, name: String },
    #[error("the {section} entry {name:?} command must contain at least one non-empty argument")]
    InvalidCommand { section: &'static str, name: String },
    #[error("the {section} entry {name:?} uses a local command, which this platform does not support")]
    UnsupportedCommandPlatform { section: &'static str, name: String },
    #[error(
        "the {section} entry {name:?} names the builtin \"claude-code\", which runs a local process this platform does not support"
    )]
    UnsupportedClaudeCodePlatform { section: &'static str, name: String },
    #[error("the embedded {section} command {name:?} has a relative working directory")]
    RelativeCommandCwd { section: &'static str, name: String },
    #[error("embedded setting {field} cannot be represented in TOML")]
    UnrepresentableEmbeddedSetting { field: &'static str },
}

/// The five sections a component binds under. Every section takes the same transports;
/// which builtin names a section accepts is the one difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Authorities,
    Sanitizers,
    Annotators,
    /// One audience source per provider: `[externals.audience.<provider>]`.
    Audience,
    /// The one custom identity implementation: `[externals.identity.<name>]`.
    Identity,
}

impl Section {
    pub(crate) const ALL: [Section; 5] = [
        Section::Authorities,
        Section::Sanitizers,
        Section::Annotators,
        Section::Audience,
        Section::Identity,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Section::Authorities => "authorities",
            Section::Sanitizers => "sanitizers",
            Section::Annotators => "annotators",
            Section::Audience => "audience",
            Section::Identity => "identity",
        }
    }

    fn parse(name: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|section| section.name() == name)
    }

    /// The origin key one command entry records: `<section>.<name>`.
    fn origin_key(self, name: &str) -> String {
        format!("{}.{name}", self.name())
    }

    /// Whether `builtin` is a name this section may bind. Authorities and sanitizers take
    /// the stock names, the model builtins, and any module-grammar name (the module's
    /// presence is checked when the deployment opens); an Annotator names a stock builtin
    /// on its policy declaration instead, and an audience source or a custom identity
    /// implementation is never a builtin.
    fn check_builtin(self, name: &str, builtin: &str) -> Result<(), ConfigError> {
        let allowed = match self {
            Section::Annotators | Section::Audience | Section::Identity => {
                return Err(ConfigError::BuiltinNotAllowed {
                    section: self.name(),
                    name: name.to_string(),
                });
            }
            Section::Authorities | Section::Sanitizers => crate::builtins::valid_implementation_name(builtin),
        };
        // The subscription transport is a local process under a process group, which only
        // Unix provides; like a `command`, it is refused where it cannot be cleaned up.
        #[cfg(not(unix))]
        if builtin == CLAUDE_CODE_BUILTIN {
            return Err(ConfigError::UnsupportedClaudeCodePlatform {
                section: self.name(),
                name: name.to_string(),
            });
        }
        if !allowed {
            return Err(ConfigError::InvalidBuiltinName {
                section: self.name(),
                name: name.to_string(),
                builtin: builtin.to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    include: Vec<String>,
    appa_composed: Option<RawComposedMetadata>,
    policy: toml::Value,
    externals: RawExternals,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComposedMetadata {
    command_cwd: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExternals {
    timeout_ms: u64,
    #[serde(default = "default_review_timeout_ms")]
    review_timeout_ms: u64,
    max_body_bytes: usize,
    #[serde(default)]
    authorities: BTreeMap<String, RawBinding>,
    #[serde(default)]
    sanitizers: BTreeMap<String, RawBinding>,
    #[serde(default)]
    annotators: BTreeMap<String, RawBinding>,
    #[serde(default)]
    audience: BTreeMap<String, RawBinding>,
    #[serde(default)]
    identity: BTreeMap<String, RawBinding>,
    claude_code: Option<RawClaudeCode>,
    llm: Option<RawLlm>,
}

impl RawExternals {
    fn section(&self, section: Section) -> &BTreeMap<String, RawBinding> {
        match section {
            Section::Authorities => &self.authorities,
            Section::Sanitizers => &self.sanitizers,
            Section::Annotators => &self.annotators,
            Section::Audience => &self.audience,
            Section::Identity => &self.identity,
        }
    }

    /// Every command entry, by origin key.
    fn command_keys(&self) -> std::collections::BTreeSet<String> {
        Section::ALL
            .into_iter()
            .flat_map(|section| {
                self.section(section)
                    .iter()
                    .filter(|(_, binding)| binding.command.is_some())
                    .map(move |(name, _)| section.origin_key(name))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaudeCode {
    command: Option<String>,
    model: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLlm {
    provider: String,
    model: String,
    url: Option<String>,
    token_env: Option<String>,
    timeout_ms: Option<u64>,
    max_concurrent: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    url: Option<String>,
    token_env: Option<String>,
    builtin: Option<String>,
    command: Option<Vec<String>>,
}

fn default_review_timeout_ms() -> u64 {
    600_000
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        Config::load_from(path, &[])
    }

    /// Load `path`, resolving `batteries/<name>/appa.toml` includes against
    /// `battery_dirs` in the given order before the root config directory.
    pub fn load_from(path: &Path, battery_dirs: &[PathBuf]) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Unreadable {
            path: path.display().to_string(),
            source,
        })?;
        let root: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::Unparsable {
            path: path.display().to_string(),
            source,
        })?;
        let source_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let source_dir = std::fs::canonicalize(source_dir).map_err(|source| ConfigError::Unreadable {
            path: source_dir.display().to_string(),
            source,
        })?;
        let mut document: toml::Value = toml::from_str(&text).map_err(|source| ConfigError::Unparsable {
            path: path.display().to_string(),
            source,
        })?;
        document
            .as_table_mut()
            .expect("RawConfig parsed the root as a table")
            .remove("include");

        let root_version = policy_version(&root.policy).ok_or(ConfigError::InvalidPolicyVersion)?;
        let root_annotators = root
            .policy
            .get("annotator")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(declaration_name)
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        if root.appa_composed.is_some() && !root.include.is_empty() {
            return Err(ConfigError::InvalidComposedMetadata {
                reason: "stored composition metadata cannot appear with include".to_string(),
            });
        }
        let mut origins = root_command_origins(&root, &source_dir)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut included_batteries = std::collections::BTreeSet::new();
        let mut replaced_annotators = std::collections::BTreeSet::new();
        for authored in &root.include {
            let include = Path::new(authored);
            if include.is_absolute() {
                return Err(ConfigError::AbsoluteInclude { path: authored.clone() });
            }
            let include_path = crate::batteries::resolve_include(authored, &source_dir, battery_dirs);
            let include_path = std::fs::canonicalize(&include_path).map_err(|source| ConfigError::Unreadable {
                path: include_path.display().to_string(),
                source,
            })?;
            if !seen.insert(include_path.clone()) {
                return Err(ConfigError::DuplicateInclude { path: authored.clone() });
            }
            if let Some(name) = crate::batteries::name_from_resolved(&include_path, battery_dirs) {
                included_batteries.insert(name);
            }
            let included_text = std::fs::read_to_string(&include_path).map_err(|source| ConfigError::Unreadable {
                path: include_path.display().to_string(),
                source,
            })?;
            let included: toml::Value = toml::from_str(&included_text).map_err(|source| ConfigError::Unparsable {
                path: include_path.display().to_string(),
                source,
            })?;
            compose_include(
                &mut document,
                included,
                &include_path,
                root_version,
                &root_annotators,
                &mut replaced_annotators,
                &mut origins,
            )?;
        }

        let composed = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        let raw: RawConfig = toml::from_str(&composed).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        add_composed_metadata(&mut document, &raw.externals, &origins)?;
        let stored = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        Config::validate_composed(stored, raw, origins, included_batteries.into_iter().collect(), |var| {
            std::env::var(var).ok()
        })
    }

    /// The configuration of a host that composes its policy in memory
    /// rather than reading `appa.toml`: the policy text it composed, and
    /// the bindings it declares. Validation is the file loader's, tokens
    /// included: a `token_env` resolves from this process's environment.
    pub fn embedded(policy: String, bindings: ExternalBindings) -> Result<Config, ConfigError> {
        let value: toml::Value = toml::from_str(&policy).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        let document = embedded_document(value, &bindings)?;
        let text = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        let raw: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        let origins = root_command_origins(&raw, Path::new("."))?;
        Config::validate_composed(text, raw, origins, Vec::new(), |var| std::env::var(var).ok())
    }

    pub fn policy_file(&self) -> &PolicyFile {
        &self.policy
    }

    pub fn included_batteries(&self) -> &[String] {
        &self.included_batteries
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.policy
            .value()
            .get("tool")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("name"))
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    #[cfg(test)]
    fn validate(
        text: String,
        raw: RawConfig,
        source_dir: &Path,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        let origins = raw
            .externals
            .command_keys()
            .into_iter()
            .map(|key| (key, source_dir.to_path_buf()))
            .collect();
        Config::validate_composed(text, raw, origins, Vec::new(), lookup)
    }

    fn validate_composed(
        text: String,
        raw: RawConfig,
        origins: BTreeMap<String, PathBuf>,
        included_batteries: Vec<String>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        debug_assert!(raw.include.is_empty(), "composed configuration has no includes");
        let RawExternals {
            timeout_ms,
            review_timeout_ms,
            max_body_bytes,
            authorities,
            sanitizers,
            annotators,
            audience,
            identity,
            claude_code,
            llm,
        } = raw.externals;
        if timeout_ms == 0 {
            return Err(ConfigError::ZeroTimeout);
        }
        if review_timeout_ms == 0 {
            return Err(ConfigError::ZeroReviewTimeout);
        }
        if max_body_bytes == 0 {
            return Err(ConfigError::ZeroByteCap);
        }
        let llm = llm.map(|raw| resolve_llm(raw, &lookup)).transpose()?;
        let resolve = |section: Section, entries: BTreeMap<String, RawBinding>| {
            resolve_bindings(section, entries, &origins, &lookup, llm.is_some())
        };
        Ok(Config {
            policy: PolicyFile::new(text.into_bytes(), raw.policy),
            included_batteries,
            externals: Externals {
                timeout: Duration::from_millis(timeout_ms),
                review_timeout: Duration::from_millis(review_timeout_ms),
                max_body_bytes,
                authorities: resolve(Section::Authorities, authorities)?,
                sanitizers: resolve(Section::Sanitizers, sanitizers)?,
                annotators: resolve(Section::Annotators, annotators)?
                    .into_iter()
                    .map(|(name, implementation)| (name, annotator_implementation(implementation)))
                    .collect(),
                audience: resolve(Section::Audience, audience)?,
                identity: resolve(Section::Identity, identity)?,
                claude_code: resolve_claude_code(claude_code)?,
                llm,
            },
        })
    }
}

fn embedded_document(policy: toml::Value, bindings: &ExternalBindings) -> Result<toml::Value, ConfigError> {
    let mut external_table = toml::value::Table::new();
    external_table.insert(
        "timeout_ms".to_string(),
        embedded_integer("externals.timeout_ms", u128::from(bindings.timeout_ms))?,
    );
    // Defaults and empty sections are left unwritten, as an authored file leaves them:
    // the same bindings persist as the same bytes whichever path declared them.
    if bindings.review_timeout_ms != default_review_timeout_ms() {
        external_table.insert(
            "review_timeout_ms".to_string(),
            embedded_integer("externals.review_timeout_ms", u128::from(bindings.review_timeout_ms))?,
        );
    }
    external_table.insert(
        "max_body_bytes".to_string(),
        embedded_integer("externals.max_body_bytes", bindings.max_body_bytes as u128)?,
    );

    let mut command_cwds = toml::value::Table::new();
    for (section, entries) in [
        (Section::Authorities, &bindings.authorities),
        (Section::Sanitizers, &bindings.sanitizers),
        (Section::Annotators, &bindings.annotators),
        (Section::Audience, &bindings.audience),
        (Section::Identity, &bindings.identity),
    ] {
        if entries.is_empty() {
            continue;
        }
        let mut table = toml::value::Table::new();
        for (name, binding) in entries {
            table.insert(
                name.clone(),
                embedded_binding(section, name, binding, &mut command_cwds)?,
            );
        }
        external_table.insert(section.name().to_string(), toml::Value::Table(table));
    }

    if bindings.claude_code != ClaudeCode::default() {
        let mut claude_code = toml::value::Table::new();
        let claude_command =
            bindings
                .claude_code
                .command
                .to_str()
                .ok_or(ConfigError::UnrepresentableEmbeddedSetting {
                    field: "externals.claude_code.command",
                })?;
        claude_code.insert("command".to_string(), toml::Value::String(claude_command.to_string()));
        claude_code.insert(
            "model".to_string(),
            toml::Value::String(bindings.claude_code.model.clone()),
        );
        // Only a budget that differs from the default is written, so a host that leaves it
        // alone composes the same bytes — and so the same policy key — as an authored table
        // that names `command` and `model` and omits `timeout_ms`.
        if bindings.claude_code.timeout != DEFAULT_CLAUDE_CODE_TIMEOUT {
            claude_code.insert(
                "timeout_ms".to_string(),
                embedded_integer(
                    "externals.claude_code.timeout_ms",
                    bindings.claude_code.timeout.as_millis(),
                )?,
            );
        }
        external_table.insert("claude_code".to_string(), toml::Value::Table(claude_code));
    }

    if let Some(llm) = &bindings.llm {
        let mut table = toml::value::Table::new();
        table.insert(
            "provider".to_string(),
            toml::Value::String(llm.provider.as_str().to_string()),
        );
        table.insert("model".to_string(), toml::Value::String(llm.model.clone()));
        if let Some(url) = &llm.url {
            table.insert("url".to_string(), toml::Value::String(url.clone()));
        }
        if let Some(token_env) = &llm.token_env {
            table.insert("token_env".to_string(), toml::Value::String(token_env.clone()));
        }
        if let Some(timeout_ms) = llm.timeout_ms {
            table.insert(
                "timeout_ms".to_string(),
                embedded_integer("externals.llm.timeout_ms", u128::from(timeout_ms))?,
            );
        }
        if let Some(max_concurrent) = llm.max_concurrent {
            table.insert(
                "max_concurrent".to_string(),
                toml::Value::Integer(i64::from(max_concurrent)),
            );
        }
        external_table.insert("llm".to_string(), toml::Value::Table(table));
    }

    let mut document = toml::value::Table::new();
    if !command_cwds.is_empty() {
        document.insert(
            "appa_composed".to_string(),
            toml::Value::Table(
                [("command_cwd".to_string(), toml::Value::Table(command_cwds))]
                    .into_iter()
                    .collect(),
            ),
        );
    }
    document.insert("policy".to_string(), policy);
    document.insert("externals".to_string(), toml::Value::Table(external_table));
    Ok(toml::Value::Table(document))
}

fn embedded_integer(field: &'static str, value: u128) -> Result<toml::Value, ConfigError> {
    i64::try_from(value)
        .map(toml::Value::Integer)
        .map_err(|_| ConfigError::UnrepresentableEmbeddedSetting { field })
}

fn embedded_binding(
    section: Section,
    name: &str,
    binding: &Binding,
    command_cwds: &mut toml::value::Table,
) -> Result<toml::Value, ConfigError> {
    let entry: Vec<(&str, toml::Value)> = match binding {
        Binding::Url { url, token_env } => {
            let mut entry = vec![("url", toml::Value::String(url.clone()))];
            if let Some(token_env) = token_env {
                entry.push(("token_env", toml::Value::String(token_env.clone())));
            }
            entry
        }
        Binding::Builtin(builtin) => vec![("builtin", toml::Value::String(builtin.clone()))],
        Binding::Command { argv, cwd, token_env } => {
            if !cwd.is_absolute() {
                return Err(ConfigError::RelativeCommandCwd {
                    section: section.name(),
                    name: name.to_string(),
                });
            }
            let cwd = cwd.to_str().ok_or(ConfigError::UnrepresentableEmbeddedSetting {
                field: "externals command cwd",
            })?;
            command_cwds.insert(section.origin_key(name), toml::Value::String(cwd.to_string()));
            let mut entry = vec![(
                "command",
                toml::Value::Array(argv.iter().cloned().map(toml::Value::String).collect()),
            )];
            if let Some(token_env) = token_env {
                entry.push(("token_env", toml::Value::String(token_env.clone())));
            }
            entry
        }
    };
    Ok(toml::Value::Table(
        entry.into_iter().map(|(key, value)| (key.to_string(), value)).collect(),
    ))
}

fn policy_version(policy: &toml::Value) -> Option<i64> {
    policy.as_table()?.get("version")?.as_integer()
}

/// The working directory of every command entry the root declares: the root's own
/// directory, or — for stored composed bytes — the directory the metadata recorded.
fn root_command_origins(root: &RawConfig, source_dir: &Path) -> Result<BTreeMap<String, PathBuf>, ConfigError> {
    let commands = root.externals.command_keys();
    let Some(metadata) = &root.appa_composed else {
        return Ok(commands
            .into_iter()
            .map(|key| (key, source_dir.to_path_buf()))
            .collect());
    };
    let recorded = metadata
        .command_cwd
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if commands != recorded {
        return Err(ConfigError::InvalidComposedMetadata {
            reason: "command entries do not match the recorded working directories".to_string(),
        });
    }
    metadata
        .command_cwd
        .iter()
        .map(|(key, cwd)| {
            let cwd = PathBuf::from(cwd);
            if !cwd.is_absolute() {
                return Err(ConfigError::InvalidComposedMetadata {
                    reason: format!("command {key:?} has a non-absolute working directory"),
                });
            }
            Ok((key.clone(), cwd))
        })
        .collect()
}

fn add_composed_metadata(
    document: &mut toml::Value,
    externals: &RawExternals,
    origins: &BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    let command_cwds = externals
        .command_keys()
        .into_iter()
        .map(|key| {
            let cwd = origins
                .get(&key)
                .expect("every composed command binding records its source");
            let cwd = cwd.to_str().ok_or_else(|| ConfigError::InvalidComposedMetadata {
                reason: format!("command {key:?} has a non-UTF-8 working directory"),
            })?;
            Ok((key, toml::Value::String(cwd.to_string())))
        })
        .collect::<Result<toml::map::Map<_, _>, ConfigError>>()?;
    let table = document.as_table_mut().expect("a TOML document is always a table");
    if command_cwds.is_empty() {
        table.remove("appa_composed");
        return Ok(());
    }
    let metadata = toml::Value::Table(
        [("command_cwd".to_string(), toml::Value::Table(command_cwds))]
            .into_iter()
            .collect(),
    );
    table.insert("appa_composed".to_string(), metadata);
    Ok(())
}

fn declaration_name(declaration: &toml::Value) -> Option<&str> {
    declaration.as_table()?.get("name")?.as_str()
}

fn compose_include(
    root: &mut toml::Value,
    included: toml::Value,
    include_path: &Path,
    root_version: i64,
    root_annotators: &std::collections::BTreeSet<String>,
    replaced_annotators: &mut std::collections::BTreeSet<String>,
    origins: &mut BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    let display = include_path.display().to_string();
    let mut included = included.as_table().expect("a TOML document is always a table").clone();
    for field in included.keys() {
        if !matches!(field.as_str(), "policy" | "externals") {
            return Err(ConfigError::IncludedTopLevel {
                path: display,
                field: field.clone(),
            });
        }
    }

    let included_policy = included
        .remove("policy")
        .and_then(|value| value.as_table().cloned())
        .ok_or_else(|| ConfigError::IncludedTopLevel {
            path: display.clone(),
            field: "policy".to_string(),
        })?;
    let found_version = included_policy
        .get("version")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| ConfigError::IncludedPolicyField {
            path: display.clone(),
            field: "version".to_string(),
        })?;
    if found_version != root_version {
        return Err(ConfigError::IncludedVersion {
            path: display,
            root: root_version,
            found: found_version,
        });
    }

    let root_table = root.as_table_mut().expect("a TOML document is always a table");
    let root_policy = root_table
        .get_mut("policy")
        .and_then(toml::Value::as_table_mut)
        .expect("RawConfig requires a policy table");
    for (field, value) in included_policy {
        if field == "version" {
            continue;
        }
        if !matches!(field.as_str(), "tool" | "annotator" | "authority" | "sanitizer") {
            return Err(ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field,
            });
        }
        let mut declarations = value
            .as_array()
            .cloned()
            .ok_or_else(|| ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field: field.clone(),
            })?;
        if field == "annotator" {
            // An Annotator is one named policy component, not an ordered matcher. The root
            // may replace one included default so a deployment can own its trusted hint and
            // mandate without editing a battery. A second included declaration with that
            // name remains in the composed policy, where normal duplicate validation refuses
            // it instead of silently choosing between two batteries.
            declarations.retain(|declaration| {
                let Some(name) = declaration_name(declaration) else {
                    return true;
                };
                !(root_annotators.contains(name) && replaced_annotators.insert(name.to_string()))
            });
        }
        let destination = root_policy
            .entry(field.clone())
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| ConfigError::IncludedPolicyField {
                path: include_path.display().to_string(),
                field,
            })?;
        destination.append(&mut declarations);
    }

    let Some(included_externals) = included.remove("externals") else {
        return Ok(());
    };
    let included_externals = included_externals
        .as_table()
        .ok_or_else(|| ConfigError::IncludedTopLevel {
            path: include_path.display().to_string(),
            field: "externals".to_string(),
        })?;
    let root_externals = root_table
        .get_mut("externals")
        .and_then(toml::Value::as_table_mut)
        .expect("RawConfig requires an externals table");
    for (section_name, entries) in included_externals {
        let Some(section) = Section::parse(section_name) else {
            return Err(ConfigError::IncludedExternalsField {
                path: include_path.display().to_string(),
                field: section_name.clone(),
            });
        };
        let entries = entries.as_table().ok_or_else(|| ConfigError::IncludedExternalsField {
            path: include_path.display().to_string(),
            field: section_name.clone(),
        })?;
        let destination = root_externals
            .entry(section_name.clone())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .expect("RawExternals requires named external tables");
        for (name, entry) in entries {
            if destination.contains_key(name) {
                return Err(ConfigError::DuplicateExternal {
                    path: include_path.display().to_string(),
                    section: section_name.clone(),
                    name: name.clone(),
                });
            }
            destination.insert(name.clone(), entry.clone());
            origins.insert(
                section.origin_key(name),
                include_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
            );
        }
    }
    Ok(())
}

/// An annotator binding after `Section::Annotators` refused every `builtin` at parse.
fn annotator_implementation(implementation: Implementation) -> AnnotatorImplementation {
    match implementation {
        Implementation::Resolver(endpoint) => AnnotatorImplementation::Resolver(endpoint),
        Implementation::Command(command) => AnnotatorImplementation::Command(command),
        Implementation::Builtin(_) => unreachable!("Section::Annotators refuses every builtin"),
    }
}

fn resolve_bindings(
    section: Section,
    raw: BTreeMap<String, RawBinding>,
    origins: &BTreeMap<String, PathBuf>,
    lookup: &impl Fn(&str) -> Option<String>,
    llm_configured: bool,
) -> Result<BTreeMap<String, Implementation>, ConfigError> {
    raw.into_iter()
        .map(|(name, entry)| {
            let implementation = resolve_binding(section, &name, entry, origins, lookup, llm_configured)?;
            Ok((name, implementation))
        })
        .collect()
}

/// One entry's implementation choice: exactly one of `url` (with an optional token), a
/// `command`, or a `builtin` the section accepts.
fn resolve_binding(
    section: Section,
    name: &str,
    entry: RawBinding,
    origins: &BTreeMap<String, PathBuf>,
    lookup: &impl Fn(&str) -> Option<String>,
    llm_configured: bool,
) -> Result<Implementation, ConfigError> {
    let RawBinding {
        url,
        token_env,
        builtin,
        command,
    } = entry;
    match (url, builtin, command) {
        (Some(url), None, None) => {
            let url = validated_url(section.name(), name, url)?;
            let token = resolve_token(section.name(), name, token_env, lookup)?;
            Ok(Implementation::Resolver(Endpoint::new(url, token)))
        }
        (None, Some(builtin), None) if token_env.is_none() => {
            section.check_builtin(name, &builtin)?;
            if builtin == LLM_BUILTIN && !llm_configured {
                return Err(ConfigError::LlmNotConfigured {
                    section: section.name(),
                    name: name.to_string(),
                });
            }
            Ok(Implementation::Builtin(builtin))
        }
        (None, None, Some(argv)) => resolve_command(section, name, argv, token_env, origins),
        _ => Err(ConfigError::ImplementationChoice {
            section: section.name(),
            name: name.to_string(),
        }),
    }
}

/// A command's `token_env` is the opposite of a URL's: the runtime sends nothing, it
/// forwards one variable to the child that reads it, and only from the passthrough
/// namespace. Presence is deliberately not checked here — the runtime never reads the
/// value, so a policy stays loadable and describable on a machine that holds no provider
/// credential, and a missing one surfaces as the child's own refusal to answer.
fn resolve_command(
    section: Section,
    name: &str,
    argv: Vec<String>,
    token_env: Option<String>,
    origins: &BTreeMap<String, PathBuf>,
) -> Result<Implementation, ConfigError> {
    if argv.is_empty() || argv.iter().any(String::is_empty) {
        return Err(ConfigError::InvalidCommand {
            section: section.name(),
            name: name.to_string(),
        });
    }
    if let Some(var) = token_env
        .as_ref()
        .filter(|var| !var.starts_with(PROVIDER_CREDENTIAL_PREFIX))
    {
        return Err(ConfigError::CommandCredentialVariable {
            section: section.name(),
            name: name.to_string(),
            var: var.clone(),
            prefix: PROVIDER_CREDENTIAL_PREFIX,
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (argv, origins, token_env);
        return Err(ConfigError::UnsupportedCommandPlatform {
            section: section.name(),
            name: name.to_string(),
        });
    }
    #[cfg(unix)]
    {
        Ok(Implementation::Command(ResolverCommand {
            argv,
            cwd: origins
                .get(&section.origin_key(name))
                .expect("every composed command binding records its source")
                .clone(),
            token_env,
        }))
    }
}

/// The `[externals.claude_code]` table with its defaults filled: bare `claude` on `PATH`,
/// the `sonnet` alias, and the model-consult budget. A zero `timeout_ms` is a refusal like
/// the shared one.
fn resolve_claude_code(raw: Option<RawClaudeCode>) -> Result<ClaudeCode, ConfigError> {
    let raw = raw.unwrap_or(RawClaudeCode {
        command: None,
        model: None,
        timeout_ms: None,
    });
    if raw.timeout_ms == Some(0) {
        return Err(ConfigError::ZeroTimeout);
    }
    Ok(ClaudeCode {
        command: raw.command.map(PathBuf::from).unwrap_or_else(|| "claude".into()),
        model: raw.model.unwrap_or_else(|| "sonnet".to_string()),
        timeout: raw
            .timeout_ms
            .map_or(DEFAULT_CLAUDE_CODE_TIMEOUT, Duration::from_millis),
    })
}

fn resolve_llm(raw: RawLlm, lookup: &impl Fn(&str) -> Option<String>) -> Result<LlmProfile, ConfigError> {
    const SECTION: &str = "llm";
    let provider = LlmProvider::parse(&raw.provider).ok_or_else(|| ConfigError::InvalidLlmProvider {
        provider: raw.provider.clone(),
    })?;
    if raw.timeout_ms == Some(0) {
        return Err(ConfigError::ZeroTimeout);
    }
    if raw.max_concurrent == Some(0) {
        return Err(ConfigError::ZeroConcurrency);
    }
    let url = raw.url.map(|url| validated_url(SECTION, SECTION, url)).transpose()?;
    let token = resolve_token(SECTION, SECTION, raw.token_env, lookup)?;
    if token.is_none() && provider != LlmProvider::Ollama {
        return Err(ConfigError::LlmTokenRequired {
            provider: provider.as_str(),
        });
    }
    Ok(LlmProfile {
        provider,
        model: raw.model,
        url,
        token,
        timeout: raw.timeout_ms.map(Duration::from_millis),
        max_concurrent: raw
            .max_concurrent
            .map(|count| count as usize)
            .unwrap_or(DEFAULT_LLM_CONCURRENCY),
    })
}

/// The URL rules every endpoint shares: `https` anywhere, cleartext `http` only to
/// loopback, and no credentials inside the URL.
fn validated_url(section: &'static str, name: &str, url: String) -> Result<String, ConfigError> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| ConfigError::InvalidEndpoint {
        section,
        name: name.to_string(),
        url: url.clone(),
    })?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            if !is_loopback(&parsed) {
                return Err(ConfigError::CleartextEndpoint {
                    section,
                    name: name.to_string(),
                    url,
                });
            }
        }
        _ => {
            return Err(ConfigError::InvalidEndpoint {
                section,
                name: name.to_string(),
                url,
            });
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ConfigError::CredentialsInUrl {
            section,
            name: name.to_string(),
        });
    }
    Ok(url)
}

fn resolve_token(
    section: &'static str,
    name: &str,
    token_env: Option<String>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Token>, ConfigError> {
    let Some(var) = token_env else {
        return Ok(None);
    };
    if !var.starts_with(RUNTIME_VARIABLE_PREFIX) {
        return Err(ConfigError::ForeignSecretVariable {
            section,
            name: name.to_string(),
            var,
        });
    }
    if var.starts_with(PROVIDER_CREDENTIAL_PREFIX) {
        return Err(ConfigError::ChildCredentialVariable {
            section,
            name: name.to_string(),
            var,
            prefix: PROVIDER_CREDENTIAL_PREFIX,
        });
    }
    match lookup(&var) {
        Some(value) if !value.is_empty() => Ok(Some(Token::new(value))),
        _ => Err(ConfigError::MissingSecret {
            section,
            name: name.to_string(),
            var,
        }),
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain == "localhost",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [policy]
        anything = "the runtime does not interpret this"

        [externals]
        timeout_ms = 5000
        max_body_bytes = 65536
    "#;

    const LLM_TABLE: &str = "[externals.llm]\nprovider = \"ollama\"\nmodel = \"llama\"\n";

    fn parse(text: &str) -> Result<Config, ConfigError> {
        parse_with(text, |_| None)
    }

    fn parse_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(text).expect("test fixture parses as TOML");
        Config::validate(text.to_string(), raw, Path::new("."), lookup)
    }

    /// The transport one resolved entry selected, the same view over every section.
    enum Bound<'a> {
        Url,
        Command(&'a ResolverCommand),
        Builtin(&'a str),
    }

    fn bound<'a>(section: Section, config: &'a Config, name: &str) -> Option<Bound<'a>> {
        let table = match section {
            Section::Authorities => &config.externals.authorities,
            Section::Sanitizers => &config.externals.sanitizers,
            Section::Audience => &config.externals.audience,
            Section::Identity => &config.externals.identity,
            Section::Annotators => {
                return config
                    .externals
                    .annotators
                    .get(name)
                    .map(|implementation| match implementation {
                        AnnotatorImplementation::Resolver(_) => Bound::Url,
                        AnnotatorImplementation::Command(command) => Bound::Command(command),
                    });
            }
        };
        table.get(name).map(|implementation| match implementation {
            Implementation::Resolver(_) => Bound::Url,
            Implementation::Command(command) => Bound::Command(command),
            Implementation::Builtin(builtin) => Bound::Builtin(builtin),
        })
    }

    #[test]
    fn a_minimal_file_loads_and_keeps_the_policy_opaque() {
        let config = parse(MINIMAL).expect("the minimal fixture validates");
        assert_eq!(config.externals.timeout, Duration::from_millis(5000));
        assert_eq!(config.externals.max_body_bytes, 65536);
        assert!(config.externals.annotators.is_empty());
        assert!(config.externals.llm.is_none());
        assert_eq!(
            config.policy_file().value().get("anything").and_then(|v| v.as_str()),
            Some("the runtime does not interpret this"),
        );
        assert_eq!(config.policy_file().bytes(), MINIMAL.as_bytes());
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let text = MINIMAL.replace("[policy]", "typo_key = 1\n[policy]");
        let result: Result<RawConfig, _> = toml::from_str(&text);
        assert!(result.is_err(), "a typo must not silently weaken a deployment");
    }

    #[test]
    fn a_zero_timeout_and_a_zero_byte_cap_are_refused() {
        let zero_timeout = MINIMAL.replace("timeout_ms = 5000", "timeout_ms = 0");
        assert!(matches!(parse(&zero_timeout), Err(ConfigError::ZeroTimeout)));

        let zero_cap = MINIMAL.replace("max_body_bytes = 65536", "max_body_bytes = 0");
        assert!(matches!(parse(&zero_cap), Err(ConfigError::ZeroByteCap)));
    }

    #[test]
    fn invalid_and_cleartext_and_credentialed_urls_are_refused() {
        let bad = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"ftp://authority.internal\"\n");
        assert!(matches!(parse(&bad), Err(ConfigError::InvalidEndpoint { .. })));

        let unparsable = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"https://\"\n");
        assert!(matches!(parse(&unparsable), Err(ConfigError::InvalidEndpoint { .. })));

        let cleartext = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"http://authority.internal\"\n");
        assert!(matches!(parse(&cleartext), Err(ConfigError::CleartextEndpoint { .. })));

        let loopback = format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"http://127.0.0.1:9000/\"\n");
        assert!(parse(&loopback).is_ok());

        let userinfo =
            format!("{MINIMAL}\n[externals.authorities.security]\nurl = \"https://user:pw@authority.internal/\"\n");
        assert!(matches!(parse(&userinfo), Err(ConfigError::CredentialsInUrl { .. })));
    }

    #[test]
    fn secrets_come_only_from_appa_variables() {
        let foreign = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"HOME\"\n"
        );
        assert!(matches!(
            parse(&foreign),
            Err(ConfigError::ForeignSecretVariable { .. }),
        ));

        let unset = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"APPA_AUTHORITY_TOKEN\"\n"
        );
        assert!(matches!(parse(&unset), Err(ConfigError::MissingSecret { .. })));

        // The passthrough namespace reaches command children, so a token this runtime sends
        // may not live there — the refusal comes before the variable is even read.
        let passthrough = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"APPA_PROVIDER_AUTHORITY_TOKEN\"\n"
        );
        assert!(matches!(
            parse(&passthrough),
            Err(ConfigError::ChildCredentialVariable { .. }),
        ));

        let empty = format!(
            "{MINIMAL}\n[externals.authorities.security]\nurl = \"https://authority.internal\"\ntoken_env = \"APPA_AUTHORITY_TOKEN\"\n"
        );
        assert!(matches!(
            parse_with(&empty, |_| Some(String::new())),
            Err(ConfigError::MissingSecret { .. }),
        ));
    }

    /// A command's `token_env` is the mirror of a URL's: nothing is sent, one variable is
    /// forwarded to the child that reads it, and only from the passthrough namespace.
    #[cfg(unix)]
    #[test]
    fn a_command_forwards_one_credential_and_only_from_the_passthrough_namespace() {
        let with = |token_env: &str| {
            format!(
                "{MINIMAL}\n[externals.audience.slack]\ncommand = [\"python3\", \"source.py\"]\ntoken_env = \"{token_env}\"\n"
            )
        };
        let set = |var: &str| (var == "APPA_PROVIDER_SLACK_TOKEN").then(|| "xoxb-fixture".to_string());

        assert!(
            matches!(
                parse_with(&with("APPA_SLACK_TOKEN"), set),
                Err(ConfigError::CommandCredentialVariable { .. })
            ),
            "the runtime's own namespace never reaches a child, so a command cannot name it"
        );
        assert!(matches!(
            parse_with(&with("SLACK_TOKEN"), set),
            Err(ConfigError::CommandCredentialVariable { .. }),
        ));
        assert!(
            parse_with(&with("APPA_PROVIDER_GITHUB_TOKEN"), set).is_ok(),
            "the runtime never reads the value, so a policy stays loadable and describable \
             without the credential on the machine"
        );

        let config = parse_with(&with("APPA_PROVIDER_SLACK_TOKEN"), set).expect("the bound credential validates");
        let Some(Implementation::Command(command)) = config.externals.audience.get("slack") else {
            panic!("the slack audience source is a command")
        };
        assert_eq!(command.token_env.as_deref(), Some("APPA_PROVIDER_SLACK_TOKEN"));
        assert!(
            !format!("{command:?}").contains("xoxb-fixture"),
            "the binding carries the variable name, never the credential"
        );
    }

    #[test]
    fn a_present_secret_resolves_and_debug_redacts_it() {
        let text = format!(
            "{MINIMAL}\n[externals.annotators.classifier]\nurl = \"https://resolver.internal\"\ntoken_env = \"APPA_RESOLVER_TOKEN\"\n"
        );
        let config = parse_with(&text, |var| {
            (var == "APPA_RESOLVER_TOKEN").then(|| "sekret".to_string())
        })
        .expect("the fixture with a set secret validates");
        assert!(!format!("{:?}", config.externals).contains("sekret"));
        let Some(AnnotatorImplementation::Resolver(annotator)) = config.externals.annotators.get("classifier") else {
            panic!("the named annotator endpoint is set")
        };
        let token = annotator.token.as_ref().expect("the token resolved");
        assert_eq!(token.reveal(), "sekret");
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }

    #[test]
    fn an_unreadable_path_is_a_named_refusal() {
        let missing = Path::new("/nonexistent/appa.toml");
        assert!(matches!(Config::load(missing), Err(ConfigError::Unreadable { .. }),));
    }

    #[test]
    fn the_claude_code_table_fills_its_defaults_and_refuses_junk() {
        let config = parse(MINIMAL).expect("no claude table is the default");
        assert_eq!(config.externals.claude_code.command, PathBuf::from("claude"));
        assert_eq!(config.externals.claude_code.model, "sonnet");
        assert_eq!(config.externals.claude_code.timeout, DEFAULT_CLAUDE_CODE_TIMEOUT);

        let text = format!(
            "{MINIMAL}\n[externals.claude_code]\ncommand = \"/opt/claude/bin/claude\"\nmodel = \"pinned\"\ntimeout_ms = 90000\n"
        );
        let config = parse(&text).expect("the claude table validates");
        assert_eq!(
            config.externals.claude_code.command,
            PathBuf::from("/opt/claude/bin/claude")
        );
        assert_eq!(config.externals.claude_code.model, "pinned");
        let pinned = Duration::from_secs(90);
        assert_ne!(
            pinned, DEFAULT_CLAUDE_CODE_TIMEOUT,
            "the pin must differ from the default"
        );
        assert_eq!(config.externals.claude_code.timeout, pinned);

        let text = format!("{MINIMAL}\n[externals.claude_code]\ntimeout_ms = 0\n");
        assert!(matches!(parse(&text), Err(ConfigError::ZeroTimeout)));
        let text = format!("{MINIMAL}\n[externals.claude_code]\nurl = \"https://x.example\"\n");
        assert!(
            toml::from_str::<RawConfig>(&text).is_err(),
            "a typo cannot silently weaken the deployment"
        );
    }

    /// Every kind × transport cell: which of `url`, `command`, and each builtin name a
    /// section accepts.
    #[test]
    fn every_section_takes_the_same_transports_and_its_own_builtins() {
        let entry = |section: Section, body: &str| {
            format!("{MINIMAL}\n{LLM_TABLE}\n[externals.{}.x]\n{body}\n", section.name())
        };
        for section in Section::ALL {
            let config = parse(&entry(section, "url = \"https://x.internal\"")).expect("a url binds everywhere");
            assert!(matches!(bound(section, &config, "x"), Some(Bound::Url)));
            #[cfg(unix)]
            {
                let config =
                    parse(&entry(section, "command = [\"python3\", \"x.py\"]")).expect("a command binds everywhere");
                assert!(matches!(bound(section, &config, "x"), Some(Bound::Command(_))));
            }
            assert!(
                matches!(
                    parse(&entry(section, "command = [\"\"]")),
                    Err(ConfigError::InvalidCommand { .. })
                ),
                "{}: an empty argument is refused",
                section.name()
            );
        }

        let cell = |section: Section, builtin: &str| parse(&entry(section, &format!("builtin = \"{builtin}\"")));
        let accepts = |section: Section, builtin: &str| {
            let config =
                cell(section, builtin).unwrap_or_else(|error| panic!("{} takes {builtin}: {error}", section.name()));
            assert!(matches!(
                bound(section, &config, "x"),
                Some(Bound::Builtin(name)) if name == builtin
            ));
        };
        for section in [Section::Authorities, Section::Sanitizers] {
            for builtin in ["hitl", "approve", "redact-email", "claude-code", "llm", "some-module"] {
                accepts(section, builtin);
            }
        }
        // An Annotator names a stock builtin on its policy declaration, never here.
        for section in [Section::Annotators, Section::Audience, Section::Identity] {
            for builtin in ["hitl", "approve", "redact-email", "claude-code", "llm", "some-module"] {
                assert!(
                    matches!(cell(section, builtin), Err(ConfigError::BuiltinNotAllowed { .. })),
                    "{} must refuse builtin {builtin}",
                    section.name()
                );
            }
        }
    }

    #[test]
    fn the_llm_builtin_needs_the_llm_table() {
        for section in [Section::Authorities, Section::Sanitizers] {
            let text = format!("{MINIMAL}\n[externals.{}.x]\nbuiltin = \"llm\"\n", section.name());
            assert!(
                matches!(parse(&text), Err(ConfigError::LlmNotConfigured { .. })),
                "{} llm without a profile must refuse",
                section.name()
            );
        }
    }

    #[test]
    fn the_llm_table_validates_like_an_endpoint() {
        let with = |body: &str| format!("{MINIMAL}\n[externals.llm]\nprovider = \"openai\"\nmodel = \"gpt\"\n{body}\n");
        let bare = format!("{MINIMAL}\n[externals.llm]\nprovider = \"ollama\"\nmodel = \"llama\"\n");
        let config = parse(&bare).expect("an ollama profile validates without a key");
        let llm = config.externals.llm.expect("the profile is set");
        assert_eq!(llm.provider, LlmProvider::Ollama);
        assert_eq!(llm.model, "llama");
        assert!(llm.url.is_none() && llm.token.is_none() && llm.timeout.is_none());
        assert_eq!(llm.max_concurrent, DEFAULT_LLM_CONCURRENCY);
        assert!(matches!(
            parse(&with("")),
            Err(ConfigError::LlmTokenRequired { provider: "openai" })
        ));

        let config = parse_with(
            &with("url = \"http://127.0.0.1:11434\"\ntoken_env = \"APPA_LLM_TOKEN\"\ntimeout_ms = 40000\nmax_concurrent = 2"),
            |var| (var == "APPA_LLM_TOKEN").then(|| "sekret".to_string()),
        )
        .expect("a full profile validates");
        let llm = config.externals.llm.expect("the profile is set");
        assert_eq!(llm.url.as_deref(), Some("http://127.0.0.1:11434"));
        assert_eq!(llm.token.as_ref().map(Token::reveal), Some("sekret"));
        assert_eq!(llm.timeout, Some(Duration::from_secs(40)));
        assert_eq!(llm.max_concurrent, 2);

        assert!(matches!(
            parse(&with("token_env = \"OPENAI_API_KEY\"")),
            Err(ConfigError::ForeignSecretVariable { section: "llm", .. })
        ));
        assert!(matches!(
            parse(&with("token_env = \"APPA_LLM_TOKEN\"")),
            Err(ConfigError::MissingSecret { section: "llm", .. })
        ));
        assert!(matches!(
            parse(&with("url = \"https://user:pw@gateway.internal/v1\"")),
            Err(ConfigError::CredentialsInUrl { section: "llm", .. })
        ));
        assert!(matches!(
            parse_with(
                &with("url = \"http://gateway.internal/v1\"\ntoken_env = \"APPA_LLM_TOKEN\""),
                |_| Some("sekret".to_string())
            ),
            Err(ConfigError::CleartextEndpoint { section: "llm", .. })
        ));
        assert!(matches!(
            parse(&with("url = \"ftp://gateway.internal\"")),
            Err(ConfigError::InvalidEndpoint { section: "llm", .. })
        ));
        assert!(matches!(parse(&with("timeout_ms = 0")), Err(ConfigError::ZeroTimeout)));
        assert!(matches!(
            parse(&with("max_concurrent = 0")),
            Err(ConfigError::ZeroConcurrency)
        ));
        let unknown = format!("{MINIMAL}\n[externals.llm]\nprovider = \"cohere\"\nmodel = \"m\"\n");
        assert!(matches!(
            parse(&unknown),
            Err(ConfigError::InvalidLlmProvider { provider }) if provider == "cohere"
        ));
        let typo = format!("{MINIMAL}\n[externals.llm]\nprovider = \"openai\"\nmodel = \"m\"\napi_key = \"x\"\n");
        assert!(toml::from_str::<RawConfig>(&typo).is_err());
    }

    #[test]
    fn an_entry_names_exactly_one_implementation() {
        let both =
            format!("{MINIMAL}\n[externals.authorities.auto]\nurl = \"https://a.example\"\nbuiltin = \"approve\"\n");
        assert!(matches!(parse(&both), Err(ConfigError::ImplementationChoice { .. })));

        let neither = format!("{MINIMAL}\n[externals.authorities.auto]\n");
        assert!(matches!(parse(&neither), Err(ConfigError::ImplementationChoice { .. })));

        let token = format!("{MINIMAL}\n[externals.authorities.auto]\nbuiltin = \"approve\"\ntoken_env = \"APPA_X\"\n");
        assert!(matches!(parse(&token), Err(ConfigError::ImplementationChoice { .. })));

        let command_and_url =
            format!("{MINIMAL}\n[externals.sanitizers.pii]\nurl = \"https://a.example\"\ncommand = [\"x\"]\n");
        assert!(matches!(
            parse(&command_and_url),
            Err(ConfigError::ImplementationChoice { .. })
        ));

        let singleton_audience = format!("{MINIMAL}\n[externals.audience]\nurl = \"https://directory.internal\"\n");
        assert!(
            toml::from_str::<RawConfig>(&singleton_audience).is_err(),
            "an audience source binds by provider name like every other section"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn a_command_binding_is_refused_on_an_unsupported_platform() {
        let text = format!("{MINIMAL}\n[externals.annotators.classifier]\ncommand = [\"python3\", \"resolver.py\"]\n");
        assert!(matches!(
            parse(&text),
            Err(ConfigError::UnsupportedCommandPlatform { name, .. }) if name == "classifier"
        ));
    }

    #[cfg(not(unix))]
    #[test]
    fn the_claude_code_builtin_is_refused_on_an_unsupported_platform() {
        let text = format!("{MINIMAL}\n[externals.sanitizers.classifier]\nbuiltin = \"claude-code\"\n");
        assert!(matches!(
            parse(&text),
            Err(ConfigError::UnsupportedClaudeCodePlatform { name, .. }) if name == "classifier"
        ));
    }

    #[test]
    fn a_builtin_name_outside_the_grammar_is_refused() {
        for bad in ["Upper", "under_score", "-lead", ""] {
            let text = format!("{MINIMAL}\n[externals.sanitizers.pii]\nbuiltin = \"{bad}\"\n");
            assert!(
                matches!(parse(&text), Err(ConfigError::InvalidBuiltinName { .. })),
                "builtin name {bad:?} must refuse",
            );
        }
    }

    #[test]
    fn includes_append_contracts_after_root_contracts_in_authored_order() {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["first.toml", "second.toml"]

                [policy]
                version = 2

                [[policy.tool]]
                name = "root"
                delta = {}

                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
            "#,
        )
        .expect("write root config");
        for (file, name) in [("first.toml", "first"), ("second.toml", "second")] {
            std::fs::write(
                dir.path().join(file),
                format!(
                    r#"
                        [policy]
                        version = 2

                        [[policy.tool]]
                        name = "{name}"
                        delta = {{}}
                    "#
                ),
            )
            .expect("write included config");
        }

        let config = Config::load(&dir.path().join("appa.toml")).expect("composed config loads");
        let names = config.policy_file().value()["tool"]
            .as_array()
            .expect("tool declarations")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, ["root", "first", "second"]);
        assert!(!String::from_utf8_lossy(config.policy_file().bytes()).contains("include"));
    }

    #[test]
    fn a_battery_include_resolves_from_the_search_path_before_the_config_directory() {
        let dir = tempfile::tempdir().expect("temp directory");
        let image = dir.path().join("image");
        std::fs::create_dir_all(image.join("slack")).expect("image battery");
        std::fs::write(
            image.join("slack/appa.toml"),
            r#"
                [policy]
                version = 2

                [[policy.tool]]
                name = "from-image"
                delta = {}
            "#,
        )
        .expect("image battery config");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["batteries/slack/appa.toml"]

                [policy]
                version = 2

                [[policy.tool]]
                name = "root"
                delta = {}

                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
            "#,
        )
        .expect("write root config");

        let config =
            Config::load_from(&dir.path().join("appa.toml"), &[image]).expect("the search path supplies the battery");
        let names = config.policy_file().value()["tool"]
            .as_array()
            .expect("tool declarations")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, ["root", "from-image"]);
        assert_eq!(config.included_batteries(), ["slack"]);
    }

    #[test]
    fn a_relative_battery_include_reports_its_resolved_name() {
        let dir = tempfile::tempdir().expect("temp directory");
        let config_dir = dir.path().join("deploy/config");
        let battery_dir = dir.path().join("batteries/github");
        std::fs::create_dir_all(&config_dir).expect("config directory");
        std::fs::create_dir_all(&battery_dir).expect("battery directory");
        std::fs::write(
            battery_dir.join("appa.toml"),
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"read\"\ndelta = {}\n",
        )
        .expect("battery config");
        std::fs::write(
            config_dir.join("appa.toml"),
            "include = [\"../../batteries/github/appa.toml\"]\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 4096\n",
        )
        .expect("root config");

        let config = Config::load(&config_dir.join("appa.toml")).expect("relative battery include loads");
        assert_eq!(config.included_batteries(), ["github"]);
    }

    #[test]
    fn a_root_annotator_replaces_one_included_default() {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["battery.toml"]

                [policy]
                version = 2

                [[policy.annotator]]
                name = "battery.classifier"
                builtin = "claude-code"
                audiences = []
                hint = "Classify commands for this deployment."

                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
            "#,
        )
        .expect("write root config");
        std::fs::write(
            dir.path().join("battery.toml"),
            r#"
                [policy]
                version = 2

                [[policy.annotator]]
                name = "battery.classifier"
                builtin = "claude-code"
                audiences = []
                hint = "The battery default."

                [[policy.tool]]
                name = "Bash"
                annotator = "battery.classifier"
            "#,
        )
        .expect("write included config");

        let config = Config::load(&dir.path().join("appa.toml")).expect("the root replaces the battery default");
        let annotators = config.policy_file().value()["annotator"]
            .as_array()
            .expect("annotator declarations");
        assert_eq!(annotators.len(), 1);
        assert_eq!(
            annotators[0]["hint"].as_str(),
            Some("Classify commands for this deployment.")
        );

        let policy = toml::to_string(config.policy_file().value()).expect("the composed policy renders");
        appa_policy::Config::from_toml_str(&policy).expect("the composed policy has one complete Annotator");
    }

    /// A command's working directory is its declaring file's, in every section, and the
    /// composed bytes record it so a stored deployment reloads the same binding.
    #[cfg(unix)]
    #[test]
    fn included_command_paths_are_relative_to_their_declaring_configs() {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::create_dir(dir.path().join("battery")).expect("create battery directory");
        std::fs::write(
            dir.path().join("appa.toml"),
            r#"
                include = ["battery/claude.toml"]
                [policy]
                version = 2
                [externals]
                timeout_ms = 5000
                max_body_bytes = 65536
                [externals.annotators.local]
                command = ["python3", "local.py"]
                [externals.authorities.desk]
                command = ["python3", "desk.py"]
            "#,
        )
        .expect("write root config");
        std::fs::write(
            dir.path().join("battery/claude.toml"),
            r#"
                [policy]
                version = 2
                [externals.annotators.battery]
                command = ["python3", "resolver.py"]
                [externals.sanitizers.scrub]
                command = ["python3", "scrub.py"]
                [externals.audience.slack]
                command = ["python3", "slack-audience.py"]
                [externals.identity.corp-identity]
                command = ["python3", "identity.py"]
            "#,
        )
        .expect("write included config");

        let config = Config::load(&dir.path().join("appa.toml")).expect("composed config loads");
        let command_cwd =
            |section: Section, name: &str| match bound(section, &config, name).expect("binding is present") {
                Bound::Command(command) => command.cwd.clone(),
                _ => panic!("binding is a command"),
            };
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical temp directory");
        assert_eq!(command_cwd(Section::Annotators, "local"), canonical);
        assert_eq!(command_cwd(Section::Authorities, "desk"), canonical);
        for (section, name) in [
            (Section::Annotators, "battery"),
            (Section::Sanitizers, "scrub"),
            (Section::Audience, "slack"),
            (Section::Identity, "corp-identity"),
        ] {
            assert_eq!(
                command_cwd(section, name),
                canonical.join("battery"),
                "{}.{name}",
                section.name()
            );
        }
        assert!(
            String::from_utf8_lossy(config.policy_file().bytes()).contains("[appa_composed"),
            "command origins are part of deployment identity"
        );

        let moved = tempfile::tempdir().expect("second temp directory");
        std::fs::create_dir(moved.path().join("battery")).expect("create second battery directory");
        std::fs::copy(dir.path().join("appa.toml"), moved.path().join("appa.toml")).expect("copy root config");
        std::fs::copy(
            dir.path().join("battery/claude.toml"),
            moved.path().join("battery/claude.toml"),
        )
        .expect("copy included config");
        let moved = Config::load(&moved.path().join("appa.toml")).expect("moved config loads");
        assert_ne!(
            config.policy_file().bytes(),
            moved.policy_file().bytes(),
            "moving a command config changes the deployment behavior and identity"
        );

        let standalone_dir = tempfile::tempdir().expect("standalone temp directory");
        let standalone_path = standalone_dir.path().join("appa.toml");
        std::fs::write(&standalone_path, config.policy_file().bytes()).expect("write stored config");
        let standalone = Config::load(&standalone_path).expect("stored command config reloads");
        let Some(Implementation::Command(command)) = standalone.externals.sanitizers.get("scrub") else {
            panic!("stored scrub binding is a command")
        };
        assert_eq!(command.cwd, canonical.join("battery"));
        assert_eq!(standalone.policy_file().bytes(), config.policy_file().bytes());

        let mismatched = String::from_utf8_lossy(config.policy_file().bytes())
            .replace("\"sanitizers.scrub\"", "\"sanitizers.other\"");
        std::fs::write(&standalone_path, mismatched).expect("write mismatched metadata");
        assert!(matches!(
            Config::load(&standalone_path),
            Err(ConfigError::InvalidComposedMetadata { .. })
        ));
    }

    fn embedded(bindings: ExternalBindings) -> Result<Config, ConfigError> {
        Config::embedded("version = 2".to_string(), bindings)
    }

    #[test]
    fn an_embedded_default_budget_stores_what_an_authored_table_omitting_it_stores() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let mut bindings = ExternalBindings::new(Duration::from_millis(5000), 65_536);
        bindings.claude_code = ClaudeCode {
            command: PathBuf::from("/opt/claude/bin/claude"),
            model: "pinned".to_string(),
            timeout: DEFAULT_CLAUDE_CODE_TIMEOUT,
        };
        let host = Config::embedded(
            "version = 2\nanything = \"the runtime does not interpret this\"\n".to_string(),
            bindings,
        )
        .expect("the embedded claude bindings load");

        // Both sides come back through the file loader, so only their content can differ,
        // never their serialization.
        let from_host = dir.path().join("host.toml");
        std::fs::write(&from_host, host.policy_file().bytes()).expect("write the stored embedded config");
        let from_host = Config::load(&from_host).expect("the stored embedded config reloads");

        let authored_path = dir.path().join("authored.toml");
        std::fs::write(
            &authored_path,
            "[policy]\nversion = 2\nanything = \"the runtime does not interpret this\"\n\n\
             [externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n\n\
             [externals.claude_code]\ncommand = \"/opt/claude/bin/claude\"\nmodel = \"pinned\"\n",
        )
        .expect("write the authored config");
        let authored = Config::load(&authored_path).expect("the authored claude table loads");

        // A budget neither side states must not be stated by one of them: an install
        // compares these policy keys, so equal deployments must compose equal bytes.
        assert_eq!(from_host.policy_file().bytes(), authored.policy_file().bytes());
        assert_eq!(from_host.externals.claude_code, authored.externals.claude_code);
        assert_eq!(authored.externals.claude_code.timeout, DEFAULT_CLAUDE_CODE_TIMEOUT);
    }

    #[cfg(unix)]
    #[test]
    fn embedded_command_bindings_are_stored_and_reloadable() {
        let first_dir = tempfile::tempdir().expect("first command directory");
        let second_dir = tempfile::tempdir().expect("second command directory");
        let make = |argument: &str, cwd: &Path| {
            let mut bindings = ExternalBindings::new(Duration::from_millis(5000), 65_536);
            bindings.sanitizers.insert(
                "classifier".to_string(),
                Binding::Command {
                    argv: vec!["python3".to_string(), argument.to_string()],
                    cwd: std::fs::canonicalize(cwd).expect("canonical command directory"),
                    token_env: None,
                },
            );
            embedded(bindings).expect("embedded command config loads")
        };

        let first = make("resolver.py", first_dir.path());
        let changed_argv = make("other.py", first_dir.path());
        let changed_cwd = make("resolver.py", second_dir.path());
        assert_ne!(first.policy_file().bytes(), changed_argv.policy_file().bytes());
        assert_ne!(first.policy_file().bytes(), changed_cwd.policy_file().bytes());

        let stored = first_dir.path().join("stored.toml");
        std::fs::write(&stored, first.policy_file().bytes()).expect("write stored embedded config");
        let reloaded = Config::load(&stored).expect("stored embedded config reloads");
        assert_eq!(reloaded.policy_file().bytes(), first.policy_file().bytes());
        let Some(Implementation::Command(command)) = reloaded.externals.sanitizers.get("classifier") else {
            panic!("reloaded classifier is a command")
        };
        assert_eq!(command.argv, ["python3", "resolver.py"]);
        assert_eq!(command.cwd, std::fs::canonicalize(first_dir.path()).unwrap());

        let mut relative = ExternalBindings::new(Duration::from_millis(5000), 65_536);
        relative.annotators.insert(
            "classifier".to_string(),
            Binding::Command {
                argv: vec!["python3".to_string()],
                cwd: PathBuf::from("battery"),
                token_env: None,
            },
        );
        assert!(matches!(
            embedded(relative),
            Err(ConfigError::RelativeCommandCwd {
                section: "annotators",
                ..
            })
        ));
    }

    /// An embedded host names its secrets like a file does: the variable is persisted,
    /// the value is resolved from the environment, and the bytes match a file that
    /// declares the same bindings.
    #[test]
    fn embedded_tokens_persist_as_variable_names_and_resolve_from_the_environment() {
        const VAR: &str = "APPA_CONFIG_TEST_EMBEDDED_TOKEN";
        let mut bindings = ExternalBindings::new(Duration::from_millis(5000), 65_536);
        bindings.authorities.insert(
            "desk".to_string(),
            Binding::Url {
                url: "https://desk.internal".to_string(),
                token_env: Some(VAR.to_string()),
            },
        );
        bindings.llm = Some(LlmBinding {
            provider: LlmProvider::Anthropic,
            model: "claude-sonnet-4-5".to_string(),
            url: None,
            token_env: Some(VAR.to_string()),
            timeout_ms: Some(30_000),
            max_concurrent: Some(2),
        });

        unsafe { std::env::remove_var(VAR) };
        assert!(matches!(
            embedded(bindings.clone()),
            Err(ConfigError::MissingSecret { .. })
        ));

        unsafe { std::env::set_var(VAR, "sekret") };
        let config = embedded(bindings).expect("the embedded bindings validate against the environment");
        unsafe { std::env::remove_var(VAR) };
        let Some(Implementation::Resolver(endpoint)) = config.externals.authorities.get("desk") else {
            panic!("desk is an endpoint")
        };
        assert_eq!(endpoint.token.as_ref().map(Token::reveal), Some("sekret"));
        let llm = config.externals.llm.as_ref().expect("the profile is set");
        assert_eq!(llm.token.as_ref().map(Token::reveal), Some("sekret"));
        assert_eq!(llm.max_concurrent, 2);
        let stored = String::from_utf8_lossy(config.policy_file().bytes()).into_owned();
        assert!(stored.contains(VAR), "the variable name is persisted");
        assert!(!stored.contains("sekret"), "the secret never reaches the stored bytes");

        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("appa.toml");
        std::fs::write(
            &path,
            format!(
                "[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n\
                 [externals.authorities.desk]\nurl = \"https://desk.internal\"\ntoken_env = \"{VAR}\"\n\
                 [externals.llm]\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-4-5\"\ntoken_env = \"{VAR}\"\ntimeout_ms = 30000\nmax_concurrent = 2\n"
            ),
        )
        .expect("write file config");
        unsafe { std::env::set_var(VAR, "sekret") };
        let from_file = Config::load(&path).expect("the file config loads");
        unsafe { std::env::remove_var(VAR) };
        assert_eq!(from_file.policy_file().bytes(), config.policy_file().bytes());
    }

    #[test]
    fn include_boundaries_are_strict() {
        let dir = tempfile::tempdir().expect("temp directory");
        let root = dir.path().join("appa.toml");
        let write_root = |include: &str| {
            std::fs::write(
                &root,
                format!(
                    "include = [{include}]\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"
                ),
            )
            .expect("write root config");
        };

        write_root("\"/absolute.toml\"");
        assert!(matches!(Config::load(&root), Err(ConfigError::AbsoluteInclude { .. })));

        write_root("\"battery.toml\", \"./battery.toml\"");
        std::fs::write(dir.path().join("battery.toml"), "[policy]\nversion = 2\n").expect("write included config");
        assert!(matches!(Config::load(&root), Err(ConfigError::DuplicateInclude { .. })));

        write_root("\"battery.toml\"");
        std::fs::write(
            dir.path().join("battery.toml"),
            "include = [\"nested.toml\"]\n[policy]\nversion = 2\n",
        )
        .expect("write nested include");
        assert!(matches!(Config::load(&root), Err(ConfigError::IncludedTopLevel { .. })));

        std::fs::write(dir.path().join("battery.toml"), "[policy]\nversion = 1\n").expect("write version mismatch");
        assert!(matches!(Config::load(&root), Err(ConfigError::IncludedVersion { .. })));
    }

    /// Per section: a fragment may add named entries and nothing else; the same name in
    /// the root or in another fragment is a refusal, never an override; the deployment
    /// tables are the root's alone.
    #[test]
    fn included_files_cannot_replace_root_settings_or_named_externals() {
        let dir = tempfile::tempdir().expect("temp directory");
        let root = dir.path().join("appa.toml");
        let write_root = |includes: &str| {
            std::fs::write(
                &root,
                format!(
                    "include = [{includes}]\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n\
                     [externals.annotators.classifier]\nurl = \"https://classifier.internal\"\n\
                     [externals.authorities.desk]\nurl = \"https://desk.internal\"\n"
                ),
            )
            .expect("write root config");
        };
        let battery = dir.path().join("battery.toml");
        write_root("\"battery.toml\"");

        std::fs::write(&battery, "[policy]\nversion = 2\nlimits = {}\n").expect("write singleton override");
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::IncludedPolicyField { .. })
        ));

        for section in Section::ALL {
            std::fs::write(
                &battery,
                format!(
                    "[policy]\nversion = 2\n[externals.{}.fresh]\nurl = \"https://fresh.internal\"\n",
                    section.name()
                ),
            )
            .expect("write fragment entry");
            let config = Config::load(&root).expect("a fragment adds an entry to any section");
            assert!(bound(section, &config, "fresh").is_some(), "{}", section.name());
        }

        for (section, name) in [("annotators", "classifier"), ("authorities", "desk")] {
            std::fs::write(
                &battery,
                format!("[policy]\nversion = 2\n[externals.{section}.{name}]\nurl = \"https://other.internal\"\n"),
            )
            .expect("write duplicate external");
            assert!(
                matches!(
                    Config::load(&root),
                    Err(ConfigError::DuplicateExternal { section: found, name: dup, .. }) if found == section && dup == name
                ),
                "{section}.{name} in a fragment must not override the root"
            );
        }

        write_root("\"battery.toml\", \"other.toml\"");
        for file in ["battery.toml", "other.toml"] {
            std::fs::write(
                dir.path().join(file),
                "[policy]\nversion = 2\n[externals.sanitizers.scrub]\nurl = \"https://scrub.internal\"\n",
            )
            .expect("write twin fragments");
        }
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::DuplicateExternal { section, name, .. }) if section == "sanitizers" && name == "scrub"
        ));
        write_root("\"battery.toml\"");

        for field in [
            "timeout_ms = 1",
            "review_timeout_ms = 1",
            "max_body_bytes = 1",
            "claude_code = { model = \"other\" }",
            "llm = { provider = \"openai\", model = \"m\" }",
        ] {
            std::fs::write(&battery, format!("[policy]\nversion = 2\n[externals]\n{field}\n"))
                .expect("write external singleton");
            assert!(
                matches!(Config::load(&root), Err(ConfigError::IncludedExternalsField { .. })),
                "{field} is root-only"
            );
        }
    }

    #[test]
    fn composed_bytes_are_stable_and_standalone() {
        let dir = tempfile::tempdir().expect("temp directory");
        let first = dir.path().join("first.toml");
        let second = dir.path().join("second.toml");
        std::fs::write(
            &first,
            "# comment\n[policy]\nversion=2\n[externals]\ntimeout_ms=5000\nmax_body_bytes=65536\n",
        )
        .expect("write first config");
        std::fs::write(
            &second,
            "[policy]\nversion = 2 # another comment\n\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
        )
        .expect("write second config");
        let first = Config::load(&first).expect("first config loads");
        let second = Config::load(&second).expect("second config loads");
        assert_eq!(first.policy_file().bytes(), second.policy_file().bytes());

        let standalone = dir.path().join("standalone.toml");
        std::fs::write(&standalone, first.policy_file().bytes()).expect("write composed bytes");
        Config::load(&standalone).expect("composed bytes load without source files");

        let ordered = |names: [&str; 2]| {
            let path = dir.path().join(format!("{}-{}.toml", names[0], names[1]));
            std::fs::write(
                &path,
                format!(
                    "[policy]\nversion = 2\n[[policy.tool]]\nname = {:?}\n[[policy.tool]]\nname = {:?}\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
                    names[0], names[1]
                ),
            )
            .expect("write ordered declarations");
            Config::load(&path).expect("ordered config loads")
        };
        assert_ne!(
            ordered(["alpha", "beta"]).policy_file().bytes(),
            ordered(["beta", "alpha"]).policy_file().bytes(),
            "declaration order is part of deployment identity"
        );
    }
}
