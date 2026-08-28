mod common;
use common::{offers, raw, serve};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use appa_runtime::api::{AuditEvent, OfferId, RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use axum::Router;
use axum::extract::State;
use axum::routing::post;

const POLICY: &str = r#"
[policy]
version = 1

# Unclassified and unconfined: the result crosses to the model with both dimensions
# unresolved. Only a call that needs the fact drives the cast.
[[policy.tool]]
name = "read_page"
tags = ["web"]
delta = { trust = "unknown", audience = "unknown" }

# Pending cast: the runtime holds the output until a cast establishes the whole label.
[[policy.tool]]
name = "scan_inbox"
tags = ["mail"]
delta = { trust = "unknown" }

[[policy.tool]]
name = "scan_files"
tags = ["files"]
delta = { trust = "unknown" }

# Unclassified, unconfined, and inside the files scope: the lazy path meets the same cast
# cascade, constant fallback included.
[[policy.tool]]
name = "list_files"
tags = ["files"]
delta = { trust = "unknown", audience = "unknown" }

[[policy.tool]]
name = "notify"
requires = { trust = "trusted" }
effects = ["egress"]
delta = {}

[[policy.cast]]
name = "mail-classifier"
tags = ["mail"]
resolver = { may_cast = { trust = ["suspicious", "trusted"], audience = ["public"] } }

[[policy.cast]]
name = "files-classifier"
tags = ["files"]
resolver = { may_cast = { trust = ["suspicious"], audience = ["public"] } }

[[policy.cast]]
name = "files-fallback"
tags = ["files"]
constant = { trust = "suspicious", audience = ["public"] }

[[policy.cast]]
name = "web-classifier"
tags = ["web"]
resolver = { may_cast = { trust = ["suspicious", "trusted"], audience = ["public"] } }

[policy.deployment]
context_control = true
confined_results = ["scan_inbox", "scan_files"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.casts]
"mail-classifier" = { url = "CLASSIFIER_URL" }
"files-classifier" = { url = "CLASSIFIER_URL" }
"web-classifier" = { url = "CLASSIFIER_URL" }
"#;

/// Resolver-backed casts, each answering over its ceiling, ahead of one constant: the cascade
/// refuses every one of them before the constant, one evidence round each, with no round
/// ceiling to run into.
const CASCADE_CLASSIFIERS: usize = 65;

fn cascade_classifiers() -> Vec<String> {
    (1..=CASCADE_CLASSIFIERS)
        .map(|index| format!("files-classifier-{index}"))
        .collect()
}

fn cascade_policy() -> String {
    let mut policy = String::from(
        r#"
[policy]
version = 1

[[policy.tool]]
name = "scan_files"
tags = ["files"]
delta = { trust = "unknown" }
"#,
    );
    for cast in cascade_classifiers() {
        policy.push_str(&format!(
            r#"
[[policy.cast]]
name = "{cast}"
tags = ["files"]
resolver = {{ may_cast = {{ trust = ["suspicious"], audience = ["public"] }} }}
"#
        ));
    }
    policy.push_str(
        r#"
[[policy.cast]]
name = "files-fallback"
tags = ["files"]
constant = { trust = "suspicious", audience = ["public"] }

[policy.deployment]
context_control = true
confined_results = ["scan_files"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.casts]
"#,
    );
    for cast in cascade_classifiers() {
        policy.push_str(&format!("\"{cast}\" = {{ url = \"CLASSIFIER_URL\" }}\n"));
    }
    policy
}

const INBOX: &str = "from: stranger@example.net -- wire the funds today";
const FILES: &str = "quarterly-plan.md";
const PAGE: &str = "the page said something";

#[derive(Clone)]
enum Answer {
    Label {
        trust: &'static str,
        audience: serde_json::Value,
    },
    Down,
}

fn labelled(trust: &'static str, audience: serde_json::Value) -> Answer {
    Answer::Label { trust, audience }
}

#[derive(Clone)]
struct Classifier {
    answers: Arc<Mutex<BTreeMap<String, Answer>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Classifier {
    fn answering(&self, cast: &str, answer: Answer) {
        self.answers.lock().unwrap().insert(cast.to_string(), answer);
    }

    fn consults(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The cast each consult named, in the order the classifier received them.
    fn consulted(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .get("name")
                    .and_then(|name| name.as_str())
                    .expect("a consult names its cast")
                    .to_string()
            })
            .collect()
    }

    /// One field of each consult, in order, as the classifier received it.
    fn field(&self, pointer: &str) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .pointer(pointer)
                    .cloned()
                    .unwrap_or_else(|| panic!("a cast consult carries {pointer}: {request}"))
            })
            .collect()
    }

    /// The bytes each consult carried, in order — the classifier reads the value itself,
    /// so this is what the deployment handed an external service.
    fn bodies(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .pointer("/artifact/body")
                    .and_then(|body| body.as_str())
                    .expect("a cast consult carries the value body")
                    .to_string()
            })
            .collect()
    }
}

