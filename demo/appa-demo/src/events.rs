//! The turn stream's vocabulary: facts from the family log rendered as wire
//! events for the chat client.
//!
//! Everything the client shows is derived from the log — the same discipline
//! as `appa-corp-agent`'s replay — so what the visitor watches is the audit
//! record, not a parallel narration. The mapping is lossy on purpose: audit
//! detail the chat does not render (cast records, per-value provenance) stays
//! in the log.

use appa_engine::fact::{BoundaryKind, CloseOutcome, Fact, ReturnDerivation};
use appa_engine::label::{Audience, Dim, Label};
use appa_engine::projection::Projection;
use appa_engine::registry::TrustChain;
use appa_engine::value::{Provenance, TrajectoryId};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireEvent {
    Says { trajectory: String, text: String },
    ToolProposed {
        trajectory: String,
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
    },
    Blocked {
        trajectory: String,
        call_id: String,
        text: String,
    },
    ToolClosed {
        trajectory: String,
        outcome: String,
        effects: Vec<String>,
    },
    ToolResult { trajectory: String, body: String },
    Label {
        trajectory: String,
        trust: String,
        audience: String,
    },
    Remedy { trajectory: String, text: String },
    Sanitized { trajectory: String, sanitizer: String },
    Fork { parent: String, child: String },
    Merge { trajectory: String },
    ApprovalRequested {
        id: String,
        tool: String,
        detail: serde_json::Value,
    },
    ApprovalResolved { id: String, approved: bool, expired: bool },
    Answer { text: String },
    Stopped { text: String },
    Error { message: String },
}

pub fn trust_text(label: &Label, chain: &TrustChain) -> String {
    match &label.trust {
        Dim::Known(trust) => chain.name_of(*trust).unwrap_or("?").to_string(),
        Dim::Unknown => "unknown".to_string(),
    }
}

pub fn audience_text(label: &Label) -> String {
    match &label.audience {
        Dim::Known(Audience::Public) => "public".to_string(),
        Dim::Known(Audience::Restricted(readers)) if readers.is_empty() => "nobody".to_string(),
        Dim::Known(Audience::Restricted(readers)) => {
            let names: Vec<&str> = readers.iter().map(|reader| reader.as_str()).collect();
            names.join(", ")
        }
        Dim::Unknown => "unknown".to_string(),
    }
}

pub fn fact_event(fact: &Fact, chain: &TrustChain) -> Vec<WireEvent> {
    match fact {
        Fact::ValueAdmitted {
            trajectory,
            value,
            provenance: Provenance::ToolResult { .. },
        } => vec![WireEvent::ToolResult {
            trajectory: trajectory.as_str().to_string(),
            body: value.body.as_str().to_string(),
        }],
        Fact::AssistantMessage {
            trajectory,
            content,
            calls,
        } => {
            let mut events = Vec::new();
            if let Some(text) = content
                && !text.trim().is_empty()
            {
                events.push(WireEvent::Says {
                    trajectory: trajectory.as_str().to_string(),
                    text: text.clone(),
                });
            }
            for call in calls {
                events.push(WireEvent::ToolProposed {
                    trajectory: trajectory.as_str().to_string(),
                    call_id: call.id.as_str().to_string(),
                    tool: call.tool.as_str().to_string(),
                    arguments: call.arguments.clone(),
                });
            }
            events
        }
        Fact::BlockFeedback {
            trajectory,
            call_id,
            content,
        } => vec![WireEvent::Blocked {
            trajectory: trajectory.as_str().to_string(),
            call_id: call_id.as_str().to_string(),
            text: content.clone(),
        }],
        Fact::DispatchClosed {
            trajectory, outcome, ..
        } => {
            let (outcome, effects) = match outcome {
                CloseOutcome::Success { effects } => {
                    ("ran", effects.iter().map(|kind| kind.as_str().to_string()).collect())
                }
                CloseOutcome::Failure => ("failed", Vec::new()),
                CloseOutcome::Indeterminate => ("indeterminate", Vec::new()),
            };
            vec![WireEvent::ToolClosed {
                trajectory: trajectory.as_str().to_string(),
                outcome: outcome.to_string(),
                effects,
            }]
        }
        Fact::Ruling {
            trajectory, authority, ..
        } => vec![WireEvent::Remedy {
            trajectory: trajectory.as_str().to_string(),
            text: format!("approved by {}", authority.as_str()),
        }],
        Fact::Acceptance {
            trajectory, narrowing, ..
        } => vec![WireEvent::Remedy {
            trajectory: trajectory.as_str().to_string(),
            text: format!(
                "narrowing accepted: {} → {}",
                label_text(&narrowing.from, chain),
                label_text(&narrowing.to, chain)
            ),
        }],
        Fact::ChildReturn {
            trajectory, derivation, ..
        } => match derivation {
            ReturnDerivation::Raw => Vec::new(),
            ReturnDerivation::Sanitized { sanitizer, .. } => vec![WireEvent::Remedy {
                trajectory: trajectory.as_str().to_string(),
                text: format!("child return crossed as the {} derivation", sanitizer.as_str()),
            }],
        },
        Fact::OutputSanitizerApplied {
            trajectory, sanitizer, ..
        } => vec![WireEvent::Sanitized {
            trajectory: trajectory.as_str().to_string(),
            sanitizer: sanitizer.as_str().to_string(),
        }],
        Fact::Boundary { trajectory, kind } => match kind {
            BoundaryKind::Fork { parent, .. } => vec![WireEvent::Fork {
                parent: parent.as_str().to_string(),
                child: trajectory.as_str().to_string(),
            }],
            BoundaryKind::Merge { .. } => vec![WireEvent::Merge {
                trajectory: trajectory.as_str().to_string(),
            }],
            BoundaryKind::TurnEnd => Vec::new(),
        },
        // Algebraically detailed records the chat does not render.
        _ => Vec::new(),
    }
}

fn label_text(label: &Label, chain: &TrustChain) -> String {
    format!("trust={} audience={}", trust_text(label, chain), audience_text(label))
}

pub fn current_label(
    facts: &[Fact],
    revision: appa_engine::fact::Revision,
    root: &TrajectoryId,
    chain: &TrustChain,
) -> (String, String) {
    let projection = Projection::build(facts, revision);
    let label = projection.view(root).current_label();
    (trust_text(&label, chain), audience_text(&label))
}
