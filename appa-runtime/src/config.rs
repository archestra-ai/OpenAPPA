//! Configuration: one immutable `appa.toml` with the policy and the
//! external bindings. The harness adapter is a CLI flag on the
//! binary, not configuration.

pub mod edit;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use appa_engine::audience::{DeclaredTemplate, SourceRegistration, well_formed_reader};
use appa_engine::label::ReaderId;
use serde::Deserialize;

/// Each policy namespace bound to the connection identities the host reports for it.
pub(crate) type ServerBindings = BTreeMap<String, Vec<String>>;

/// Each child-credential variable a battery's helpers read, bound to the key the host's
/// own store holds its value under. The runtime never looks a key up; see
/// [`Config::credentials`].
pub(crate) type CredentialBindings = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct Config {
    policy: PolicyFile,
    /// Each policy namespace bound to the connection identities the host reports for it.
    pub(crate) server_aliases: ServerBindings,
    credentials: CredentialBindings,
    pub(crate) inventory: appa_runtime_api::inventory::ToolInventory,
    pub externals: Externals,
    /// Deployment knobs that describe this machine's reporting posture, not its policy.
    /// Deliberately outside [`PolicyFile`]: changing one must not move the policy file key
    /// that every session's opening binds to.
    pub reporting: Reporting,
    included_batteries: Vec<String>,
}

/// What this deployment reports about itself, and to whom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reporting {
    /// May an agent send a report on its own, through the `yell` tool? Off unless the
    /// deployment says otherwise, so an upgrade never starts an agent reporting.
    pub agent_yell: bool,
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

/// The two mandatory `[externals]` settings a host supplies for a hosted document that
/// states neither. A document that states one keeps its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostDefaults {
    pub consult_timeout: Duration,
    pub max_body_bytes: usize,
}

/// One battery a host composes under its root document: the name the host lists it
/// under, which is what errors and [`Config::included_batteries`] say, its `appa.toml`
/// text, and the runtime variables its externals may read. A battery is another
/// author's text: whatever `token_env` it names, the runtime would send to the URL
/// beside it, so a variable the host did not grant is refused.
#[derive(Debug, Clone, Copy)]
pub struct HostedBattery<'a> {
    pub name: &'a str,
    pub policy: &'a str,
    pub token_env: &'a [&'a str],
}

/// Why a host could not answer an include entry of a root document it composes with
/// [`Config::hosted_included`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncludeResolution {
    /// The host holds no battery under this spelling.
    Unknown,
    /// The host holds one but cannot serve it now: a package that fails its own
    /// validation, a store that cannot be read.
    Unavailable(String),
}

/// Whether a hosted root document may list the batteries that compose under it.
/// [`Config::hosted_included`] is the one entry point that admits `include`, and it
/// consumes the list, so the document it stores carries none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncludeAdmission {
    Refused,
    Consumed,
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
    /// One entry per audience source provider the policy's `[audience]` table references,
    /// under the provider's name, plus one per entry a provider's `lookup` names.
    pub audience: BTreeMap<String, AudienceBinding>,
    /// One program per `$input.<name>` an annotator reads, under the input's name.
    pub inputs: BTreeMap<String, AnnotatorImplementation>,
    /// Deployment knobs for the stock `claude-code` builtin.
    pub claude_code: ClaudeCode,
    /// The profile the stock `llm` builtin consults, where the deployment declares one.
    pub llm: Option<LlmProfile>,
}

impl Externals {
    /// The lookup routing these bindings declare: each redirected audience provider and
    /// the entry that answers its member lookups.
    pub(crate) fn lookup_targets(&self) -> BTreeMap<String, String> {
        self.audience
            .iter()
            .filter_map(|(name, binding)| Some((name.clone(), binding.lookup.clone()?)))
            .collect()
    }

    /// The audience sources these bindings declare: each entry with selector templates,
    /// as the policy compiles under them.
    pub(crate) fn source_registrations(&self) -> Vec<SourceRegistration> {
        self.audience
            .iter()
            .filter(|(_, binding)| !binding.templates.is_empty())
            .map(|(name, binding)| SourceRegistration {
                provider: name.clone(),
                templates: binding.templates.clone(),
            })
            .collect()
    }

    /// How many `llm` consults this deployment lets run at once: `max_concurrent` of its
    /// profile, none without one.
    pub(crate) fn llm_bound(&self) -> usize {
        self.llm.as_ref().map_or(0, |profile| profile.max_concurrent)
    }
}

/// The audience sources a composed document declares, read from its `[externals.audience]`
/// table: what a stored policy file compiles under at replay, and what a file that does not
/// load is described with.
pub(crate) fn source_registrations_of(document: &toml::Value) -> Result<Vec<SourceRegistration>, ConfigError> {
    appa_policy::declared_sources(document).map_err(|error| ConfigError::SelectorDeclaration(Box::new(error)))
}

/// The lookup routing a composed document declares, read from its `[externals.audience]`
/// table: what a stored policy file compiles under at replay, and what a file that does not
/// load is described with.
pub(crate) fn lookup_targets_of(document: &toml::Value) -> BTreeMap<String, String> {
    document
        .get("externals")
        .and_then(|externals| externals.get("audience"))
        .and_then(toml::Value::as_table)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(name, entry)| Some((name.clone(), entry.get("lookup")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
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
    /// The usable defaults every construction path shares — the `claude` on `PATH`, the
    /// `sonnet` alias, and the consult budget the file loader fills in, never an empty
    /// command.
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

/// One `[externals.audience.<name>]` entry: how it answers, and — for a provider whose
/// member lookups go elsewhere — the entry that answers them instead.
#[derive(Debug, Clone)]
pub struct AudienceBinding {
    pub implementation: AudienceImplementation,
    /// The entry that answers this provider's member lookups. `None`: the provider's own.
    pub lookup: Option<String>,
    /// The selector templates this source declares it serves. Empty for a roster and for
    /// an entry that only answers another provider's lookups: neither is a policy source.
    pub templates: Vec<DeclaredTemplate>,
}

/// How one audience entry answers: an HTTP endpoint, a local command, or an inline roster
/// the runtime reads itself — the reader each qualified member maps to. A roster answers
/// member lookups only, so it binds a `lookup` target and never a policy provider.
#[derive(Debug, Clone)]
pub enum AudienceImplementation {
    Resolver(Endpoint),
    Command(ResolverCommand),
    Readers(BTreeMap<ReaderId, ReaderId>),
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
    #[error("include path {path:?} must not contain a \"..\" component")]
    TraversingInclude { path: String },
    #[error("the root config includes {path:?} more than once")]
    DuplicateInclude { path: String },
    #[error(
        "the root config includes {path:?}, which the host does not compose{}",
        reason.as_deref().map(|reason| format!(": {reason}")).unwrap_or_default()
    )]
    UnresolvedInclude { path: String, reason: Option<String> },
    #[error("included config {path} has unsupported top-level field {field:?}")]
    IncludedTopLevel { path: String, field: String },
    #[error("included config {path} cannot set policy field {field:?}")]
    IncludedPolicyField { path: String, field: String },
    #[error("included config {path} confines the results of {tool:?}, a tool it does not declare")]
    IncludedConfinesForeignTool { path: String, tool: String },
    #[error("included config {path} cannot set externals field {field:?}")]
    IncludedExternalsField { path: String, field: String },
    #[error("[externals.audience] {0}")]
    SelectorDeclaration(Box<appa_policy::ConfigError>),
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
    #[error("the audience entry {name:?} maps {member:?}, {reason}")]
    BadReaderMapping {
        name: String,
        member: String,
        reason: String,
    },
    #[error("the audience entry {name:?} sends its lookups to {target:?}, which is not an [externals.audience] entry")]
    UnknownLookupTarget { name: String, target: String },
    #[error(
        "the audience entry {name:?} sends its lookups to {target:?}, which sends its own lookups elsewhere; a lookup target answers directly"
    )]
    ChainedLookup { name: String, target: String },
    #[error("the audience entry {name:?} is a readers table, which only another entry's lookup can name")]
    ReadersWithoutLookup { name: String },
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
    #[error("a hosted document declares {key:?}, which is the host's to declare, not the policy's")]
    HostedKey { key: String },
    #[error("the hosted {section} entry {name:?} runs a local command, which a hosted document cannot declare")]
    HostedCommand { section: &'static str, name: String },
    #[error("the host default {setting} is too large to write into a policy document")]
    UnrepresentableHostDefault { setting: &'static str },
    #[error("root config field {field:?} must be a table")]
    RootField { field: String },
    #[error("the root config declares {field:?}, which only a host that embeds the runtime declares")]
    FileRootField { field: String },
    #[error(
        "[credentials] declares {var}, which no battery helper reads as its own credential: the name starts with {}",
        PROVIDER_CREDENTIAL_PREFIX
    )]
    CredentialVariable { var: String },
    #[error("[credentials] binds {var} to no store key: the value is a string the host can look up")]
    CredentialValue { var: String },
    #[error("the document cannot be edited: {reason}")]
    UneditableDocument { reason: String },
    #[error("battery {battery} binds externals.{section}.{name:?} to {var:?}, a variable the host did not grant it")]
    UngrantedBatteryCredential {
        battery: String,
        section: String,
        name: String,
        var: String,
    },
}

