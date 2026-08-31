//! The deployment profile: the policy file's `[deployment]` coverage declaration as the engine
//! holds it, the policy × profile validation matrix, the derived open vectors, and the versioned
//! policy identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contract::{ToolAnnotation, ToolDeclaration};
use crate::fact::ReturnPolicy;
use crate::label::{Label, Trust};
use crate::names::SurfaceName;
use crate::registry::{LoadError, PlannerCap, Registry, RegistryConfig, TrustChain, check_rank, check_readers};
use crate::value::ToolName;

/// How a tool executes relative to the engine's release: a component consumes the
/// release before execution (`Enforced`), execution is assumed faithful (`Assumed`, an open
/// vector), or the provider runs the tool inside the inference call (`ProviderRun` — an ingestion
/// surface, not a checkable call).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorClass {
    Enforced,
    Assumed,
    ProviderRun,
}

/// How the host binds engine-relevant events to trajectories. Neither mode carries a
/// recovery choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingMode {
    Harness,
    Token,
}

/// How an allowed provider surface is handled: mediated through the ordinary admission
/// machinery, or declared an open vector. A surface the declaration does not list is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMode {
    Mediated,
    Open,
}

/// One capability the deployment leaves uncovered, derived canonically from the normalized
/// declaration and the registered tool set — never a caller-supplied acknowledgement list.
/// Exactly three kinds exist.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OpenVector {
    AssumedExecutor { tool: ToolName },
    ProviderRunDispatch { tool: ToolName },
    OpenProviderSurface { surface: SurfaceName },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageSlot {
    ExecutorException,
    ConfinedResult,
}

impl std::fmt::Display for CoverageSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CoverageSlot::ExecutorException => "the executor exceptions",
            CoverageSlot::ConfinedResult => "the confined result points",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRunConstruct {
    Requires,
}

impl std::fmt::Display for ProviderRunConstruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProviderRunConstruct::Requires => "a `requires`",
        })
    }
}

/// The unvalidated `[deployment]` declaration, one public field per coverage slot — the form
/// [`crate::engine::Engine::open`] takes, so the coverage matrix sees every declared entry
/// before normalization converges equivalent declarations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDeclaration {
    pub starting_label: Label,
    pub context_control: bool,
    pub dispatch: ExecutorClass,
    pub executor_exceptions: BTreeMap<ToolName, ExecutorClass>,
    pub confined_results: BTreeSet<ToolName>,
    pub confined_child_return: bool,
    pub provider_surfaces: BTreeMap<SurfaceName, SurfaceMode>,
    pub binding: BindingMode,
}

/// The immutable, normalized deployment profile — the typed form of the policy file's
/// `[deployment]` table. Construction and deserialization both run
/// [`DeploymentProfile::declare`], so a profile with an
/// exception equal to its own default is unrepresentable, and a replayed opening record meets the
/// same rules the loader reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeploymentProfile {
    starting_label: Label,
    context_control: bool,
    dispatch: ExecutorClass,
    executor_exceptions: BTreeMap<ToolName, ExecutorClass>,
    confined_results: BTreeSet<ToolName>,
    confined_child_return: bool,
    provider_surfaces: BTreeMap<SurfaceName, SurfaceMode>,
    binding: BindingMode,
}

impl ProfileDeclaration {
    /// The declaration of a policy file with no `[deployment]` table: every tool is
    /// assumed, no point is confined, context is uncontrolled, no provider surface is allowed,
    /// binding is `harness`, and the starting label is the neutral value.
    pub fn no_coverage(chain: &TrustChain) -> ProfileDeclaration {
        ProfileDeclaration {
            starting_label: neutral_starting_label(chain),
            context_control: false,
            dispatch: ExecutorClass::Assumed,
            executor_exceptions: BTreeMap::new(),
            confined_results: BTreeSet::new(),
            confined_child_return: false,
            provider_surfaces: BTreeMap::new(),
            binding: BindingMode::Harness,
        }
    }
}

impl DeploymentProfile {
    /// Normalize and validate a declaration: both starting dimensions established, and
    /// exceptions equal to the deployment default dropped, so equivalent declarations converge to
    /// one canonical value. Registration of the named tools is the coverage matrix's job, over
    /// the pre-normalization declaration.
    pub(crate) fn declare(declaration: ProfileDeclaration) -> Result<DeploymentProfile, LoadError> {
        let ProfileDeclaration {
            starting_label,
            context_control,
            dispatch,
            mut executor_exceptions,
            confined_results,
            confined_child_return,
            provider_surfaces,
            binding,
        } = declaration;
        executor_exceptions.retain(|_, class| *class != dispatch);
        Ok(DeploymentProfile {
            starting_label,
            context_control,
            dispatch,
            executor_exceptions,
            confined_results,
            confined_child_return,
            provider_surfaces,
            binding,
        })
    }

    pub fn starting_label(&self) -> &Label {
        &self.starting_label
    }

    pub fn context_control(&self) -> bool {
        self.context_control
    }

    pub fn binding(&self) -> BindingMode {
        self.binding
    }

    pub fn executor_class(&self, tool: &ToolName) -> ExecutorClass {
        self.executor_exceptions.get(tool).copied().unwrap_or(self.dispatch)
    }

    /// The one shared provider-run predicate: the registry split, the plan families, and the
    /// proposal refusal all agree through this.
    pub fn is_provider_run(&self, tool: &ToolName) -> bool {
        self.executor_class(tool) == ExecutorClass::ProviderRun
    }

    pub fn confines_result(&self, tool: &ToolName) -> bool {
        self.confined_results.contains(tool)
    }

    pub fn confines_child_return(&self) -> bool {
        self.confined_child_return
    }

    pub fn provider_surfaces(&self) -> impl Iterator<Item = (&SurfaceName, SurfaceMode)> {
        self.provider_surfaces.iter().map(|(name, mode)| (name, *mode))
    }
}

