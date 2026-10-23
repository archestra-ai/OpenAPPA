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

use appa_engine::label::Label;
use appa_engine::registry::TrustChain;
use appa_example_agent::{Record, Recorded};
use appa_runtime::api::{AuditEntry, AuditEvent, AuditLabel, DispatchOutcome};
use serde::Serialize;

// The website reads this type as `website/components/playground/wire-events.ts`,
// which `wire_events::the_website_reads_what_the_service_writes` renders from
// it, and fails on when the two differ.
/// One event of a turn's stream.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// A sanitizer's derivation crossed in place of a raw value. `call_id` names the call
    /// whose result it rewrote; `None` when it rewrote something else, such as a call's
    /// arguments before its dispatch.
    Sanitized {
        trajectory: String,
        call_id: Option<String>,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// The stream's reader: maps both records to wire events. Every event about
/// a tool call names it by the harness's call id; the audit carries that id
/// for each dispatch the runtime opened for an identified call.
#[derive(Default)]
pub struct Reader {
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
                // A close for a call the host did not identify ends no call;
                // its result still ends it.
                match entry.call_id.clone() {
                    Some(call_id) => vec![WireEvent::ToolClosed {
                        trajectory,
                        call_id,
                        outcome: outcome.to_string(),
                        effects,
                    }],
                    None => Vec::new(),
                }
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
                call_id: entry.call_id.clone(),
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

/// The website's copy of the wire vocabulary is generated from `WireEvent`, so a
/// field renamed, added, retyped or dropped here fails this test until the copy
/// is regenerated and the website compiles against it.
#[cfg(test)]
mod wire_events {
    use std::path::PathBuf;

    use schemars::generate::SchemaSettings;
    use serde_json::Value;

    use super::WireEvent;

    const REGENERATE: &str = "APPA_BLESS=1 cargo test -p website-chat-playground --lib wire_events";

    #[test]
    fn the_website_reads_what_the_service_writes() {
        let schema = SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<WireEvent>();
        let rendered = module(schema.as_value());
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../website/components/playground/wire-events.ts");
        if std::env::var_os("APPA_BLESS").is_some() {
            std::fs::write(&path, &rendered).expect("the website's copy is writable");
            return;
        }
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            current == rendered,
            "{} no longer matches `WireEvent`; regenerate it with `{REGENERATE}`, then fix what the website no longer compiles",
            path.display()
        );
    }

    fn module(root: &Value) -> String {
        let mut out = format!(
            "// Generated from `WireEvent` in website-chat-playground/src/events.rs. Do not edit.\n\
             // Regenerate with `{REGENERATE}`.\n"
        );
        let variants = root["oneOf"].as_array().expect("a tagged enum renders as oneOf");
        out.push_str(&format!("\n{}export type WireEvent =\n", doc(root, "")));
        for variant in variants {
            out.push_str(&doc(variant, "  "));
            out.push_str(&format!("  | {}\n", object(variant)));
        }
        out.pop();
        out.push_str(";\n");
        for (name, definition) in root["$defs"].as_object().into_iter().flatten() {
            out.push_str(&format!(
                "\n{}export type {name} = {};\n",
                doc(definition, ""),
                ts(definition)
            ));
        }
        out
    }

    fn doc(schema: &Value, indent: &str) -> String {
        match schema["description"].as_str() {
            None => String::new(),
            Some(text) => format!("{indent}/** {} */\n", text.replace('\n', " ")),
        }
    }

    /// An object's properties with the tag first, as the wire writes it.
    fn object(schema: &Value) -> String {
        let required: Vec<&str> = schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let properties = schema["properties"]
            .as_object()
            .expect("an object names its properties");
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort_by_key(|name| *name != "type");
        let fields: Vec<String> = names
            .into_iter()
            .map(|name| {
                let optional = if required.contains(&name.as_str()) { "" } else { "?" };
                let doc = doc(&properties[name], "").replace('\n', " ");
                format!("{doc}{name}{optional}: {}", ts(&properties[name]))
            })
            .collect();
        format!("{{ {} }}", fields.join("; "))
    }

    fn ts(schema: &Value) -> String {
        if schema == &Value::Bool(true) || schema.as_object().is_some_and(|object| object.is_empty()) {
            return "unknown".to_string();
        }
        if let Some(reference) = schema["$ref"].as_str() {
            return reference
                .rsplit('/')
                .next()
                .expect("a reference names its target")
                .to_string();
        }
        if let Some(constant) = schema.get("const") {
            return constant.to_string();
        }
        if let Some(alternatives) = schema["anyOf"].as_array().or(schema["oneOf"].as_array()) {
            return alternatives.iter().map(ts).collect::<Vec<_>>().join(" | ");
        }
        match &schema["type"] {
            Value::Array(types) => types
                .iter()
                .map(|kind| ts(&serde_json::json!({ "type": kind, "items": schema["items"] })))
                .collect::<Vec<_>>()
                .join(" | "),
            Value::String(kind) => match kind.as_str() {
                "string" => "string".to_string(),
                "boolean" => "boolean".to_string(),
                "integer" | "number" => "number".to_string(),
                "null" => "null".to_string(),
                "array" => match ts(&schema["items"]) {
                    item if item.contains(' ') => format!("({item})[]"),
                    item => format!("{item}[]"),
                },
                "object" if schema.get("properties").is_some() => object(schema),
                "object" => "Record<string, unknown>".to_string(),
                other => panic!("no TypeScript rendering for JSON Schema type {other}"),
            },
            _ => panic!("no TypeScript rendering for {schema}"),
        }
    }
}
