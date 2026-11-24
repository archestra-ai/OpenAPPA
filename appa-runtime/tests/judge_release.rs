//! `Runtime::judge_release` over a real runtime: receipts the hooks hand out, read back from
//! the log they name, and judged into a sink's contract.

use crate::common;
use common::{actor, last_offer, propose, raw, root};

use std::sync::Arc;

use appa_runtime::api::{RemedyOutcome, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{
    HookDecision, HookEvent, OpenCalls, OutcomeBody, ProposedCall, ReleaseDigest, ReleaseReceipt, ReleaseRefusal,
    RetryHint, ToolOutcome, TrajectoryId,
};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"

[[policy.tool]]
name = "lookup"
delta = {}

[[policy.tool]]
name = "read_hr"
delta = { audience = ["alice@corp.example", "bob@corp.example"] }

[[policy.tool]]
name = "post"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
delta = {}

[[policy.tool]]
name = "post_unaudited"
requires = { effects = { excludes = ["egress"] } }
delta = {}

[[policy.tool]]
name = "post_classified"
annotator = "classifier"

[[policy.authority]]
name = "auto"
[policy.authority.permits]
audience_missing = ["carol@corp.example"]

[[policy.authority]]
name = "reviewer"
[policy.authority.permits]
audience_missing = ["dave@corp.example"]

[externals]
timeout_ms = 1000
max_body_bytes = 4096

[externals.authorities.auto]
builtin = "approve"

[externals.authorities.reviewer]
builtin = "hitl"

[externals.annotators.classifier]
url = "http://127.0.0.1:9/annotate"
"#;

fn call(tool: &str) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    }
}

async fn opened(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
                settings: Default::default(),
            }
        )
        .await,
        HookDecision::Ack
    );
    runtime
}

/// The root runs `tool` and reports `body`; the receipt the result's decision carries.
async fn result_receipt(runtime: &Arc<Runtime>, tool: &str, body: &str) -> ReleaseReceipt {
    assert_eq!(
        propose(runtime, call(tool)).await,
        HookDecision::AllowCall { spawn: None }
    );
    let outcome = hooks::handle_embedded(
        runtime,
        HookEvent::ToolResult {
            actor: actor(),
            call: call(tool),
            call_id: None,
            outcome: ToolOutcome::Success {
                body: OutcomeBody::Available(body.to_string()),
            },
        },
    )
    .await;
    assert_eq!(outcome.decision, HookDecision::Ack);
    let receipt = outcome.receipt.expect("a delivered result carries its receipt");
    assert_eq!(receipt.session, root());
    assert_eq!(receipt.digest, ReleaseDigest::of(body.as_bytes()));
    receipt
}

/// The root's audience narrowed to Alice and Bob by an accepted `read_hr`, and its result's
/// receipt.
async fn narrowed(runtime: &Arc<Runtime>) -> ReleaseReceipt {
    let HookDecision::DenyCall { feedback, .. } = propose(runtime, call("read_hr")).await else {
        panic!("the narrowing read is offered for acceptance");
    };
    assert!(matches!(
        runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
        RemedyOutcome::Authorized { .. }
    ));
    result_receipt(runtime, "read_hr", "the HR record").await
}

async fn turn_end(runtime: &Arc<Runtime>, answer: Option<&str>) -> appa_runtime::api::EmbeddedHookOutcome {
    hooks::handle_embedded(
        runtime,
        HookEvent::TurnEnd {
            actor: actor(),
            open_calls: OpenCalls::Abandon,
            answer: answer.map(str::to_string),
        },
    )
    .await
}

async fn judge(runtime: &Runtime, receipts: &[ReleaseReceipt], sink: &str, to: &str) -> Result<(), ReleaseRefusal> {
    runtime
        .judge_release(receipts, sink, &raw(serde_json::json!({ "to": to })))
        .await
}

#[tokio::test]
async fn a_result_releases_to_recipients_its_label_covers_or_stock_approve_covers() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    let hr = narrowed(&runtime).await;

    assert_eq!(
        judge(&runtime, std::slice::from_ref(&hr), "post", "alice@corp.example").await,
        Ok(())
    );
    assert_eq!(
        judge(&runtime, std::slice::from_ref(&hr), "post", "carol@corp.example").await,
        Ok(())
    );
    assert_eq!(
        judge(&runtime, std::slice::from_ref(&hr), "post", "dave@corp.example").await,
        Err(ReleaseRefusal::HardBlock),
        "only a stock approve covers a release's gap; a person's review is never asked"
    );
    assert_eq!(
        judge(&runtime, &[hr], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::HardBlock)
    );
}

