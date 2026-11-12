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
//! An accepted form must contain an explicit boolean `approval`: true
//! approves, and false denies. Decline is also a denial that ends every
//! offer naming this authority for this call, and cancel —
//! like a timeout, a client without the capability, or any transport
//! failure — is no answer at all, which grants nothing and leaves the
//! offer standing.
//!
//! The required boolean has no default. Accept alone is not approval:
//! missing or malformed content grants nothing and leaves the offer
//! standing. The whole staged review travels in the message.
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
    BooleanSchema, ClientResult, ElicitRequest, ElicitRequestParams, ElicitResult, ElicitationAction,
    ElicitationSchema, InputRequest, InputRequiredResult, InputResponses, PrimitiveSchemaDefinition, ProtocolVersion,
    ServerRequest,
};
#[cfg(feature = "daemon")]
use rmcp::service::{PeerRequestOptions, RequestContext, RoleServer, ServiceError};

#[cfg(feature = "daemon")]
use crate::consult::AuthorityAnswer;
use crate::consult::{AudienceRequirement, AuthorityArtifact, AuthorityDeclaration, Requirement, ReviewedNarrowing};
use crate::engine::{recipient_list, terminal_safe};
use crate::external::ConsultOutcome;
#[cfg(feature = "daemon")]
use crate::external::NoAnswerReason;
#[cfg(feature = "daemon")]
use appa_runtime_api::Ruling;
use appa_runtime_api::{ReviewNarrowing, ReviewRequirement, ReviewScopeItem};

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
    /// the client shows it and repeats the call carrying this result.
    #[cfg(feature = "daemon")]
    Returned(ElicitResult),
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
                Ok(result) => Elicitation::Returned(result),
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
            Elicitation::Returned(result) => return ruled(result),
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
            Ok(result) => ruled(&result),
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

