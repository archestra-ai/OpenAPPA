//! The tools the agent advertises, and the one way it runs them.

use std::time::Duration;

use appa_runtime_api::{OutcomeBody, ProposedCall, ToolOutcome};

use crate::http::{HttpClient, read_body_capped};
use crate::wire::WireTool;

/// Runtime control tool for executing remedies.
pub(crate) const CONTROL_TOOL: &str = "execute_remedy_plan";

/// Host tools combined with the runtime control tool.
#[derive(Clone, Debug)]
pub struct ToolCatalogue {
    tools: Vec<WireTool>,
}

impl ToolCatalogue {
    /// Builds the catalogue with the control tool appended.
    pub fn new(tools: Vec<WireTool>) -> Self {
        let mut tools = tools;
        tools.push(control_tool_schema());
        ToolCatalogue { tools }
    }

    /// Builds the catalogue excluding a tool that cannot run in the current frame.
    pub(crate) fn advertised_without(&self, excluded: Option<&str>) -> Vec<WireTool> {
        self.tools
            .iter()
            .filter(|tool| excluded != Some(tool.function.name.as_str()))
            .cloned()
            .collect()
    }
}

fn control_tool_schema() -> WireTool {
    WireTool::new(
        CONTROL_TOOL,
        "Execute one remedy plan by the offer id that blocking feedback surfaced. The id must be \
         quoted exactly. Accepting a narrowing permanently restricts this trajectory, so run any \
         later work that needs its current label before you accept. A plan that declares a \
         subagent's return takes `label`, the lowest label this trajectory accepts from the \
         return; a plan that attests it also takes `return_schema`. After execution succeeds, \
         re-call the original tool or receive the admitted output.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "offer_id": {
                    "type": "string",
                    "description": "The offer id exactly as the feedback surfaced it.",
                },
                "label": {
                    "type": "object",
                    "description": "For a return declaration: the lowest label accepted from the subagent's \
                                    return, in the policy's delta spelling. An omitted dimension keeps this \
                                    trajectory's current value.",
                    "properties": {
                        "trust": { "type": "string" },
                        "audience": { "type": "array", "items": { "type": "string" } }
                    },
                    "additionalProperties": false,
                },
                "return_schema": {
                    "type": "object",
                    "description": "For an attested return: the JSON schema the subagent's return must match.",
                }
            },
            "required": ["offer_id"],
            "additionalProperties": false,
        }),
    )
}

/// The host's tool listener. One loopback address serves every tool;
/// the body names which.
#[derive(Clone, Debug)]
pub struct ToolShim {
    url: String,
    timeout: Duration,
    body_cap_bytes: usize,
    client: HttpClient,
}

/// The largest tool body the agent carries into an outcome. Over it,
/// the call still happened and its effects still committed — the body
/// is simply not carried (`Q14`).
pub const DEFAULT_TOOL_BODY_CAP_BYTES: usize = 256 * 1024;
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

impl ToolShim {
    pub fn new(url: impl Into<String>) -> Self {
        ToolShim {
            url: url.into(),
            timeout: DEFAULT_TOOL_TIMEOUT,
            body_cap_bytes: DEFAULT_TOOL_BODY_CAP_BYTES,
            client: HttpClient::loopback(),
        }
    }

    /// Run one released call and classify what was observed. The
    /// mapping is total over the shapes this transport can produce,
    /// and it assumes least where it cannot tell: a call that may have
    /// reached the host is `Indeterminate`, never a failure.
    pub(crate) async fn run(&self, call: &ProposedCall) -> ToolOutcome {
        let response = match self
            .client
            .inner()
            .post(&self.url)
            .timeout(self.timeout)
            .header("content-type", "application/json")
            .body(body_of(call))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return ToolOutcome::Indeterminate,
        };
        let status = response.status();
        if !status.is_success() {
            return ToolOutcome::Failure {
                message: format!("the tool endpoint answered {}", status.as_u16()),
            };
        }
        let mut response = response;
        match read_body_capped(&mut response, self.body_cap_bytes).await {
            Ok(body) if body.len() <= self.body_cap_bytes => ToolOutcome::Success {
                body: OutcomeBody::Available(String::from_utf8_lossy(&body).into_owned()),
            },
            Ok(_) => ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            },
            Err(_) => ToolOutcome::Indeterminate,
        }
    }
}

fn body_of(call: &ProposedCall) -> Vec<u8> {
    let tool = serde_json::to_string(&call.tool).expect("a Rust string always serializes");
    format!(r#"{{"tool":{tool},"arguments":{}}}"#, call.arguments.get()).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool: &str, arguments: &str) -> ProposedCall {
        ProposedCall {
            tool: tool.to_string(),
            arguments: serde_json::value::RawValue::from_string(arguments.to_string()).expect("valid JSON"),
            cwd: None,
        }
    }

    #[test]
    fn the_dispatch_body_carries_the_arguments_verbatim() {
        assert_eq!(
            String::from_utf8(body_of(&call("pay", r#"{"amount":1,"amount":2}"#))).expect("utf8"),
            r#"{"tool":"pay","arguments":{"amount":1,"amount":2}}"#,
        );
    }

    #[test]
    fn a_tool_name_needing_escapes_stays_valid_json() {
        let body = String::from_utf8(body_of(&call(r#"we"ird"#, "{}"))).expect("utf8");
        assert_eq!(body, r#"{"tool":"we\"ird","arguments":{}}"#);
        serde_json::from_str::<serde_json::Value>(&body).expect("the body is JSON whatever the name spells");
    }

    #[test]
    fn the_catalogue_always_carries_the_control_tool() {
        let catalogue = ToolCatalogue::new(vec![WireTool::new("read_hr", "read", serde_json::json!({}))]);
        let names: Vec<String> = catalogue
            .advertised_without(None)
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(names, vec!["read_hr".to_string(), CONTROL_TOOL.to_string()]);
    }

    #[test]
    fn one_unavailable_host_tool_can_be_hidden_without_hiding_control() {
        let catalogue = ToolCatalogue::new(vec![
            WireTool::new("fork", "spawn", serde_json::json!({})),
            WireTool::new("read_hr", "read", serde_json::json!({})),
        ]);
        let names: Vec<String> = catalogue
            .advertised_without(Some("fork"))
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(names, vec!["read_hr".to_string(), CONTROL_TOOL.to_string()]);
    }
}
