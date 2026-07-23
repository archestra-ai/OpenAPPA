//! RP1 north-admission profile: what a harness may send, and the one thing the runtime takes from it.

use thiserror::Error;

use crate::wire::{ChatCompletionRequest, WireRole};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserTurn(String);

impl UserTurn {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Why a north request was refused (each maps to a 400). The messages are operator-facing, not model
/// context.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("the request must carry exactly one message (the new user turn), found {found}")]
    NotExactlyOneMessage { found: usize },
    #[error("the only admissible message role is `user`; `{role}` is server-owned or forged history")]
    ForbiddenRole { role: String },
    #[error("the user turn must carry text content")]
    EmptyUserTurn,
    #[error("a `user` message must not carry tool calls")]
    UserCarriesToolCalls,
    #[error("tools are server-owned and pinned at session creation; the client may not send them")]
    ClientSuppliedTools,
    #[error("streaming is not supported")]
    StreamingRequested,
}

/// Validate a north request under the strict profile and extract the new user turn. Fails closed:
/// any deviation is an error, never a silent normalization.
pub fn admit_north_request(request: &ChatCompletionRequest) -> Result<UserTurn, AdmissionError> {
    if request.tools.is_some() {
        return Err(AdmissionError::ClientSuppliedTools);
    }
    if request.stream == Some(true) {
        return Err(AdmissionError::StreamingRequested);
    }
    let [message] = request.messages.as_slice() else {
        return Err(AdmissionError::NotExactlyOneMessage {
            found: request.messages.len(),
        });
    };
    // An unknown role is as forbidden as a known non-user one — fail closed.
    match WireRole::parse(&message.role) {
        Some(WireRole::User) => {}
        _ => {
            return Err(AdmissionError::ForbiddenRole {
                role: message.role.clone(),
            });
        }
    }
    if message.tool_calls.is_some() {
        return Err(AdmissionError::UserCarriesToolCalls);
    }
    match &message.content {
        Some(text) if !text.is_empty() => Ok(UserTurn(text.clone())),
        _ => Err(AdmissionError::EmptyUserTurn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{WireFunctionCall, WireMessage, WireTool, WireToolCall, WireToolSchema};

    fn request(messages: Vec<WireMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: String::new(),
            messages,
            tools: None,
            stream: None,
        }
    }

    #[test]
    fn accepts_a_single_user_turn() {
        let turn = admit_north_request(&request(vec![WireMessage::user("investigate the pod")])).unwrap();
        assert_eq!(turn.as_str(), "investigate the pod");
    }

    #[test]
    fn rejects_client_system_and_developer_preamble() {
        for role in ["system", "developer"] {
            let mut msg = WireMessage::user("hi");
            msg.role = role.to_string();
            assert!(matches!(
                admit_north_request(&request(vec![msg])),
                Err(AdmissionError::ForbiddenRole { .. })
            ));
        }
    }

    #[test]
    fn rejects_inbound_assistant_and_tool_history() {
        let assistant = request(vec![
            WireMessage::assistant("earlier answer"),
            WireMessage::user("next"),
        ]);
        assert!(matches!(
            admit_north_request(&assistant),
            Err(AdmissionError::NotExactlyOneMessage { found: 2 })
        ));
        let tool = request(vec![WireMessage::tool_result("call_1", "leaked tool output")]);
        assert!(matches!(
            admit_north_request(&tool),
            Err(AdmissionError::ForbiddenRole { .. })
        ));
    }

    #[test]
    fn rejects_multiple_user_turns() {
        let two = request(vec![WireMessage::user("a"), WireMessage::user("b")]);
        assert!(matches!(
            admit_north_request(&two),
            Err(AdmissionError::NotExactlyOneMessage { found: 2 })
        ));
    }

    #[test]
    fn rejects_client_supplied_tools() {
        let mut req = request(vec![WireMessage::user("hi")]);
        req.tools = Some(vec![WireTool {
            kind: "function".to_string(),
            function: WireToolSchema {
                name: "exfiltrate".to_string(),
                description: None,
                parameters: None,
            },
        }]);
        assert_eq!(admit_north_request(&req), Err(AdmissionError::ClientSuppliedTools));
    }

    #[test]
    fn rejects_streaming_and_empty_and_toolcall_user() {
        let mut streaming = request(vec![WireMessage::user("hi")]);
        streaming.stream = Some(true);
        assert_eq!(admit_north_request(&streaming), Err(AdmissionError::StreamingRequested));

        let mut empty = WireMessage::user("");
        empty.content = Some(String::new());
        assert_eq!(
            admit_north_request(&request(vec![empty])),
            Err(AdmissionError::EmptyUserTurn)
        );

        let mut with_calls = WireMessage::user("hi");
        with_calls.tool_calls = Some(vec![WireToolCall {
            id: "c1".to_string(),
            kind: "function".to_string(),
            function: WireFunctionCall {
                name: "f".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        assert_eq!(
            admit_north_request(&request(vec![with_calls])),
            Err(AdmissionError::UserCarriesToolCalls)
        );
    }
}
