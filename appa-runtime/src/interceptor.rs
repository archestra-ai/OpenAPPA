//! The deployment's `[[interceptor]]`s: compiled once when the deployment opens, then asked
//! about each step they are bound to. A builtin answers here, in process; any other
//! Interceptor is a consult of its `[externals.interceptors.<name>]` binding. An Interceptor
//! that cannot answer — an evaluation error, a transport failure, a malformed answer — gives
//! no answer, and a step without every answer does not proceed: it is refused, never turned
//! into a `no` an unblocker could lift.

#[cfg(feature = "cedar")]
use std::str::FromStr;

use appa_engine::intercept::{InterceptorAnswer, Verdict};
use appa_engine::names::InterceptorName;
use appa_policy::{InterceptorBinding, InterceptorBuiltin, InterceptorSteps};

use crate::consult::InterceptedStep;

/// Every `[[interceptor]]` of one policy, compiled, in authored order.
#[derive(Debug, Default)]
pub(crate) struct Interceptors(Vec<Interceptor>);

#[derive(Debug)]
pub(crate) struct Interceptor {
    pub(crate) name: InterceptorName,
    pub(crate) on: InterceptorSteps,
    pub(crate) hint: Option<String>,
    judge: Judge,
}

#[derive(Debug)]
enum Judge {
    Regex {
        pattern: regex::Regex,
        on_match: Verdict,
    },
    #[cfg(feature = "cedar")]
    Cedar {
        policies: Box<cedar_policy::PolicySet>,
    },
    /// Answered by the consult of the deployment's binding.
    Bound,
}

/// Why an Interceptor's settings do not compile.
#[derive(Debug, thiserror::Error)]
#[error("interceptor {name}: {reason}")]
pub struct InterceptorError {
    name: String,
    reason: String,
}

/// Why an Interceptor gave no answer about a step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("interceptor {interceptor} gave no answer: {reason}")]
pub struct NoVerdict {
    pub interceptor: String,
    pub reason: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RegexSettings {
    pattern: String,
    #[serde(default)]
    on_match: Option<String>,
}

#[cfg(feature = "cedar")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CedarSettings {
    policies: String,
}

/// Every Cedar set judges against this rule too, so a call no `forbid` names is allowed.
#[cfg(feature = "cedar")]
const DEFAULT_PERMIT: &str = "permit(principal, action, resource);";
#[cfg(feature = "cedar")]
const DEFAULT_PERMIT_ID: &str = "appa-default-permit";

impl Interceptors {
    /// Compile the policy's Interceptors. A builtin's settings are checked here, so a
    /// deployment whose regex or Cedar text does not compile does not open.
    pub(crate) fn compile(policy: &appa_policy::Config) -> Result<Interceptors, InterceptorError> {
        policy
            .interceptors()
            .map(|(name, binding)| Interceptor::compile(name, binding))
            .collect::<Result<_, _>>()
            .map(Interceptors)
    }

    pub(crate) fn get(&self, name: &InterceptorName) -> Option<&Interceptor> {
        self.0.iter().find(|interceptor| &interceptor.name == name)
    }

    /// The Interceptors bound to the user's prompt.
    pub(crate) fn on_prompt(&self) -> impl Iterator<Item = &Interceptor> {
        self.0.iter().filter(|interceptor| interceptor.on.prompt)
    }

    /// Every Interceptor the deployment binds by name rather than naming a builtin.
    pub(crate) fn bound(&self) -> impl Iterator<Item = &InterceptorName> {
        self.0
            .iter()
            .filter(|interceptor| matches!(interceptor.judge, Judge::Bound))
            .map(|interceptor| &interceptor.name)
    }
}

impl Interceptor {
    fn compile(name: &InterceptorName, binding: &InterceptorBinding) -> Result<Interceptor, InterceptorError> {
        let failed = |reason: String| InterceptorError {
            name: name.as_str().to_string(),
            reason,
        };
        let judge = match binding.builtin {
            None => Judge::Bound,
            Some(InterceptorBuiltin::Regex) => {
                let settings: RegexSettings = serde_json::from_value(binding.settings.clone())
                    .map_err(|error| failed(format!("regex settings: {error}")))?;
                let pattern = regex::Regex::new(&settings.pattern).map_err(|error| failed(error.to_string()))?;
                let on_match = match settings.on_match.as_deref() {
                    None | Some("no") => Verdict::No,
                    Some("ask") => Verdict::Ask,
                    Some(other) => return Err(failed(format!("on_match {other:?}: expected \"no\" or \"ask\""))),
                };
                Judge::Regex { pattern, on_match }
            }
            #[cfg(not(feature = "cedar"))]
            Some(InterceptorBuiltin::Cedar) => {
                return Err(failed(
                    "this build of appa has no Cedar evaluator; build it with `--features cedar`".into(),
                ));
            }
            #[cfg(feature = "cedar")]
            Some(InterceptorBuiltin::Cedar) => {
                let settings: CedarSettings = serde_json::from_value(binding.settings.clone())
                    .map_err(|error| failed(format!("cedar settings: {error}")))?;
                let mut policies =
                    cedar_policy::PolicySet::from_str(&settings.policies).map_err(|error| failed(error.to_string()))?;
                let permit =
                    cedar_policy::Policy::parse(Some(cedar_policy::PolicyId::new(DEFAULT_PERMIT_ID)), DEFAULT_PERMIT)
                        .expect("the default permit parses");
                policies.add(permit).map_err(|error| failed(error.to_string()))?;
                Judge::Cedar {
                    policies: Box::new(policies),
                }
            }
        };
        Ok(Interceptor {
            name: name.clone(),
            on: binding.on,
            hint: binding.hint.clone(),
            judge,
        })
    }

