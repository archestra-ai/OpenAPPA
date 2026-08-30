//! The turn stream's vocabulary, and where each event comes from.
//!
//! Two records feed it, and neither could feed it alone. The agent's own
//! record holds the conversation — what the model said, what it proposed,
//! and what the runtime told it — including the blocks, which the runtime's
//! audit cannot carry: a refused call appends no fact, so nothing moved and
//! nothing was written. The audit holds the flows — what actually crossed,
//! what it was labelled, what committed. The chat shows both, and neither is
//! a narration the service invented.
//!
//! The mapping is lossy on purpose: audit detail the chat does not render
//! (per-value provenance) stays in the log.

use appa_engine::label::{Audience, Dim, Label};
use appa_engine::registry::TrustChain;
use appa_example_agent::{Record, Recorded};
use appa_runtime::api::{AuditEntry, AuditEvent, AuditLabel, DispatchOutcome};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireEvent {
    Says {
        trajectory: String,
        text: String,
    },
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
    ToolResult {
        trajectory: String,
        body: String,
    },
    Label {
        trajectory: String,
        trust: String,
        audience: String,
        unresolved_trust: Vec<u64>,
        unresolved_audience: Vec<u64>,
    },
    Remedy {
        trajectory: String,
        text: String,
    },
    Sanitized {
        trajectory: String,
        sanitizer: String,
    },
    Fork {
        parent: String,
        child: String,
    },
    Merge {
        trajectory: String,
    },
    ApprovalRequested {
        id: String,
        tool: String,
        detail: serde_json::Value,
    },
    ApprovalResolved {
        id: String,
        approved: bool,
        expired: bool,
    },
    Answer {
        text: String,
    },
    Stopped {
        text: String,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelText {
    pub trust: String,
    pub audience: String,
}

impl LabelText {
    /// Render an engine label. Used once per session, for the boundary the
    /// policy declares; everything the runtime renders arrives as strings
    /// already.
    pub fn of(label: &Label, chain: &TrustChain) -> LabelText {
        LabelText {
            trust: match &label.trust {
                Dim::Known(trust) => chain.name_of(*trust).unwrap_or("?").to_string(),
                Dim::Unknown => "unknown".to_string(),
            },
            audience: match &label.audience {
                Dim::Known(Audience::Public) => "public".to_string(),
                Dim::Known(Audience::Restricted(readers)) if readers.is_empty() => "nobody".to_string(),
                Dim::Known(Audience::Restricted(readers)) => readers
                    .iter()
                    .map(|reader| reader.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                Dim::Unknown => "unknown".to_string(),
            },
        }
    }
}

pub fn record_event(recorded: &Recorded) -> Vec<WireEvent> {
    let trajectory = recorded.trajectory.0.clone();
    match &recorded.record {
        Record::Says { text } => vec![WireEvent::Says {
            trajectory,
            text: text.clone(),
        }],
        Record::Proposes { call, tool, arguments } => vec![WireEvent::ToolProposed {
            trajectory,
            call_id: call.0.clone(),
            tool: tool.clone(),
            arguments: serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::Value::String(arguments.clone())),
        }],
        Record::Blocked { call, feedback, .. } => vec![WireEvent::Blocked {
            trajectory,
            call_id: call.0.clone(),
            text: feedback.clone(),
        }],
        Record::OutputBlocked { call, reason } => vec![WireEvent::Blocked {
            trajectory,
            call_id: call.0.clone(),
            text: reason.clone(),
        }],
        Record::Admitted { body, .. } | Record::Substituted { body, .. } => {
            vec![WireEvent::ToolResult {
                trajectory,
                body: body.clone(),
            }]
        }
        Record::OfferRefused { feedback } => vec![WireEvent::Remedy {
            trajectory,
            text: feedback.clone(),
        }],
        Record::OfferTaken { .. }
        | Record::Forked { .. }
        | Record::ReturnBlocked { .. }
        | Record::ProviderRetried { .. }
        | Record::BudgetFinalized
        | Record::Answers { .. } => Vec::new(),
    }
}

/// The audit reader's own state: one dispatch writes its committed effects
/// and its close as two entries, and the chat resolves one card.
#[derive(Default)]
pub struct AuditReader {
    committed: Vec<String>,
}

impl AuditReader {
    pub fn event(&mut self, entry: &AuditEntry) -> Vec<WireEvent> {
        let trajectory = entry.trajectory.clone();
        match &entry.event {
            AuditEvent::EffectsCommitted { effects } => {
                self.committed = effects.clone();
                Vec::new()
            }
            AuditEvent::Closed { outcome } => {
                let (outcome, effects) = match outcome {
                    DispatchOutcome::Ran { effects } => {
                        ("ran", [std::mem::take(&mut self.committed), effects.clone()].concat())
                    }
                    DispatchOutcome::Failed => ("failed", Vec::new()),
                    DispatchOutcome::Unknown => ("indeterminate", Vec::new()),
                };
                vec![WireEvent::ToolClosed {
                    trajectory,
                    outcome: outcome.to_string(),
                    effects,
                }]
            }
            AuditEvent::Released { label, .. } | AuditEvent::Admitted { label } => {
                vec![label_event(trajectory, label.clone())]
            }
            AuditEvent::Ruled { authority } => vec![WireEvent::Remedy {
                trajectory,
                text: format!("approved by {authority}"),
            }],
            AuditEvent::Denied { authority } => vec![WireEvent::Remedy {
                trajectory,
                text: format!("{authority} denied this call"),
            }],
            AuditEvent::Narrowed { from, to } => vec![
                WireEvent::Remedy {
                    trajectory: trajectory.clone(),
                    text: format!(
                        "narrowing accepted: trust={} audience={} → trust={} audience={}",
                        from.trust, from.audience, to.trust, to.audience
                    ),
                },
                label_event(trajectory, to.clone()),
            ],
            AuditEvent::Sanitized { sanitizer } => vec![WireEvent::Sanitized {
                trajectory,
                sanitizer: sanitizer.clone(),
            }],
            AuditEvent::Forked { parent, .. } => vec![WireEvent::Fork {
                parent: parent.clone(),
                child: trajectory,
            }],
            AuditEvent::Merged => vec![WireEvent::Merge { trajectory }],
            AuditEvent::ChildReturn { sanitizer, label } => {
                let mut events = match sanitizer {
                    None => Vec::new(),
                    Some(sanitizer) => vec![WireEvent::Remedy {
                        trajectory: trajectory.clone(),
                        text: format!("child return crossed as the {sanitizer} derivation"),
                    }],
                };
                events.push(label_event(trajectory, label.clone()));
                events
            }
            // Algebraically detailed records the chat does not render.
            AuditEvent::SanitizerBound { .. } | AuditEvent::VoidReturn => Vec::new(),
        }
    }
}

fn label_event(trajectory: String, label: AuditLabel) -> WireEvent {
    WireEvent::Label {
        trajectory,
        trust: label.trust,
        audience: label.audience,
        unresolved_trust: label.unresolved_trust,
        unresolved_audience: label.unresolved_audience,
    }
}
