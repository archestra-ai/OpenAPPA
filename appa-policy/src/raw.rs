use std::collections::BTreeMap;

use serde::Deserialize;

use appa_engine::profile::{BindingMode, ExecutorClass, SurfaceMode};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    pub(crate) version: u32,
    pub(crate) trust_chain: Option<Vec<String>>,
    pub(crate) boundary: Option<RawBoundary>,
    #[serde(default)]
    pub(crate) tool: Vec<RawTool>,
    #[serde(default)]
    pub(crate) authority: Vec<RawAuthority>,
    #[serde(default)]
    pub(crate) sanitizer: Vec<RawSanitizer>,
    #[serde(default)]
    pub(crate) annotator: Vec<RawAnnotator>,
    pub(crate) audience: Option<RawAudience>,
    pub(crate) limits: Option<RawLimits>,
    pub(crate) deployment: Option<RawDeployment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDeployment {
    pub(crate) starting_label: Option<RawStartingLabel>,
    pub(crate) binding: Option<BindingMode>,
    pub(crate) context_control: Option<bool>,
    pub(crate) dispatch: Option<ExecutorClass>,
    #[serde(default)]
    pub(crate) assumed_tools: Vec<String>,
    #[serde(default)]
    pub(crate) provider_run_tools: Vec<String>,
    #[serde(default)]
    pub(crate) confined_results: Vec<String>,
    #[serde(default)]
    pub(crate) provider_surfaces: BTreeMap<String, SurfaceMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawStartingLabel {
    pub(crate) trust: Option<String>,
    pub(crate) audience: Option<RawStartingAudience>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum RawStartingAudience {
    Token(String),
    List(Vec<String>),
}

impl RawDeployment {
    /// Every field of this table that names a tool, with the field's spelling. The list
    /// lives beside the fields themselves, so a field added here reaches every reader
    /// that checks how a deployment names tools.
    pub(crate) fn tool_names(&self) -> Vec<(&'static str, String)> {
        [
            ("[deployment] assumed_tools", &self.assumed_tools),
            ("[deployment] provider_run_tools", &self.provider_run_tools),
            ("[deployment] confined_results", &self.confined_results),
        ]
        .into_iter()
        .flat_map(|(field, names)| names.iter().map(move |name| (field, name.clone())))
        .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLimits {
    pub(crate) planner_cap: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAnnotator {
    pub(crate) name: String,
    /// The deployer's trusted instruction to this Annotator. It calibrates policy-specific
    /// vocabulary but grants no value outside the mandate.
    pub(crate) hint: Option<String>,
    pub(crate) implementation: Option<toml::Value>,
    /// The stock model transport this Annotator carries: `"claude-code"` or `"llm"`. An
    /// Annotator without it is bound by the deployment under `[externals.annotators]`.
    pub(crate) builtin: Option<String>,
    /// The consult inputs, each a `$tool_call` source. Omitted sends the complete call.
    pub(crate) inputs: Option<BTreeMap<String, String>>,
    /// The trust ranks a produced annotation may write. Omitted admits every chain rank.
    pub(crate) ranks: Option<Vec<String>>,
    /// The audiences a produced annotation may name: chain words, group references, and
    /// literal readers. Omitted admits the whole policy vocabulary; `public` is always
    /// admissible and never listed.
    pub(crate) audiences: Option<Vec<String>>,
    /// The attention marks a produced annotation may require. Omitted admits every declared
    /// mark.
    pub(crate) marks: Option<Vec<String>>,
    /// The effect kinds a produced annotation may emit or require. Omitted admits every
    /// declared kind.
    pub(crate) effects: Option<Vec<String>>,
}

/// `[audience]`: `self` and `internal` list the selectors that feed each built-in audience,
/// and `[audience.group.<name>]` configures one named audience.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAudience {
    #[serde(rename = "self")]
    pub(crate) self_from: Option<Vec<String>>,
    pub(crate) internal: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) group: BTreeMap<String, RawAudienceGroup>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAudienceGroup {
    pub(crate) within: Option<String>,
    pub(crate) from: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawBoundary {
    pub(crate) trust: Option<String>,
    pub(crate) audience: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTool {
    pub(crate) name: String,
    pub(crate) server: Option<String>,
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) delta: Option<RawDelta>,
    pub(crate) requires: Option<RawRequires>,
    #[serde(default)]
    pub(crate) effects: Vec<String>,
    pub(crate) implementation: Option<toml::Value>,
    pub(crate) parameters: Option<serde_json::Value>,
    /// The registered `[[annotator]]` that produces this tool's semantics per call. Replaces
    /// the static `delta`/`requires`/`effects` — a declaration carries one recipe.
    pub(crate) annotator: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDelta {
    pub(crate) trust: Option<String>,
    pub(crate) audience: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRequires {
    pub(crate) trust: Option<String>,
    pub(crate) audience: Option<RawRequiresAudience>,
    pub(crate) effects: Option<RawHistory>,
    #[serde(default)]
    pub(crate) attention: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRequiresAudience {
    pub(crate) contains: Option<Vec<String>>,
    pub(crate) within: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHistory {
    #[serde(default)]
    pub(crate) contains: Vec<String>,
    #[serde(default)]
    pub(crate) excludes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAuthority {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) hint: Option<String>,
    #[serde(default)]
    pub(crate) permits: RawPermits,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) implementation: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPermits {
    pub(crate) trust_below: Option<String>,
    pub(crate) audience_missing: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) effects_containing: Vec<String>,
    #[serde(default)]
    pub(crate) attention: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSanitizer {
    pub(crate) name: String,
    pub(crate) on: Vec<String>,
    #[serde(default)]
    pub(crate) hint: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) permits: RawSanitizerPermits,
    pub(crate) implementation: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSanitizerPermits {
    #[serde(default)]
    pub(crate) audience: Option<RawAudienceTransition>,
    #[serde(default)]
    pub(crate) trust: Option<RawTrustTransition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAudienceTransition {
    pub(crate) from: Vec<String>,
    pub(crate) to: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTrustTransition {
    pub(crate) from: String,
    pub(crate) to: String,
}