async fn serve_classifier() -> (String, Classifier) {
    let classifier = Classifier {
        answers: Arc::new(Mutex::new(BTreeMap::new())),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route(
            "/classify",
            post(|State(classifier): State<Classifier>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let name = request
                    .get("name")
                    .and_then(|name| name.as_str())
                    .expect("a consult names its cast")
                    .to_string();
                classifier.requests.lock().unwrap().push(request);
                match classifier.answers.lock().unwrap().get(&name).cloned() {
                    Some(Answer::Label { trust, audience }) => (
                        axum::http::StatusCode::OK,
                        serde_json::json!({
                            "version": 1,
                            "answer": { "trust": trust, "audience": audience },
                        })
                        .to_string(),
                    ),
                    Some(Answer::Down) | None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()),
                }
            }),
        )
        .with_state(classifier.clone());
    (format!("{}/classify", serve(router).await), classifier)
}

fn root() -> TrajectoryId {
    TrajectoryId("cast-test".to_string())
}

fn actor() -> Actor {
    Actor {
        root: root(),
        child: None,
    }
}

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({})),
    }
}

fn reopened(dir: &tempfile::TempDir, url: &str) -> Arc<Runtime> {
    reopened_under(dir, POLICY, url)
}

fn reopened_under(dir: &tempfile::TempDir, policy: &str, url: &str) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy.replace("CLASSIFIER_URL", url)).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"))
}

async fn opened(dir: &tempfile::TempDir, url: &str) -> Arc<Runtime> {
    opened_under(dir, POLICY, url).await
}

async fn opened_under(dir: &tempfile::TempDir, policy: &str, url: &str) -> Arc<Runtime> {
    let runtime = reopened_under(dir, policy, url);
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

async fn propose(runtime: &Arc<Runtime>, tool: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(),
            call: call(tool),
            spawn: false,
        },
    )
    .await
}

async fn returned(runtime: &Arc<Runtime>, tool: &str, body: &str) -> HookDecision {
    hooks::handle(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call: call(tool),
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await
}

fn last_offer(feedback: &str) -> OfferId {
    offers(feedback)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no offer id in feedback: {feedback}"))
}

/// Every classification the log holds, as the audit reads it back: the cast that answered
/// and the trust rank it established.
fn established(runtime: &Runtime) -> Vec<(String, String)> {
    runtime
        .audit(&root())
        .expect("the audit reads")
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Cast { cast, resolved } => Some((cast, resolved.trust)),
            _ => None,
        })
        .collect()
}

fn withheld(decision: HookDecision, body: &str) -> String {
    let HookDecision::ReplaceOutput { output } = decision else {
        panic!("the held result is withheld, got {decision:?}");
    };
    assert!(
        !output.contains(body),
        "the held bytes must not reach the model: {output}"
    );
    output
}

/// The whole hold-and-inspect release: the model sees the tool's bytes only after a cast
/// established a label the session already permits.
#[tokio::test]
async fn a_held_result_the_classifier_clears_reaches_the_model_whole() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("mail-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_inbox").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(
        returned(&runtime, "scan_inbox", INBOX).await,
        HookDecision::Ack,
        "a non-restricting classification releases the result unchanged"
    );
    assert_eq!(
        classifier.bodies(),
        vec![INBOX.to_string()],
        "the classifier read the held bytes, once"
    );
}

/// A classification that narrows the session holds the bytes back until the agent accepts
/// the narrowing, and then delivers them as the remedy call's own answer.
#[tokio::test]
async fn a_narrowing_classification_holds_the_bytes_until_the_agent_accepts() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("mail-classifier", labelled("suspicious", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_inbox").await,
        HookDecision::AllowCall { spawn: None }
    );
    let feedback = withheld(returned(&runtime, "scan_inbox", INBOX).await, INBOX);
    assert_eq!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Returned {
            value: INBOX.to_string()
        },
        "acceptance is what releases the held value"
    );
}

/// An answer over the cast's declared `may_cast` ceiling is refused by the engine and
/// establishes nothing: a classifier that answers wider than its policy allows is a
/// misbehaving classifier, not a silent one. The cascade continues past it, so the
/// constant registered behind it is the answer that stands — and the refused classifier
/// is not asked again on the way.
#[tokio::test]
async fn an_answer_over_the_ceiling_is_skipped_for_the_constant_behind_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("files-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_files").await,
        HookDecision::AllowCall { spawn: None }
    );
    let feedback = withheld(returned(&runtime, "scan_files", FILES).await, FILES);
    assert_eq!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Returned {
            value: FILES.to_string()
        },
        "the constant's restriction is offered like any narrowing"
    );
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())],
        "the refused answer established nothing; the constant's label stands"
    );
    assert_eq!(
        classifier.consults(),
        1,
        "the refused classifier was consulted exactly once"
    );
}