impl<'de> Deserialize<'de> for DeploymentProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            starting_label: Label,
            context_control: bool,
            dispatch: ExecutorClass,
            executor_exceptions: BTreeMap<ToolName, ExecutorClass>,
            confined_results: BTreeSet<ToolName>,
            confined_child_return: bool,
            provider_surfaces: BTreeMap<SurfaceName, SurfaceMode>,
            binding: BindingMode,
        }
        let wire = Wire::deserialize(deserializer)?;
        DeploymentProfile::declare(ProfileDeclaration {
            starting_label: wire.starting_label,
            context_control: wire.context_control,
            dispatch: wire.dispatch,
            executor_exceptions: wire.executor_exceptions,
            confined_results: wire.confined_results,
            confined_child_return: wire.confined_child_return,
            provider_surfaces: wire.provider_surfaces,
            binding: wire.binding,
        })
        .map_err(serde::de::Error::custom)
    }
}

/// The neutral, least restrictive starting label: the `public` audience and the top
/// rank of the trust chain.
pub fn neutral_starting_label(chain: &TrustChain) -> Label {
    let top = Trust::new(chain.len().saturating_sub(1) as u8);
    Label::new(top, crate::label::Audience::Public)
}

/// The version of the policy configuration dialect a policy file was written in, carried on the
/// durable opening record. Never part of the policy identity: normalization already
/// erases source syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDialectVersion(u32);

