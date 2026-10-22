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

use std::collections::HashMap;

use appa_engine::label::Label;
use appa_engine::registry::TrustChain;
use appa_example_agent::{CallId, Record, Recorded};
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
    /// A held call. `text` is what the model read; `reasons` and `options` say the same
    /// for the person watching, without the instructions written for the model.
    Blocked {
        trajectory: String,
        call_id: String,
        text: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        reasons: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        options: Vec<BlockOption>,
    },
    ToolClosed {
        trajectory: String,
        call_id: String,
        outcome: String,
        effects: Vec<String>,
    },
    /// The body the model read as the answer to `call_id`.
    ToolResult {
        trajectory: String,
        call_id: String,
        body: String,
    },
    /// The remedy the control call `call_id` chose was carried out; `value`
    /// is what crossed in place of the held one, when it delivered one.
    OfferTaken {
        trajectory: String,
        call_id: String,
        detail: String,
        value: Option<String>,
    },
    Label {
        trajectory: String,
        trust: String,
        audience: String,
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

/// One remedy a block offers, keyed by the `offer_id` the model passes to take it.
#[derive(Clone, Debug, Serialize)]
pub struct BlockOption {
    pub offer_id: String,
    pub text: String,
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
            trust: chain.name_of(label.trust).unwrap_or("?").to_string(),
            audience: if label.audience.is_public() {
                "public".to_string()
            } else {
                let clauses: Vec<String> = label
                    .audience
                    .clauses()
                    .map(|clause| {
                        let entries: Vec<String> = clause
                            .chain()
                            .map(|chain| chain.as_str().to_string())
                            .into_iter()
                            .chain(clause.groups().map(|group| group.to_string()))
                            .chain(clause.readers().iter().map(|reader| reader.as_str().to_string()))
                            .collect();
                        if entries.is_empty() {
                            "nobody".to_string()
                        } else {
                            entries.join(", ")
                        }
                    })
                    .collect();
                clauses.join(" ∩ ")
            },
        }
    }
}

/// The stream's reader: maps both records to wire events, and names the
/// call behind each dispatch the audit closes.
///
/// The audit identifies a dispatch by its tool, not by the harness's call
/// id, so the reader keeps the correlation. The agent runs one call at a
/// time and records each proposal before the runtime opens its dispatch;
/// the pump drains records before it reads the audit. So when the audit
/// shows a dispatch opened, its proposal has been read, and it is the
/// trajectory's oldest unresolved proposal of that tool.
#[derive(Default)]
pub struct Reader {
    /// Proposals not yet blocked, refused, answered by a remedy, or
    /// dispatched, per trajectory, oldest first.
    proposed: HashMap<String, Vec<(String, String)>>,
    /// The call whose dispatch is open, per trajectory.
    open: HashMap<String, String>,
    /// One dispatch writes its committed effects and its close as two
    /// entries, and the chat resolves one call.
    committed: Vec<String>,
}