/// The five sections a component binds under. Every section takes a URL or a command;
/// which builtin names a section accepts is the one difference, and the audience section
/// alone takes a `readers` roster and a `lookup` redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Authorities,
    Sanitizers,
    Annotators,
    /// One audience source per provider, plus lookup targets: `[externals.audience.<name>]`.
    Audience,
    /// One program per `$input.<name>` an annotator reads: `[externals.inputs.<name>]`.
    Inputs,
}

impl Section {
    pub(crate) const ALL: [Section; 5] = [
        Section::Authorities,
        Section::Sanitizers,
        Section::Annotators,
        Section::Audience,
        Section::Inputs,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Section::Authorities => "authorities",
            Section::Sanitizers => "sanitizers",
            Section::Annotators => "annotators",
            Section::Audience => "audience",
            Section::Inputs => "inputs",
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
    /// on its policy declaration instead, and an audience entry is never a builtin.
    fn check_builtin(self, name: &str, builtin: &str) -> Result<(), ConfigError> {
        let allowed = match self {
            Section::Annotators | Section::Audience | Section::Inputs => {
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
    /// Root-file only, and stripped before the composed document is rendered, so it never
    /// reaches [`PolicyFile::bytes`]. See [`Config::load`].
    #[serde(default)]
    reporting: RawReporting,
    /// Offline packaging inputs, not part of the policy identity.
    #[serde(default)]
    bundle: RawBundle,
    #[serde(default)]
    server_aliases: ServerBindings,
    /// Hosted documents only: the host's credential declaration, carried and never
    /// resolved. See [`Config::credentials`].
    #[serde(default)]
    credentials: CredentialBindings,
    #[serde(default)]
    appa_inventory: appa_runtime_api::inventory::ToolInventory,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReporting {
    #[serde(default)]
    agent_yell: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    #[serde(default, deserialize_with = "bundle_files")]
    files: Vec<String>,
}

fn bundle_files<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let files = Vec::<String>::deserialize(deserializer)?;
    let mut seen = std::collections::BTreeSet::new();
    for file in &files {
        let windows_prefix =
            file.as_bytes().get(1) == Some(&b':') && file.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        if file.is_empty() || file.contains(['\\', '\0']) || Path::new(file).is_absolute() || windows_prefix {
            return Err(serde::de::Error::custom(format!(
                "bundle file {file:?} must be a nonempty relative source path using forward slashes"
            )));
        }
        if !seen.insert(file) {
            return Err(serde::de::Error::custom(format!(
                "bundle file {file:?} is declared more than once"
            )));
        }
    }
    Ok(files)
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
    audience: BTreeMap<String, RawAudienceBinding>,
    #[serde(default)]
    inputs: BTreeMap<String, RawBinding>,
    claude_code: Option<RawClaudeCode>,
    llm: Option<RawLlm>,
}

impl RawExternals {
    /// Every entry that declares a `command`, by section and name.
    fn commanded(&self) -> impl Iterator<Item = (Section, &str)> {
        let bindings = [
            (Section::Authorities, &self.authorities),
            (Section::Sanitizers, &self.sanitizers),
            (Section::Annotators, &self.annotators),
            (Section::Inputs, &self.inputs),
        ];
        bindings
            .into_iter()
            .flat_map(|(section, table)| {
                table
                    .iter()
                    .filter(|(_, binding)| binding.command.is_some())
                    .map(move |(name, _)| (section, name.as_str()))
            })
            .chain(
                self.audience
                    .iter()
                    .filter(|(_, binding)| binding.command.is_some())
                    .map(|(name, _)| (Section::Audience, name.as_str())),
            )
    }

    /// Every command entry, by origin key.
    fn command_keys(&self) -> std::collections::BTreeSet<String> {
        self.commanded()
            .map(|(section, name)| section.origin_key(name))
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAudienceBinding {
    url: Option<String>,
    token_env: Option<String>,
    command: Option<Vec<String>>,
    readers: Option<BTreeMap<String, String>>,
    lookup: Option<String>,
    /// The selector templates this source serves, with what each may feed.
    selectors: Option<Vec<appa_policy::SelectorDeclaration>>,
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
        // A credential of a battery's helper reaches the child through its own `command`
        // binding here, from this process's environment; the declaration a host keeps in
        // its document names a store this loader has nothing to ask.
        if document
            .as_table()
            .expect("RawConfig parsed the root as a table")
            .contains_key("credentials")
        {
            return Err(ConfigError::FileRootField {
                field: "credentials".to_string(),
            });
        }

        let root_version = policy_version(&root.policy).ok_or(ConfigError::InvalidPolicyVersion)?;
        let root_annotators = declared_annotators(&root.policy);
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
            if let Some(name) = crate::batteries::name_from_include(include) {
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

        // The knob is this machine's, not the policy's. It is read from the root file's own
        // parse and dropped from the composed document, so the stored text — and with it the
        // policy file key every opening binds to — is byte-identical whether or not the
        // deployment set it. Carrying the value forward explicitly is what keeps it from
        // reverting to the default when the composed text is parsed back.
        let reporting = Reporting {
            agent_yell: root.reporting.agent_yell,
        };
        document
            .as_table_mut()
            .expect("RawConfig parsed the root as a table")
            .remove("reporting");
        // Keep authored packaging inputs out of the composed policy, just as
        // reporting settings remain local to the deployment root.
        let _ = &root.bundle.files;
        document
            .as_table_mut()
            .expect("RawConfig parsed the root as a table")
            .remove("bundle");

        let composed = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        let raw: RawConfig = toml::from_str(&composed).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        add_composed_metadata(&mut document, &raw.externals, &origins)?;
        let stored = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        Config::validate_composed(
            stored,
            raw,
            reporting,
            origins,
            included_batteries.into_iter().collect(),
            |var| std::env::var(var).ok(),
        )
    }

    /// The configuration of a host that holds its document in memory rather than reading
    /// `appa.toml`: a `[policy]` table, an optional `[externals]` table and the optional
    /// `[server_aliases]` and `[credentials]` tables, plus the two settings the host fills
    /// in where the document states neither. The document comes from an author who may run
    /// nothing on this machine and read none of its files, so every other top-level key and
    /// every `command` binding is refused. `server_aliases` stays admitted because it is the
    /// host's own declaration of which connections a namespace names, and a document that
    /// carries it is one snapshot: reopened or replayed under the same bytes, a trajectory
    /// resolves its rules the way it did when it opened. `[credentials]` is admitted on the
    /// same terms and carried, never resolved: it names, for each child-credential variable
    /// a battery's helpers read, the key the host's store holds it under, and the host's own
    /// authorization over who may write that table is the boundary. Validation is otherwise
    /// the file loader's, tokens included: a `token_env` this runtime resolves and sends
    /// itself reads this process's environment, here as in a file.
    pub fn hosted(text: &str, defaults: HostDefaults) -> Result<Config, ConfigError> {
        Config::hosted_composed(text, &[], defaults)
    }

    /// [`Config::hosted`] with batteries composed under the root document the way a
    /// root file's `include` composes them: each battery's declarations append to the
    /// root's, a root annotator replaces one included default, a repeated external is
    /// refused, and the composed document is what the configuration stores. The host
    /// holds every text in memory, so nothing here reads a file; a battery's `command`
    /// bindings are refused as the root's are, and a host that runs a battery's helpers
    /// rewrites them into `url` bindings it serves before composing.
    pub fn hosted_composed(
        root: &str,
        batteries: &[HostedBattery<'_>],
        defaults: HostDefaults,
    ) -> Result<Config, ConfigError> {
        let document = hosted_root(root, IncludeAdmission::Refused)?;
        Config::compose_hosted(document, batteries, defaults)
    }

    /// [`Config::hosted_composed`] where the root document's own `include` list says which
    /// batteries compose under it. Each entry, as authored, is handed to `resolve`; the
    /// battery it answers composes exactly as a [`Config::hosted_composed`] battery does,
    /// and the composed bytes are the same. The runtime does not read an entry as a path:
    /// it refuses an absolute one and one with a `..` component, and hands the
    /// rest to the host, whose answer names the battery. `include` is admitted here alone
    /// and consumed, so the stored document carries none and a stored snapshot reopens
    /// through [`Config::hosted`] unchanged.
    pub fn hosted_included<'a>(
        root: &str,
        defaults: HostDefaults,
        resolve: impl Fn(&str) -> Result<HostedBattery<'a>, IncludeResolution>,
    ) -> Result<Config, ConfigError> {
        let mut document = hosted_root(root, IncludeAdmission::Consumed)?;
        let entries = take_include(&mut document)?;
        let batteries = entries
            .iter()
            .map(|entry| {
                resolve(entry).map_err(|resolution| ConfigError::UnresolvedInclude {
                    path: entry.clone(),
                    reason: match resolution {
                        IncludeResolution::Unknown => None,
                        IncludeResolution::Unavailable(reason) => Some(reason),
                    },
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Config::compose_hosted(document, &batteries, defaults)
    }

    fn compose_hosted(
        mut document: toml::Value,
        batteries: &[HostedBattery<'_>],
        defaults: HostDefaults,
    ) -> Result<Config, ConfigError> {
        let document_table = document.as_table_mut().expect("a TOML document parses as a table");
        let root_policy = document_table.get("policy").ok_or(ConfigError::InvalidPolicyVersion)?;
        let root_version = policy_version(root_policy).ok_or(ConfigError::InvalidPolicyVersion)?;
        let root_annotators = declared_annotators(root_policy);
        // Composition appends included externals to the root's table, so an absent one
        // is the empty table it would be in a file loaded with none.
        document_table
            .entry("externals")
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        let mut included_batteries = std::collections::BTreeSet::new();
        let mut replaced_annotators = std::collections::BTreeSet::new();
        let mut origins = BTreeMap::new();
        for battery in batteries {
            if !included_batteries.insert(battery.name.to_string()) {
                return Err(ConfigError::DuplicateInclude {
                    path: battery.name.to_string(),
                });
            }
            let included: toml::Value = toml::from_str(battery.policy).map_err(|source| ConfigError::Unparsable {
                path: battery.name.to_string(),
                source,
            })?;
            refuse_ungranted_credentials(battery, &included)?;
            compose_include(
                &mut document,
                included,
                Path::new(battery.name),
                root_version,
                &root_annotators,
                &mut replaced_annotators,
                &mut origins,
            )?;
        }
        let document_table = document.as_table_mut().expect("a TOML document parses as a table");
        let timeout_ms = i64::try_from(defaults.consult_timeout.as_millis()).map_err(|_| {
            ConfigError::UnrepresentableHostDefault {
                setting: "consult_timeout",
            }
        })?;
        let max_body_bytes =
            i64::try_from(defaults.max_body_bytes).map_err(|_| ConfigError::UnrepresentableHostDefault {
                setting: "max_body_bytes",
            })?;
        // A document whose `externals` is not a table takes no defaults; parsing it as a
        // configuration below is what refuses it.
        if let Some(externals) = document_table
            .entry("externals")
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
            .as_table_mut()
        {
            externals
                .entry("timeout_ms")
                .or_insert(toml::Value::Integer(timeout_ms));
            externals
                .entry("max_body_bytes")
                .or_insert(toml::Value::Integer(max_body_bytes));
        }

        let rendered = toml::to_string(&document).map_err(|source| ConfigError::UnrenderablePolicy { source })?;
        let raw: RawConfig = toml::from_str(&rendered).map_err(|source| ConfigError::UnparsablePolicy { source })?;
        refuse_hosted_commands(&raw.externals)?;
        // No command survives the refusal above, so no entry has a working directory.
        Config::validate_composed(
            rendered,
            raw,
            Reporting::default(),
            BTreeMap::new(),
            included_batteries.into_iter().collect(),
            |var| std::env::var(var).ok(),
        )
    }

    pub fn policy_file(&self) -> &PolicyFile {
        &self.policy
    }

    /// Pin host observations beside the authored policy in the same stored snapshot.
    /// No process-local alias map is needed to reopen or replay this session.
    pub(crate) fn with_inventory(&self, inventory: appa_runtime_api::inventory::ToolInventory) -> Result<Self, String> {
        let mut document: toml::Value =
            toml::from_str(std::str::from_utf8(self.policy.bytes()).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        document.as_table_mut().ok_or("policy document is not a table")?.insert(
            "appa_inventory".into(),
            toml::Value::try_from(&inventory).map_err(|error| error.to_string())?,
        );
        let mut config = self.clone();
        config.policy = PolicyFile::new(
            toml::to_string(&document)
                .map_err(|error| error.to_string())?
                .into_bytes(),
            self.policy.value.clone(),
        );
        config.inventory = inventory;
        Ok(config)
    }

    pub fn included_batteries(&self) -> &[String] {
        &self.included_batteries
    }

    /// The host's own credential declaration: each child-credential variable a battery's
    /// helpers read, bound to the key the host's store holds its value under. The runtime
    /// carries this table and never reads a key: a hosted document runs no `command`, so
    /// the host that runs a battery's helper is the one that delivers the credential.
    pub fn credentials(&self) -> &BTreeMap<String, String> {
        &self.credentials
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
        Config::validate_composed(text, raw, Reporting::default(), origins, Vec::new(), lookup)
    }

    fn validate_composed(
        text: String,
        raw: RawConfig,
        reporting: Reporting,
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
            inputs,
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
            server_aliases: raw.server_aliases,
            credentials: raw.credentials,
            inventory: raw.appa_inventory,
            reporting,
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
                audience: resolve_audience_bindings(audience, &origins, &lookup)?,
                inputs: resolve(Section::Inputs, inputs)?
                    .into_iter()
                    .map(|(name, implementation)| (name, annotator_implementation(implementation)))
                    .collect(),
                claude_code: resolve_claude_code(claude_code)?,
                llm,
            },
        })
    }
}

/// The root document of a host, parsed and admitted: every top-level key is one a host
/// declares, and a `[credentials]` table names child-credential variables and store keys.
fn hosted_root(text: &str, include: IncludeAdmission) -> Result<toml::Value, ConfigError> {
    let document: toml::Value = toml::from_str(text).map_err(|source| ConfigError::UnparsablePolicy { source })?;
    let table = document.as_table().expect("a TOML document parses as a table");
    if let Some(key) = table.keys().find(|key| match key.as_str() {
        "policy" | "externals" | "server_aliases" | "credentials" => false,
        "include" => include == IncludeAdmission::Refused,
        _ => true,
    }) {
        return Err(ConfigError::HostedKey { key: key.clone() });
    }
    if let Some(credentials) = table.get("credentials") {
        refuse_foreign_credentials(credentials)?;
    }
    Ok(document)
}

/// Every `[credentials]` key is a child-credential variable, and every value is a store
/// key the host can look up. Which helper reads which variable is the host's concern:
/// the runtime carries the table and resolves nothing in it.
fn refuse_foreign_credentials(credentials: &toml::Value) -> Result<(), ConfigError> {
    let credentials = credentials.as_table().ok_or_else(|| ConfigError::RootField {
        field: "credentials".to_string(),
    })?;
    for (var, key) in credentials {
        refuse_foreign_credential(var, key.as_str())?;
    }
    Ok(())
}

/// One `[credentials]` entry: a child-credential variable and the store key the host
/// holds its value under. `key` is nothing where a document states something that is not
/// a string. [`edit::set_credential`] refuses an entry here before it writes one, so a
/// host hears the same refusal whether it edits the document or opens it.
pub(crate) fn refuse_foreign_credential(var: &str, key: Option<&str>) -> Result<(), ConfigError> {
    if !var.starts_with(PROVIDER_CREDENTIAL_PREFIX) {
        return Err(ConfigError::CredentialVariable { var: var.to_string() });
    }
    if !key.is_some_and(|key| !key.is_empty()) {
        return Err(ConfigError::CredentialValue { var: var.to_string() });
    }
    Ok(())
}

/// The include list a hosted root authored, taken out of the document it composes, with
/// every entry refused that no host answer could rescue: one this runtime cannot read as
/// a relative spelling, and one the root states twice.
fn take_include(document: &mut toml::Value) -> Result<Vec<String>, ConfigError> {
    let table = document.as_table_mut().expect("a TOML document parses as a table");
    let Some(include) = table.remove("include") else {
        return Ok(Vec::new());
    };
    let entries = Vec::<String>::deserialize(include).map_err(|source| ConfigError::UnparsablePolicy { source })?;
    let mut seen = std::collections::BTreeSet::new();
    for entry in &entries {
        refuse_include_entry(entry)?;
        if !seen.insert(entry) {
            return Err(ConfigError::DuplicateInclude { path: entry.clone() });
        }
    }
    Ok(entries)
}

/// One include entry, as authored: a spelling this runtime resolves beside the root
/// config, or hands to a host that holds the battery under it. Neither reads a spelling
/// that starts elsewhere or leaves the root. [`edit::add_include`] refuses an entry here
/// before it writes one, so a document the editor wrote is one a loader reads.
pub(crate) fn refuse_include_entry(entry: &str) -> Result<(), ConfigError> {
    let path = Path::new(entry);
    if path.is_absolute() {
        return Err(ConfigError::AbsoluteInclude {
            path: entry.to_string(),
        });
    }
    if path.components().any(|component| component == Component::ParentDir) {
        return Err(ConfigError::TraversingInclude {
            path: entry.to_string(),
        });
    }
    Ok(())
}

/// A hosted document's author runs nothing on the machine that holds it: no binding takes
/// a `command`, and the `claude-code` builtin stays on the executable the host installed.
fn refuse_hosted_commands(externals: &RawExternals) -> Result<(), ConfigError> {
    if let Some((section, name)) = externals.commanded().next() {
        return Err(ConfigError::HostedCommand {
            section: section.name(),
            name: name.to_string(),
        });
    }
    match externals.claude_code.as_ref().and_then(|table| table.command.as_ref()) {
        Some(_) => Err(ConfigError::HostedCommand {
            section: "claude_code",
            name: "claude_code".to_string(),
        }),
        None => Ok(()),
    }
}

fn policy_version(policy: &toml::Value) -> Option<i64> {
    policy.as_table()?.get("version")?.as_integer()
}

/// Resolve root command origins without reading their working directories.
/// The caller supplies the absolute directory of the declaring root config.
#[cfg(feature = "daemon")]
pub(crate) fn root_command_directories(
    text: &str,
    source_dir: &Path,
) -> Result<BTreeMap<String, PathBuf>, ConfigError> {
    let root: RawConfig = toml::from_str(text).map_err(|source| ConfigError::UnparsablePolicy { source })?;
    root_command_origins(&root, source_dir)
}

/// The working directory of every command entry the root declares: the root's own
/// directory, or the directory its metadata recorded. Included commands acquire
/// their own origins during composition and are not keys in this root map.
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

/// The annotator names a policy declares; a root's set decides which included
/// defaults its own declarations replace.
fn declared_annotators(policy: &toml::Value) -> std::collections::BTreeSet<String> {
    policy
        .get("annotator")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(declaration_name)
        .map(str::to_owned)
        .collect()
}

/// Every `token_env` a battery's externals name is one the host granted it. Shapes
/// other than a table of tables are left to composition, which refuses them.
fn refuse_ungranted_credentials(battery: &HostedBattery<'_>, included: &toml::Value) -> Result<(), ConfigError> {
    let sections = included
        .get("externals")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|externals| {
            externals
                .iter()
                .filter_map(|(section, entries)| Some((section, entries.as_table()?)))
        });
    for (section, entries) in sections {
        for (name, entry) in entries {
            if let Some(var) = entry.get("token_env").and_then(toml::Value::as_str)
                && !battery.token_env.contains(&var)
            {
                return Err(ConfigError::UngrantedBatteryCredential {
                    battery: battery.name.to_string(),
                    section: section.clone(),
                    name: name.clone(),
                    var: var.to_string(),
                });
            }
        }
    }
    Ok(())
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
    let mut included_policy = included_policy;
    if let Some(deployment) = included_policy.remove("deployment") {
        compose_included_confinement(root_policy, &included_policy, deployment, include_path)?;
    }
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
        .ok_or_else(|| ConfigError::RootField {
            field: "externals".to_string(),
        })?;
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
            .ok_or_else(|| ConfigError::RootField {
                field: format!("externals.{section_name}"),
            })?;
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

/// The one deployment setting a fragment may carry: `confined_results` over tools the
/// fragment itself declares, so a battery that ships an output sanitizer also names the
/// result it withholds. The names join the root's list; every other deployment key stays
/// the root's alone.
fn compose_included_confinement(
    root_policy: &mut toml::map::Map<String, toml::Value>,
    included_policy: &toml::map::Map<String, toml::Value>,
    deployment: toml::Value,
    include_path: &Path,
) -> Result<(), ConfigError> {
    let refused = |field: &str| ConfigError::IncludedPolicyField {
        path: include_path.display().to_string(),
        field: format!("deployment.{field}"),
    };
    let deployment = deployment.as_table().ok_or_else(|| refused(""))?;
    if let Some(field) = deployment.keys().find(|field| field.as_str() != "confined_results") {
        return Err(refused(field));
    }
    let confined: Vec<String> = deployment
        .get("confined_results")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| refused("confined_results"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| refused("confined_results"))
        })
        .collect::<Result<_, _>>()?;
    let declared: std::collections::BTreeSet<&str> = included_policy
        .get("tool")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(declaration_name)
        .map(|name| name.split('(').next().unwrap_or(name))
        .collect();
    if let Some(tool) = confined.iter().find(|tool| !declared.contains(tool.as_str())) {
        return Err(ConfigError::IncludedConfinesForeignTool {
            path: include_path.display().to_string(),
            tool: tool.clone(),
        });
    }
    let destination = root_policy
        .entry("deployment".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| refused(""))?
        .entry("confined_results".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| refused("confined_results"))?;
    for tool in confined {
        if !destination.iter().any(|entry| entry.as_str() == Some(&tool)) {
            destination.push(toml::Value::String(tool));
        }
    }
    Ok(())
}

/// An annotator or input binding after its section refused every `builtin` at parse.
fn annotator_implementation(implementation: Implementation) -> AnnotatorImplementation {
    match implementation {
        Implementation::Resolver(endpoint) => AnnotatorImplementation::Resolver(endpoint),
        Implementation::Command(command) => AnnotatorImplementation::Command(command),
        Implementation::Builtin(_) => unreachable!("the annotators and inputs sections refuse every builtin"),
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
        (None, None, Some(argv)) => Ok(Implementation::Command(resolve_command(
            section, name, argv, token_env, origins,
        )?)),
        _ => Err(ConfigError::ImplementationChoice {
            section: section.name(),
            name: name.to_string(),
        }),
    }
}

/// The audience section's entries: a URL or command source, or a `readers` roster, each
/// with the optional `lookup` redirect. A redirect names another entry of this table that
/// answers directly — never one that redirects in turn — and a roster exists only to be
/// named by one.
fn resolve_audience_bindings(
    raw: BTreeMap<String, RawAudienceBinding>,
    origins: &BTreeMap<String, PathBuf>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, AudienceBinding>, ConfigError> {
    let section = Section::Audience;
    let mut bindings = BTreeMap::new();
    for (name, entry) in raw {
        let RawAudienceBinding {
            url,
            token_env,
            command,
            readers,
            lookup: redirect,
            selectors,
        } = entry;
        let templates = match &selectors {
            None => Vec::new(),
            Some(selectors) => appa_policy::declare_templates(&name, selectors)
                .map_err(|error| ConfigError::SelectorDeclaration(Box::new(error)))?,
        };
        // A roster answers lookups only and a source declares what it serves, so a roster
        // with `selectors` is neither.
        let implementation = match (url, command, readers) {
            (Some(url), None, None) => {
                let url = validated_url(section.name(), &name, url)?;
                let token = resolve_token(section.name(), &name, token_env, lookup)?;
                AudienceImplementation::Resolver(Endpoint::new(url, token))
            }
            (None, Some(argv), None) => {
                AudienceImplementation::Command(resolve_command(section, &name, argv, token_env, origins)?)
            }
            (None, None, Some(readers)) if token_env.is_none() && templates.is_empty() => {
                AudienceImplementation::Readers(resolve_readers(&name, readers)?)
            }
            _ => {
                return Err(ConfigError::ImplementationChoice {
                    section: section.name(),
                    name,
                });
            }
        };
        bindings.insert(
            name,
            AudienceBinding {
                implementation,
                lookup: redirect,
                templates,
            },
        );
    }
    for (name, binding) in &bindings {
        let Some(target) = &binding.lookup else {
            continue;
        };
        let Some(answering) = bindings.get(target) else {
            return Err(ConfigError::UnknownLookupTarget {
                name: name.clone(),
                target: target.clone(),
            });
        };
        if answering.lookup.is_some() {
            return Err(ConfigError::ChainedLookup {
                name: name.clone(),
                target: target.clone(),
            });
        }
    }
    // A roster exists for the providers that send it their lookups, and every key belongs
    // to one of them: a key under any other prefix could never be asked.
    for (name, binding) in &bindings {
        let AudienceImplementation::Readers(readers) = &binding.implementation else {
            continue;
        };
        let routed: std::collections::BTreeSet<&str> = bindings
            .iter()
            .filter(|(_, other)| other.lookup.as_deref() == Some(name))
            .map(|(provider, _)| provider.as_str())
            .collect();
        if routed.is_empty() {
            return Err(ConfigError::ReadersWithoutLookup { name: name.clone() });
        }
        if let Some(member) = readers.keys().find(|member| {
            !member
                .provider_prefix()
                .is_some_and(|provider| routed.contains(provider))
        }) {
            return Err(ConfigError::BadReaderMapping {
                name: name.clone(),
                member: member.as_str().to_string(),
                reason: "whose provider sends no lookups to this roster".to_string(),
            });
        }
    }
    Ok(bindings)
}

/// A roster's entries under the one reader shape rule: each key a provider-qualified
/// member, each value an address or a reader in that member's own provider namespace.
fn resolve_readers(name: &str, readers: BTreeMap<String, String>) -> Result<BTreeMap<ReaderId, ReaderId>, ConfigError> {
    let refused = |member: &str, reason: &str| ConfigError::BadReaderMapping {
        name: name.to_string(),
        member: member.to_string(),
        reason: reason.to_string(),
    };
    readers
        .into_iter()
        .map(|(member, principal)| {
            let member = ReaderId::new(member);
            let Some(provider) = member.provider_prefix().filter(|_| member.is_literal()) else {
                return Err(refused(member.as_str(), "which is not a <provider>:<id> member"));
            };
            let principal = ReaderId::new(principal);
            if !well_formed_reader(provider, &principal) {
                return Err(refused(
                    member.as_str(),
                    &format!(
                        "to {:?}, which is neither an address nor a {provider}-qualified id",
                        principal.as_str()
                    ),
                ));
            }
            Ok((member, principal))
        })
        .collect()
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
) -> Result<ResolverCommand, ConfigError> {
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
        Ok(ResolverCommand {
            argv,
            cwd: origins
                .get(&section.origin_key(name))
                .expect("every composed command binding records its source")
                .clone(),
            token_env,
        })
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

    const BUNDLE_ROOT: &str = "[policy]\nversion=2\n[externals]\ntimeout_ms=5000\nmax_body_bytes=65536\n";

    #[test]
    fn bundle_files_do_not_change_policy_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, BUNDLE_ROOT).unwrap();
        let before = Config::load(&path).unwrap();
        std::fs::write(
            &path,
            format!("{BUNDLE_ROOT}[bundle]\nfiles=[\"local.py\", \"../shared/data.json\"]\n"),
        )
        .unwrap();
        let after = Config::load(&path).unwrap();
        assert_eq!(before.policy_file().bytes(), after.policy_file().bytes());
        assert!(!String::from_utf8_lossy(after.policy_file().bytes()).contains("bundle"));
    }

    #[test]
    fn bundle_files_reject_unknown_types_and_unsafe_source_paths() {
        for table in [
            "file=[]",
            "files=\"helper.py\"",
            "files=[1]",
            "files=[\"\"]",
            "files=[\"/absolute.py\"]",
            "files=['C:/helper.py']",
            "files=['C:helper.py']",
            "files=['helper\\file.py']",
            "files=['helper.py', 'helper.py']",
        ] {
            let text = format!("{BUNDLE_ROOT}[bundle]\n{table}\n");
            assert!(toml::from_str::<RawConfig>(&text).is_err(), "accepted {table}");
        }
        assert!(toml::from_str::<RawConfig>(&format!("{BUNDLE_ROOT}[bundle]\n")).is_ok());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("appa.toml"),
            format!("include=['included.toml']\n{BUNDLE_ROOT}"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("included.toml"),
            "[policy]\nversion=2\n[bundle]\nfiles=[]\n",
        )
        .unwrap();
        assert!(
            matches!(Config::load(&dir.path().join("appa.toml")), Err(ConfigError::IncludedTopLevel { field, .. }) if field == "bundle")
        );
    }

    #[cfg(feature = "daemon")]
    #[test]
    #[cfg(unix)]
    fn root_command_metadata_and_include_origins_compose() {
        let dir = tempfile::tempdir().unwrap();
        let root_dir = dir.path().canonicalize().unwrap();
        let command_dir = root_dir.join("snapshot-root");
        let included_dir = root_dir.join("shared");
        std::fs::create_dir(&included_dir).unwrap();
        std::fs::write(
            included_dir.join("policy.toml"),
            "[policy]\nversion=2\n[externals.sanitizers.shared]\ncommand=['python3','helper.py']\n",
        )
        .unwrap();
        let text = format!(
            "include=['shared/policy.toml']\n{BUNDLE_ROOT}[externals.annotators.root]\ncommand=['python3','root.py']\n[appa_composed.command_cwd]\n'annotators.root'={}\n",
            toml::Value::String(command_dir.to_str().unwrap().to_owned())
        );
        let origins = root_command_directories(&text, &root_dir).unwrap();
        assert_eq!(
            origins,
            BTreeMap::from([("annotators.root".to_owned(), command_dir.clone())])
        );
        // No filesystem access to the recorded cwd is needed for composition.
        assert!(!command_dir.exists());
        std::fs::write(root_dir.join("appa.toml"), &text).unwrap();
        let config = Config::load(&root_dir.join("appa.toml")).unwrap();
        let AnnotatorImplementation::Command(root) = &config.externals.annotators["root"] else {
            panic!("root command missing")
        };
        let Implementation::Command(shared) = &config.externals.sanitizers["shared"] else {
            panic!("included command missing")
        };
        assert_eq!(root.cwd, command_dir);
        assert_eq!(shared.cwd, included_dir);
        let mismatched = text.replace("'annotators.root'=", "'sanitizers.shared'=");
        assert!(matches!(
            root_command_directories(&mismatched, &root_dir),
            Err(ConfigError::InvalidComposedMetadata { .. })
        ));
    }

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
        // Config rejects command bindings on non-Unix hosts.
        Command(#[cfg_attr(not(unix), allow(dead_code))] &'a ResolverCommand),
        Builtin(&'a str),
        Readers,
    }

    fn bound<'a>(section: Section, config: &'a Config, name: &str) -> Option<Bound<'a>> {
        let table = match section {
            Section::Authorities => &config.externals.authorities,
            Section::Sanitizers => &config.externals.sanitizers,
            Section::Audience => {
                return config
                    .externals
                    .audience
                    .get(name)
                    .map(|binding| match &binding.implementation {
                        AudienceImplementation::Resolver(_) => Bound::Url,
                        AudienceImplementation::Command(command) => Bound::Command(command),
                        AudienceImplementation::Readers(_) => Bound::Readers,
                    });
            }
            Section::Annotators | Section::Inputs => {
                let table = match section {
                    Section::Inputs => &config.externals.inputs,
                    _ => &config.externals.annotators,
                };
                return table.get(name).map(|implementation| match implementation {
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
        let Some(AudienceImplementation::Command(command)) = config
            .externals
            .audience
            .get("slack")
            .map(|binding| &binding.implementation)
        else {
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
        // An Annotator names a stock builtin on its policy declaration, never here; an
        // audience entry has no builtin key at all.
        for builtin in ["hitl", "approve", "redact-email", "claude-code", "llm", "some-module"] {
            assert!(
                matches!(
                    cell(Section::Annotators, builtin),
                    Err(ConfigError::BuiltinNotAllowed { .. })
                ),
                "annotators must refuse builtin {builtin}"
            );
            assert!(
                toml::from_str::<RawConfig>(&entry(Section::Audience, &format!("builtin = \"{builtin}\""))).is_err(),
                "audience must refuse builtin {builtin}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_audience_lookup_names_a_direct_target_and_a_roster_is_only_a_target() {
        let with = |audience: &str| format!("{MINIMAL}\n{audience}\n");
        let source = "[externals.audience.github]\ncommand = [\"python3\", \"source.py\"]\nlookup = \"people\"\n";
        let roster = "[externals.audience.people]\nreaders = { \"github:alice\" = \"alice@corp.example\", \"github:bob\" = \"github:robert\" }\n";

        let config = parse(&with(&format!("{source}{roster}"))).expect("a roster target validates");
        assert_eq!(config.externals.audience["github"].lookup.as_deref(), Some("people"));
        let Some(AudienceImplementation::Readers(readers)) = config
            .externals
            .audience
            .get("people")
            .map(|binding| &binding.implementation)
        else {
            panic!("the roster binds as readers");
        };
        assert_eq!(
            readers.get(&ReaderId::new("github:alice")).map(ReaderId::as_str),
            Some("alice@corp.example")
        );

        assert!(matches!(
            parse(&with(source)),
            Err(ConfigError::UnknownLookupTarget { .. })
        ));
        assert!(matches!(
            parse(&with(roster)),
            Err(ConfigError::ReadersWithoutLookup { .. })
        ));
        let chained = format!(
            "{source}[externals.audience.people]\ncommand = [\"python3\", \"people.py\"]\nlookup = \"github\"\n"
        );
        assert!(matches!(parse(&with(&chained)), Err(ConfigError::ChainedLookup { .. })));
        let self_lookup = "[externals.audience.github]\ncommand = [\"python3\", \"source.py\"]\nlookup = \"github\"\n";
        assert!(matches!(
            parse(&with(self_lookup)),
            Err(ConfigError::ChainedLookup { .. })
        ));

        for (member, principal) in [
            ("alice", "alice@corp.example"),
            ("github:alice", "public"),
            ("github:alice", "slack:U1"),
            ("github:alice", "alice"),
            ("github:", "alice@corp.example"),
            ("githuub:alice", "alice@corp.example"),
        ] {
            let bad = format!("{source}[externals.audience.people]\nreaders = {{ \"{member}\" = \"{principal}\" }}\n");
            assert!(
                matches!(parse(&with(&bad)), Err(ConfigError::BadReaderMapping { .. })),
                "{member} -> {principal} must be refused"
            );
        }
        let two = format!("{source}[externals.audience.people]\nreaders = {{}}\nurl = \"https://x.internal\"\n");
        assert!(matches!(
            parse(&with(&two)),
            Err(ConfigError::ImplementationChoice { .. })
        ));
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
    fn another_spelling_of_a_battery_file_is_not_a_battery_include() {
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
        assert!(config.included_batteries().is_empty());
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
                lookup = "people"
                [externals.audience.people]
                command = ["python3", "people.py"]
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
            (Section::Audience, "people"),
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

    const HOST_DEFAULTS: HostDefaults = HostDefaults {
        consult_timeout: Duration::from_millis(5000),
        max_body_bytes: 65_536,
    };

    fn hosted(text: &str) -> Result<Config, ConfigError> {
        Config::hosted(text, HOST_DEFAULTS)
    }

    fn hosted_composed(root: &str, batteries: &[HostedBattery<'_>]) -> Result<Config, ConfigError> {
        Config::hosted_composed(root, batteries, HOST_DEFAULTS)
    }

    fn hosted_included<'a>(
        root: &str,
        resolve: impl Fn(&str) -> Result<HostedBattery<'a>, IncludeResolution>,
    ) -> Result<Config, ConfigError> {
        Config::hosted_included(root, HOST_DEFAULTS, resolve)
    }

    /// Every battery the host of these tests holds, under the spelling its root
    /// documents include it as.
    fn hosted_store(entry: &str) -> Result<HostedBattery<'static>, IncludeResolution> {
        match entry {
            "batteries/notes@sha256-1111/appa.toml" => Ok(HostedBattery {
                name: "notes",
                policy: NOTES_BATTERY,
                token_env: &[],
            }),
            "batteries/desk@sha256-2222/appa.toml" => Ok(HostedBattery {
                name: "desk",
                policy: DESK_BATTERY,
                token_env: &[],
            }),
            _ => Err(IncludeResolution::Unknown),
        }
    }

    const NOTES_BATTERY: &str = "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/notes/read\"\ndelta = {}\n";

    const DESK_BATTERY: &str = "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/desk/list\"\ndelta = {}\n";

    fn tool_names(config: &Config) -> Vec<String> {
        config.tool_names()
    }

    const HOSTED_ROOT: &str = "[policy]\nversion = 2\n[[policy.tool]]\nname = \"root__read\"\ndelta = {}\n";

    const GITHUB_BATTERY: &str = r#"
        [policy]
        version = 2

        [[policy.annotator]]
        name = "github.visibility"
        ranks = ["suspicious"]
        audiences = []
        marks = []

        [[policy.tool]]
        name = "mcp/github/get_file_contents"
        annotator = "github.visibility"

        [externals.annotators."github.visibility"]
        url = "http://127.0.0.1:9000/api/openappa/helpers/install-1/github.visibility"
        token_env = "APPA_HOSTED_TEST_BRIDGE_TOKEN"
    "#;

    /// The host's own binding of policy namespaces to connections rides inside the
    /// document, so the bytes a trajectory opens under resolve the same way on reload.
    #[test]
    fn a_hosted_document_may_bind_server_aliases() {
        let config = hosted("[server_aliases]\ngithub = [\"github_prod\", \"github_dev\"]\n[policy]\nversion = 2\n")
            .expect("server aliases are the host's to declare");
        assert_eq!(
            config.server_aliases,
            BTreeMap::from([(
                "github".to_string(),
                vec!["github_prod".to_string(), "github_dev".to_string()]
            )])
        );
        assert!(String::from_utf8_lossy(config.policy_file().bytes()).contains("server_aliases"));
    }

    #[test]
    fn hosted_batteries_compose_under_the_root_and_reload_as_one_document() {
        // SAFETY: the test process sets its own variable and every reader is this test.
        unsafe { std::env::set_var("APPA_HOSTED_TEST_BRIDGE_TOKEN", "bridge") };
        let config = hosted_composed(
            HOSTED_ROOT,
            &[HostedBattery {
                name: "github",
                policy: GITHUB_BATTERY,
                token_env: &["APPA_HOSTED_TEST_BRIDGE_TOKEN"],
            }],
        )
        .expect("the battery composes");
        assert_eq!(tool_names(&config), ["root__read", "mcp/github/get_file_contents"]);
        assert_eq!(config.included_batteries(), ["github"]);
        assert!(config.externals.annotators.contains_key("github.visibility"));

        let stored = String::from_utf8(config.policy_file().bytes().to_vec()).expect("UTF-8");
        let reloaded = hosted(&stored).expect("the composed document is itself a hosted document");
        assert_eq!(reloaded.policy_file().bytes(), config.policy_file().bytes());
        assert_eq!(
            hosted_composed(
                HOSTED_ROOT,
                &[HostedBattery {
                    name: "github",
                    policy: GITHUB_BATTERY,
                    token_env: &["APPA_HOSTED_TEST_BRIDGE_TOKEN"],
                }],
            )
            .expect("composes again")
            .policy_file()
            .bytes(),
            config.policy_file().bytes(),
            "composition is deterministic"
        );
    }

    const INCLUDED_ROOT: &str = "include = [\"batteries/notes@sha256-1111/appa.toml\", \"batteries/desk@sha256-2222/appa.toml\"]\n[policy]\nversion = 2\n[[policy.tool]]\nname = \"root__read\"\ndelta = {}\n[credentials]\nAPPA_PROVIDER_NOTES_TOKEN = \"notes_prod\"\n";

    /// A root that lists its own batteries composes what the host would have composed by
    /// handing the same batteries in, and the list it composed from is spent: the stored
    /// document is one a host reopens with no list of its own.
    #[test]
    fn a_hosted_root_composes_the_batteries_its_include_list_names() {
        let config = hosted_included(INCLUDED_ROOT, hosted_store).expect("the host answers both entries");
        assert_eq!(
            tool_names(&config),
            ["root__read", "mcp/notes/read", "mcp/desk/list"],
            "each battery appends in the order the list states it"
        );
        assert_eq!(config.included_batteries(), ["desk", "notes"]);

        let handed = hosted_composed(
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"root__read\"\ndelta = {}\n[credentials]\nAPPA_PROVIDER_NOTES_TOKEN = \"notes_prod\"\n",
            &[
                hosted_store("batteries/notes@sha256-1111/appa.toml").unwrap(),
                hosted_store("batteries/desk@sha256-2222/appa.toml").unwrap(),
            ],
        )
        .expect("the same batteries compose");
        assert_eq!(config.policy_file().bytes(), handed.policy_file().bytes());

        let stored = String::from_utf8(config.policy_file().bytes().to_vec()).expect("UTF-8");
        assert!(!stored.contains("include"), "the include list is consumed");
        let reopened = hosted(&stored).expect("the stored document is a hosted document");
        assert_eq!(reopened.policy_file().bytes(), config.policy_file().bytes());
        assert_eq!(reopened.credentials(), config.credentials());
        assert_eq!(
            reopened.credentials(),
            &BTreeMap::from([("APPA_PROVIDER_NOTES_TOKEN".to_string(), "notes_prod".to_string())])
        );
    }

    /// The runtime does not read an include entry as anything but text: the host's own
    /// spelling reaches it whole, and the host's answer names the battery.
    #[test]
    fn an_include_entry_reaches_the_host_as_authored() {
        let seen = std::cell::RefCell::new(Vec::new());
        let config = hosted_included(INCLUDED_ROOT, |entry| {
            seen.borrow_mut().push(entry.to_string());
            hosted_store(entry)
        })
        .expect("the host answers both entries");
        assert_eq!(
            seen.into_inner(),
            [
                "batteries/notes@sha256-1111/appa.toml",
                "batteries/desk@sha256-2222/appa.toml"
            ]
        );
        assert_eq!(config.included_batteries(), ["desk", "notes"]);
    }

    /// An entry this runtime would have to read as a path of its own is refused without
    /// asking the host, and so is one the root states twice.
    #[test]
    fn an_include_entry_the_runtime_refuses_never_reaches_the_host() {
        let unasked = |_: &str| -> Result<HostedBattery<'static>, IncludeResolution> {
            panic!("a refused entry is never resolved")
        };
        let root = |include: &str| format!("{include}\n{HOSTED_ROOT}");
        for list in ["include = \"batteries/notes@sha256-1111/appa.toml\"", "include = [7]"] {
            assert!(
                matches!(
                    hosted_included(&root(list), unasked),
                    Err(ConfigError::UnparsablePolicy { .. })
                ),
                "{list}"
            );
        }
        assert!(matches!(
            hosted_included(&root("include = [\"/srv/batteries/notes/appa.toml\"]"), unasked),
            Err(ConfigError::AbsoluteInclude { path }) if path == "/srv/batteries/notes/appa.toml"
        ));
        assert!(matches!(
            hosted_included(&root("include = [\"../notes/appa.toml\"]"), unasked),
            Err(ConfigError::TraversingInclude { path }) if path == "../notes/appa.toml"
        ));
        assert!(matches!(
            hosted_included(&root("include = [\"batteries/notes@sha256-1111/appa.toml\", \"batteries/notes@sha256-1111/appa.toml\"]"), unasked),
            Err(ConfigError::DuplicateInclude { path }) if path == "batteries/notes@sha256-1111/appa.toml"
        ));
    }

    /// The host answers for the batteries it holds: an entry it does not know, one it
    /// cannot serve now, and two entries that name one battery are each refused.
    #[test]
    fn a_host_that_cannot_answer_an_entry_refuses_the_document() {
        let root = |include: &str| format!("{include}\n{HOSTED_ROOT}");
        assert!(matches!(
            hosted_included(&root("include = [\"batteries/gone@sha256-9999/appa.toml\"]"), hosted_store),
            Err(ConfigError::UnresolvedInclude { path, reason: None }) if path == "batteries/gone@sha256-9999/appa.toml"
        ));
        assert!(matches!(
            hosted_included(&root("include = [\"batteries/notes@sha256-1111/appa.toml\"]"), |_| Err(
                IncludeResolution::Unavailable("the upload does not validate".to_string())
            )),
            Err(ConfigError::UnresolvedInclude { path, reason: Some(reason) })
                if path == "batteries/notes@sha256-1111/appa.toml" && reason == "the upload does not validate"
        ));
        assert!(matches!(
            hosted_included(
                &root("include = [\"batteries/notes@sha256-1111/appa.toml\", \"batteries/notes@sha256-3333/appa.toml\"]"),
                |_| Ok(HostedBattery {
                    name: "notes",
                    policy: NOTES_BATTERY,
                    token_env: &[],
                })
            ),
            Err(ConfigError::DuplicateInclude { path }) if path == "notes"
        ));
    }

    /// The host's credential declaration rides inside the document, as its server
    /// bindings do: it survives composition, reopening, and naming a variable no
    /// environment of this process holds, because the runtime resolves none of it.
    #[test]
    fn a_hosted_document_carries_the_hosts_credential_declaration() {
        const TEXT: &str = "[credentials]\nAPPA_PROVIDER_GITHUB_TOKEN = \"github_prod\"\n[policy]\nversion = 2\n";
        let declared = BTreeMap::from([("APPA_PROVIDER_GITHUB_TOKEN".to_string(), "github_prod".to_string())]);
        let config = hosted(TEXT).expect("credentials are the host's to declare");
        assert_eq!(config.credentials(), &declared);
        let stored = String::from_utf8(config.policy_file().bytes().to_vec()).expect("UTF-8");
        let reopened = hosted(&stored).expect("the stored document reopens");
        assert_eq!(reopened.credentials(), &declared);
        assert_eq!(reopened.policy_file().bytes(), config.policy_file().bytes());

        let composed = hosted_composed(
            TEXT,
            &[HostedBattery {
                name: "notes",
                policy: NOTES_BATTERY,
                token_env: &[],
            }],
        )
        .expect("a battery composes under a document that declares credentials");
        assert_eq!(composed.credentials(), &declared);
    }

    /// Nothing here says which helper reads which variable — that is the host's — but a
    /// key outside the child-credential namespace names no helper's credential at all,
    /// and a value the host cannot look up binds nothing.
    #[test]
    fn a_credential_declaration_names_child_credentials_and_store_keys() {
        for var in ["APPA_BRIDGE_TOKEN", "GITHUB_TOKEN"] {
            let text = format!("[credentials]\n{var} = \"github_prod\"\n[policy]\nversion = 2\n");
            assert!(
                matches!(hosted(&text), Err(ConfigError::CredentialVariable { var: refused }) if refused == var),
                "{var}"
            );
        }
        for value in ["\"\"", "3", "[\"github_prod\"]"] {
            let text = format!("[credentials]\nAPPA_PROVIDER_GITHUB_TOKEN = {value}\n[policy]\nversion = 2\n");
            assert!(
                matches!(hosted(&text), Err(ConfigError::CredentialValue { var }) if var == "APPA_PROVIDER_GITHUB_TOKEN"),
                "{value}"
            );
        }
        assert!(matches!(
            hosted("credentials = \"github_prod\"\n[policy]\nversion = 2\n"),
            Err(ConfigError::RootField { field }) if field == "credentials"
        ));
    }

    /// The declaration belongs to the host that composes the document: a file root has no
    /// store to name, and a battery is another author's text.
    #[test]
    fn only_a_host_declares_credentials() {
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("appa.toml");
        std::fs::write(
            &path,
            "[credentials]\nAPPA_PROVIDER_GITHUB_TOKEN = \"github_prod\"\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
        )
        .expect("write file config");
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::FileRootField { field }) if field == "credentials"
        ));

        assert!(matches!(
            hosted_composed(
                HOSTED_ROOT,
                &[HostedBattery {
                    name: "rogue",
                    policy: "[credentials]\nAPPA_PROVIDER_GITHUB_TOKEN = \"github_prod\"\n[policy]\nversion = 2\n",
                    token_env: &[],
                }],
            ),
            Err(ConfigError::IncludedTopLevel { path, field }) if path == "rogue" && field == "credentials"
        ));
    }

    /// The host's own declarations are admitted before the document says anything else,
    /// so a root that gets one wrong hears about that one.
    #[test]
    fn host_declarations_are_refused_before_the_include_list() {
        let unasked = |_: &str| -> Result<HostedBattery<'static>, IncludeResolution> {
            panic!("a refused document is never resolved")
        };
        assert!(matches!(
            Config::hosted_included(
                "[reporting]\nagent_yell = true\n[credentials]\nGITHUB_TOKEN = \"k\"\ninclude = [\"/srv/notes.toml\"]\n[policy]\nversion = 2\n",
                HOST_DEFAULTS,
                unasked,
            ),
            Err(ConfigError::HostedKey { key }) if key == "reporting"
        ));
        assert!(matches!(
            Config::hosted_included(
                "include = [\"/srv/notes.toml\"]\n[credentials]\nGITHUB_TOKEN = \"k\"\n[policy]\nversion = 2\n",
                HOST_DEFAULTS,
                unasked,
            ),
            Err(ConfigError::CredentialVariable { var }) if var == "GITHUB_TOKEN"
        ));
    }

    #[test]
    fn a_hosted_root_whose_externals_are_not_tables_is_refused_with_batteries() {
        for (root, field) in [
            ("externals = \"bad\"\n[policy]\nversion = 2\n", "externals"),
            (
                "[policy]\nversion = 2\n[externals]\nannotators = \"bad\"\n",
                "externals.annotators",
            ),
        ] {
            let result = hosted_composed(
                root,
                &[HostedBattery {
                    name: "github",
                    policy: GITHUB_BATTERY,
                    token_env: &["APPA_HOSTED_TEST_BRIDGE_TOKEN"],
                }],
            );
            assert!(
                matches!(&result, Err(ConfigError::RootField { field: refused }) if refused == field),
                "{field}: {result:?}"
            );
        }
    }

    #[test]
    fn a_battery_reads_only_the_variables_its_host_granted() {
        // SAFETY: the test process sets its own variable and every reader is this test.
        unsafe { std::env::set_var("APPA_HOSTED_TEST_BRIDGE_TOKEN", "bridge") };
        let ungranted = hosted_composed(
            HOSTED_ROOT,
            &[HostedBattery {
                name: "github",
                policy: GITHUB_BATTERY,
                token_env: &[],
            }],
        );
        assert!(
            matches!(
                &ungranted,
                Err(ConfigError::UngrantedBatteryCredential { battery, section, name, var })
                    if battery == "github" && section == "annotators" && name == "github.visibility" && var == "APPA_HOSTED_TEST_BRIDGE_TOKEN"
            ),
            "{ungranted:?}"
        );
    }

    #[test]
    fn a_hosted_root_annotator_replaces_a_battery_default() {
        // SAFETY: the test process sets its own variable and every reader is this test.
        unsafe { std::env::set_var("APPA_HOSTED_TEST_BRIDGE_TOKEN", "bridge") };
        let root = "[policy]\nversion = 2\n[[policy.annotator]]\nname = \"github.visibility\"\nranks = [\"trusted\"]\naudiences = []\nmarks = []\n";
        let config = hosted_composed(
            root,
            &[HostedBattery {
                name: "github",
                policy: GITHUB_BATTERY,
                token_env: &["APPA_HOSTED_TEST_BRIDGE_TOKEN"],
            }],
        )
        .expect("the root's annotator wins");
        let annotators = config.policy_file().value()["annotator"]
            .as_array()
            .expect("annotator declarations")
            .iter()
            .map(|declaration| declaration["ranks"][0].as_str().expect("rank"))
            .collect::<Vec<_>>();
        assert_eq!(annotators, ["trusted"]);
    }

    #[test]
    fn hosted_batteries_are_refused_where_a_file_include_would_be() {
        let battery = |policy: &'static str| HostedBattery {
            name: "github",
            policy,
            token_env: &["APPA_HOSTED_TEST_BRIDGE_TOKEN"],
        };
        assert!(matches!(
            hosted_composed(HOSTED_ROOT, &[battery(GITHUB_BATTERY), battery(GITHUB_BATTERY)]),
            Err(ConfigError::DuplicateInclude { path }) if path == "github"
        ));
        assert!(matches!(
            hosted_composed(HOSTED_ROOT, &[battery("[policy]\nversion = 1\n")]),
            Err(ConfigError::IncludedVersion { path, .. }) if path == "github"
        ));
        assert!(matches!(
            hosted_composed(HOSTED_ROOT, &[battery("[policy]\nversion = 2\n[server_aliases]\nx = [\"y\"]\n")]),
            Err(ConfigError::IncludedTopLevel { field, .. }) if field == "server_aliases"
        ));
        assert!(matches!(
            hosted_composed(
                HOSTED_ROOT,
                &[battery("[policy]\nversion = 2\n[externals.annotators.x]\ncommand = [\"python3\", \"x.py\"]\n")]
            ),
            Err(ConfigError::HostedCommand { section: "annotators", name }) if name == "x"
        ));
        let root_with_external =
            "[policy]\nversion = 2\n[externals.annotators.\"github.visibility\"]\nurl = \"https://example.com/v\"\n";
        assert!(matches!(
            hosted_composed(root_with_external, &[battery(GITHUB_BATTERY)]),
            Err(ConfigError::DuplicateExternal { path, section, name }) if path == "github" && section == "annotators" && name == "github.visibility"
        ));
    }

    /// A hosted document declares its externals the way a file does, and they resolve to
    /// the same implementations.
    #[test]
    fn a_hosted_document_resolves_the_externals_it_declares() {
        let config = hosted(
            r#"
            [policy]
            version = 2

            [externals.llm]
            provider = "ollama"
            model = "llama"

            [externals.claude_code]
            model = "pinned"

            [externals.audience.slack]
            url = "https://slack.internal"
            lookup = "people"
            selectors = [{ template = "channel/<id>" }]

            [externals.audience.people]
            readers = { "slack:alice" = "alice@corp.example" }
            "#,
        )
        .expect("the hosted document validates");

        let llm = config.externals.llm.as_ref().expect("the profile is declared");
        assert_eq!(llm.provider, LlmProvider::Ollama);
        assert_eq!(llm.model, "llama");
        assert_eq!(config.externals.claude_code.model, "pinned");
        let slack = &config.externals.audience["slack"];
        assert!(matches!(slack.implementation, AudienceImplementation::Resolver(_)));
        assert_eq!(slack.lookup.as_deref(), Some("people"));
        assert_eq!(slack.templates.len(), 1);
        assert!(matches!(
            config.externals.audience["people"].implementation,
            AudienceImplementation::Readers(_)
        ));
    }

    /// Everything outside `[policy]` and `[externals]` describes the deployment the host
    /// runs, not the policy its author wrote.
    #[test]
    fn a_hosted_document_cannot_declare_what_the_host_declares() {
        for (key, declaration) in [
            ("include", "include = [\"battery.toml\"]"),
            ("appa_composed", "[appa_composed]\ncommand_cwd = {}"),
            ("reporting", "[reporting]\nagent_yell = true"),
            ("bundle", "[bundle]\nfiles = []"),
            ("appa_inventory", "[appa_inventory]\ntools = []"),
        ] {
            let text = format!("{declaration}\n[policy]\nversion = 2\n");
            assert!(
                matches!(hosted(&text), Err(ConfigError::HostedKey { key: refused }) if refused == key),
                "{key}"
            );
        }
        // Only the loader that consumes an include list admits one: a host that hands its
        // batteries in keeps saying which they are, and the document does not.
        let text = "include = [\"batteries/notes@sha256-1111/appa.toml\"]\n[policy]\nversion = 2\n";
        assert!(matches!(
            hosted_composed(
                text,
                &[HostedBattery {
                    name: "notes",
                    policy: NOTES_BATTERY,
                    token_env: &[],
                }],
            ),
            Err(ConfigError::HostedKey { key }) if key == "include"
        ));
    }

    #[test]
    fn a_hosted_document_cannot_run_a_local_command() {
        for section in Section::ALL {
            let text = format!(
                "[policy]\nversion = 2\n[externals.{}.local]\ncommand = [\"python3\", \"resolver.py\"]\n",
                section.name()
            );
            assert!(
                matches!(
                    hosted(&text),
                    Err(ConfigError::HostedCommand { section: refused, name }) if refused == section.name() && name == "local"
                ),
                "{}",
                section.name()
            );
        }
        assert!(matches!(
            hosted("[policy]\nversion = 2\n[externals.claude_code]\ncommand = \"/opt/claude/bin/claude\"\n"),
            Err(ConfigError::HostedCommand {
                section: "claude_code",
                ..
            })
        ));
    }

    #[test]
    fn a_hosted_document_takes_the_host_settings_it_states_none_of() {
        let host = hosted("[policy]\nversion = 2\n").expect("a document without an externals table validates");
        assert_eq!(host.externals.timeout, Duration::from_millis(5000));
        assert_eq!(host.externals.max_body_bytes, 65_536);

        let stated = hosted("[policy]\nversion = 2\n[externals]\ntimeout_ms = 1500\nmax_body_bytes = 1024\n")
            .expect("a document stating both validates");
        assert_eq!(stated.externals.timeout, Duration::from_millis(1500));
        assert_eq!(stated.externals.max_body_bytes, 1024);
    }

    /// A hosted document names its secrets like a file does: the variable is persisted,
    /// the value is resolved from the environment, and the same text read from a file
    /// composes the same bytes.
    #[test]
    fn hosted_tokens_persist_as_variable_names_and_resolve_from_the_environment() {
        const VAR: &str = "APPA_CONFIG_TEST_HOSTED_TOKEN";
        let text = format!(
            "[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n\
             [externals.authorities.desk]\nurl = \"https://desk.internal\"\ntoken_env = \"{VAR}\"\n\
             [externals.llm]\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-4-5\"\ntoken_env = \"{VAR}\"\ntimeout_ms = 30000\nmax_concurrent = 2\n"
        );

        unsafe { std::env::remove_var(VAR) };
        assert!(matches!(hosted(&text), Err(ConfigError::MissingSecret { .. })));

        unsafe { std::env::set_var(VAR, "sekret") };
        let config = hosted(&text).expect("the hosted document validates against the environment");
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, &text).expect("write file config");
        let from_file = Config::load(&path).expect("the file config loads");
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

    fn reporting_config(dir: &std::path::Path, table: &str) -> Config {
        let path = dir.join("appa.toml");
        std::fs::write(
            &path,
            format!("[policy]\nversion=2\n[externals]\ntimeout_ms=5000\nmax_body_bytes=65536\n{table}"),
        )
        .expect("the configuration is written");
        Config::load(&path).expect("the configuration loads")
    }

    #[test]
    fn agent_yell_is_off_unless_the_deployment_says_otherwise() {
        let dir = tempfile::tempdir().expect("temp directory");
        assert!(
            !reporting_config(dir.path(), "").reporting.agent_yell,
            "a file with no [reporting] table never starts an agent reporting"
        );
        assert!(!reporting_config(dir.path(), "[reporting]\n").reporting.agent_yell);
        assert!(
            !reporting_config(dir.path(), "[reporting]\nagent_yell=false\n")
                .reporting
                .agent_yell
        );
    }

    /// The knob survives composition. The composed text is parsed back a second time, and
    /// reading the value from that parse instead of carrying it forward would silently
    /// return it to the default.
    #[test]
    fn agent_yell_survives_the_second_parse() {
        let dir = tempfile::tempdir().expect("temp directory");
        assert!(
            reporting_config(dir.path(), "[reporting]\nagent_yell=true\n")
                .reporting
                .agent_yell
        );
    }

    /// The whole reason the table is stripped before the document is rendered: a machine's
    /// reporting posture is not part of the policy every session's opening binds to.
    #[test]
    fn the_reporting_table_does_not_move_the_policy_file_key() {
        let dir = tempfile::tempdir().expect("temp directory");
        let without = reporting_config(dir.path(), "");
        let with = reporting_config(dir.path(), "[reporting]\nagent_yell=true\n");
        assert_eq!(
            without.policy_file().bytes(),
            with.policy_file().bytes(),
            "the composed policy bytes are identical either way"
        );
    }

    #[test]
    fn an_unknown_key_under_reporting_is_refused() {
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("appa.toml");
        std::fs::write(
            &path,
            "[policy]\nversion=2\n[externals]\ntimeout_ms=5000\nmax_body_bytes=65536\n[reporting]\nagent_yel=true\n",
        )
        .expect("the configuration is written");
        assert!(matches!(Config::load(&path), Err(ConfigError::Unparsable { .. })));
    }

    /// A fragment confines the results of tools it declares itself, and nothing else of the
    /// deployment: the names join the root's list, a foreign tool is refused, and every
    /// other deployment key stays the root's.
    #[test]
    fn a_fragment_confines_only_the_results_of_its_own_tools() {
        let dir = tempfile::tempdir().expect("temp directory");
        let root = dir.path().join("appa.toml");
        let battery = dir.path().join("battery.toml");
        let write_root = |deployment: &str| {
            std::fs::write(
                &root,
                format!(
                    "include = [\"battery.toml\"]\n[policy]\nversion = 2\n[[policy.tool]]\nname = \"root_read\"\n\
                     delta = {{}}\n{deployment}[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"
                ),
            )
            .expect("write root config");
        };
        let confined = |config: &Config| -> Vec<String> {
            config.policy_file().value()["deployment"]["confined_results"]
                .as_array()
                .expect("the composed deployment lists confined results")
                .iter()
                .map(|entry| entry.as_str().expect("a tool name").to_string())
                .collect()
        };

        write_root("");
        std::fs::write(
            &battery,
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"fetch(url:*)\"\ndelta = {}\n[[policy.tool]]\n\
             name = \"fetch\"\ndelta = {}\n[policy.deployment]\nconfined_results = [\"fetch\"]\n",
        )
        .expect("write battery");
        let config = Config::load(&root).expect("a fragment confines its own tool");
        assert_eq!(confined(&config), vec!["fetch".to_string()]);

        write_root("[policy.deployment]\nconfined_results = [\"root_read\", \"fetch\"]\n");
        let config = Config::load(&root).expect("the lists join");
        assert_eq!(confined(&config), vec!["root_read".to_string(), "fetch".to_string()]);

        write_root("");
        std::fs::write(
            &battery,
            "[policy]\nversion = 2\n[[policy.tool]]\nname = \"fetch\"\ndelta = {}\n[policy.deployment]\n\
             confined_results = [\"root_read\"]\n",
        )
        .expect("write foreign confinement");
        assert!(matches!(
            Config::load(&root),
            Err(ConfigError::IncludedConfinesForeignTool { tool, .. }) if tool == "root_read"
        ));

        for deployment in [
            "[policy.deployment]\ncontext_control = true\n",
            "[policy.deployment]\nconfined_results = \"fetch\"\n",
        ] {
            std::fs::write(
                &battery,
                format!("[policy]\nversion = 2\n[[policy.tool]]\nname = \"fetch\"\ndelta = {{}}\n{deployment}"),
            )
            .expect("write refused deployment key");
            assert!(
                matches!(Config::load(&root), Err(ConfigError::IncludedPolicyField { .. })),
                "{deployment} is root-only"
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
