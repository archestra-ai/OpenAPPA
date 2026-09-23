//! A host's account of the consults a governed call made: one [`ConsultRecord`] per
//! external consult a session raised, handed to the [`ConsultRecorder`] the host attached
//! with [`crate::api::Runtime::recording`].
//!
//! Like [`crate::events`], a record is never an input to a decision. The recorder is called
//! after the consult's outcome is known, and nothing it does reaches that outcome.

use serde::Serialize;

use crate::consult::Consult;
use crate::events::{ExternalOutcome, ExternalRole};
use crate::external::{ConsultOutcome, Transcript};

/// Receives one record per recorded consult. Called on the consult's own task, so an
/// implementation hands the record off rather than doing work of its own here.
pub trait ConsultRecorder: Send + Sync {
    fn record(&self, record: ConsultRecord);
}

/// Which transport served a recorded consult. Stock implementations, inline rosters and
/// `appa replay`'s stand-in answer in process and are never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultBackend {
    Url,
    Command,
    Module,
    Llm,
    ClaudeCode,
    Hitl,
}

/// What a record joins on: the trajectory that raised the consult, and the host call id,
/// the offer, or the canonical call digest it was raised for, where the event had one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsultContext {
    pub root: String,
    pub trajectory: String,
    pub call_id: Option<String>,
    pub offer_id: Option<String>,
    pub call_digest: Option<String>,
}

/// One external consult as it went over the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConsultRecord {
    /// A UUID v7, minted in this process.
    pub id: uuid::Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub role: ExternalRole,
    pub external_name: String,
    pub backend: ConsultBackend,
    /// The consult envelope every backend receives, as JSON.
    pub request: serde_json::Value,
    pub outcome: ExternalOutcome,
    pub answer: Option<serde_json::Value>,
    /// What a `url` or `command` external returned, whatever its status or shape, and a
    /// module's output; at most the deployment's `max_body_bytes`.
    pub raw_response: Option<Vec<u8>>,
    pub http_status: Option<u16>,
    /// A `url` external's `X-Appa-Diagnostics` header, or the tail of a command's stderr:
    /// at most 8 KiB, never read by the runtime.
    pub diagnostics: Option<Vec<u8>>,
    pub diagnostics_truncated: bool,
    pub context: ConsultContext,
}

impl ConsultRecord {
    pub(crate) fn new(
        consult: &Consult,
        outcome: &ConsultOutcome,
        transcript: Transcript,
        started_at: std::time::SystemTime,
        duration_ms: u64,
        context: ConsultContext,
    ) -> ConsultRecord {
        let (diagnostics, diagnostics_truncated) = match transcript.diagnostics {
            Some(diagnostics) => (Some(diagnostics.bytes), diagnostics.truncated),
            None => (None, false),
        };
        ConsultRecord {
            id: uuid::Uuid::now_v7(),
            started_at: started_at.into(),
            duration_ms,
            role: consult.kind().into(),
            external_name: consult.name.clone(),
            backend: transcript.backend,
            request: serde_json::to_value(consult).expect("a consult serializes: its wire form is plain JSON"),
            outcome: outcome.into(),
            answer: match outcome {
                ConsultOutcome::Answer(answer) => Some(answer.clone()),
                ConsultOutcome::NoAnswer(_) => None,
            },
            raw_response: transcript.raw_response,
            http_status: transcript.http_status,
            diagnostics,
            diagnostics_truncated,
            context,
        }
    }
}