#[tokio::test]
async fn the_content_label_is_the_meet_of_its_components() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    let plain = result_receipt(&runtime, "lookup", "public facts").await;
    let hr = narrowed(&runtime).await;

    assert_eq!(
        judge(&runtime, std::slice::from_ref(&plain), "post", "eve@corp.example").await,
        Ok(())
    );
    assert_eq!(
        judge(&runtime, &[plain, hr], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::HardBlock)
    );
}

#[tokio::test]
async fn a_root_answer_is_recorded_at_the_roots_label() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    narrowed(&runtime).await;

    let ended = turn_end(&runtime, Some("the summary")).await;
    assert_eq!(ended.decision, HookDecision::Ack);
    let answer = ended.receipt.expect("a recorded answer carries its receipt");
    assert_eq!(answer.digest, ReleaseDigest::of(b"the summary"));
    assert_eq!(
        judge(&runtime, std::slice::from_ref(&answer), "post", "bob@corp.example").await,
        Ok(())
    );
    assert_eq!(
        judge(&runtime, &[answer], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::HardBlock)
    );
}

#[tokio::test]
async fn an_absent_answer_records_nothing_and_an_empty_one_is_an_answer() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;

    let silent = turn_end(&runtime, None).await;
    assert_eq!(silent.decision, HookDecision::Ack);
    assert_eq!(silent.receipt, None);

    let empty = turn_end(&runtime, Some(""))
        .await
        .receipt
        .expect("an empty answer is recorded");
    assert_eq!(empty.digest, ReleaseDigest::of(b""));
    assert_eq!(judge(&runtime, &[empty], "post", "eve@corp.example").await, Ok(()));
}

#[tokio::test]
async fn a_retried_answer_keeps_its_receipt_and_a_changed_one_gets_another() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;

    let first = turn_end(&runtime, Some("draft")).await.receipt.expect("recorded");
    let retried = turn_end(&runtime, Some("draft")).await.receipt.expect("recorded");
    assert_eq!(retried, first, "the same bytes again name the same record");

    let changed = turn_end(&runtime, Some("final")).await.receipt.expect("recorded");
    assert_ne!(changed.fact_position, first.fact_position);
    assert_eq!(changed.digest, ReleaseDigest::of(b"final"));
    assert_eq!(
        judge(&runtime, std::slice::from_ref(&changed), "post", "eve@corp.example").await,
        Ok(())
    );
    let stale = ReleaseReceipt {
        digest: first.digest,
        ..changed
    };
    assert_eq!(
        judge(&runtime, &[stale], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::Integrity),
        "the earlier bytes do not stand where the changed answer was recorded"
    );
}

#[tokio::test]
async fn a_receipt_that_disagrees_with_its_log_is_an_integrity_failure() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    let receipt = result_receipt(&runtime, "lookup", "public facts").await;

    let forged = [
        ReleaseReceipt {
            digest: ReleaseDigest::of(b"other facts"),
            ..receipt.clone()
        },
        ReleaseReceipt {
            session: TrajectoryId("another-root".to_string()),
            ..receipt.clone()
        },
        ReleaseReceipt {
            fact_position: 0,
            ..receipt.clone()
        },
        ReleaseReceipt {
            fact_position: receipt.fact_position + 1000,
            ..receipt.clone()
        },
    ];
    for forged in forged {
        assert_eq!(
            judge(&runtime, &[receipt.clone(), forged.clone()], "post", "eve@corp.example").await,
            Err(ReleaseRefusal::Integrity),
            "{forged:?}"
        );
    }
}

#[tokio::test]
async fn content_without_a_receipt_is_a_missing_fact() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    assert_eq!(
        judge(&runtime, &[], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::MissingFact(RetryHint::AfterRepair))
    );
}

#[tokio::test]
async fn a_sink_that_reads_history_or_an_annotator_or_no_declaration_is_an_integrity_failure() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    let receipt = result_receipt(&runtime, "lookup", "public facts").await;
    for sink in ["post_unaudited", "post_classified", "mail"] {
        assert_eq!(
            runtime
                .judge_release(std::slice::from_ref(&receipt), sink, &raw(serde_json::json!({})))
                .await,
            Err(ReleaseRefusal::Integrity),
            "{sink}"
        );
    }
}

#[tokio::test]
async fn a_judgment_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    let hr = narrowed(&runtime).await;
    let before = common::audit_len(&runtime);
    for to in ["alice@corp.example", "carol@corp.example", "eve@corp.example"] {
        let _ = judge(&runtime, std::slice::from_ref(&hr), "post", to).await;
    }
    assert_eq!(common::audit_len(&runtime), before);
}

