//! The `hitl` authority's channel: MCP elicitation back to the harness.
//!
//! An authority consult happens only while an offer executes, and an
//! offer executes only inside the `execute_remedy_plan` MCP call
//! ([`crate::mcp`]). How the review reaches the person depends on the
//! protocol version the request speaks.
//!
//! Before 2026-07-28 that call is still open when the engine asks for
//! the ruling, so the runtime asks the harness's own user through the
//! client request already in flight. The association is task-local —
//! it does not survive a `tokio::spawn`.
//!
//! From 2026-07-28 there is no session, so an answer to a request sent
//! inside the open call has nowhere to return. The call instead ends
//! with the review as an input request, before anything executes and
//! without spending the vouch; the client shows it and repeats the call
//! carrying the answer, which that execution spends. A client declares
//! its capabilities on each such request, never once at `initialize`.
//! The returned answer is bound to the review by the retried call's own
//! `offer_id`: an offer names one exact call and the requirements its
//! rulings cover, and an offer that no longer stands is refused. The
//! answer carries no further proof that the person saw the review, as
//! an in-call answer carries none: either way the harness is trusted
//! to have asked.
//! Both rules are MCP's own (SEP-2322, SEP-2567), not APPA rule ids.
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

#[cfg(feature = "daemon")]
use std::time::Duration;

#[cfg(feature = "daemon")]
const WITHDRAW_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(feature = "daemon")]
use rmcp::model::{
    ClientResult, ElicitRequest, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema, InputRequest,
    InputRequiredResult, InputResponses, ProtocolVersion, ServerRequest,
};
#[cfg(feature = "daemon")]
use rmcp::service::{PeerRequestOptions, RequestContext, RoleServer, ServiceError};

use crate::consult::{AudienceRequirement, AuthorityArtifact, AuthorityDeclaration, Requirement};
use crate::external::ConsultOutcome;
#[cfg(feature = "daemon")]
use crate::external::NoAnswerReason;

/// Which channel carries one review back to a person. An enum rather than a
/// struct because a build without the server stack has no channel at all: the
/// type is then uninhabited, every `Option<&Elicitation>` threaded through the
/// engine is `None`, and no signature changes between the two builds.
pub enum Elicitation {
    /// The open `execute_remedy_plan` request, and the one way back to the
    /// person while it runs. Borrowed for the length of that call and never
    /// stored: the peer is usable only from the handler task that owns the
    /// request, and MCP opens no second window once the call returns.
    #[cfg(feature = "daemon")]
    Mcp {
        request: RequestContext<RoleServer>,
        timeout: Duration,
    },
    /// The person's answer, returned with the retried call. From MCP 2026-07-28 a server
    /// asks by ending the call with the review as an input request ([`Elicitation::open`]);
    /// the client shows it and repeats the call carrying this action.
    #[cfg(feature = "daemon")]
    Returned(ElicitationAction),
    /// The retried call carried an answer this runtime cannot read, which is no answer.
    #[cfg(feature = "daemon")]
    Unreadable,
}

/// What one `execute_remedy_plan` request does about its review.
#[cfg(feature = "daemon")]
pub enum Review {
    /// End the call with the review; nothing executes until the answer returns.
    Ask(InputRequiredResult),
    /// Execute, with the channel this request has.
    Proceed(Option<Elicitation>),
}

#[cfg(feature = "daemon")]
enum Ending {
    Answered(Result<ClientResult, ServiceError>),
    Cancelled,
    TimedOut,
}

impl Elicitation {
    /// How this `execute_remedy_plan` request reaches the person. Before MCP 2026-07-28 the
    /// open request is the channel. From that version there is no session to return an
    /// answer to a request still open, so the call first ends with the review as an input
    /// request, and the retried call carries the answer. `pending` is the review this
    /// offer would raise, read only when it may be asked.
    #[cfg(feature = "daemon")]
    pub fn open(
        request: RequestContext<RoleServer>,
        responses: Option<InputResponses>,
        timeout: Duration,
        pending: impl FnOnce() -> Option<String>,
    ) -> Review {
        if !asks_by_round_trip(&request) {
            return Review::Proceed(Some(Elicitation::Mcp { request, timeout }));
        }
        if let Some(answer) = responses.and_then(|mut responses| responses.remove(REVIEW_KEY)) {
            return Review::Proceed(Some(match serde_json::from_value::<ElicitResult>(answer) {
                Ok(result) => Elicitation::Returned(result.action),
                Err(error) => {
                    tracing::warn!(%error, "the returned review is unreadable");
                    Elicitation::Unreadable
                }
            }));
        }
        let review = if asks_by_form(&request) { pending() } else { None };
        match review {
            Some(review) => Review::Ask(InputRequiredResult::from_input_requests(
                [(REVIEW_KEY.to_string(), InputRequest::Elicitation(review_form(review)))].into(),
            )),
            None => Review::Proceed(None),
        }
    }

