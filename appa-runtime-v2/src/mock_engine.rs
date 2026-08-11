//! The engine boundary as one temporary file: a transcription of the
//! target contract in `docs/engine.md`, plus the `MockEngine` that
//! stands in for the real engine. The whole file is deleted when the
//! engine team's published engine and types replace it.

use std::collections::VecDeque;
use std::sync::Mutex;

pub use crate::api::{ChildTask, DispatchId, OfferId, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

/// The count of writes to one family log. A decision names the
/// revision it was based on; the store accepts its batch only if the
/// log is still at that revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogRevision(pub u64);

/// A fresh 256-bit random number, used once and never repeated. The
/// engine has no randomness of its own; the runtime supplies one for
/// each act that can surface offers, and the engine mixes it into
/// every `OfferId` it derives. `OfferNonce` is the spec's
/// wire name for this input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferNonce(pub [u8; 32]);

/// A call the engine released: the exact canonical bytes the harness
/// must execute. The runtime and the adapter deliver these bytes
/// unchanged — never re-rendered, never edited.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleasedCall {
    pub dispatch: DispatchId,
    pub tool: String,
    pub bytes: Vec<u8>,
}

/// One blocked sibling's feedback for the model, with the offers it
/// surfaces named explicitly so the runtime can route later
/// `execute_remedy_plan` calls by id without reading facts.
#[derive(Debug, Clone, PartialEq)]
pub struct Feedback {
    pub text: String,
    pub offers: Vec<OfferId>,
}

/// One typed request for external evidence. The runtime resolves it —
/// authority, sanitizer, cast resolver, membership resolver, or
/// dynamic resolver — and feeds the answer back on the same
/// semantic event. An answer grants nothing until the engine validates
/// it.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalRequest {
    Authority { name: String, question: serde_json::Value },
    Sanitizer { name: String, input: serde_json::Value },
    Cast { name: String, input: serde_json::Value },
    Membership { name: String, question: serde_json::Value },
    Dynamic { name: String, question: serde_json::Value },
}

/// A typed answer to one [`ExternalRequest`], fed back to the engine
/// on the same semantic event. `NoAnswer` records that the external
/// did not answer: it grants nothing and denies nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalEvidence {
    Answer {
        request: ExternalRequest,
        body: serde_json::Value,
    },
    NoAnswer {
        request: ExternalRequest,
    },
}

/// One observed event, reported to the engine. Five semantic
/// families — variants of one boundary, not steps the runtime composes
/// into a policy decision.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    PrincipalRequest,
    ModelResponse {
        call: ProposedCall,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    ExecuteOffer {
        offer: OfferId,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    ToolOutcome {
        dispatch: DispatchId,
        outcome: ToolOutcome,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
    ChildReturn {
        child: TrajectoryId,
        value: Option<String>,
        evidence: Vec<ExternalEvidence>,
        entropy: OfferNonce,
    },
}

/// Facts the runtime must append to the family log before it acts on
/// the decision. Opaque bytes plus the revision the engine based the
/// decision on.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedFactBatch {
    pub bytes: Vec<u8>,
    pub based_on: LogRevision,
}

/// One decision: facts to append, then the complete typed follow-up
/// package. The runtime appends first and acts second; on an append
/// conflict it discards `then`, rebuilds the view, and replays the
/// event.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDecision {
    pub append: Option<ValidatedFactBatch>,
    pub then: Next,
}

/// The follow-up work a decision hands the runtime. It is not another
/// policy decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    Done,
    ModelResponse {
        invocations: Vec<ReleasedCall>,
        feedback: Vec<Feedback>,
    },
    PresentToModel(Presentation),
    InvokeTool(ReleasedCall),
    ResolveExternal(Vec<ExternalRequest>),
}

/// The typed outcome a `PresentToModel` follow-up carries. The
/// variants cover what the runtime API's decision enums deliver; the
/// engine team's published type replaces this at integration. Where a
/// variant surfaces offers, it names them by id so the runtime can
/// route `execute_remedy_plan` calls without reading facts.
#[derive(Debug, Clone, PartialEq)]
pub enum Presentation {
    KeepOutput,
    ReplaceOutput { placeholder: String, offers: Vec<OfferId> },
    Value { value: String },
    Staged { feedback: String, offers: Vec<OfferId> },
    Declined { feedback: String },
    NoAnswer { feedback: String },
    NoValue,
    Blocked { feedback: String, offers: Vec<OfferId> },
}

