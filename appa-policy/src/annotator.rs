use std::collections::BTreeMap;

use appa_engine::authority::Hint;
use appa_engine::contract::ToolDeclaration;
use appa_engine::fact::EffectKind;
use appa_engine::names::{AnnotatorName, MarkName};
use appa_engine::registry::{AnnotatorDeclaration, LoadError, MAX_HINT_CHARS, TrustChain};

use crate::convert::{parse_annotator_audiences, parse_trust, refuse_inline_binding};
use crate::error::ConfigError;
use crate::raw::RawAnnotator;

/// The stock annotators an `[[annotator]]` may name on its declaration with `builtin`: the
/// two model transports and TypeSafe's Jev classifier. Closed: the runtime compiles all three
/// in. An Annotator that names one takes no deployment binding; every other Annotator is
/// bound by the deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotatorBuiltin {
    ClaudeCode,
    Llm,
    Jev,
}

impl AnnotatorBuiltin {
    pub const ALL: [AnnotatorBuiltin; 3] = [
        AnnotatorBuiltin::ClaudeCode,
        AnnotatorBuiltin::Llm,
        AnnotatorBuiltin::Jev,
    ];

    /// The name a policy writes: `claude-code`, `llm`, or `jev`.
    pub const fn wire_name(self) -> &'static str {
        match self {
            AnnotatorBuiltin::ClaudeCode => "claude-code",
            AnnotatorBuiltin::Llm => "llm",
            AnnotatorBuiltin::Jev => "jev",
        }
    }

    fn parse(name: &str) -> Option<AnnotatorBuiltin> {
        AnnotatorBuiltin::ALL
            .into_iter()
            .find(|builtin| builtin.wire_name() == name)
    }
}

/// What one `[[annotator]]` input reads: a value of the tool call, or the answer of a program
/// the deployment binds under `[externals.inputs.<name>]` and runs before the annotator. The
/// mapping is policy syntax the runtime executes when it builds a consult artifact; the engine
/// never sees it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputSource {
    Call(ToolCallSource),
    /// `$input.<name>`
    External(String),
}

impl InputSource {
    pub fn parse(spelling: &str) -> Option<InputSource> {
        match spelling.strip_prefix("$input.") {
            Some(name) if !name.is_empty() => Some(InputSource::External(name.to_string())),
            Some(_) => None,
            None => ToolCallSource::parse(spelling).map(InputSource::Call),
        }
    }

    pub fn spelling(&self) -> String {
        match self {
            InputSource::Call(source) => source.spelling(),
            InputSource::External(name) => format!("$input.{name}"),
        }
    }
}

/// The five values of the tool call an input can read.
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

/// One registered `[[annotator]]` as the runtime consumes it: the deployer's instruction,
/// the stock builtin it names, if any, and the input mapping its consult artifacts carry.
/// An empty mapping sends the complete call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatorBinding {
    pub hint: Option<Hint>,
    pub builtin: Option<AnnotatorBuiltin>,
    pub inputs: BTreeMap<String, InputSource>,
}

/// Each `[[annotator]]` as its runtime-owned binding and as the declaration the registry
/// validates. A duplicate name reaches the registry (the Vec keeps both) and is refused there.
pub(crate) fn compile_annotators(
    raw: Vec<RawAnnotator>,
    trust_chain: &TrustChain,
) -> Result<(BTreeMap<AnnotatorName, AnnotatorBinding>, Vec<AnnotatorDeclaration>), ConfigError> {
    let mut annotators: BTreeMap<AnnotatorName, AnnotatorBinding> = BTreeMap::new();
    let mut annotator_declarations = Vec::new();
    for annotator in raw {
        if annotator.name.is_empty() {
            return Err(ConfigError::BadAnnotatorName(annotator.name));
        }
        let name = AnnotatorName::new(annotator.name);
        refuse_inline_binding("annotator", name.as_str(), annotator.implementation.as_ref())?;
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
        let hint = annotator.hint.map(Hint::new);
        if let Some(hint) = &hint
            && hint.as_str().chars().count() > MAX_HINT_CHARS
        {
            return Err(ConfigError::Registry(LoadError::HintTooLong {
                context: format!("annotator {}", name.as_str()),
                len: hint.as_str().chars().count(),
                max: MAX_HINT_CHARS,
            }));
        }
        let mut inputs = BTreeMap::new();
        for (input, spelling) in annotator.inputs.unwrap_or_default() {
            let Some(source) = InputSource::parse(&spelling) else {
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
                        .map(|rank| parse_trust(rank, trust_chain, &ctx()))
                        .collect()
                })
                .transpose()?,
            audiences: annotator
                .audiences
                .map(|entries| parse_annotator_audiences(&entries, &ctx()))
                .transpose()?,
            marks: annotator
                .marks
                .map(|marks| marks.into_iter().map(MarkName::new).collect()),
            effects: annotator
                .effects
                .map(|effects| effects.into_iter().map(EffectKind::new).collect()),
        });
        annotators.insert(name, AnnotatorBinding { hint, builtin, inputs });
    }
    Ok((annotators, annotator_declarations))
}

/// An input mapping is validated against every tool that routes through its Annotator:
/// a mapped argument must be a required top-level property of that tool's schema, and a
/// description read needs a declared description. A tool naming an unregistered
/// annotator is skipped here — the registry refuses it.
pub(crate) fn validate_annotator_inputs(
    tools: &[ToolDeclaration],
    annotators: &BTreeMap<AnnotatorName, AnnotatorBinding>,
) -> Result<(), ConfigError> {
    for tool in tools {
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
                    InputSource::Call(ToolCallSource::Argument(argument)) => parameters
                        .required_property(argument)
                        .err()
                        .map(|fault| format!("which {fault}")),
                    InputSource::Call(ToolCallSource::Description) if description.is_none() => {
                        Some("but the tool declares no description".to_string())
                    }
                    InputSource::Call(_) | InputSource::External(_) => None,
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
    Ok(())
}