    /// The answer a builtin gives about a step, or `None` for an Interceptor the deployment
    /// binds, which is consulted instead.
    pub(crate) fn judge_locally(&self, step: &InterceptedStep) -> Option<Result<InterceptorAnswer, NoVerdict>> {
        match &self.judge {
            Judge::Bound => None,
            Judge::Regex { pattern, on_match } => {
                let text = match step {
                    InterceptedStep::Prompt { prompt } => prompt.clone(),
                    InterceptedStep::ToolCall { arguments, .. } => arguments.to_string(),
                };
                Some(Ok(match pattern.is_match(&text) {
                    true => InterceptorAnswer {
                        verdict: *on_match,
                        reason: Some(format!("the {} matches /{}/", step_noun(step), pattern.as_str())),
                    },
                    false => InterceptorAnswer {
                        verdict: Verdict::Yes,
                        reason: None,
                    },
                }))
            }
            #[cfg(feature = "cedar")]
            Judge::Cedar { policies } => Some(cedar_answer(policies, step).map_err(|reason| NoVerdict {
                interceptor: self.name.as_str().to_string(),
                reason,
            })),
        }
    }
}

fn step_noun(step: &InterceptedStep) -> &'static str {
    match step {
        InterceptedStep::Prompt { .. } => "prompt",
        InterceptedStep::ToolCall { .. } => "call",
    }
}