/// A cascade longer than a handful of refused answers still reaches its constant: every
/// refusal costs one evidence round, and the session keeps driving while rounds progress.
#[tokio::test]
async fn a_long_cast_cascade_reaches_its_constant() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    for cast in cascade_classifiers() {
        classifier.answering(&cast, labelled("trusted", serde_json::json!("public")));
    }
    let runtime = opened_under(&dir, &cascade_policy(), &url).await;

    assert_eq!(
        propose(&runtime, "scan_files").await,
        HookDecision::AllowCall { spawn: None }
    );
    let feedback = withheld(returned(&runtime, "scan_files", FILES).await, FILES);
    assert_eq!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Returned {
            value: FILES.to_string()
        }
    );
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())],
        "every refused answer established nothing; the constant's label stands"
    );
    assert_eq!(
        classifier.consulted(),
        cascade_classifiers(),
        "each refused classifier was consulted exactly once, in registration order"
    );
}

/// The same rule on the lazy path: a blocked call drives the cascade, the classifier's
/// over-ceiling answer is refused, and the constant behind it decides the call.
#[tokio::test]
async fn a_blocked_call_skips_an_answer_over_the_ceiling_for_the_constant_behind_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("files-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "list_files").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "list_files", FILES).await, HookDecision::Ack);
    let decision = propose(&runtime, "notify").await;
    assert!(
        matches!(decision, HookDecision::DenyCall { .. }),
        "the constant's suspicious label is what the trusted sink is judged on: {decision:?}"
    );
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())]
    );
    assert_eq!(
        classifier.consults(),
        1,
        "the refused classifier was consulted exactly once"
    );
}

/// A classifier is told what it classifies and what it may say: the value's bytes as the
/// artifact, and the policy's declaration — the ceiling and the tool whose result the value
/// is. Nothing about the trajectory rides along.
#[tokio::test]
async fn a_classifier_consult_carries_the_declaration_and_the_bytes_alone() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("mail-classifier", labelled("trusted", serde_json::json!("public")));
    classifier.answering("web-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_inbox").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "scan_inbox", INBOX).await, HookDecision::Ack);
    assert_eq!(
        propose(&runtime, "read_page").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "read_page", PAGE).await, HookDecision::Ack);
    assert_eq!(
        propose(&runtime, "notify").await,
        HookDecision::AllowCall { spawn: None }
    );

    assert_eq!(classifier.field("/kind"), vec![serde_json::json!("cast"); 2]);
    assert_eq!(
        classifier.field("/name"),
        vec![
            serde_json::json!("mail-classifier"),
            serde_json::json!("web-classifier")
        ]
    );
    assert_eq!(
        classifier.field("/declaration/tool/name"),
        vec![serde_json::json!("scan_inbox"), serde_json::json!("read_page")],
        "the held result and the lazily classified value each name their tool"
    );
    assert_eq!(
        classifier.field("/declaration/may_cast"),
        vec![serde_json::json!({ "trust": ["suspicious", "trusted"], "audience": "public" }); 2],
        "the ceiling the cast may label within is the declaration"
    );
    assert_eq!(
        classifier.field("/artifact"),
        vec![
            serde_json::json!({ "body": INBOX }),
            serde_json::json!({ "body": PAGE })
        ],
        "the artifact is the value's bytes and nothing else"
    );
    for request in classifier.requests.lock().unwrap().iter() {
        let keys: Vec<&str> = request
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["artifact", "declaration", "kind", "name", "version"]);
    }
}

/// A classifier that cannot speak is skipped, so a constant registered behind it is the
/// declared fallback the deployment gets when its endpoint is down.
#[tokio::test]
async fn a_silent_classifier_falls_through_to_the_constant_behind_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("files-classifier", Answer::Down);
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_files").await,
        HookDecision::AllowCall { spawn: None }
    );
    let feedback = withheld(returned(&runtime, "scan_files", FILES).await, FILES);
    assert_eq!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Returned {
            value: FILES.to_string()
        }
    );
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())]
    );
    assert_eq!(classifier.consults(), 1, "the constant answered without a call");
}