/// The engine's derived working picture of one family log. Opaque and
/// disposable: only the engine constructs it, the runtime never edits
/// it, and the log stays the source of truth. This runtime
/// stores no view — it rebuilds one per event and drops it.
#[derive(Debug)]
pub struct EngineView {
    revision: LogRevision,
}

impl EngineView {
    /// The revision this view was built at. The store's
    /// compare-and-swap names it.
    pub fn revision(&self) -> LogRevision {
        self.revision
    }
}

/// The mock engine: it stands in for the real engine until the real
/// one replaces it at integration. One concrete struct, no trait.
pub struct MockEngine {
    mode: Mode,
}

enum Mode {
    Default,
    Test {
        queue: Mutex<VecDeque<EngineDecision>>,
        seen: Mutex<Vec<EngineEvent>>,
    },
}

impl MockEngine {
    /// The binary's engine: permit everything, admit everything,
    /// append nothing. No policy is enforced.
    pub fn permissive() -> MockEngine {
        tracing::warn!(
            "a MOCK engine is deciding: every call is permitted, every result is \
             admitted, and the policy is NOT validated or enforced"
        );
        MockEngine { mode: Mode::Default }
    }

    /// The tests' engine: every `handle` call pops the next enqueued
    /// decision.
    pub fn test_mode() -> MockEngine {
        MockEngine {
            mode: Mode::Test {
                queue: Mutex::new(VecDeque::new()),
                seen: Mutex::new(Vec::new()),
            },
        }
    }

    /// Enqueue the decision the next `handle` call returns. Test mode
    /// only.
    pub fn enqueue(&self, decision: EngineDecision) {
        match &self.mode {
            Mode::Test { queue, .. } => queue
                .lock()
                .expect("mock queue lock is never poisoned")
                .push_back(decision),
            Mode::Default => {
                panic!("enqueue is meaningful only in test mode")
            }
        }
    }

    pub fn seen(&self) -> Vec<EngineEvent> {
        match &self.mode {
            Mode::Test { seen, .. } => seen.lock().expect("mock seen lock is never poisoned").clone(),
            Mode::Default => panic!("seen is meaningful only in test mode"),
        }
    }

    /// Rebuild the view by replaying the family log. The mock
    /// validates nothing; the real engine validates every batch.
    pub fn rebuild_view(&self, log: &[Vec<u8>]) -> EngineView {
        EngineView {
            revision: LogRevision(log.len() as u64),
        }
    }

    pub fn handle(&self, view: &EngineView, event: EngineEvent) -> EngineDecision {
        match &self.mode {
            Mode::Test { queue, seen } => {
                seen.lock().expect("mock seen lock is never poisoned").push(event);
                queue
                    .lock()
                    .expect("mock queue lock is never poisoned")
                    .pop_front()
                    .expect("test mode holds an enqueued decision for every event")
            }
            Mode::Default => permissive_decision(view, event),
        }
    }
}