    /// No channel is compiled in, so no caller can hold one of these.
    #[cfg(not(feature = "daemon"))]
    pub async fn ask(
        &self,
        _authority: &str,
        _declaration: &AuthorityDeclaration,
        _artifact: &AuthorityArtifact,
    ) -> ConsultOutcome {
        match *self {}
    }

    /// Ask the reviewer to rule on one consult, and return the answer
    /// in the same shape the authority wire uses, so a component
    /// switched between backends yields identical evidence.
    #[cfg(feature = "daemon")]
    pub async fn ask(
        &self,
        authority: &str,
        declaration: &AuthorityDeclaration,
        artifact: &AuthorityArtifact,
    ) -> ConsultOutcome {
        let (request, timeout) = match self {
            Elicitation::Mcp { request, timeout } => (request, timeout),
            Elicitation::Returned(action) => return ruled(action.clone()),
            Elicitation::Unreadable => return ConsultOutcome::NoAnswer(NoAnswerReason::Malformed),
        };
        let peer = &request.peer;
        if !asks_by_form(request) {
            tracing::warn!(
                client = ?request.client_info(),
                "this client declares no elicitation capability: no human ruling is available here",
            );
            return ConsultOutcome::NoAnswer(NoAnswerReason::Unreachable);
        }
        let mut handle = match peer
            .send_cancellable_request(
                ServerRequest::ElicitRequest(review_form(review_text(authority, declaration, artifact))),
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
                () = request.ct.cancelled() => Ending::Cancelled,
                () = tokio::time::sleep(*timeout) => Ending::TimedOut,
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
                    // Not `Unreachable`: that reason tells the model no retry reaches the
                    // person, and a fresh call opens a fresh request.
                    Ending::Cancelled => ("the call that asked for this ruling ended", NoAnswerReason::Transport),
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
            Ok(result) => ruled(result.action),
            Err(error) => {
                tracing::warn!(%error, "the elicitation produced no answer: no human ruling in this session");
                ConsultOutcome::NoAnswer(NoAnswerReason::Transport)
            }
        }
    }
}

/// The name of the one input request a review round trip carries.
#[cfg(feature = "daemon")]
const REVIEW_KEY: &str = "appa-review";

/// The person's action as the authority wire's answer.
#[cfg(feature = "daemon")]
fn ruled(action: ElicitationAction) -> ConsultOutcome {
    match action {
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
    }
}

#[cfg(feature = "daemon")]
fn review_form(message: String) -> ElicitRequest {
    ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        // No fields: the action is the answer.
        requested_schema: ElicitationSchema::builder()
            .build()
            .expect("an empty schema declares no required property"),
    })
}

#[cfg(feature = "daemon")]
fn asks_by_round_trip(request: &RequestContext<RoleServer>) -> bool {
    request
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

/// Whether the client behind this request shows a form elicitation. The capabilities are
/// the request's own: from MCP 2026-07-28 a client declares them on each request and
/// there is no `initialize` to read them from, so the peer's session info says nothing.
#[cfg(feature = "daemon")]
fn asks_by_form(request: &RequestContext<RoleServer>) -> bool {
    request
        .client_capabilities()
        .and_then(|capabilities| capabilities.elicitation)
        // A capability naming neither mode predates the split and means form.
        .is_some_and(|elicitation| elicitation.form.is_some() || elicitation.url.is_none())
}

/// The review as the person reads it: a pure rendering of the consult, nothing from the
/// trajectory beside it.
pub(crate) fn review_text(authority: &str, declaration: &AuthorityDeclaration, artifact: &AuthorityArtifact) -> String {
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
