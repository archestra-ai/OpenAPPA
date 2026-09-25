use thiserror::Error;

use appa_engine::registry::LoadError;

use crate::config::SUPPORTED_VERSION;

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
    #[error("{context} permits: `*` must be the only attention entry")]
    MixedAttentionWildcard { context: String },
    #[error(
        "{kind} {name} binds an implementation inline; this runtime binds implementations outside the policy, so the policy must not carry the binding"
    )]
    ForbiddenInlineBinding { kind: &'static str, name: String },
    #[error("tool {tool}: {source}")]
    ToolParameters {
        tool: String,
        source: appa_engine::params::ParamsError,
    },
    #[error("tool {tool}: invalid MCP server qualifier: {reason}")]
    ToolServer { tool: String, reason: String },
    #[error("tool {tool} declares effect {kind:?} twice — `effects` is a set")]
    DuplicateEffect { tool: String, kind: String },
    #[error("annotator name {0:?} is empty")]
    BadAnnotatorName(String),
    #[error(
        "annotator {name} names unknown builtin {builtin:?}; the stock annotator builtins are \"claude-code\" and \"llm\""
    )]
    UnknownAnnotatorBuiltin { name: String, builtin: String },
    #[error(
        "annotator {annotator} input {input} reads {spelling:?}, which is not an input source: an input reads `$tool_call`, `$tool_call.name`, `$tool_call.description`, `$tool_call.arguments`, `$tool_call.arguments.<name>`, or `$input.<name>`"
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
    #[error(
        "audience source {provider:?} in {context} declares no selectors: bind it under [externals.audience.{provider}] with `selectors`"
    )]
    UndeclaredProvider { context: String, provider: String },
    #[error("bad selector declaration for audience source {provider:?}: {template:?} {reason}")]
    BadSelectorDeclaration {
        provider: String,
        template: String,
        reason: String,
    },
    #[error("registry rejected: {0}")]
    Registry(#[from] LoadError),
}