impl PolicyDialectVersion {
    pub const fn new(version: u32) -> Self {
        PolicyDialectVersion(version)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Everything one policy file declares, normalized — the input of the one validated engine
/// constructor ([`crate::engine::Engine::open`]). Policy and declaration validate together in one
/// load: the structural registry lints, the provider-run split, the planner-cap bound,
/// and the coverage matrix all run behind this single entry.
#[derive(Clone, Debug)]
pub struct DeploymentPolicy {
    pub registry: RegistryConfig,
    pub planner_cap: PlannerCap,
    pub dialect: PolicyDialectVersion,
    pub child_return: ReturnPolicy,
    pub profile: ProfileDeclaration,
}

/// The key of a stored policy file: the SHA-256 of its exact bytes, lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PolicyFileKey(String);

impl PolicyFileKey {
    pub fn of(bytes: &[u8]) -> PolicyFileKey {
        use sha2::Digest as _;
        PolicyFileKey(format!("{:x}", sha2::Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The versioned policy identity: a domain-separated SHA-256 digest over the RFC 8785
/// canonical bytes of the identity document — the normalized engine-visible policy and deployment
/// declaration. The document is field-aware: it names every included field explicitly, preserves
/// the semantic sequences (the trust chain's rank order, authority registration order), sorts
/// true maps and sets, and excludes source syntax, runtime bindings, `[limits]`, and hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentityV1(#[serde(with = "crate::hex32")] [u8; 32]);

impl PolicyIdentityV1 {
    pub(crate) fn of_registry(registry: &Registry, child_return: &ReturnPolicy) -> Self {
        let document = identity_document_from_registry(registry, child_return);
        let canonical = serde_json_canonicalizer::to_vec(&document).expect("an identity document canonicalizes");
        let mut hasher = Sha256::new();
        hasher.update(b"appa:policy-identity:v1");
        hasher.update([0u8]);
        hasher.update(&canonical);
        PolicyIdentityV1(hasher.finalize().into())
    }

    /// The digest bytes, for hosts that persist the identity beside a
    /// trajectory's durable opening record.
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn identity_document_from_registry(registry: &Registry, child_return: &ReturnPolicy) -> serde_json::Value {
    fn render_annotation(matcher: &crate::registry::ToolMatcher, tool: &ToolAnnotation) -> serde_json::Value {
        serde_json::json!({
            "name": tool.name,
            "matcher": matcher,
            "tags": sorted_set(&tool.tags),
            "parameters": tool.parameters.normalized(),
            "description": tool.description,
            "delta": tool.delta,
            "emits": tool.emits,
            "requires": {
                "trust_floor": tool.requires.label.trust_floor,
                "audience": sorted_set(&tool.requires.label.audience),
                "history": sorted_set(&tool.requires.history),
                "attention": sorted_set(&tool.requires.attention),
            },
        })
    }
    let render = |matcher: &crate::registry::ToolMatcher, declaration: &ToolDeclaration| match declaration {
        ToolDeclaration::Declared(tool) => render_annotation(matcher, tool),
        ToolDeclaration::Annotated {
            name,
            tags,
            description,
            parameters,
            annotator,
        } => serde_json::json!({
            "name": name,
            "matcher": matcher,
            "tags": sorted_set(tags),
            "parameters": parameters.normalized(),
            "description": description,
            "annotator": annotator,
        }),
    };
    let mut tools: Vec<_> = registry
        .semantic_tools()
        .map(|(matcher, declaration)| render(matcher, declaration))
        .collect();
    tools.extend(
        registry
            .provider_run_annotations()
            .map(|tool| render_annotation(&crate::registry::ToolMatcher::Bare, tool)),
    );
    let annotators: Vec<serde_json::Value> = registry
        .annotators()
        .map(|(name, mandate)| {
            serde_json::json!({
                "name": name,
                "trust": mandate.trust_ranks().collect::<Vec<_>>(),
                "audiences": mandate.audiences().collect::<Vec<_>>(),
                "marks": mandate.marks().collect::<Vec<_>>(),
                "effects": mandate.effects().collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut document = identity_document(
        &RegistryConfig {
            trust_chain: registry.trust_chain().clone(),
            tools: Vec::new(),
            annotators: Vec::new(),
            authorities: registry.authorities().to_vec(),
            sanitizers: registry.sanitizers().cloned().collect(),
            membership: registry.membership().cloned(),
        },
        child_return,
        registry.profile(),
    );
    let document_map = document.as_object_mut().expect("identity document is an object");
    document_map.insert("tools".into(), tools.into());
    document_map.insert("annotators".into(), annotators.into());
    document
}

/// The digest of a configuration as the engine that serves it would stamp it. Test-only:
/// production reads [`crate::engine::Engine::identity`], which is this same digest taken
/// from the registry the engine actually holds.
#[cfg(test)]
pub(crate) fn identity_of(
    registry: &RegistryConfig,
    child_return: &ReturnPolicy,
    profile: &DeploymentProfile,
) -> PolicyIdentityV1 {
    let built = crate::registry::Registry::build(
        registry.clone(),
        crate::registry::PlannerCap::default(),
        profile.clone(),
    )
    .expect("a fixture configuration builds");
    PolicyIdentityV1::of_registry(&built, child_return)
}

fn canonical_value(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("an engine declaration serializes")
}

fn sorted_set(values: &[impl Serialize]) -> Vec<serde_json::Value> {
    let mut rendered: Vec<serde_json::Value> = values.iter().map(canonical_value).collect();
    rendered.sort_by_cached_key(|value| {
        serde_json_canonicalizer::to_vec(value).expect("a serde_json::Value canonicalizes")
    });
    rendered.dedup();
    rendered
}

fn identity_document(
    registry: &RegistryConfig,
    child_return: &ReturnPolicy,
    profile: &DeploymentProfile,
) -> serde_json::Value {
    let authorities: Vec<serde_json::Value> = registry
        .authorities
        .iter()
        .map(|authority| {
            serde_json::json!({
                "name": authority.name,
                "mandate": {
                    "trust_ceiling": authority.mandate.trust_ceiling,
                    "reader_ceiling": authority.mandate.reader_ceiling,
                    "waivers": sorted_set(&authority.mandate.waivers),
                    "attends": sorted_set(&authority.mandate.attends),
                },
                "scope": sorted_set(&authority.scope.tags),
            })
        })
        .collect();

    let mut sanitizers: Vec<_> = registry.sanitizers.iter().collect();
    sanitizers.sort_by(|a, b| a.name.cmp(&b.name));
    let sanitizers: Vec<serde_json::Value> = sanitizers
        .into_iter()
        .map(|sanitizer| {
            serde_json::json!({
                "name": sanitizer.name,
                "on": sanitizer.on,
                "transition": sanitizer.transition,
                "scope": sorted_set(&sanitizer.scope.tags),
            })
        })
        .collect();

    serde_json::json!({
        "trust_chain": registry.trust_chain,
        "authorities": authorities,
        "sanitizers": sanitizers,
        // Which directory expands a group is part of what the policy means.
        "membership": registry.membership,
        "child_return": child_return,
        "deployment": profile,
    })
}

/// Derive the open vectors from the normalized declaration and the registered tool set:
/// one per assumed executor, one per allowed provider-run dispatch, one per `open` provider
/// surface, in canonical order.
pub(crate) fn derive_open_vectors<'a>(
    profile: &DeploymentProfile,
    tools: impl Iterator<Item = &'a ToolName>,
) -> Vec<OpenVector> {
    let mut vectors = Vec::new();
    for tool in tools {
        match profile.executor_class(tool) {
            ExecutorClass::Enforced => {}
            ExecutorClass::Assumed => vectors.push(OpenVector::AssumedExecutor { tool: tool.clone() }),
            ExecutorClass::ProviderRun => vectors.push(OpenVector::ProviderRunDispatch { tool: tool.clone() }),
        }
    }
    for (surface, mode) in profile.provider_surfaces() {
        if mode == SurfaceMode::Open {
            vectors.push(OpenVector::OpenProviderSurface {
                surface: surface.clone(),
            });
        }
    }
    vectors.sort();
    vectors
}

/// The pure policy × profile validation matrix (run by the loader through
/// [`crate::engine::Engine::open`]): a policy construct that names an engine behavior the
/// deployment cannot perform is a load error, and the error names the missing coverage. Weaker
/// choices load and surface as open vectors instead.
pub(crate) fn validate_coverage(
    registry: &Registry,
    declaration: &ProfileDeclaration,
    child_return: &ReturnPolicy,
) -> Result<(), LoadError> {
    let profile = registry.profile();
    let chain = registry.trust_chain();
    check_rank(chain, Some(profile.starting_label.trust), || {
        "deployment starting label".to_string()
    })?;
    check_readers(&profile.starting_label.audience, || {
        "deployment starting label".to_string()
    })?;

    // Coverage names declared tools only: the wildcard covers a name at a proposal, but a
    // deployment declaration naming an unwritten tool is a typo.
    let registered = |tool: &ToolName| registry.declared(tool) || registry.provider_run_annotation(tool).is_some();
    for tool in declaration.executor_exceptions.keys() {
        if !registered(tool) {
            return Err(LoadError::UnknownDeploymentTool {
                slot: CoverageSlot::ExecutorException,
                tool: tool.as_str().to_string(),
            });
        }
    }
    for tool in &declaration.confined_results {
        if !registered(tool) {
            return Err(LoadError::UnknownDeploymentTool {
                slot: CoverageSlot::ConfinedResult,
                tool: tool.as_str().to_string(),
            });
        }
        // A provider-run result reaches the model inside the inference call, before any host
        // could withhold it: declaring it confined would be a false declaration.
        if profile.is_provider_run(tool) {
            return Err(LoadError::ConfinedProviderRun {
                tool: tool.as_str().to_string(),
            });
        }
    }

    if let ReturnPolicy::Sanitized(name) = child_return {
        if !profile.context_control {
            return Err(LoadError::ChildWithoutContextControl);
        }
        match registry.sanitizer(name) {
            Some(sanitizer) if sanitizer.on.output && sanitizer.scope.is_unscoped() => {}
            Some(sanitizer) if sanitizer.on.output => {
                return Err(LoadError::ChildReturnSanitizerScoped(name.as_str().to_string()));
            }
            Some(_) => {
                return Err(LoadError::ChildReturnSanitizerNotOutput(name.as_str().to_string()));
            }
            None => {
                return Err(LoadError::ChildReturnSanitizerUnknown(name.as_str().to_string()));
            }
        }
    }

    // An output sanitizer runs on a confined result its scope reaches, or on a confined
    // child return, which originates from no tool and so is reached only by an unscoped
    // one. A sanitizer no confined source can reach never runs, and the deployment that
    // declares it believes a derivation is available that is not.
    for sanitizer in registry.sanitizers().filter(|sanitizer| sanitizer.on.output) {
        let reaches_a_result = profile.confined_results.iter().any(|tool| {
            registry
                .variants(tool)
                .any(|declaration| sanitizer.scope.covers(declaration.tags()))
        });
        let reaches_the_child_return = profile.confined_child_return && sanitizer.scope.is_unscoped();
        if !reaches_a_result && !reaches_the_child_return {
            return Err(LoadError::OutputSanitizerUncovered {
                sanitizer: sanitizer.name.as_str().to_string(),
            });
        }
    }

    for annotation in registry.provider_run_annotations() {
        if annotation.requires != crate::contract::Requires::default() {
            return Err(LoadError::ProviderRunConstruct {
                tool: annotation.name.as_str().to_string(),
                construct: ProviderRunConstruct::Requires,
            });
        }
    }

    Ok(())
}

/// A profile that covers everything a test registry declares: every tool enforced and confined,
/// child return confined, context control on, no provider surfaces, harness binding, the neutral
/// starting label. The engine's own tests open under this so coverage never masks the behavior
/// under test.
#[cfg(test)]
pub(crate) fn covering_profile(config: &RegistryConfig) -> DeploymentProfile {
    DeploymentProfile::declare(covering_declaration(config)).expect("the covering test profile validates")
}

/// A synthetic opening record for a raw-fold test fixture: `trajectory` opens as a
/// root at `starting_label` under a no-coverage profile. It carries no engine's identity, so it
/// belongs only in logs the transition validator never judges — `Projection::build` fixtures. A
/// validated log opens through `Engine::open_trajectory` instead.
#[cfg(test)]
pub(crate) fn opening_at(trajectory: crate::value::TrajectoryId, starting_label: Label) -> crate::fact::Fact {
    let chain = TrustChain::new(vec!["a".to_string(), "b".to_string()]);
    let config = RegistryConfig {
        trust_chain: chain.clone(),
        tools: Vec::new(),
        annotators: Vec::new(),
        authorities: Vec::new(),
        sanitizers: Vec::new(),
        membership: None,
    };
    let profile = DeploymentProfile::declare(ProfileDeclaration {
        starting_label,
        ..ProfileDeclaration::no_coverage(&chain)
    })
    .expect("an established starting label declares");
    crate::fact::Fact::TrajectoryOpened {
        trajectory,
        dialect: PolicyDialectVersion::new(1),
        policy_digest: identity_of(&config, &ReturnPolicy::Raw, &profile),
        profile,
        policy_file_key: PolicyFileKey::of(b"fixture"),
        open_vectors: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn covering_declaration(config: &RegistryConfig) -> ProfileDeclaration {
    ProfileDeclaration {
        starting_label: neutral_starting_label(&config.trust_chain),
        context_control: true,
        dispatch: ExecutorClass::Enforced,
        executor_exceptions: BTreeMap::new(),
        confined_results: config
            .tools
            .iter()
            // Coverage names written tools only: the wildcard is not a name a deployment confines.
            .filter(|declaration| declaration.name().as_str() != crate::registry::WILDCARD_TOOL_NAME)
            .map(|declaration| {
                crate::registry::base_tool_name(declaration.name()).expect("test contracts have valid names")
            })
            .collect(),
        confined_child_return: true,
        provider_surfaces: BTreeMap::new(),
        binding: BindingMode::Harness,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, DeclaredTransition, Hint, Mandate, Sanitizer, SanitizerPoints, Scope};
    use crate::contract::{Delta, LabelRequirements, Requires};
    use crate::engine::Engine;
    use crate::fact::EffectSet;
    use crate::groups::DeclaredAudience;
    use crate::label::{Audience, ReaderId};
    use crate::names::{AnnotatorName, AuthorityName, SanitizerName, TagName};

    fn chain() -> TrustChain {
        TrustChain::new(vec!["suspicious".into(), "trusted".into()])
    }

    fn tool(name: &str) -> ToolAnnotation {
        ToolAnnotation {
            description: Some("A test tool.".to_string()),
            name: ToolName::new(name),
            tags: vec![],
            delta: Delta::NONE,
            parameters: crate::params::ToolParameters::open(),
            emits: EffectSet::default(),
            requires: Requires::default(),
        }
    }

    fn config(tools: Vec<ToolAnnotation>) -> RegistryConfig {
        RegistryConfig {
            trust_chain: chain(),
            tools: tools.into_iter().map(ToolDeclaration::Declared).collect(),
            annotators: vec![],
            authorities: vec![],
            sanitizers: vec![],
            membership: None,
        }
    }

    fn classifier() -> crate::registry::AnnotatorDeclaration {
        crate::registry::AnnotatorDeclaration {
            name: AnnotatorName::new("classifier"),
            trust: None,
            audiences: None,
            marks: None,
            effects: None,
        }
    }

    fn annotated(name: &str) -> ToolDeclaration {
        ToolDeclaration::Annotated {
            name: ToolName::new(name),
            tags: vec![],
            description: None,
            parameters: crate::params::ToolParameters::open(),
            annotator: AnnotatorName::new("classifier"),
        }
    }

    fn output_sanitizer(name: &str) -> Sanitizer {
        Sanitizer {
            name: SanitizerName::new(name),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Trust {
                from_floor: Trust::new(0),
                to: Trust::new(1),
            },
            scope: Scope::default(),
            hint: None,
        }
    }

    fn open(
        cfg: RegistryConfig,
        declaration: ProfileDeclaration,
        child_return: ReturnPolicy,
    ) -> Result<Engine, LoadError> {
        Engine::open(DeploymentPolicy {
            registry: cfg,
            planner_cap: PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return,
            profile: declaration,
        })
    }

    fn provider_run(declaration: &mut ProfileDeclaration, name: &str) {
        declaration
            .executor_exceptions
            .insert(ToolName::new(name), ExecutorClass::ProviderRun);
        declaration.confined_results.remove(&ToolName::new(name));
    }

    #[test]
    fn an_output_sanitizer_needs_some_confined_application_point() {
        let mut cfg = config(vec![tool("fetch")]);
        cfg.sanitizers = vec![output_sanitizer("redactor")];
        let uncovered = |cfg: &RegistryConfig| {
            let mut declaration = covering_declaration(cfg);
            declaration.confined_results.clear();
            declaration.confined_child_return = false;
            declaration
        };
        assert!(matches!(
            open(cfg.clone(), uncovered(&cfg), ReturnPolicy::Raw),
            Err(LoadError::OutputSanitizerUncovered { sanitizer }) if sanitizer == "redactor"
        ));
        let mut child_only = uncovered(&cfg);
        child_only.confined_child_return = true;
        assert!(open(cfg.clone(), child_only, ReturnPolicy::Raw).is_ok());
        let mut result_only = uncovered(&cfg);
        result_only.confined_results.insert(ToolName::new("fetch"));
        assert!(open(cfg, result_only, ReturnPolicy::Raw).is_ok());
    }

    /// Coverage is per sanitizer and per scope: some other tool being confined does not
    /// give a scoped sanitizer an application point. A child return originates from no
    /// tool, so only an unscoped sanitizer reaches it.
    #[test]
    fn a_scoped_output_sanitizer_needs_a_confined_result_its_scope_reaches() {
        let mut post = tool("post");
        post.tags = vec![TagName::new("outbound")];
        let mut cfg = config(vec![tool("fetch"), post]);
        let mut scoped = output_sanitizer("redactor");
        scoped.scope = Scope {
            tags: vec![TagName::new("outbound")],
        };
        cfg.sanitizers = vec![scoped];

        let confining = |cfg: &RegistryConfig, tools: &[&str], child: bool| {
            let mut declaration = covering_declaration(cfg);
            declaration.confined_results = tools.iter().map(|name| ToolName::new(*name)).collect();
            declaration.confined_child_return = child;
            declaration
        };

        assert!(
            matches!(
                open(cfg.clone(), confining(&cfg, &["fetch"], false), ReturnPolicy::Raw),
                Err(LoadError::OutputSanitizerUncovered { sanitizer }) if sanitizer == "redactor"
            ),
            "a confined tool outside the scope is not an application point"
        );
        assert!(
            matches!(
                open(cfg.clone(), confining(&cfg, &["fetch"], true), ReturnPolicy::Raw),
                Err(LoadError::OutputSanitizerUncovered { sanitizer }) if sanitizer == "redactor"
            ),
            "a scoped sanitizer never reaches a child return"
        );
        let covered = confining(&cfg, &["post"], false);
        assert!(open(cfg, covered, ReturnPolicy::Raw).is_ok());
    }

    #[test]
    fn a_child_return_binding_needs_context_control() {
        let mut cfg = config(vec![tool("fetch")]);
        cfg.sanitizers = vec![output_sanitizer("redactor")];
        let bound = ReturnPolicy::Sanitized(SanitizerName::new("redactor"));
        let mut declaration = covering_declaration(&cfg);
        declaration.context_control = false;
        assert!(matches!(
            open(cfg.clone(), declaration, bound.clone()),
            Err(LoadError::ChildWithoutContextControl)
        ));
        assert!(open(cfg.clone(), covering_declaration(&cfg), bound).is_ok());
        let mut uncontrolled = covering_declaration(&cfg);
        uncontrolled.context_control = false;
        assert!(open(cfg, uncontrolled, ReturnPolicy::Raw).is_ok());
    }

    #[test]
    fn a_provider_run_contract_may_declare_only_a_static_delta() {
        let asking = {
            let mut t = tool("search");
            t.requires = Requires {
                label: LabelRequirements {
                    trust_floor: Some(Trust::new(1)),
                    audience: vec![],
                },
                ..Requires::default()
            };
            t
        };
        let cfg = config(vec![asking]);
        let mut declaration = covering_declaration(&cfg);
        provider_run(&mut declaration, "search");
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::ProviderRunConstruct { tool, construct })
                if tool == "search" && construct == ProviderRunConstruct::Requires
        ));

        // A provider-run result reaches the model inside the inference call, so nothing would
        // consume a per-call annotation: routing one through an Annotator is refused.
        let mut cfg = config(vec![]);
        cfg.annotators = vec![classifier()];
        cfg.tools = vec![annotated("search")];
        let mut declaration = covering_declaration(&cfg);
        provider_run(&mut declaration, "search");
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::ProviderRunAnnotated(tool)) if tool == "search"
        ));
        let cfg = config(vec![tool("search")]);
        let mut declaration = covering_declaration(&cfg);
        provider_run(&mut declaration, "search");
        assert!(open(cfg, declaration, ReturnPolicy::Raw).is_ok());

        let cfg = config(vec![tool("search(query:*)")]);
        let mut declaration = covering_declaration(&cfg);
        provider_run(&mut declaration, "search");
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::ProviderRunSelector(tool)) if tool == "search"
        ));
    }

    #[test]
    fn a_coverage_slot_naming_an_unregistered_tool_is_refused() {
        let cfg = config(vec![tool("fetch")]);
        let mut declaration = covering_declaration(&cfg);
        declaration
            .executor_exceptions
            .insert(ToolName::new("ghost"), ExecutorClass::Assumed);
        assert!(matches!(
            open(cfg.clone(), declaration, ReturnPolicy::Raw),
            Err(LoadError::UnknownDeploymentTool { slot: CoverageSlot::ExecutorException, tool }) if tool == "ghost"
        ));
        let mut declaration = covering_declaration(&cfg);
        declaration.confined_results.insert(ToolName::new("ghost"));
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::UnknownDeploymentTool { slot: CoverageSlot::ConfinedResult, tool }) if tool == "ghost"
        ));
    }