#[cfg(feature = "cedar")]
/// Judge a step under the Cedar policies: principal `Agent::"agent"`, action
/// `Action::"prompt"` or `Action::"tool_call"`, resource `Prompt::"prompt"` or
/// `Tool::"<tool>"`, and the context `{prompt}` or `{arguments}`. A `forbid` that matches
/// answers `no`, or `ask` when it carries `@verdict("ask")`; a `no` outweighs an `ask`. Any
/// evaluation error is no answer: Cedar skips an erroring policy, which would let the call
/// through.
fn cedar_answer(policies: &cedar_policy::PolicySet, step: &InterceptedStep) -> Result<InterceptorAnswer, String> {
    let (action, resource, context) = match step {
        InterceptedStep::Prompt { prompt } => (
            "prompt",
            r#"Prompt::"prompt""#.to_string(),
            serde_json::json!({ "prompt": prompt }),
        ),
        InterceptedStep::ToolCall { tool, arguments } => (
            "tool_call",
            format!("Tool::{}", serde_json::Value::String(tool.clone())),
            serde_json::json!({ "arguments": cedar_value(arguments) }),
        ),
    };
    let uid = |text: &str| cedar_policy::EntityUid::from_str(text).map_err(|error| error.to_string());
    let request = cedar_policy::Request::new(
        uid(r#"Agent::"agent""#)?,
        uid(&format!("Action::{}", serde_json::Value::String(action.into())))?,
        uid(&resource)?,
        cedar_policy::Context::from_json_value(context, None).map_err(|error| error.to_string())?,
        None,
    )
    .map_err(|error| error.to_string())?;
    let response = cedar_policy::Authorizer::new().is_authorized(&request, policies, &cedar_policy::Entities::empty());
    if let Some(error) = response.diagnostics().errors().next() {
        return Err(error.to_string());
    }
    if response.decision() == cedar_policy::Decision::Allow {
        return Ok(InterceptorAnswer {
            verdict: Verdict::Yes,
            reason: None,
        });
    }
    let mut verdict = Verdict::Ask;
    let mut reasons = Vec::new();
    for id in response.diagnostics().reason() {
        let policy = policies.policy(id).expect("a reason names a policy of the set");
        if policy.annotation("verdict") != Some("ask") {
            verdict = Verdict::No;
        }
        reasons.push(
            policy
                .annotation("reason")
                .or_else(|| policy.annotation("id"))
                .map_or_else(|| id.to_string(), str::to_string),
        );
    }
    Ok(InterceptorAnswer {
        verdict,
        reason: Some(reasons.join("; ")),
    })
}

#[cfg(feature = "cedar")]
/// A call's arguments as a Cedar record: Cedar has no floats and no null, so a float reads
/// as its decimal text and a null field is left out.
fn cedar_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(number) if !number.is_i64() => serde_json::Value::String(number.to_string()),
        serde_json::Value::Array(items) => items.iter().filter(|item| !item.is_null()).map(cedar_value).collect(),
        serde_json::Value::Object(fields) => fields
            .iter()
            .filter(|(_, field)| !field.is_null())
            .map(|(key, field)| (key.clone(), cedar_value(field)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(builtin: InterceptorBuiltin, settings: serde_json::Value) -> Result<Interceptor, InterceptorError> {
        Interceptor::compile(
            &InterceptorName::new("check"),
            &InterceptorBinding {
                on: InterceptorSteps {
                    prompt: true,
                    tool_call: true,
                },
                builtin: Some(builtin),
                settings,
                hint: None,
            },
        )
    }

    fn call(tool: &str, arguments: serde_json::Value) -> InterceptedStep {
        InterceptedStep::ToolCall {
            tool: tool.into(),
            arguments,
        }
    }

    fn verdict(interceptor: &Interceptor, step: &InterceptedStep) -> Verdict {
        interceptor
            .judge_locally(step)
            .expect("a builtin answers locally")
            .expect("the builtin answers")
            .verdict
    }

    #[test]
    fn a_regex_answers_its_on_match_verdict_when_it_matches() {
        let cards = compiled(
            InterceptorBuiltin::Regex,
            serde_json::json!({ "pattern": r"\d{4}( ?\d{4}){3}" }),
        )
        .unwrap();
        let prompt = |text: &str| InterceptedStep::Prompt { prompt: text.into() };
        assert_eq!(verdict(&cards, &prompt("pay with 4111 1111 1111 1111")), Verdict::No);
        assert_eq!(verdict(&cards, &prompt("pay later")), Verdict::Yes);
        let asking = compiled(
            InterceptorBuiltin::Regex,
            serde_json::json!({ "pattern": "(?i)orlando", "on_match": "ask" }),
        )
        .unwrap();
        assert_eq!(
            verdict(
                &asking,
                &call("Bash", serde_json::json!({ "command": "git commit -m 'ORLANDO'" }))
            ),
            Verdict::Ask
        );
    }

    #[test]
    fn a_regex_refuses_bad_settings() {
        assert!(compiled(InterceptorBuiltin::Regex, serde_json::json!({ "pattern": "(" })).is_err());
        assert!(compiled(InterceptorBuiltin::Regex, serde_json::json!({ "patern": "a" })).is_err());
        assert!(
            compiled(
                InterceptorBuiltin::Regex,
                serde_json::json!({ "pattern": "a", "on_match": "maybe" })
            )
            .is_err()
        );
    }

    #[cfg(feature = "cedar")]
    const RULES: &str = r#"
        @id("no-orlando-commits")
        @reason("commit messages must not name Orlando")
        forbid (principal, action == Action::"tool_call", resource == Tool::"Bash")
        when { context.arguments has command && context.arguments.command like "*Orlando*" };

        @verdict("ask")
        @reason("a push needs a person")
        forbid (principal, action == Action::"tool_call", resource == Tool::"Bash")
        when { context.arguments has command && context.arguments.command like "git push*" };
    "#;

    #[cfg(feature = "cedar")]
    #[test]
    fn cedar_answers_no_for_a_matching_forbid_and_ask_where_it_says_so() {
        let rules = compiled(InterceptorBuiltin::Cedar, serde_json::json!({ "policies": RULES })).unwrap();
        let bash = |command: &str| call("Bash", serde_json::json!({ "command": command, "timeout": 1.5 }));
        assert_eq!(verdict(&rules, &bash("ls")), Verdict::Yes);
        let refused = rules
            .judge_locally(&bash("git commit -m 'for Orlando'"))
            .unwrap()
            .unwrap();
        assert_eq!(refused.verdict, Verdict::No);
        assert_eq!(refused.reason.as_deref(), Some("commit messages must not name Orlando"));
        assert_eq!(verdict(&rules, &bash("git push origin main")), Verdict::Ask);
        assert_eq!(
            verdict(&rules, &call("Read", serde_json::json!({ "path": "x" }))),
            Verdict::Yes
        );
    }

    #[cfg(feature = "cedar")]
    #[test]
    fn a_cedar_evaluation_error_is_no_answer() {
        let rules = compiled(
            InterceptorBuiltin::Cedar,
            serde_json::json!({ "policies": r#"forbid (principal, action, resource) when { context.arguments.command like "*x*" };"# }),
        )
        .unwrap();
        assert!(
            rules
                .judge_locally(&call("Read", serde_json::json!({})))
                .unwrap()
                .is_err()
        );
    }

    #[cfg(feature = "cedar")]
    #[test]
    fn cedar_refuses_text_that_does_not_parse() {
        assert!(compiled(InterceptorBuiltin::Cedar, serde_json::json!({ "policies": "forbid (" })).is_err());
    }
}

#[cfg(all(test, not(feature = "cedar")))]
mod without_cedar {
    use super::*;

    #[test]
    fn a_build_without_cedar_refuses_the_cedar_builtin() {
        let refused = Interceptor::compile(
            &InterceptorName::new("rules"),
            &InterceptorBinding {
                on: InterceptorSteps {
                    prompt: false,
                    tool_call: true,
                },
                builtin: Some(InterceptorBuiltin::Cedar),
                settings: serde_json::json!({ "policies": "" }),
                hint: None,
            },
        );
        assert!(refused.is_err_and(|error| error.to_string().contains("--features cedar")));
    }
}
