//! `[[interceptor]]` and `unblocks`: the checks the runtime runs on a step, and the tools an
//! authority or sanitizer is bound to unblock. Both bind to tools the same way — by the tool
//! selector as the policy writes it — and compile to a reserved tag on each bound declaration.

use appa_engine::authority::Scope;
use appa_engine::contract::ToolDeclaration;
use appa_engine::intercept::InterceptorDeclaration;
use appa_engine::names::{InterceptorName, TagName};

use crate::convert::{parse_hint, split_selector};
use crate::error::ConfigError;
use crate::raw::RawInterceptor;

/// The stock implementations an `[[interceptor]]` may name with `builtin`. Any other
/// Interceptor is bound by the deployment under `[externals.interceptors.<name>]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptorBuiltin {
    /// `settings = { pattern, on_match = "no" | "ask" }`: the answer when the pattern matches.
    Regex,
    /// `settings = { policies }`: Cedar policies; a matching `forbid` answers `no`, or `ask`
    /// where it carries `@verdict("ask")`.
    Cedar,
}

impl InterceptorBuiltin {
    pub const fn wire_name(self) -> &'static str {
        match self {
            InterceptorBuiltin::Regex => "regex",
            InterceptorBuiltin::Cedar => "cedar",
        }
    }

    fn parse(name: &str) -> Option<InterceptorBuiltin> {
        [InterceptorBuiltin::Regex, InterceptorBuiltin::Cedar]
            .into_iter()
            .find(|builtin| builtin.wire_name() == name)
    }
}

/// The steps an Interceptor judges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InterceptorSteps {
    /// The user's prompt, before the model reads it.
    pub prompt: bool,
    /// A tool call, before it runs.
    pub tool_call: bool,
}

/// One `[[interceptor]]` as the runtime runs it.
#[derive(Clone, Debug, PartialEq)]
pub struct InterceptorBinding {
    pub on: InterceptorSteps,
    /// The stock implementation, or `None` for one the deployment binds.
    pub builtin: Option<InterceptorBuiltin>,
    /// The implementation's settings, as written.
    pub settings: serde_json::Value,
    pub hint: Option<String>,
}

/// Every `[[interceptor]]` compiled: the runtime bindings in authored order, and the
/// tool-call Interceptors the engine pins answers for, each scoped to the tools it names.
pub(crate) struct CompiledInterceptors {
    pub(crate) bindings: Vec<(InterceptorName, InterceptorBinding)>,
    pub(crate) declarations: Vec<InterceptorDeclaration>,
}

pub(crate) fn compile_interceptors(
    raw: Vec<RawInterceptor>,
    tools: &mut [ToolDeclaration],
) -> Result<CompiledInterceptors, ConfigError> {
    let mut bindings = Vec::new();
    let mut declarations = Vec::new();
    for interceptor in raw {
        let name = interceptor.name;
        if name.is_empty() {
            return Err(ConfigError::BadInterceptor {
                name,
                reason: "the name is empty".into(),
            });
        }
        let bad = |reason: String| ConfigError::BadInterceptor {
            name: name.clone(),
            reason,
        };
        let mut on = InterceptorSteps::default();
        for step in &interceptor.on {
            match step.as_str() {
                "prompt" => on.prompt = true,
                "tool_call" => on.tool_call = true,
                other => {
                    return Err(bad(format!(
                        "unknown step {other:?}: expected \"prompt\" or \"tool_call\""
                    )));
                }
            }
        }
        if !on.prompt && !on.tool_call {
            return Err(bad("`on` names no step".into()));
        }
        let builtin = interceptor
            .builtin
            .map(|builtin| {
                InterceptorBuiltin::parse(&builtin)
                    .ok_or_else(|| bad(format!("unknown builtin {builtin:?}: expected \"regex\" or \"cedar\"")))
            })
            .transpose()?;
        let settings = match interceptor.settings {
            Some(table) => serde_json::to_value(table).map_err(|error| bad(format!("settings: {error}")))?,
            None => serde_json::Value::Object(serde_json::Map::new()),
        };
        let scope = match (interceptor.tools, on.tool_call) {
            (Some(_), false) => return Err(bad("`tools` binds tool calls, but `on` names no \"tool_call\"".into())),
            (Some(selectors), true) => {
                let tag = TagName::new(format!("{}{name}", InterceptorDeclaration::TAG_PREFIX));
                bind_tools(&selectors, tools, &tag, &format!("interceptor {name}"))?;
                Scope { tags: vec![tag] }
            }
            (None, _) => Scope::default(),
        };
        if on.tool_call {
            declarations.push(InterceptorDeclaration {
                name: InterceptorName::new(name.clone()),
                scope,
            });
        }
        let hint = parse_hint(interceptor.hint, &format!("interceptor {name}"))?.map(|hint| hint.as_str().to_string());
        bindings.push((
            InterceptorName::new(name),
            InterceptorBinding {
                on,
                builtin,
                settings,
                hint,
            },
        ));
    }
    Ok(CompiledInterceptors { bindings, declarations })
}

/// The scope `unblocks` gives a component: the component's own reserved tag, added to every
/// tool declaration the selectors name.
pub(crate) fn unblock_scope(
    component: &str,
    selectors: &[String],
    tools: &mut [ToolDeclaration],
    context: &str,
) -> Result<Scope, ConfigError> {
    let tag = TagName::new(format!("{}{component}", Scope::UNBLOCK_PREFIX));
    bind_tools(selectors, tools, &tag, context)?;
    Ok(Scope { tags: vec![tag] })
}

/// Policy tags never carry a reserved prefix: the compiler alone writes those.
pub(crate) fn refuse_reserved_tags(tags: &[String], context: &str) -> Result<(), ConfigError> {
    match tags
        .iter()
        .find(|tag| tag.starts_with(Scope::UNBLOCK_PREFIX) || tag.starts_with(InterceptorDeclaration::TAG_PREFIX))
    {
        Some(tag) => Err(ConfigError::ReservedTag {
            context: context.to_string(),
            tag: tag.clone(),
        }),
        None => Ok(()),
    }
}

/// Tag every declaration a selector names: `*` names every declaration, a selector with an
/// argument clause names the declaration written exactly so, and a bare tool name names every
/// declaration of that tool. A selector that names nothing is refused.
fn bind_tools(
    selectors: &[String],
    tools: &mut [ToolDeclaration],
    tag: &TagName,
    context: &str,
) -> Result<(), ConfigError> {
    if selectors.is_empty() {
        return Err(ConfigError::UnboundTools {
            context: context.to_string(),
            selector: String::new(),
        });
    }
    for selector in selectors {
        let mut bound = false;
        for tool in tools.iter_mut() {
            let name = tool.name().as_str();
            let names = selector == "*"
                || name == selector
                || (!selector.contains('(') && split_selector(name).0 == selector.as_str());
            if names {
                tag_declaration(tool, tag);
                bound = true;
            }
        }
        if !bound {
            return Err(ConfigError::UnboundTools {
                context: context.to_string(),
                selector: selector.clone(),
            });
        }
    }
    Ok(())
}

fn tag_declaration(tool: &mut ToolDeclaration, tag: &TagName) {
    let tags = match tool {
        ToolDeclaration::Declared(annotation) => &mut annotation.tags,
        ToolDeclaration::Annotated { tags, .. } => tags,
    };
    if !tags.contains(tag) {
        tags.push(tag.clone());
    }
}
