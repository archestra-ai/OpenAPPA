//! The `hitl` authority's channel: MCP elicitation back to the harness.
//!
//! An authority consult happens only while an offer executes, and an
//! offer executes only inside the `execute_remedy_plan` MCP call
//! ([`crate::mcp`]). That call is still open when the engine asks for
//! the ruling, so the runtime can ask the harness's own user through
//! the client request already in flight. This is the one window MCP
//! allows: from protocol version 2026-07-28 a server-to-client request
//! MUST be issued while handling a client request, and the association
//! is task-local — it does not survive a `tokio::spawn`. That rule is
//! MCP's own SEP-2260, not an APPA rule id.
//!
//! What crosses is the same consult every other authority receives:
//! the authority's declaration and the artifact — the exact tool, the
//! canonical arguments the engine would dispatch, and the requirements
//! the ruling would cover. The model's account of what it is doing never
//! reaches the reviewer, and the reviewer's answer never passes back
//! through the model.
//!
//! The three MCP actions map onto three APPA outcomes that already
//! differ: accept is an approval, decline is a denial that ends every
//! offer naming this authority for this call, and cancel —
//! like a timeout, a client without the capability, or any transport
//! failure — is no answer at all, which grants nothing and leaves the
//! offer standing.
//!
//! The request carries an empty schema on purpose. The verdict is the
//! elicitation action itself, so a form field asking the same question
//! again would be a second place to disagree with the first. The whole
//! staged review travels in the message; nothing comes back but the
//! action.
//!
//! A review that ends without an answer is withdrawn from the
//! reviewer's screen, whether its call was cancelled or its window
//! closed. Otherwise the dialog outlives the call that raised it and
//! answering it does nothing.
//!
//! Scope: form mode only — the harnesses this serves declare no URL
//! mode — and no reviewer identity, which APPA does not model.
//! A ruling here proves that the harness's elicitation answered. A
//! deployment that configures Claude Code's `Elicitation` or
//! `ElicitationResult` hook to answer has replaced the person
//! deliberately, which this runtime cannot detect.

use std::time::Duration;

const WITHDRAW_TIMEOUT: Duration = Duration::from_secs(5);

use rmcp::model::{
    ClientResult, ElicitRequest, ElicitRequestParams, ElicitationAction, ElicitationSchema, ServerRequest,
};
use rmcp::service::{ElicitationMode, PeerRequestOptions, RequestContext, RoleServer, ServiceError};

use crate::consult::{AudienceRequirement, AuthorityArtifact, AuthorityDeclaration, Requirement};
use crate::external::{ConsultOutcome, NoAnswerReason};

/// The open `execute_remedy_plan` request, and the one way back to the
/// person while it runs. Borrowed for the length of that call and never
/// stored: the peer is usable only from the handler task that owns the
/// request, and MCP opens no second window once the call returns.
pub struct Elicitation {
    request: RequestContext<RoleServer>,
    timeout: Duration,
}

enum Ending {
    Answered(Result<ClientResult, ServiceError>),
    Cancelled,
    TimedOut,
}

impl Elicitation {
    pub fn new(request: RequestContext<RoleServer>, timeout: Duration) -> Elicitation {
        Elicitation { request, timeout }
    }