/// The root has read the HR record (audience Alice and Bob), then runs a public tool whose
/// result may echo what the root sent it. The result releases at the label the root sent the
/// call under, met with the tool's own: the echo never reaches Eve.
#[tokio::test]
async fn a_result_released_after_a_narrowing_read_carries_the_label_its_call_was_sent_under() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = opened(&dir).await;
    narrowed(&runtime).await;
    let echoed = result_receipt(&runtime, "lookup", "no match for: the HR record").await;
    assert_eq!(
        judge(&runtime, &[echoed], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::HardBlock),
        "a result computed from arguments the narrowed root wrote is no more public than the root"
    );
}

const CHAIN_V1: &str = r#"
[policy]
version = 2
trust_chain = ["low", "vetted", "mid", "high"]

[[policy.tool]]
name = "read_mid"
delta = { trust = "mid" }

[[policy.tool]]
name = "read_vetted"
delta = { trust = "vetted" }

[[policy.tool]]
name = "post"
requires = { trust = "vetted" }
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

const CHAIN_V2: &str = r#"
[policy]
version = 2
trust_chain = ["low", "vetted", "high"]

[[policy.tool]]
name = "read_vetted"
delta = { trust = "vetted" }

[[policy.tool]]
name = "post"
requires = { trust = "vetted" }
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#;

/// An answer's label is rebuilt from the terms the root's label was folded from, crossed by
/// rank name: across a revision that drops a rank it lowers as the root's own label does.
#[tokio::test]
async fn an_answers_label_crosses_a_revision_as_the_roots_label_does() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, CHAIN_V1).expect("the fixture writes");
    let runtime = Arc::new(
        Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens"),
    );
    assert_eq!(
        hooks::handle(
            &runtime,
            HookEvent::SessionStart {
                root: root(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
                settings: Default::default(),
            }
        )
        .await,
        HookDecision::Ack
    );
    for (tool, body) in [("read_mid", "a mid fact"), ("read_vetted", "a vetted fact")] {
        if let HookDecision::DenyCall { feedback, .. } = propose(&runtime, call(tool)).await {
            assert!(matches!(
                runtime.execute_remedy(&actor(), last_offer(&feedback)).await,
                RemedyOutcome::Authorized { .. }
            ));
            result_receipt(&runtime, tool, body).await;
        } else {
            panic!("{tool} narrows the root and is offered for acceptance");
        }
    }
    let answer = turn_end(&runtime, Some("built from both facts"))
        .await
        .receipt
        .expect("recorded");
    assert_eq!(runtime.status(&root()).expect("status").trust, "vetted");

    std::fs::write(&path, CHAIN_V2).expect("the fixture writes");
    runtime
        .reload(Config::load(&path).expect("the revision validates"))
        .expect("the revision publishes");
    let root_trust = runtime.status(&root()).expect("status").trust;
    let judged = runtime
        .judge_release(&[answer], "post", &raw(serde_json::json!({})))
        .await;
    assert_eq!(
        (root_trust.as_str(), judged),
        ("low", Err(ReleaseRefusal::HardBlock)),
        "the root stands at the lowest rank under the head, and so does what it answered"
    );
}

/// Components of families that act for different readers do not combine; components of
/// families acting for one reader do.
#[tokio::test]
async fn components_of_two_families_combine_only_under_one_principal() {
    use appa_runtime_api::Actor;
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, POLICY).expect("the fixture writes");
    let runtime = Arc::new(
        Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the deployment opens"),
    );
    let mut receipts = Vec::new();
    for (name, principal) in [
        ("fam-a", "alice@corp.example"),
        ("fam-b", "bob@corp.example"),
        ("fam-c", "alice@corp.example"),
    ] {
        let root = TrajectoryId(name.to_string());
        assert_eq!(
            hooks::handle(
                &runtime,
                HookEvent::SessionStart {
                    root: root.clone(),
                    principal: Some(principal.to_string()),
                    address: None,
                    title: None,
                    launch: None,
                    start: None,
                    settings: Default::default(),
                }
            )
            .await,
            HookDecision::Ack
        );
        let ended = hooks::handle_embedded(
            &runtime,
            HookEvent::TurnEnd {
                actor: Actor { root, child: None },
                open_calls: OpenCalls::Abandon,
                answer: Some(format!("answer of {name}")),
            },
        )
        .await;
        receipts.push(ended.receipt.expect("recorded"));
    }
    let [a, b, c] = receipts.try_into().expect("three receipts");
    assert_eq!(
        judge(&runtime, &[a.clone(), b], "post", "eve@corp.example").await,
        Err(ReleaseRefusal::Integrity)
    );
    assert_eq!(judge(&runtime, &[a, c], "post", "eve@corp.example").await, Ok(()));
}