    #[test]
    fn a_redundant_exception_naming_an_unregistered_tool_still_refuses() {
        let cfg = config(vec![tool("fetch")]);
        let mut declaration = ProfileDeclaration::no_coverage(&cfg.trust_chain);
        declaration
            .executor_exceptions
            .insert(ToolName::new("ghost"), ExecutorClass::Assumed);
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::UnknownDeploymentTool { slot: CoverageSlot::ExecutorException, tool }) if tool == "ghost"
        ));
    }

    #[test]
    fn a_provider_run_result_point_cannot_be_declared_confined() {
        let cfg = config(vec![tool("search")]);
        let mut declaration = covering_declaration(&cfg);
        declaration
            .executor_exceptions
            .insert(ToolName::new("search"), ExecutorClass::ProviderRun);
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::ConfinedProviderRun { tool }) if tool == "search"
        ));
    }

    #[test]
    fn the_starting_label_must_be_in_the_deployment_vocabulary() {
        let cfg = config(vec![tool("fetch")]);
        let mut declaration = covering_declaration(&cfg);
        declaration.starting_label = Label::new(Trust::new(9), Audience::Public);
        assert!(matches!(
            open(cfg.clone(), declaration, ReturnPolicy::Raw),
            Err(LoadError::RankOutOfChain { rank: 9, .. })
        ));
        let mut declaration = covering_declaration(&cfg);
        declaration.starting_label = Label::new(Trust::new(1), Audience::restricted([ReaderId::new("@auditors")]));
        assert!(matches!(
            open(cfg, declaration, ReturnPolicy::Raw),
            Err(LoadError::NonLiteralReader { reader, .. }) if reader == "@auditors"
        ));
    }

    #[test]
    fn the_no_coverage_default_loads_plain_policies_and_refuses_coverage_needing_constructs() {
        let cfg = config(vec![tool("fetch")]);
        let declaration = ProfileDeclaration::no_coverage(&cfg.trust_chain);
        let engine = Engine::open(DeploymentPolicy {
            registry: cfg.clone(),
            planner_cap: PlannerCap::default(),
            dialect: PolicyDialectVersion::new(1),
            child_return: ReturnPolicy::Raw,
            profile: declaration.clone(),
        })
        .unwrap();
        assert_eq!(
            engine.profile().starting_label(),
            &neutral_starting_label(&cfg.trust_chain)
        );
        assert_eq!(
            engine.open_vectors(),
            vec![OpenVector::AssumedExecutor {
                tool: ToolName::new("fetch")
            }]
        );
        let mut with_sanitizer = cfg;
        with_sanitizer.sanitizers = vec![output_sanitizer("redactor")];
        assert!(matches!(
            Engine::open(DeploymentPolicy {
                registry: with_sanitizer,
                planner_cap: PlannerCap::default(),
                dialect: PolicyDialectVersion::new(1),
                child_return: ReturnPolicy::Raw,
                profile: declaration,
            }),
            Err(LoadError::OutputSanitizerUncovered { .. })
        ));
    }

    #[test]
    fn provider_run_candidates_leave_the_planner_cap_bound() {
        use crate::contract::HistoryRequirement;
        use crate::fact::EffectKind;

        let mut target = tool("wire");
        target.requires = Requires {
            history: vec![HistoryRequirement::Prior(EffectKind::new("k"))],
            ..Requires::default()
        };
        let mut tools = vec![target];
        for i in 0..4 {
            let mut emitter = tool(&format!("emit{i}"));
            emitter.emits = EffectSet::new([EffectKind::new("k")]).unwrap();
            tools.push(emitter);
        }
        let cfg = config(tools);
        let open_capped = |declaration: ProfileDeclaration, cap: u64| {
            Engine::open(DeploymentPolicy {
                registry: cfg.clone(),
                planner_cap: PlannerCap::new(cap).expect("nonzero"),
                dialect: PolicyDialectVersion::new(1),
                child_return: ReturnPolicy::Raw,
                profile: declaration,
            })
        };
        assert!(matches!(
            open_capped(covering_declaration(&cfg), 4),
            Err(LoadError::TooManyPlanAlternatives { count: 5, max: 4, ref tool }) if tool == "wire"
        ));
        let mut split = covering_declaration(&cfg);
        for i in 0..4 {
            provider_run(&mut split, &format!("emit{i}"));
        }
        assert!(open_capped(split, 1).is_ok());
    }

    fn narrowing_catalogue() -> RegistryConfig {
        let mut leak = tool("leak");
        leak.delta = Delta {
            trust: None,
            audience: Some(DeclaredAudience::literal(Audience::restricted([ReaderId::new(
                "internal",
            )]))),
        };
        let mut cfg = config(vec![leak]);
        cfg.sanitizers = vec![Sanitizer {
            name: SanitizerName::new("scrub"),
            on: SanitizerPoints {
                input: false,
                output: true,
            },
            transition: DeclaredTransition::Audience {
                from_includes: DeclaredAudience::literal(Audience::restricted([ReaderId::new("internal")])),
                to: DeclaredAudience::literal(Audience::Public),
            },
            scope: Scope::default(),
            hint: None,
        }];
        cfg
    }

    fn public_trajectory_log() -> Vec<crate::fact::Fact> {
        vec![opening_at(
            crate::value::TrajectoryId::new("t"),
            Label::new(Trust::new(1), Audience::Public),
        )]
    }

    #[test]
    fn an_unconfined_result_point_offers_no_sanitizer_settlement() {
        use crate::check::CheckOutcome;
        use crate::plan::{RemedyPlan, RemedyStep};
        use crate::projection::Projection;

        let log = public_trajectory_log();
        let trajectory = crate::value::TrajectoryId::new("t");
        let plans_for = |declaration: ProfileDeclaration| {
            let engine = open(narrowing_catalogue(), declaration, ReturnPolicy::Raw).unwrap();
            let projection = Projection::build(&log, 1);
            let views = projection.view(&trajectory);
            let call = engine.resolve_call(ToolName::new("leak"), b"{}").unwrap();
            let raw = match engine.check(&views, &call).unwrap() {
                CheckOutcome::Block(raw) => raw,
                other => panic!("expected a narrowing block, got {other:?}"),
            };
            engine.plan(&views, &call, &raw).unwrap().plans
        };

        let confined = plans_for(covering_declaration(&narrowing_catalogue()));
        assert_eq!(confined.len(), 2);
        assert!(confined.iter().any(|plan| matches!(
            plan,
            RemedyPlan::Executable(executable)
                if executable.steps.iter().any(|step| matches!(step, RemedyStep::Sanitize(_)))
        )));

        let mut unconfined = covering_declaration(&narrowing_catalogue());
        unconfined.confined_results.clear();
        unconfined.confined_child_return = true;
        let offered = plans_for(unconfined);
        assert_eq!(offered.len(), 1);
        assert!(matches!(
            &offered[0],
            RemedyPlan::Executable(executable)
                if matches!(executable.steps.as_slice(), [RemedyStep::Accept(_)])
        ));
    }

    #[test]
    fn unconfined_points_leave_the_sanitizer_factors_out_of_the_cap() {
        let mut cfg = narrowing_catalogue();
        for i in 0..4 {
            let mut sanitizer = cfg.sanitizers[0].clone();
            sanitizer.name = SanitizerName::new(format!("scrub-{i}"));
            cfg.sanitizers.push(sanitizer);
        }
        let open_capped = |result: bool, child: bool, cap: u64| {
            let mut declaration = covering_declaration(&cfg);
            if !result {
                declaration.confined_results.clear();
            }
            declaration.confined_child_return = child;
            Engine::open(DeploymentPolicy {
                registry: cfg.clone(),
                planner_cap: PlannerCap::new(cap).expect("nonzero"),
                dialect: PolicyDialectVersion::new(1),
                child_return: ReturnPolicy::Raw,
                profile: declaration,
            })
        };
        assert!(matches!(
            open_capped(true, false, 5),
            Err(LoadError::TooManyPlanAlternatives { count: 6, max: 5, .. })
        ));
        assert!(matches!(
            open_capped(false, true, 5),
            Err(LoadError::TooManyReturnPlanAlternatives { count: 6, max: 5 })
        ));
        assert!(open_capped(true, false, 6).is_ok());
        assert!(open_capped(false, true, 6).is_ok());
    }

    #[test]
    fn the_child_return_binding_is_validated_at_the_engine_choke_point() {
        let cfg = narrowing_catalogue();
        assert!(matches!(
            open(
                cfg.clone(),
                covering_declaration(&cfg),
                ReturnPolicy::Sanitized(SanitizerName::new("ghost")),
            ),
            Err(LoadError::ChildReturnSanitizerUnknown(name)) if name == "ghost"
        ));
        let mut input_only = cfg.clone();
        input_only.sanitizers[0].on = SanitizerPoints {
            input: true,
            output: false,
        };
        assert!(matches!(
            open(
                input_only,
                covering_declaration(&cfg),
                ReturnPolicy::Sanitized(SanitizerName::new("scrub")),
            ),
            Err(LoadError::ChildReturnSanitizerNotOutput(name)) if name == "scrub"
        ));
        let mut scoped = cfg.clone();
        scoped.sanitizers[0].scope = Scope {
            tags: vec![crate::names::TagName::new("outbound")],
        };
        assert!(matches!(
            open(
                scoped,
                covering_declaration(&cfg),
                ReturnPolicy::Sanitized(SanitizerName::new("scrub")),
            ),
            Err(LoadError::ChildReturnSanitizerScoped(name)) if name == "scrub"
        ));
    }

    #[test]
    fn an_exception_equal_to_the_default_normalizes_away() {
        let cfg = config(vec![tool("fetch")]);
        let mut declaration = covering_declaration(&cfg);
        declaration
            .executor_exceptions
            .insert(ToolName::new("fetch"), ExecutorClass::Enforced);
        let normalized = DeploymentProfile::declare(declaration).unwrap();
        assert_eq!(normalized, covering_profile(&cfg));
    }

    #[test]
    fn a_profile_round_trips_through_serde_and_revalidates_on_the_way_in() {
        let cfg = config(vec![tool("fetch")]);
        let mut declaration = covering_declaration(&cfg);
        declaration
            .provider_surfaces
            .insert(SurfaceName::new("web_search"), SurfaceMode::Open);
        let profile = DeploymentProfile::declare(declaration).unwrap();
        let wire = serde_json::to_string(&profile).unwrap();
        assert_eq!(serde_json::from_str::<DeploymentProfile>(&wire).unwrap(), profile);
        let corrupt = wire.replace(r#""trust":1"#, r#""trust":"bogus""#);
        assert!(serde_json::from_str::<DeploymentProfile>(&corrupt).is_err());
    }

    #[test]
    fn vectors_derive_one_per_weak_choice_in_canonical_order() {
        let cfg = config(vec![tool("a_search"), tool("fetch"), tool("send")]);
        let mut declaration = covering_declaration(&cfg);
        declaration
            .executor_exceptions
            .insert(ToolName::new("send"), ExecutorClass::Assumed);
        provider_run(&mut declaration, "a_search");
        declaration
            .provider_surfaces
            .insert(SurfaceName::new("web_search"), SurfaceMode::Open);
        declaration
            .provider_surfaces
            .insert(SurfaceName::new("file_search"), SurfaceMode::Mediated);
        let engine = open(cfg, declaration, ReturnPolicy::Raw).unwrap();
        assert_eq!(
            engine.open_vectors(),
            vec![
                OpenVector::AssumedExecutor {
                    tool: ToolName::new("send")
                },
                OpenVector::ProviderRunDispatch {
                    tool: ToolName::new("a_search")
                },
                OpenVector::OpenProviderSurface {
                    surface: SurfaceName::new("web_search")
                },
            ]
        );
    }

    #[test]
    fn a_fully_enforced_deployment_derives_no_vectors() {
        let cfg = config(vec![tool("fetch")]);
        let engine = open(cfg.clone(), covering_declaration(&cfg), ReturnPolicy::Raw).unwrap();
        assert_eq!(engine.open_vectors(), vec![]);
    }

    fn identity(cfg: &RegistryConfig, child: &ReturnPolicy, profile: &DeploymentProfile) -> PolicyIdentityV1 {
        super::identity_of(cfg, child, profile)
    }

    #[test]
    fn duplicate_set_members_do_not_move_the_identity() {
        let single = {
            let mut t = tool("fetch");
            t.tags = vec![crate::names::TagName::new("finance")];
            config(vec![t])
        };
        let doubled = {
            let mut t = tool("fetch");
            t.tags = vec![
                crate::names::TagName::new("finance"),
                crate::names::TagName::new("finance"),
            ];
            config(vec![t])
        };
        let profile = covering_profile(&single);
        assert_eq!(
            identity(&single, &ReturnPolicy::Raw, &profile),
            identity(&doubled, &ReturnPolicy::Raw, &profile)
        );
    }

    #[test]
    fn hints_never_move_the_identity() {
        let mut cfg = config(vec![tool("fetch")]);
        cfg.authorities = vec![Authority {
            name: AuthorityName::new("officer"),
            mandate: Mandate {
                trust_ceiling: Some(Trust::new(1)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        }];
        cfg.sanitizers = vec![output_sanitizer("redactor")];
        let profile = covering_profile(&cfg);
        let bare = identity(&cfg, &ReturnPolicy::Raw, &profile);
        cfg.authorities[0].hint = Some(Hint::new("the wire-approval desk"));
        cfg.sanitizers[0].hint = Some(Hint::new("strips PII"));
        assert_eq!(identity(&cfg, &ReturnPolicy::Raw, &profile), bare);
    }

    #[test]
    fn rescoping_a_sanitizer_moves_the_identity() {
        let mut cfg = config(vec![tool("fetch")]);
        cfg.sanitizers = vec![output_sanitizer("redactor")];
        let profile = covering_profile(&cfg);
        let unscoped = identity(&cfg, &ReturnPolicy::Raw, &profile);

        cfg.sanitizers[0].scope = Scope {
            tags: vec![TagName::new("outbound")],
        };
        let scoped = identity(&cfg, &ReturnPolicy::Raw, &profile);
        assert_ne!(scoped, unscoped);

        cfg.sanitizers[0].scope = Scope {
            tags: vec![TagName::new("inbound")],
        };
        assert_ne!(identity(&cfg, &ReturnPolicy::Raw, &profile), scoped);
    }

    #[test]
    fn declaration_order_moves_the_identity_only_where_order_is_semantic() {
        let mut cfg = config(vec![tool("a"), tool("b")]);
        let officer = |name: &str| Authority {
            name: AuthorityName::new(name),
            mandate: Mandate {
                trust_ceiling: Some(Trust::new(1)),
                ..Mandate::default()
            },
            scope: Scope::default(),
            hint: None,
        };
        cfg.authorities = vec![officer("first"), officer("second")];
        let profile = covering_profile(&cfg);
        let base = identity(&cfg, &ReturnPolicy::Raw, &profile);

        let mut permuted = cfg.clone();
        permuted.tools.reverse();
        assert_eq!(identity(&permuted, &ReturnPolicy::Raw, &profile), base);

        let mut rerouted = cfg.clone();
        rerouted.authorities.reverse();
        assert_ne!(identity(&rerouted, &ReturnPolicy::Raw, &profile), base);
    }

    #[test]
    fn every_semantic_edit_moves_the_identity() {
        let cfg = config(vec![tool("fetch")]);
        let profile = covering_profile(&cfg);
        let base = identity(&cfg, &ReturnPolicy::Raw, &profile);

        let delta_edit = config(vec![{
            let mut t = tool("fetch");
            t.delta = Delta {
                trust: Some(Trust::new(0)),
                audience: None,
            };
            t
        }]);
        assert_ne!(identity(&delta_edit, &ReturnPolicy::Raw, &profile), base);

        // Routing the tool through an Annotator, and then narrowing that Annotator's mandate,
        // each move the identity: what a produced annotation may say is part of the policy.
        let routed = |trust: Option<std::collections::BTreeSet<Trust>>| {
            let mut cfg = config(vec![]);
            cfg.annotators = vec![crate::registry::AnnotatorDeclaration { trust, ..classifier() }];
            cfg.tools = vec![annotated("fetch")];
            cfg
        };
        let annotated_edit = routed(None);
        assert_ne!(identity(&annotated_edit, &ReturnPolicy::Raw, &profile), base);
        assert_ne!(
            identity(
                &routed(Some(std::collections::BTreeSet::from([Trust::new(0)]))),
                &ReturnPolicy::Raw,
                &profile
            ),
            identity(&annotated_edit, &ReturnPolicy::Raw, &profile)
        );

        let sanitized = ReturnPolicy::Sanitized(SanitizerName::new("redactor"));
        assert_ne!(identity(&cfg, &sanitized, &profile), base);

        let mut weaker = covering_declaration(&cfg);
        weaker
            .executor_exceptions
            .insert(ToolName::new("fetch"), ExecutorClass::Assumed);
        let weaker = DeploymentProfile::declare(weaker).unwrap();
        assert_ne!(identity(&cfg, &ReturnPolicy::Raw, &weaker), base);
    }
}