    /// Ask the reviewer to rule on one consult, and return the answer
    /// in the same shape the authority wire uses, so a component
    /// switched between backends yields identical evidence.
    pub async fn ask(
        &self,
        authority: &str,
        declaration: &AuthorityDeclaration,
        artifact: &AuthorityArtifact,
    ) -> ConsultOutcome {
        let peer = &self.request.peer;
        if !peer.supported_elicitation_modes().contains(&ElicitationMode::Form) {
            tracing::warn!(
                client = ?peer.peer_info().map(|info| info.client_info.clone()),
                "this client declares no elicitation capability: no human ruling is available here",
            );
            return ConsultOutcome::NoAnswer(NoAnswerReason::Unreachable);
        }
        let request = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: review_text(authority, declaration, artifact),
            // No fields: the action is the answer.
            requested_schema: ElicitationSchema::builder()
                .build()
                .expect("an empty schema declares no required property"),
        };
        let mut handle = match peer
            .send_cancellable_request(
                ServerRequest::ElicitRequest(ElicitRequest::new(request)),
                PeerRequestOptions::no_options(),
            )
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                tracing::warn!(%error, "the review could not be presented: no human ruling here");
                return ConsultOutcome::NoAnswer(NoAnswerReason::Transport);
            }
        };
        let ending = {
            let answer = &mut handle.rx;
            tokio::select! {
                biased;
                () = self.request.ct.cancelled() => Ending::Cancelled,
                () = tokio::time::sleep(self.timeout) => Ending::TimedOut,
                received = answer => match received {
                    Ok(answered) => Ending::Answered(answered),
                    Err(_) => Ending::Answered(Err(ServiceError::TransportClosed)),
                },
            }
        };
        let answered = match ending {
            Ending::Answered(answered) => answered,
            ended => {
                let (reason, outcome) = match ended {
                    Ending::Cancelled => ("the call that asked for this ruling ended", NoAnswerReason::Unreachable),
                    _ => ("the review window closed", NoAnswerReason::Timeout),
                };
                tracing::debug!(reason, "withdrawing the review");
                let withdrawn = tokio::time::timeout(WITHDRAW_TIMEOUT, handle.cancel(Some(reason.to_string())));
                match withdrawn.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::debug!(%error, "the review could not be withdrawn"),
                    Err(_) => tracing::debug!("withdrawing the review did not complete"),
                }
                return ConsultOutcome::NoAnswer(outcome);
            }
        };
        let answered = answered.and_then(|result| match result {
            ClientResult::ElicitResult(result) => Ok(result),
            _ => Err(ServiceError::UnexpectedResponse),
        });
        match answered {
            Ok(result) => match result.action {
                ElicitationAction::Accept => {
                    tracing::debug!("the reviewer approved");
                    ConsultOutcome::Answer(serde_json::json!({ "ruling": "approve" }))
                }
                ElicitationAction::Decline => {
                    tracing::debug!("the reviewer refused");
                    ConsultOutcome::Answer(serde_json::json!({ "ruling": "deny" }))
                }
                ElicitationAction::Cancel => {
                    tracing::debug!("the reviewer dismissed the review");
                    ConsultOutcome::NoAnswer(NoAnswerReason::Dismissed)
                }
                action => {
                    tracing::warn!(?action, "an unreadable elicitation action is no answer");
                    ConsultOutcome::NoAnswer(NoAnswerReason::Malformed)
                }
            },
            Err(error) => {
                tracing::warn!(%error, "the elicitation produced no answer: no human ruling in this session");
                ConsultOutcome::NoAnswer(NoAnswerReason::Transport)
            }
        }
    }
}

/// The review as the person reads it: a pure rendering of the consult, nothing from the
/// trajectory beside it.
fn review_text(authority: &str, declaration: &AuthorityDeclaration, artifact: &AuthorityArtifact) -> String {
    let arguments =
        serde_json::to_string_pretty(&artifact.arguments).unwrap_or_else(|_| artifact.arguments.to_string());
    let requirements = match artifact.requirements.as_slice() {
        [] => "  (none)".to_string(),
        requirements => requirements
            .iter()
            .map(|requirement| format!("  - {}", requirement_text(requirement)))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let hint = match &declaration.hint {
        Some(hint) => format!("{hint}\n"),
        None => String::new(),
    };
    format!(
        "APPA asks you to rule as the authority \"{authority}\".\n\
         {hint}\
         \n\
         Tool: {tool}\n\
         Arguments:\n{arguments}\n\
         \n\
         What this ruling would cover:\n{requirements}\n\
         \n\
         Accept only if this exact call, with these exact arguments, \
         may run. Decline refuses it. Cancel answers nothing and \
         leaves the call blocked. The agent's own description of what \
         it is doing is not shown here on purpose.",
        tool = artifact.tool,
    )
}

fn requirement_text(requirement: &Requirement) -> String {
    match requirement {
        Requirement::Trust { required } => format!("trust must reach {required}"),
        Requirement::Audience {
            required: AudienceRequirement::Public,
        } => "the readers must be the public audience".to_string(),
        Requirement::Audience {
            required: AudienceRequirement::Readers(count),
        } => format!("the readers must include {count} required recipient(s)"),
        Requirement::Effect { excludes } => format!("no prior {excludes} effect may have happened"),
        Requirement::Attention { mark } => format!("attention: {mark}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_review_never_leaves_the_handler_task() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for module in ["elicit.rs", "mcp.rs"] {
            let text = std::fs::read_to_string(src.join(module)).expect("the module is readable");
            let spawns = text
                .split("#[cfg(test)]")
                .next()
                .expect("split yields the head")
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains("spawn"));
            assert!(
                !spawns,
                "{module} must not spawn: the elicitation would lose its originating request",
            );
        }
    }
}
