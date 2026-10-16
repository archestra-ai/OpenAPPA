//! `appa/do_something`: the call a replay trace proposes to ask what the trajectory allows
//! at one step without naming a tool. Its one argument, `requires`, is a tool contract's
//! `requires` table written as JSON; its contract is that table and nothing else — no
//! `delta`, no `forwards`, no `effects` — so judging it never narrows the trajectory.
//!
//! Only a deployment whose configuration opts in serves it ([`Config::with_do_something`]),
//! which `appa replay` and an embedding host's replay do. The deployment then declares it as
//! an Annotated tool beside the authored policy, and the runtime pins the annotation from the
//! call's own arguments instead of consulting anyone. Every other deployment refuses the
//! name before it is judged, and no authored policy may declare it.
//!
//! [`Config::with_do_something`]: crate::config::Config::with_do_something

use appa_engine::contract::{
    AudienceRequirement, Delta, HistoryRequirement, PinnedAnnotation, ProducedAnnotation, RecipientSpec,
    ToolDeclaration,
};
use appa_engine::fact::EffectSet;
use appa_engine::label::DeclaredAudience;
use appa_engine::names::AnnotatorName;
use appa_engine::params::ToolParameters;
use appa_engine::registry::{AnnotatorDeclaration, Registry};
use appa_engine::value::{ResolvedCall, ToolName};

/// The call's canonical name, which is also the name of the Annotator its declaration names.
pub(crate) const NAME: &str = "appa/do_something";

const ARGUMENT: &str = "requires";

/// Whether a deployment serves `appa/do_something`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DoSomething {
    #[default]
    Refused,
    Served,
}

/// The declarations a serving deployment adds to its policy: the tool, and an Annotator whose
/// mandate is the whole policy vocabulary, so a `requires` may name anything the policy can.
pub(crate) fn declarations() -> (ToolDeclaration, AnnotatorDeclaration) {
    let annotator = AnnotatorName::new(NAME);
    let tool = ToolDeclaration::Annotated {
        name: ToolName::new(NAME),
        tags: Vec::new(),
        description: None,
        parameters: ToolParameters::open(),
        annotator: annotator.clone(),
    };
    let mandate = AnnotatorDeclaration {
        name: annotator,
        trust: None,
        audiences: None,
        marks: None,
        effects: None,
    };
    (tool, mandate)
}

/// Whether `annotator` is the one a serving deployment declares for `appa/do_something`. An
/// authored policy cannot declare that name, so only a serving deployment registers it.
pub(crate) fn is_annotator(annotator: &AnnotatorName) -> bool {
    annotator.as_str() == NAME
}

/// Whether the deployment this registry belongs to serves `appa/do_something`.
pub(crate) fn is_served(registry: &Registry) -> bool {
    registry.annotator_mandate(&AnnotatorName::new(NAME)).is_some()
}

/// The annotation one `appa/do_something` call carries, read from its `requires` argument.
/// A requirement the policy cannot state refuses the call: the step cannot run, which a trace
/// must never read as the call being denied.
pub(crate) fn pin(registry: &Registry, call: &ResolvedCall) -> Result<PinnedAnnotation, String> {
    let arguments = call
        .arguments()
        .as_object()
        .ok_or_else(|| format!("{NAME} takes one argument, `{ARGUMENT}`"))?;
    if let Some(key) = arguments.keys().find(|key| *key != ARGUMENT) {
        return Err(format!("{NAME} takes one argument, `{ARGUMENT}`; found `{key}`"));
    }
    let value = arguments
        .get(ARGUMENT)
        .ok_or_else(|| format!("{NAME} takes one argument, `{ARGUMENT}`"))?;
    let requires =
        appa_policy::parse_requires(value, registry.trust_chain(), NAME).map_err(|error| error.to_string())?;
    let mandate = registry
        .annotator_mandate(&AnnotatorName::new(NAME))
        .expect("a deployment serving appa/do_something registers its Annotator");
    for requirement in requires.audience_requirements() {
        match requirement {
            AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::Public)) => {}
            AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::Union(clause))) => {
                if !mandate.permits_clause(clause) {
                    return Err(format!(
                        "{NAME}: `requires.audience.contains` names an audience the policy does not; it may name `public`, `self`, `internal`, and the audiences the policy names"
                    ));
                }
            }
            AudienceRequirement::Includes(RecipientSpec::Placeholder(_) | RecipientSpec::Selector(_)) => {
                return Err(format!(
                    "{NAME}: `requires.audience.contains` names a call argument; write the audience itself"
                ));
            }
        }
    }
    if let Some(mark) = requires
        .attention_marks()
        .iter()
        .find(|mark| !mandate.permits_mark(mark))
    {
        return Err(format!(
            "{NAME}: the policy declares no attention mark {}",
            mark.as_str()
        ));
    }
    if let Some(kind) = requires
        .history
        .iter()
        .map(|(HistoryRequirement::Prior(kind) | HistoryRequirement::NoPrior(kind))| kind)
        .find(|kind| !mandate.permits_effect(kind))
    {
        return Err(format!("{NAME}: the policy declares no effect {}", kind.as_str()));
    }
    let produced = ProducedAnnotation {
        delta: Delta::NONE,
        forwards: Delta::NONE,
        emits: EffectSet::default(),
        requires,
    };
    Ok(PinnedAnnotation::new(AnnotatorName::new(NAME), call.digest(), produced))
}
