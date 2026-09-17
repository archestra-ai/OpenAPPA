//! The tools the agent advertises, and the one way it runs them.

use std::time::Duration;

use appa_runtime_api::{ADVERTISED_CONTROL_TOOL, OutcomeBody, ProposedCall, ToolOutcome, is_reserved_tool_name};
use thiserror::Error;

use crate::http::{HttpClient, read_body_capped};
use crate::wire::WireTool;

/// Why a host's tools cannot become a catalogue, and why a spawn tool cannot be named.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CatalogueError {
    /// The control tool's names belong to the runtime. A host tool spelled either way
    /// would be unreachable — every call to it reaches the control tool instead — so the
    /// catalogue is refused rather than rerouting the host's tool.
    #[error("{0} is a reserved control tool name; rename the host tool")]
    ReservedToolName(String),
}

/// Host tools combined with the runtime control tool. Every catalogue that exists
/// advertises the control tool exactly once, under the name the runtime routes.
#[derive(Clone, Debug)]
pub struct ToolCatalogue {
    tools: Vec<WireTool>,
}

impl ToolCatalogue {
    /// Builds the catalogue with the control tool appended, or refuses a host tool
    /// that claims either of the control tool's names.
    pub fn new(tools: Vec<WireTool>) -> Result<Self, CatalogueError> {
        let mut tools = tools;
        if let Some(reserved) = tools.iter().find(|tool| is_reserved_tool_name(&tool.function.name)) {
            return Err(CatalogueError::ReservedToolName(reserved.function.name.clone()));
        }
        tools.push(control_tool_schema());
        Ok(ToolCatalogue { tools })
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
        ADVERTISED_CONTROL_TOOL,
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
            // Only the endpoint saying the call itself was bad reports a
            // failure. Every other status is the endpoint answering about
            // itself, and it may have run the call before it did: a 500 after
            // the effect committed, reported as a failure, tells the engine
            // nothing was admitted when something was.
            return match status.as_u16() {
                422 => ToolOutcome::Failure {
                    message: "the tool endpoint reported a failed call".to_string(),
                },
                other => {
                    tracing::debug!(status = other, "tool endpoint answered a non-success status");
                    ToolOutcome::Indeterminate
                }
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
    use appa_runtime_api::canonical_tool_name;

    fn call(tool: &str, arguments: &str) -> ProposedCall {
        ProposedCall {
            tool: tool.to_string(),
            arguments: serde_json::value::RawValue::from_string(arguments.to_string()).expect("valid JSON"),
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
        let catalogue = ToolCatalogue::new(vec![WireTool::new("read_hr", "read", serde_json::json!({}))])
            .expect("no host tool claims the reserved name");
        let names: Vec<String> = catalogue
            .advertised_without(None)
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(names, vec!["read_hr".to_string(), ADVERTISED_CONTROL_TOOL.to_string()]);
    }

    #[test]
    fn one_unavailable_host_tool_can_be_hidden_without_hiding_control() {
        let catalogue = ToolCatalogue::new(vec![
            WireTool::new("fork", "spawn", serde_json::json!({})),
            WireTool::new("read_hr", "read", serde_json::json!({})),
        ])
        .expect("no host tool claims the reserved name");
        let names: Vec<String> = catalogue
            .advertised_without(Some("fork"))
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert_eq!(names, vec!["read_hr".to_string(), ADVERTISED_CONTROL_TOOL.to_string()]);
    }

    /// A host tool under either of the control tool's names has no catalogue to be
    /// advertised in: every call to those names reaches the control tool — the alias is
    /// translated into the canonical id, and the canonical id passes through — so the host
    /// tool would be unreachable and a spawn under it unrecognized.
    #[test]
    fn a_host_tool_cannot_claim_either_control_tool_name() {
        for reserved in [ADVERTISED_CONTROL_TOOL, appa_runtime_api::CONTROL_TOOL] {
            assert_eq!(
                ToolCatalogue::new(vec![
                    WireTool::new("read_hr", "read", serde_json::json!({})),
                    WireTool::new(reserved, "the host's own", serde_json::json!({})),
                ])
                .err(),
                Some(CatalogueError::ReservedToolName(reserved.to_string())),
            );
            assert_eq!(
                canonical_tool_name(reserved),
                appa_runtime_api::CONTROL_TOOL,
                "both spellings reach the control tool"
            );
        }
        assert!(
            ToolCatalogue::new(vec![WireTool::new(
                "mcp/evil/execute_remedy_plan",
                "a lookalike on another server",
                serde_json::json!({}),
            )])
            .is_ok(),
            "only the runtime's own two names are reserved"
        );
    }
}