fn permissive_decision(view: &EngineView, event: EngineEvent) -> EngineDecision {
    let then = match event {
        EngineEvent::PrincipalRequest => Next::Done,
        EngineEvent::ModelResponse { call, entropy, .. } => {
            let bytes =
                serde_json::to_vec(&call).expect("a proposed call serializes: it was deserialized from wire JSON");
            let dispatch = DispatchId(format!(
                "mock-dispatch-{}-{:02x}{:02x}{:02x}{:02x}",
                view.revision().0,
                entropy.0[0],
                entropy.0[1],
                entropy.0[2],
                entropy.0[3],
            ));
            Next::ModelResponse {
                invocations: vec![ReleasedCall {
                    dispatch,
                    tool: call.tool,
                    bytes,
                }],
                feedback: Vec::new(),
            }
        }
        EngineEvent::ExecuteOffer { .. } => Next::PresentToModel(Presentation::Declined {
            feedback: "the mock engine keeps no offers; there is nothing to execute".to_string(),
        }),
        EngineEvent::ToolOutcome { .. } => Next::PresentToModel(Presentation::KeepOutput),
        EngineEvent::ChildReturn { value, .. } => match value {
            Some(value) => Next::PresentToModel(Presentation::Value { value }),
            None => Next::PresentToModel(Presentation::NoValue),
        },
    };
    EngineDecision { append: None, then }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entropy(byte: u8) -> OfferNonce {
        OfferNonce([byte; 32])
    }

    #[test]
    fn test_mode_returns_queued_decisions_and_batches_append_in_order() {
        let engine = MockEngine::test_mode();
        let mut log: Vec<Vec<u8>> = Vec::new();

        let view = engine.rebuild_view(&log);
        assert_eq!(view.revision(), LogRevision(0));

        let queued = EngineDecision {
            append: Some(ValidatedFactBatch {
                bytes: b"fact-batch-0".to_vec(),
                based_on: LogRevision(0),
            }),
            then: Next::Done,
        };
        engine.enqueue(queued.clone());

        let decision = engine.handle(&view, EngineEvent::PrincipalRequest);
        assert_eq!(decision, queued);

        let batch = decision.append.expect("the queued decision carries a batch");
        assert_eq!(batch.based_on, LogRevision(log.len() as u64));
        log.push(batch.bytes);

        let rebuilt = engine.rebuild_view(&log);
        assert_eq!(rebuilt.revision(), LogRevision(1));

        assert_eq!(engine.seen(), vec![EngineEvent::PrincipalRequest],);
    }

    #[test]
    fn permissive_mode_releases_every_call() {
        let engine = MockEngine::permissive();
        let view = engine.rebuild_view(&[]);
        let call = ProposedCall {
            tool: "Bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
        };
        let decision = engine.handle(
            &view,
            EngineEvent::ModelResponse {
                call: call.clone(),
                evidence: Vec::new(),
                entropy: entropy(7),
            },
        );
        assert_eq!(decision.append, None);
        match decision.then {
            Next::ModelResponse { invocations, feedback } => {
                assert!(feedback.is_empty());
                assert_eq!(invocations.len(), 1);
                assert_eq!(invocations[0].tool, "Bash");
                assert_eq!(
                    invocations[0].bytes,
                    serde_json::to_vec(&call).expect("test call serializes"),
                );
            }
            other => panic!("expected a ModelResponse follow-up, got {other:?}"),
        }
    }

    #[test]
    fn permissive_mode_admits_results_and_child_returns() {
        let engine = MockEngine::permissive();
        let view = engine.rebuild_view(&[]);

        let outcome = engine.handle(
            &view,
            EngineEvent::ToolOutcome {
                dispatch: DispatchId("d1".to_string()),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available("output".to_string()),
                },
                evidence: Vec::new(),
                entropy: entropy(1),
            },
        );
        assert_eq!(outcome.then, Next::PresentToModel(Presentation::KeepOutput));

        let returned = engine.handle(
            &view,
            EngineEvent::ChildReturn {
                child: TrajectoryId("cc:child".to_string()),
                value: Some("summary".to_string()),
                evidence: Vec::new(),
                entropy: entropy(2),
            },
        );
        assert_eq!(
            returned.then,
            Next::PresentToModel(Presentation::Value {
                value: "summary".to_string()
            }),
        );
    }

    #[test]
    fn only_the_api_module_calls_the_boundary() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        check_dir(&src, &src, &mut offenders);
        assert!(
            offenders.is_empty(),
            "engine-boundary references outside src/api and src/lib.rs: {offenders:?}",
        );
    }

    fn check_dir(root: &std::path::Path, dir: &std::path::Path, offenders: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("crate source directory is readable") {
            let path = entry.expect("crate source entry is readable").path();
            if path.is_dir() {
                check_dir(root, &path, offenders);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("entry sits under the crate source root")
                .to_string_lossy()
                .into_owned();
            if relative == "mock_engine.rs" || relative == "lib.rs" || relative.starts_with("api/") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("crate source file is readable");
            if text.contains("mock_engine::")
                || text.contains("crate::mock_engine")
                || text.contains("super::mock_engine")
            {
                offenders.push(relative);
            }
        }
    }
}
