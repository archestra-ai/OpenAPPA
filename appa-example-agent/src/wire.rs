//! The OpenAI-compatible `/v1/chat/completions` types the agent speaks
//! upstream, non-streaming.
//!
//! Deliberately minimal — string content, function tool calls — enough
//! to drive a loop, not a mirror of any vendor's API. Nothing a
//! provider returns is trusted: the transcript these types build is
//! the agent's own record, and every flow in it was gated by the
//! runtime first.

use serde::{Deserialize, Serialize};

/// A chat-completions request, built from the agent's transcript and
/// its own tool catalogue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl WireMessage {
    pub fn user(content: impl Into<String>) -> Self {
        WireMessage::of("user", Some(content.into()))
    }

    pub fn system(content: impl Into<String>) -> Self {
        WireMessage::of("system", Some(content.into()))
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        WireMessage::of("assistant", Some(content.into()))
    }

    pub fn assistant_tool_calls(content: Option<String>, calls: Vec<WireToolCall>) -> Self {
        WireMessage {
            tool_calls: Some(calls),
            ..WireMessage::of("assistant", content)
        }
    }

    /// The result of one proposed call, answering it by its wire id.
    /// This is where every runtime decision lands: an allowed call's
    /// admitted output, a denial's feedback, a remedy's answer.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        WireMessage {
            tool_call_id: Some(tool_call_id.into()),
            ..WireMessage::of("tool", Some(content.into()))
        }
    }

    fn of(role: &str, content: Option<String>) -> Self {
        WireMessage {
            role: role.to_string(),
            content,
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

/// A tool call the model proposed. `arguments` is a JSON **string**,
/// the OpenAI convention — and the artifact the engine judges, so it
/// travels to the runtime as the provider spelled it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: WireFunctionCall,
}

fn function_type() -> String {
    "function".to_string()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTool {
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: WireToolSchema,
}

impl WireTool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        WireTool {
            kind: function_type(),
            function: WireToolSchema {
                name: name.into(),
                description: Some(description.into()),
                parameters: Some(parameters),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<WireChoice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct WireChoice {
    pub message: WireMessage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_calls_arguments_stay_the_provider_s_own_text() {
        let response: ChatCompletionResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","tool_calls":[
                 {"id":"call_1","function":{"name":"pay","arguments":"{\"amount\":1,\"amount\":2}"}}]}}]}"#,
        )
        .expect("the completion parses");
        let call = &response.choices[0].message.tool_calls.as_ref().expect("calls")[0];
        assert_eq!(call.function.arguments, r#"{"amount":1,"amount":2}"#);
        assert_eq!(call.kind, "function", "the type defaults to function");
    }
}