/// Validate the person's form answer before producing an authority ruling.
#[cfg(feature = "daemon")]
fn ruled(result: &ElicitResult) -> ConsultOutcome {
    match result.action {
        ElicitationAction::Accept => match result
            .content
            .as_ref()
            .and_then(|content| content.get("approval"))
            .and_then(serde_json::Value::as_bool)
        {
            Some(true) => {
                tracing::debug!("the reviewer approved");
                ConsultOutcome::Answer(AuthorityAnswer::to_wire(Ruling::Approve))
            }
            Some(false) => {
                tracing::debug!("the reviewer refused");
                ConsultOutcome::Answer(AuthorityAnswer::to_wire(Ruling::Deny))
            }
            None => {
                tracing::warn!("the accepted review has no boolean approval: no human ruling");
                ConsultOutcome::NoAnswer(NoAnswerReason::Malformed)
            }
        },
        ElicitationAction::Decline => {
            tracing::debug!("the reviewer refused");
            ConsultOutcome::Answer(AuthorityAnswer::to_wire(Ruling::Deny))
        }
        ElicitationAction::Cancel => {
            tracing::debug!("the reviewer dismissed the review");
            ConsultOutcome::NoAnswer(NoAnswerReason::Dismissed)
        }
        ref action => {
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
        // A required choice without a default: accepting the dialog alone grants nothing.
        requested_schema: ElicitationSchema::builder()
            .required_property(
                "approval",
                PrimitiveSchemaDefinition::Boolean(BooleanSchema::new().title("Approve this exact call?")),
            )
            .build()
            .expect("the required approval property is declared"),
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

/// APPA's two-row mark, drawn wherever APPA itself speaks in the harness: the status
/// line and the head of a review.
pub(crate) const MARK_TOP: &str = "▄█▄▄▄█▄";
pub(crate) const MARK_BOTTOM: &str = "██▄█▄██";

/// The review as the person reads it: a pure rendering of the consult, nothing from the
/// trajectory beside it.
pub(crate) fn review_text(authority: &str, declaration: &AuthorityDeclaration, artifact: &AuthorityArtifact) -> String {
    let arguments =
        serde_json::to_string_pretty(&artifact.arguments).unwrap_or_else(|_| artifact.arguments.to_string());
    let coverage = review_scope(artifact)
        .iter()
        .map(|item| format!("  - {}", item.text()))
        .collect::<Vec<_>>();
    let coverage = match coverage.as_slice() {
        [] => "  (none)".to_string(),
        bullets => bullets.join("\n"),
    };
    let hint = match &declaration.hint {
        Some(hint) => format!("{hint}\n"),
        None => String::new(),
    };
    format!(
        "{headline}\n\
         APPA asks you to rule as the authority \"{authority}\".\n\
         {hint}\
         \n\
         Tool: {tool}\n\
         Arguments:\n{arguments}\n\
         \n\
         What this ruling would cover:\n{coverage}\n\
         \n\
         Set approval to true only if this exact call, with these exact arguments, \
         may run. Submit false or decline to refuse it. Cancel answers nothing and \
         leaves the call blocked. The agent's own description of what \
         it is doing is not shown here on purpose.",
        headline = headline(artifact),
        tool = artifact.tool,
    )
}

/// What approving this consult accepts. The review's coverage bullets are printed from these
/// items, so a bullet and its item cannot disagree.
pub(crate) fn review_scope(artifact: &AuthorityArtifact) -> Vec<ReviewScopeItem> {
    artifact
        .requirements
        .iter()
        .map(requirement_item)
        .chain(artifact.narrows.iter().flat_map(narrowing_items))
        .collect()
}

/// The widest a headline line's call text runs: a harness counts a wrapped row against
/// the few lines it shows, so a longer line would push the rest of the headline out.
const HEADLINE_WIDTH: usize = 64;

/// The review's first lines, for a harness that shows only a few of them — Claude Code
/// folds the rest behind "+N more lines". APPA's mark asks the question beside the call
/// in brief: a shell command and the hosts it sends to, or the description it carries when
/// it names no host, or else the tool and its arguments on one line. The full review below
/// repeats all of it untrimmed.
fn headline(artifact: &AuthorityArtifact) -> String {
    let indent = " ".repeat(MARK_TOP.chars().count() + 7);
    let text = |key: &str| artifact.arguments.get(key).and_then(serde_json::Value::as_str);
    let hosts: Vec<&str> = artifact
        .arguments
        .get("network_hosts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let sends = (!hosts.is_empty()).then(|| format!("sends to {}", hosts.join(", ")));
    let (call, description) = match text("command") {
        Some(command) => (format!("$ {command}"), sends.as_deref().or(text("description"))),
        None => (format!("{} {}", artifact.tool, artifact.arguments), None),
    };
    let mut lines = format!(
        "{MARK_TOP}  ▀▀█  Approve this call?\n{MARK_BOTTOM}   ▄   {}\n",
        one_line(&call)
    );
    if let Some(description) = description {
        lines.push_str(&format!("{indent}{}\n", one_line(description)));
    }
    lines
}

/// `text` on one row of at most [`HEADLINE_WIDTH`] characters: whitespace runs, line
/// breaks included, become one space, any other character [`terminal_safe`] replaces is
/// replaced, and a cut shows as `…`.
fn one_line(text: &str) -> String {
    let flat = terminal_safe(&text.split_whitespace().collect::<Vec<_>>().join(" "));
    let mut cuts = flat.char_indices().skip(HEADLINE_WIDTH - 1).map(|(at, _)| at);
    match (cuts.next(), cuts.next()) {
        (Some(cut), Some(_)) => format!("{}…", &flat[..cut]),
        _ => flat,
    }
}

fn requirement_text(requirement: &Requirement) -> String {
    match requirement {
        Requirement::Trust { .. } => "unverified input for this step".to_string(),
        Requirement::Audience {
            required: AudienceRequirement::Public,
        } => "public sharing".to_string(),
        Requirement::Audience {
            required: AudienceRequirement::Readers(entries),
        } => format!("sharing with {}", recipient_list(entries)),
        Requirement::Effect { excludes } => {
            format!("an earlier {} action in this conversation", terminal_safe(excludes))
        }
        Requirement::Attention { mark } => format!("attention: {}", terminal_safe(mark)),
    }
}

fn requirement_item(requirement: &Requirement) -> ReviewScopeItem {
    let text = requirement_text(requirement);
    let requirement = match requirement {
        Requirement::Attention { mark } => {
            return ReviewScopeItem::Attention {
                mark: mark.clone(),
                text,
            };
        }
        Requirement::Trust { required } => ReviewRequirement::Trust {
            required: required.clone(),
        },
        Requirement::Audience {
            required: AudienceRequirement::Public,
        } => ReviewRequirement::PublicAudience,
        Requirement::Audience {
            required: AudienceRequirement::Readers(entries),
        } => ReviewRequirement::Audience {
            readers: entries.clone(),
        },
        Requirement::Effect { excludes } => ReviewRequirement::Effect {
            excludes: excludes.clone(),
        },
    };
    ReviewScopeItem::Requirement { requirement, text }
}

/// The most tools one bullet names before it counts the rest.
const NAMED_TOOLS: usize = 5;

enum ToolsJoinedBy {
    And,
    Or,
}

fn tool_list(tools: &[String], joined_by: ToolsJoinedBy) -> String {
    let quoted = |tools: &[String]| {
        tools
            .iter()
            .map(|tool| format!("`{tool}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let word = match joined_by {
        ToolsJoinedBy::And => "and",
        ToolsJoinedBy::Or => "or",
    };
    match (tools.len().checked_sub(NAMED_TOOLS), tools) {
        (Some(rest @ 1..), _) => format!("{} and {rest} more", quoted(&tools[..NAMED_TOOLS])),
        (_, [named @ .., last]) if !named.is_empty() => format!("{} {word} `{last}`", quoted(named)),
        _ => quoted(tools),
    }
}

fn narrowing_items(narrows: &ReviewedNarrowing) -> Vec<ReviewScopeItem> {
    let item = |narrowing, text| ReviewScopeItem::Narrowing { narrowing, text };
    match narrows {
        ReviewedNarrowing::Unjudged => vec![item(
            ReviewNarrowing::Unjudged,
            "after this, some tools may be blocked or need approval in this conversation".to_string(),
        )],
        ReviewedNarrowing::Judged {
            blocked,
            sign_off,
            recipient_checked,
            per_call,
        } => [
            (!blocked.is_empty()).then(|| {
                item(
                    ReviewNarrowing::Blocked { tools: blocked.clone() },
                    format!(
                        "after this, the agent can't use {} in this conversation",
                        tool_list(blocked, ToolsJoinedBy::Or)
                    ),
                )
            }),
            (!sign_off.is_empty()).then(|| {
                item(
                    ReviewNarrowing::SignOff {
                        tools: sign_off.clone(),
                    },
                    format!(
                        "after this, {} will need approval in this conversation",
                        tool_list(sign_off, ToolsJoinedBy::And)
                    ),
                )
            }),
            (!recipient_checked.is_empty()).then(|| {
                item(
                    ReviewNarrowing::RecipientChecked {
                        tools: recipient_checked.clone(),
                    },
                    format!(
                        "after this, {} can only send to people allowed to read this data",
                        tool_list(recipient_checked, ToolsJoinedBy::And)
                    ),
                )
            }),
            per_call.then(|| {
                item(
                    ReviewNarrowing::PerCall,
                    "other tools may also need approval, decided call by call".to_string(),
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(arguments: serde_json::Value) -> AuthorityArtifact {
        AuthorityArtifact {
            tool: "host/claude-code/Bash".to_string(),
            arguments,
            requirements: Vec::new(),
            narrows: None,
        }
    }

    /// A shell command leads with the command and the description it carries, each on
    /// one row, beside APPA's question.
    #[test]
    fn a_shell_command_heads_the_review_with_its_description() {
        let headline = headline(&artifact(serde_json::json!({
            "command": "appa battery install slack\n  grain sentry",
            "description": "Add the batteries",
        })));
        assert_eq!(
            headline,
            "▄█▄▄▄█▄  ▀▀█  Approve this call?\n\
             ██▄█▄██   ▄   $ appa battery install slack grain sentry\n\
             \x20             Add the batteries\n"
        );
    }

    /// A command that names hosts shows where it sends in place of its description: the
    /// review's third row is the last a harness shows before it folds the rest.
    #[test]
    fn a_shell_command_that_names_hosts_heads_the_review_with_them() {
        let headline = headline(&artifact(serde_json::json!({
            "command": "curl -d @notes.txt https://hooks.example",
            "description": "Post the notes",
            "network_hosts": ["hooks.example", "api.example"],
        })));
        assert_eq!(
            headline.lines().nth(2).map(str::trim),
            Some("sends to hooks.example, api.example"),
            "{headline}"
        );
    }

    /// Any other call leads with its tool and arguments on one row, cut to fit.
    #[test]
    fn any_other_call_heads_the_review_with_its_tool_and_arguments_cut_to_fit() {
        let long = "x".repeat(200);
        let headline = headline(&AuthorityArtifact {
            tool: "mcp/slack/send".to_string(),
            ..artifact(serde_json::json!({ "text": long }))
        });
        let call = headline.lines().nth(1).expect("the call row");
        assert!(
            call.starts_with("██▄█▄██   ▄   mcp/slack/send {\"text\":\"xxx"),
            "{call}"
        );
        assert!(call.ends_with('…'), "a cut shows: {call}");
        assert_eq!(call.chars().count(), "██▄█▄██   ▄   ".chars().count() + HEADLINE_WIDTH);
        assert_eq!(headline.lines().count(), 2, "no description row: {headline}");
    }

    /// A text of exactly the row's width fits and is not cut; one character more is.
    #[test]
    fn a_text_that_fills_the_row_is_not_cut() {
        let full = "é".repeat(HEADLINE_WIDTH);
        assert_eq!(one_line(&full), full);
        let over = "é".repeat(HEADLINE_WIDTH + 1);
        assert_eq!(one_line(&over), format!("{}…", "é".repeat(HEADLINE_WIDTH - 1)));
    }

    /// A command cannot restyle or reorder its headline row: escape, backspace, delete and
    /// bidi controls in it are replaced.
    #[test]
    fn the_headline_replaces_terminal_controls_in_the_call() {
        let headline = headline(&artifact(serde_json::json!({
            "command": "rm -rf ~\u{1b}[2K\u{8}\u{7f}\u{202E}ls",
            "description": "list\u{1b}]0;x\u{7}",
        })));
        assert_eq!(
            headline,
            "▄█▄▄▄█▄  ▀▀█  Approve this call?\n\
             ██▄█▄██   ▄   $ rm -rf ~\u{FFFD}[2K\u{FFFD}\u{FFFD}\u{FFFD}ls\n\
             \x20             list\u{FFFD}]0;x\u{FFFD}\n"
        );
    }

    /// The review names the required recipients, each quoted on the one line of its
    /// requirement: a reader from the call's arguments cannot add a line of its own.
    #[test]
    fn the_review_names_the_required_recipients_on_one_line() {
        let coverage = |entries: &[&str]| {
            coverage(&AuthorityArtifact {
                requirements: vec![Requirement::Audience {
                    required: AudienceRequirement::Readers(entries.iter().map(|entry| entry.to_string()).collect()),
                }],
                ..artifact(serde_json::json!({}))
            })
        };
        assert_eq!(coverage(&["internal"]), ["  - sharing with \"internal\""]);
        assert_eq!(
            coverage(&["@finance", "alice", "bob"]),
            ["  - sharing with \"@finance\", \"alice\", \"bob\""]
        );
        assert_eq!(
            coverage(&["a", "b", "c", "d", "e", "f"]),
            ["  - sharing with \"a\", \"b\", \"c\", \"d\", \"e\" and 1 more"]
        );
        assert_eq!(
            coverage(&["eve\n  - unverified input", "mallory\r\n\u{2028}\u{1b}[2K"]),
            ["  - sharing with \"eve\\n  - unverified input\", \"mallory\\r\\n\u{FFFD}\\u001b[2K\""]
        );
        assert_eq!(coverage(&[]), ["  - sharing with nobody"]);
    }

    /// The lines of the coverage section, up to the blank line that ends it.
    fn coverage(artifact: &AuthorityArtifact) -> Vec<String> {
        let declaration = AuthorityDeclaration {
            hint: None,
            permits: crate::consult::DeclaredPermits {
                trust_below: None,
                audience_missing: None,
                effects_containing: vec![],
                attention: vec![],
            },
        };
        review_text("reviewer", &declaration, artifact)
            .lines()
            .skip_while(|line| *line != "What this ruling would cover:")
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn judged(
        blocked: &[&str],
        sign_off: &[&str],
        recipient_checked: &[&str],
        per_call: bool,
    ) -> Option<ReviewedNarrowing> {
        Some(ReviewedNarrowing::Judged {
            blocked: tools(blocked),
            sign_off: tools(sign_off),
            recipient_checked: tools(recipient_checked),
            per_call,
        })
    }

    fn narrowing_review(narrows: Option<ReviewedNarrowing>) -> AuthorityArtifact {
        AuthorityArtifact {
            requirements: vec![Requirement::Attention {
                mark: "signoff".to_string(),
            }],
            narrows,
            ..artifact(serde_json::json!({}))
        }
    }

    #[test]
    fn each_impact_of_a_narrowing_is_one_bullet_inside_the_coverage() {
        assert_eq!(
            coverage(&narrowing_review(judged(
                &["create_issue", "send_mail"],
                &["deploy"],
                &["send_mail"],
                true,
            ))),
            [
                "  - attention: signoff",
                "  - after this, the agent can't use `create_issue` or `send_mail` in this conversation",
                "  - after this, `deploy` will need approval in this conversation",
                "  - after this, `send_mail` can only send to people allowed to read this data",
                "  - other tools may also need approval, decided call by call",
            ]
        );
    }

    #[test]
    fn several_tools_read_as_a_list_in_each_bullet() {
        assert_eq!(
            coverage(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(judged(&["a", "b", "c"], &["d", "e"], &["f", "g", "h"], false))
            }),
            [
                "  - after this, the agent can't use `a`, `b` or `c` in this conversation",
                "  - after this, `d` and `e` will need approval in this conversation",
                "  - after this, `f`, `g` and `h` can only send to people allowed to read this data",
            ]
        );
    }

    #[test]
    fn a_bullet_names_five_tools_and_counts_the_rest() {
        let five = ["a", "b", "c", "d", "e"];
        let seven = ["a", "b", "c", "d", "e", "f", "g"];
        assert_eq!(
            coverage(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(judged(&five, &seven, &[], false))
            }),
            [
                "  - after this, the agent can't use `a`, `b`, `c`, `d` or `e` in this conversation",
                "  - after this, `a`, `b`, `c`, `d`, `e` and 2 more will need approval in this conversation",
            ]
        );
    }

    #[test]
    fn a_narrowing_with_no_impact_adds_no_bullet() {
        let expected = ["  - attention: signoff"];
        assert_eq!(coverage(&narrowing_review(None)), expected);
        assert_eq!(coverage(&narrowing_review(judged(&[], &[], &[], false))), expected);
    }

    #[test]
    fn an_execution_time_review_says_tools_may_be_limited() {
        assert_eq!(
            coverage(&narrowing_review(Some(ReviewedNarrowing::Unjudged))),
            [
                "  - attention: signoff",
                "  - after this, some tools may be blocked or need approval in this conversation",
            ]
        );
    }

    /// A review with only narrowing bullets lists them alone, with no `(none)` beside them.
    #[test]
    fn narrowing_bullets_stand_alone_without_requirements() {
        assert_eq!(
            coverage(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(judged(&["deploy"], &[], &[], false))
            }),
            ["  - after this, the agent can't use `deploy` in this conversation"]
        );
        assert_eq!(
            coverage(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(None)
            }),
            ["  (none)"]
        );
    }

    /// A line break in a policy-authored name cannot start a line of its own.
    #[test]
    fn a_line_break_in_a_policy_name_stays_inside_its_bullet() {
        assert_eq!(
            coverage(&AuthorityArtifact {
                requirements: vec![
                    Requirement::Effect {
                        excludes: "deploy\nextra".to_string()
                    },
                    Requirement::Attention {
                        mark: "signoff\r\nextra".to_string()
                    },
                ],
                ..artifact(serde_json::json!({}))
            }),
            [
                "  - an earlier deploy\u{FFFD}extra action in this conversation",
                "  - attention: signoff\u{FFFD}\u{FFFD}extra",
            ]
        );
    }

    fn requirement_item(requirement: ReviewRequirement, text: &str) -> ReviewScopeItem {
        ReviewScopeItem::Requirement {
            requirement,
            text: text.to_string(),
        }
    }

    fn narrowing_item(narrowing: ReviewNarrowing, text: &str) -> ReviewScopeItem {
        ReviewScopeItem::Narrowing {
            narrowing,
            text: text.to_string(),
        }
    }

    #[test]
    fn every_requirement_is_one_scope_item_with_its_identifiers_and_bullet() {
        let scope = review_scope(&AuthorityArtifact {
            requirements: vec![
                Requirement::Trust {
                    required: "trusted".to_string(),
                },
                Requirement::Audience {
                    required: AudienceRequirement::Public,
                },
                Requirement::Audience {
                    required: AudienceRequirement::Readers(vec!["internal".to_string(), "alice".to_string()]),
                },
                Requirement::Effect {
                    excludes: "deploy".to_string(),
                },
                Requirement::Attention {
                    mark: "signoff".to_string(),
                },
            ],
            ..artifact(serde_json::json!({}))
        });
        assert_eq!(
            scope,
            [
                requirement_item(
                    ReviewRequirement::Trust {
                        required: "trusted".to_string()
                    },
                    "unverified input for this step"
                ),
                requirement_item(ReviewRequirement::PublicAudience, "public sharing"),
                requirement_item(
                    ReviewRequirement::Audience {
                        readers: vec!["internal".to_string(), "alice".to_string()]
                    },
                    "sharing with \"internal\", \"alice\""
                ),
                requirement_item(
                    ReviewRequirement::Effect {
                        excludes: "deploy".to_string()
                    },
                    "an earlier deploy action in this conversation"
                ),
                ReviewScopeItem::Attention {
                    mark: "signoff".to_string(),
                    text: "attention: signoff".to_string(),
                },
            ]
        );
    }

    #[test]
    fn each_impact_of_a_narrowing_is_one_scope_item_naming_every_tool() {
        let seven = ["a", "b", "c", "d", "e", "f", "g"];
        assert_eq!(
            review_scope(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(judged(&["create_issue"], &seven, &["send_mail"], true))
            }),
            [
                narrowing_item(
                    ReviewNarrowing::Blocked {
                        tools: tools(&["create_issue"])
                    },
                    "after this, the agent can't use `create_issue` in this conversation"
                ),
                narrowing_item(
                    ReviewNarrowing::SignOff { tools: tools(&seven) },
                    "after this, `a`, `b`, `c`, `d`, `e` and 2 more will need approval in this conversation"
                ),
                narrowing_item(
                    ReviewNarrowing::RecipientChecked {
                        tools: tools(&["send_mail"])
                    },
                    "after this, `send_mail` can only send to people allowed to read this data"
                ),
                narrowing_item(
                    ReviewNarrowing::PerCall,
                    "other tools may also need approval, decided call by call"
                ),
            ]
        );
    }

    #[test]
    fn an_unjudged_narrowing_is_one_scope_item() {
        assert_eq!(
            review_scope(&AuthorityArtifact {
                requirements: vec![],
                ..narrowing_review(Some(ReviewedNarrowing::Unjudged))
            }),
            [narrowing_item(
                ReviewNarrowing::Unjudged,
                "after this, some tools may be blocked or need approval in this conversation"
            )]
        );
    }

    /// `(none)` in the text is an empty scope, known to accept nothing else.
    #[test]
    fn a_review_covering_nothing_has_an_empty_scope() {
        let nothing = artifact(serde_json::json!({}));
        assert_eq!(review_scope(&nothing), []);
        assert_eq!(coverage(&nothing), ["  (none)"]);
    }

    /// An identifier stays exact for a host to match on; only its display text is made safe.
    #[test]
    fn a_scope_identifier_stays_exact_while_its_text_is_terminal_safe() {
        let mark = "signoff\r\nextra".to_string();
        assert_eq!(
            review_scope(&AuthorityArtifact {
                requirements: vec![Requirement::Attention { mark: mark.clone() }],
                ..artifact(serde_json::json!({}))
            }),
            [ReviewScopeItem::Attention {
                mark,
                text: "attention: signoff\u{FFFD}\u{FFFD}extra".to_string(),
            }]
        );
    }

    #[test]
    fn each_coverage_bullet_is_the_text_of_its_scope_item() {
        let full = AuthorityArtifact {
            requirements: vec![
                Requirement::Attention {
                    mark: "signoff".to_string(),
                },
                Requirement::Audience {
                    required: AudienceRequirement::Public,
                },
            ],
            ..narrowing_review(judged(&["a"], &["b", "c"], &["d"], true))
        };
        let bullets = review_scope(&full)
            .iter()
            .map(|item| format!("  - {}", item.text()))
            .collect::<Vec<_>>();
        assert_eq!(
            bullets,
            [
                "  - attention: signoff",
                "  - public sharing",
                "  - after this, the agent can't use `a` in this conversation",
                "  - after this, `b` and `c` will need approval in this conversation",
                "  - after this, `d` can only send to people allowed to read this data",
                "  - other tools may also need approval, decided call by call",
            ]
        );
        assert_eq!(coverage(&full), bullets);
    }

    #[test]
    fn every_requirement_reads_in_plain_words() {
        let line = |requirement| {
            coverage(&AuthorityArtifact {
                requirements: vec![requirement],
                ..artifact(serde_json::json!({}))
            })
        };
        assert_eq!(
            line(Requirement::Trust {
                required: "trusted".to_string()
            }),
            ["  - unverified input for this step"]
        );
        assert_eq!(
            line(Requirement::Audience {
                required: AudienceRequirement::Public
            }),
            ["  - public sharing"]
        );
        assert_eq!(
            line(Requirement::Effect {
                excludes: "deploy".to_string()
            }),
            ["  - an earlier deploy action in this conversation"]
        );
        assert_eq!(
            line(Requirement::Attention {
                mark: "signoff".to_string()
            }),
            ["  - attention: signoff"]
        );
    }
}
