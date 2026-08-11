//! The engine boundary as one temporary file: a transcription of the
//! target contract in `docs/engine.md`, plus the `MockEngine` that
//! stands in for the real engine. The whole file is deleted when the
//! engine team's published engine and types replace it.

use std::collections::VecDeque;
use std::sync::Mutex;

pub(crate) use crate::api::{DispatchId, OfferId, ProposedCall, ToolOutcome, TrajectoryId};

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

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
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
    pending: Vec<PendingOffer>,
}

#[derive(Debug, Clone)]
struct PendingOffer {
    offer: OfferId,
    tool: String,
    bytes: Vec<u8>,
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
    Permissive,
    Offer,
    Test {
        queue: Mutex<VecDeque<EngineDecision>>,
        seen: Mutex<Vec<EngineEvent>>,
    },
}

impl MockEngine {
    /// The binary's default engine: permit everything, admit
    /// everything, append nothing. No policy is enforced.
    pub fn permissive() -> MockEngine {
        tracing::warn!(
            "a MOCK engine is deciding: every call is permitted, every result is \
             admitted, and the policy is NOT validated or enforced"
        );
        MockEngine { mode: Mode::Permissive }
    }

    /// The binary's `--mock offer` engine: block every call with a
    /// narrowing offer, authorize it at `execute_remedy_plan`. Still
    /// no policy: every call goes through, one accepted offer later.
    pub fn offer_mode() -> MockEngine {
        tracing::warn!(
            "a MOCK engine is deciding: every call is first blocked with a \
             narrowing offer, executing the offer authorizes exactly that call, \
             and the policy is NOT validated or enforced"
        );
        MockEngine { mode: Mode::Offer }
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
            Mode::Permissive | Mode::Offer => {
                panic!("enqueue is meaningful only in test mode")
            }
        }
    }

    pub fn seen(&self) -> Vec<EngineEvent> {
        match &self.mode {
            Mode::Test { seen, .. } => seen.lock().expect("mock seen lock is never poisoned").clone(),
            Mode::Permissive | Mode::Offer => panic!("seen is meaningful only in test mode"),
        }
    }

    /// Rebuild the view by replaying the family log. The mock
    /// validates nothing; the real engine validates every batch.
    /// Offer mode's facts replay into the pending-offer
    /// set; entries the mock did not write are ignored.
    pub fn rebuild_view(&self, log: &[Vec<u8>]) -> EngineView {
        let mut pending: Vec<PendingOffer> = Vec::new();
        for entry in log {
            let Ok(fact) = serde_json::from_slice::<serde_json::Value>(entry) else {
                continue;
            };
            match fact["fact"].as_str() {
                Some("turn") => pending.clear(),
                Some("offer") => {
                    let (Some(id), Some(tool), Some(call)) =
                        (fact["id"].as_str(), fact["tool"].as_str(), fact["call"].as_str())
                    else {
                        continue;
                    };
                    pending.push(PendingOffer {
                        offer: OfferId(id.to_string()),
                        tool: tool.to_string(),
                        bytes: call.as_bytes().to_vec(),
                    });
                }
                Some("executed") => {
                    let Some(id) = fact["id"].as_str() else { continue };
                    pending.retain(|entry| entry.offer.0 != id);
                }
                _ => {}
            }
        }
        EngineView {
            revision: LogRevision(log.len() as u64),
            pending,
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
            Mode::Permissive => permissive_decision(view, event),
            Mode::Offer => offer_decision(view, event),
        }
    }
}