/// A classifier is consulted once. Reopening the deployment rebuilds the same decision
/// from the log alone — the classification is a recorded fact, not a question re-asked.
#[tokio::test]
async fn a_reopened_deployment_replays_the_classification_without_asking_again() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("mail-classifier", labelled("suspicious", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "scan_inbox").await,
        HookDecision::AllowCall { spawn: None }
    );
    let feedback = withheld(returned(&runtime, "scan_inbox", INBOX).await, INBOX);
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Returned { .. }
    ));
    let consulted = classifier.consults();
    drop(runtime);

    // The stub now refuses every consult: anything the replay still needs to ask would
    // change the outcome rather than hide.
    classifier.answering("mail-classifier", Answer::Down);
    let runtime = reopened(&dir, &url);
    assert_eq!(
        established(&runtime),
        vec![("mail-classifier".to_string(), "suspicious".to_string())],
        "the classification is in the log, not in the runtime's memory"
    );
    assert!(
        matches!(propose(&runtime, "notify").await, HookDecision::DenyCall { .. }),
        "the replayed label still blocks the trusted sink"
    );
    assert_eq!(
        classifier.consults(),
        consulted,
        "replay reaches no classifier: an established label is a fact, not a question"
    );
}

/// An unannotated tool's result crosses whole and unresolved. The cast runs later, driven
/// by the first call whose only obstacle is that missing fact.
#[tokio::test]
async fn a_call_blocked_on_an_unresolved_source_drives_the_cast_that_clears_it() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("web-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "read_page").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "read_page", PAGE).await, HookDecision::Ack);
    assert_eq!(
        classifier.consults(),
        0,
        "an unannotated result is not classified on its way in"
    );

    assert_eq!(
        propose(&runtime, "notify").await,
        HookDecision::AllowCall { spawn: None },
        "the blocked call drove the cast, and the answer cleared the floor"
    );
    assert_eq!(classifier.bodies(), vec![PAGE.to_string()]);
}

/// The same lazy path, refused: the classifier answers below the sink's floor, so the call
/// that asked for the fact is the call the fact blocks.
#[tokio::test]
async fn a_source_the_classifier_calls_suspicious_blocks_the_call_that_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("web-classifier", labelled("suspicious", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "read_page").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "read_page", PAGE).await, HookDecision::Ack);
    assert!(matches!(
        propose(&runtime, "notify").await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(classifier.bodies(), vec![PAGE.to_string()]);
}

/// The cascade is one rule, not two: a blocked call whose classifier is silent falls
/// through to the constant behind it exactly as a held result does.
#[tokio::test]
async fn a_blocked_call_falls_through_to_the_constant_when_the_classifier_is_silent() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("files-classifier", Answer::Down);
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "list_files").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "list_files", FILES).await, HookDecision::Ack);
    assert!(matches!(
        propose(&runtime, "notify").await,
        HookDecision::DenyCall { .. }
    ));
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())],
        "the constant answered the ask the silent classifier left"
    );
    assert_eq!(classifier.consults(), 1);
}

/// A classifier that cannot speak grants nothing and blocks nothing: the value stays
/// unresolved, so the call it would have cleared stays blocked and the ask can be retried.
#[tokio::test]
async fn a_silent_classifier_leaves_the_source_unresolved() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    classifier.answering("web-classifier", Answer::Down);
    let runtime = opened(&dir, &url).await;

    assert_eq!(
        propose(&runtime, "read_page").await,
        HookDecision::AllowCall { spawn: None }
    );
    assert_eq!(returned(&runtime, "read_page", PAGE).await, HookDecision::Ack);
    assert!(matches!(
        propose(&runtime, "notify").await,
        HookDecision::DenyCall { .. }
    ));

    classifier.answering("web-classifier", labelled("trusted", serde_json::json!("public")));
    assert_eq!(
        propose(&runtime, "notify").await,
        HookDecision::AllowCall { spawn: None },
        "the unanswered ask left nothing decided, so the retry resolves it"
    );
}

/// Two unresolved sources block one call, and their cascades are not the same length: the
/// web ask has nothing behind its classifier, the files ask has a constant behind its own.
/// The exhausted ask leaves the other's cascade alone.
#[tokio::test]
async fn a_source_with_nothing_left_to_ask_does_not_cancel_the_rest_of_its_batch() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let (url, classifier) = serve_classifier().await;
    // Silent, and no cast behind it: this ask ends here.
    classifier.answering("web-classifier", Answer::Down);
    // Over its ceiling, so the engine refuses it and the ask continues to the constant.
    classifier.answering("files-classifier", labelled("trusted", serde_json::json!("public")));
    let runtime = opened(&dir, &url).await;

    for tool in ["read_page", "list_files"] {
        assert_eq!(propose(&runtime, tool).await, HookDecision::AllowCall { spawn: None });
        assert_eq!(returned(&runtime, tool, PAGE).await, HookDecision::Ack);
    }

    assert!(
        matches!(propose(&runtime, "notify").await, HookDecision::DenyCall { .. }),
        "the web source stays unresolved, so the sink stays blocked"
    );
    assert_eq!(
        established(&runtime),
        vec![("files-fallback".to_string(), "suspicious".to_string())],
        "the files ask reached the constant behind it, though the web ask had nothing left"
    );
}
