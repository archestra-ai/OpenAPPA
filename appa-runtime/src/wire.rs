//! The OpenAI-compatible `/v1/chat/completions` wire types (non-streaming), shared by runtime consumers.
//!
//! The same shapes serve both faces: the **north** request a thin harness sends, and the
//! **upstream** request/response the runtime exchanges with the model. They are deliberately minimal
//! — string content only, function tool calls — enough for the confining executor, not a full mirror
//! of the vendor API. North input is untrusted and validated by the runtime admission layer before anything
//! reaches the engine; upstream output is likewise never trusted as history (RP1/CC2).

use serde::{Deserialize, Serialize};

/// A chat-completions request. As a **north** request it is validated under the strict admission
/// profile (RP1); as an **upstream** request the runtime builds it solely from server-held log facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl crate::Completion {
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// One message in a completion exchange. `role` is a free string on the wire and narrowed to a
/// [`WireRole`] during admission.
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
        WireMessage {
            role: "user".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        WireMessage {
            role: "system".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn developer(content: impl Into<String>) -> Self {
        WireMessage {
            role: "developer".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        WireMessage {
            role: "assistant".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(calls: Vec<WireToolCall>) -> Self {
        WireMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        WireMessage {
            role: "tool".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl WireRole {
    pub fn parse(role: &str) -> Option<WireRole> {
        match role {
            "system" => Some(WireRole::System),
            "developer" => Some(WireRole::Developer),
            "user" => Some(WireRole::User),
            "assistant" => Some(WireRole::Assistant),
            "tool" => Some(WireRole::Tool),
            _ => None,
        }
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    #[serde(default = "chat_completion_object")]
    pub object: String,
    pub choices: Vec<WireChoice>,
}

fn chat_completion_object() -> String {
    "chat.completion".to_string()
}

impl ChatCompletionResponse {
    pub fn single(id: impl Into<String>, message: WireMessage, finish_reason: impl Into<String>) -> Self {
        ChatCompletionResponse {
            id: id.into(),
            object: chat_completion_object(),
            choices: vec![WireChoice {
                index: 0,
                message,
                finish_reason: Some(finish_reason.into()),
            }],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireChoice {
    pub index: u32,
    pub message: WireMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_tool_call_completion() {
        let response = ChatCompletionResponse::single(
            "cmpl-1",
            WireMessage::assistant_tool_calls(vec![WireToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireFunctionCall {
                    name: "get_ticket".to_string(),
                    arguments: r#"{"id":7}"#.to_string(),
                },
            }]),
            "tool_calls",
        );
        let json = serde_json::to_string(&response).unwrap();
        let back: ChatCompletionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn parses_a_minimal_north_request() {
        let req: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"hi"}]}"#).unwrap();
        assert_eq!(req.messages, vec![WireMessage::user("hi")]);
        assert!(req.tools.is_none());
    }

    #[test]
    fn tool_call_type_defaults_to_function() {
        let call: WireToolCall =
            serde_json::from_str(r#"{"id":"c1","function":{"name":"f","arguments":"{}"}}"#).unwrap();
        assert_eq!(call.kind, "function");
    }
}