fn permissive_decision(view: &EngineView, event: EngineEvent) -> EngineDecision {
    let then = match event {
        EngineEvent::PrincipalRequest => Next::Done,
        EngineEvent::ModelResponse { call, entropy, .. } => {
            let bytes =
                serde_json::to_vec(&call).expect("a proposed call serializes: it was deserialized from wire JSON");
            Next::ModelResponse {
                invocations: vec![ReleasedCall {
                    dispatch: mock_dispatch(view, &entropy),
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

fn offer_decision(view: &EngineView, event: EngineEvent) -> EngineDecision {
    match event {
        EngineEvent::PrincipalRequest => EngineDecision {
            append: Some(fact_batch(view, serde_json::json!({ "fact": "turn" }))),
            then: Next::Done,
        },
        EngineEvent::ModelResponse { call, entropy, .. } => {
            let bytes =
                serde_json::to_vec(&call).expect("a proposed call serializes: it was deserialized from wire JSON");
            let text = String::from_utf8(bytes).expect("serde_json writes UTF-8");
            let offer = OfferId(format!("offer-{}", hex(&entropy.0[..16])));
            let batch = fact_batch(
                view,
                serde_json::json!({
                    "fact": "offer",
                    "id": offer.0,
                    "tool": call.tool,
                    "call": text,
                }),
            );
            let feedback = format!(
                "appa: this call is blocked. A narrowing remedy is offered: run \
                 exactly this call. Call execute_remedy_plan with offer id \
                 \"{}\" to accept it, then propose the call again unchanged.",
                offer.0,
            );
            EngineDecision {
                append: Some(batch),
                then: Next::ModelResponse {
                    invocations: Vec::new(),
                    feedback: vec![Feedback {
                        text: feedback,
                        offers: vec![offer],
                    }],
                },
            }
        }
        EngineEvent::ExecuteOffer { offer, entropy, .. } => match view.pending.iter().find(|p| p.offer == offer) {
            Some(pending) => EngineDecision {
                append: Some(fact_batch(
                    view,
                    serde_json::json!({ "fact": "executed", "id": offer.0 }),
                )),
                then: Next::InvokeTool(ReleasedCall {
                    dispatch: mock_dispatch(view, &entropy),
                    tool: pending.tool.clone(),
                    bytes: pending.bytes.clone(),
                }),
            },
            None => EngineDecision {
                append: None,
                then: Next::PresentToModel(Presentation::Declined {
                    feedback: "this offer lapsed with its turn or was already executed".to_string(),
                }),
            },
        },
        EngineEvent::ToolOutcome { .. } => EngineDecision {
            append: None,
            then: Next::PresentToModel(Presentation::KeepOutput),
        },
        EngineEvent::ChildReturn { value, .. } => EngineDecision {
            append: None,
            then: match value {
                Some(value) => Next::PresentToModel(Presentation::Value { value }),
                None => Next::PresentToModel(Presentation::NoValue),
            },
        },
    }
}

fn fact_batch(view: &EngineView, fact: serde_json::Value) -> ValidatedFactBatch {
    ValidatedFactBatch {
        bytes: serde_json::to_vec(&fact).expect("a mock fact serializes"),
        based_on: view.revision(),
    }
}

fn mock_dispatch(view: &EngineView, entropy: &OfferNonce) -> DispatchId {
    DispatchId(format!("mock-dispatch-{}-{}", view.revision().0, hex(&entropy.0[..4])))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::OutcomeBody;

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
    fn offer_mode_blocks_then_authorizes_the_exact_call() {
        let engine = MockEngine::offer_mode();
        let mut log: Vec<Vec<u8>> = Vec::new();
        let call = ProposedCall {
            tool: "Bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
        };

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(
            &view,
            EngineEvent::ModelResponse {
                call: call.clone(),
                evidence: Vec::new(),
                entropy: entropy(7),
            },
        );
        let batch = decision.append.expect("the offer is remembered in a fact");
        assert_eq!(batch.based_on, LogRevision(0));
        log.push(batch.bytes);
        let offer = match decision.then {
            Next::ModelResponse { invocations, feedback } => {
                assert!(invocations.is_empty(), "offer mode releases nothing at first");
                assert_eq!(feedback.len(), 1);
                assert_eq!(feedback[0].offers.len(), 1);
                assert!(
                    feedback[0].text.contains(&feedback[0].offers[0].0),
                    "the feedback must name the offer id",
                );
                feedback[0].offers[0].clone()
            }
            other => panic!("expected a blocking ModelResponse follow-up, got {other:?}"),
        };

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(
            &view,
            EngineEvent::ExecuteOffer {
                offer: offer.clone(),
                evidence: Vec::new(),
                entropy: entropy(9),
            },
        );
        log.push(decision.append.expect("the execution is remembered in a fact").bytes);
        match decision.then {
            Next::InvokeTool(released) => {
                assert_eq!(released.tool, "Bash");
                assert_eq!(
                    released.bytes,
                    serde_json::to_vec(&call).expect("the test call serializes"),
                    "the authorized call is byte-exact",
                );
            }
            other => panic!("expected an InvokeTool follow-up, got {other:?}"),
        }

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(
            &view,
            EngineEvent::ExecuteOffer {
                offer,
                evidence: Vec::new(),
                entropy: entropy(11),
            },
        );
        assert!(matches!(
            decision.then,
            Next::PresentToModel(Presentation::Declined { .. }),
        ));
    }

    #[test]
    fn offer_mode_offers_lapse_at_the_next_prompt() {
        let engine = MockEngine::offer_mode();
        let mut log: Vec<Vec<u8>> = Vec::new();
        let call = ProposedCall {
            tool: "Bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
        };

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(
            &view,
            EngineEvent::ModelResponse {
                call,
                evidence: Vec::new(),
                entropy: entropy(3),
            },
        );
        log.push(decision.append.expect("the offer fact appends").bytes);
        let offer = match decision.then {
            Next::ModelResponse { feedback, .. } => feedback[0].offers[0].clone(),
            other => panic!("expected a blocking ModelResponse follow-up, got {other:?}"),
        };

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(&view, EngineEvent::PrincipalRequest);
        log.push(decision.append.expect("the turn fact appends").bytes);

        let view = engine.rebuild_view(&log);
        let decision = engine.handle(
            &view,
            EngineEvent::ExecuteOffer {
                offer,
                evidence: Vec::new(),
                entropy: entropy(5),
            },
        );
        assert!(matches!(
            decision.then,
            Next::PresentToModel(Presentation::Declined { .. }),
        ));
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