impl Reader {
    pub fn record(&mut self, recorded: &Recorded) -> Vec<WireEvent> {
        let trajectory = recorded.trajectory.0.clone();
        match &recorded.record {
            Record::Says { text } => vec![WireEvent::Says {
                trajectory,
                text: text.clone(),
            }],
            Record::Proposes { call, tool, arguments } => {
                self.proposed
                    .entry(trajectory.clone())
                    .or_default()
                    .push((call.0.clone(), tool.clone()));
                vec![WireEvent::ToolProposed {
                    trajectory,
                    call_id: call.0.clone(),
                    tool: tool.clone(),
                    arguments: serde_json::from_str(arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments.clone())),
                }]
            }
            Record::Blocked {
                call,
                feedback,
                display,
                ..
            } => {
                self.settle(&trajectory, call);
                let (reasons, options) = match display {
                    None => (Vec::new(), Vec::new()),
                    Some(display) => {
                        // A display plan repeats the reasons after its description, for a host that
                        // shows one plan alone; the chat shows the reasons once, above the options.
                        let why = format!("\nWhy: {}", display.reasons.join("; "));
                        let options = display
                            .plans
                            .iter()
                            .map(|plan| BlockOption {
                                offer_id: plan.offer_id.clone(),
                                text: plan.text.strip_suffix(&why).unwrap_or(&plan.text).to_string(),
                            })
                            .collect();
                        (display.reasons.clone(), options)
                    }
                };
                vec![WireEvent::Blocked {
                    trajectory,
                    call_id: call.0.clone(),
                    text: feedback.clone(),
                    reasons,
                    options,
                }]
            }
            Record::OutputBlocked { call, reason } => vec![WireEvent::Blocked {
                trajectory,
                call_id: call.0.clone(),
                text: reason.clone(),
                reasons: Vec::new(),
                options: Vec::new(),
            }],
            // A refused call shows as the call that did not run: the text names it as the
            // runtime's refusal, not a policy block.
            Record::Refused { call, detail, .. } => {
                self.settle(&trajectory, call);
                vec![WireEvent::Blocked {
                    trajectory,
                    call_id: call.0.clone(),
                    text: format!("[appa] refused: {detail}"),
                    reasons: Vec::new(),
                    options: Vec::new(),
                }]
            }
            Record::Admitted { call, body } | Record::Substituted { call, body } | Record::Returned { call, body } => {
                vec![WireEvent::ToolResult {
                    trajectory,
                    call_id: call.0.clone(),
                    body: body.clone(),
                }]
            }
            Record::OfferTaken { call, detail, value } => {
                self.settle(&trajectory, call);
                vec![WireEvent::OfferTaken {
                    trajectory,
                    call_id: call.0.clone(),
                    detail: detail.clone(),
                    value: value.clone(),
                }]
            }
            // The control channel refusing the agent's move is a block on the
            // control call itself.
            Record::OfferRefused { call, feedback } => {
                self.settle(&trajectory, call);
                vec![WireEvent::Blocked {
                    trajectory,
                    call_id: call.0.clone(),
                    text: feedback.clone(),
                    reasons: Vec::new(),
                    options: Vec::new(),
                }]
            }
            Record::Forked { .. }
            | Record::ReturnBlocked { .. }
            | Record::ProviderRetried { .. }
            | Record::BudgetFinalized
            | Record::Answers { .. } => Vec::new(),
        }
    }

    /// The proposal was answered without a dispatch.
    fn settle(&mut self, trajectory: &str, call: &CallId) {
        if let Some(proposed) = self.proposed.get_mut(trajectory) {
            proposed.retain(|(id, _)| *id != call.0);
        }
    }

    pub fn audit(&mut self, entry: &AuditEntry) -> Vec<WireEvent> {
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
                // A close the reader cannot place ends no call; its
                // result still ends it.
                match self.open.remove(&trajectory) {
                    Some(call_id) => vec![WireEvent::ToolClosed {
                        trajectory,
                        call_id,
                        outcome: outcome.to_string(),
                        effects,
                    }],
                    None => Vec::new(),
                }
            }
            AuditEvent::Released { tool, label, .. } => {
                // Any older proposal still listed was answered without a
                // record; dropping it keeps it from claiming a later dispatch.
                let proposed = self.proposed.entry(trajectory.clone()).or_default();
                match proposed.iter().position(|(_, proposed)| proposed == tool) {
                    Some(at) => {
                        let (call, _) = proposed
                            .drain(..=at)
                            .next_back()
                            .expect("the drained range ends at the match");
                        self.open.insert(trajectory.clone(), call);
                    }
                    None => {
                        self.open.remove(&trajectory);
                    }
                }
                vec![label_event(trajectory, label.clone())]
            }
            AuditEvent::Admitted { label } => vec![label_event(trajectory, label.clone())],
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
            AuditEvent::OutputWithheld => vec![WireEvent::Remedy {
                trajectory,
                text: "tool result withheld".to_string(),
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
            AuditEvent::SanitizerBound { .. } | AuditEvent::VoidReturn | AuditEvent::Resumed { .. } => Vec::new(),
        }
    }
}

fn label_event(trajectory: String, label: AuditLabel) -> WireEvent {
    WireEvent::Label {
        trajectory,
        trust: label.trust,
        audience: label.audience,
    }
}
