//! `share_legal_packet` reads a finance file server-side and emails it, so the
//! packet never passes through the agent's arguments. Each shipped policy that
//! registers the composite labels that packet with `forwards`, and the
//! recipient check reads it before dispatch: from a clean, public trajectory,
//! an outsider is refused and no email exists, while an authorized recipient
//! gets the packet once the agent accepts the result's narrowing.
//!
//! The real agent loop, runtime, shim and `corp-systems` run here; only
//! inference is scripted, and annotated policies consult a loopback annotator
//! that serves the scenario's own `annotator_answer` fixtures.

mod model;

use std::path::Path;
use std::sync::Arc;

use appa_example_agent::wire::WireMessage;
use appa_example_agent::{
    Agent, Limits, OpenAiCompatible, OpenAiConfig, Outcome, ToolCatalogue, ToolShim, TranscriptHead,
};
use appa_runtime::api::{AuditEvent, Runtime, TrajectoryId};
use appa_runtime::config::{Config, Transport};
use corp_systems::systems::System;
use corporate_agent_demo::catalogue;
use corporate_agent_demo::shim::{self, CorpWorld};
use model::Provider;

const PACKET: &str = "legal-packet.md";
const MARKER: &str = "PACKET-MARKER-5150";
const OUTSIDE_COUNSEL: &str = "outside-counsel@external.example";

struct Policy {
    name: &'static str,
    toml: &'static str,
    /// The scenario manifest whose `annotator_answer` fixtures the policy's
    /// annotators consult, for an annotated policy.
    answers: Option<&'static str>,
    packet: &'static str,
}

const CORP: Policy = Policy {
    name: "corp",
    toml: include_str!("../../corp/policies/appa.toml"),
    answers: None,
    packet: PACKET,
};
const SHARE_LEGAL_PACKET: Policy = Policy {
    name: "share-legal-packet",
    toml: include_str!("../../corp/scenarios/share-legal-packet/policy/appa.toml"),
    answers: None,
    packet: PACKET,
};
const JOINT_MERGER_BRIEF: Policy = Policy {
    name: "joint-merger-brief",
    toml: include_str!("../../corp/scenarios/joint-merger-brief/policy/appa.toml"),
    answers: None,
    packet: PACKET,
};
const ROUTE_PROJECT_PACKET: Policy = Policy {
    name: "route-project-packet",
    toml: include_str!("../../corp/scenarios/route-project-packet/policy/appa.toml"),
    answers: Some(include_str!("../../corp/scenarios/route-project-packet/scenario.toml")),
    packet: "project-onyx-packet.md",
};

fn root() -> TrajectoryId {
    TrajectoryId("share-legal-packet".to_string())
}

/// Serve the scenario's annotator answers as the bench runner does: the
/// annotator's name and the exact `args` it was sent are the whole key, and an
/// unknown consult is a 404, which grants nothing.
async fn annotator(manifest: &str) -> String {
    let manifest: toml::Value = toml::from_str(manifest).expect("the scenario manifest parses");
    let answers: Vec<(String, serde_json::Value, serde_json::Value)> = manifest["annotator_answer"]
        .as_array()
        .expect("the scenario answers its annotators")
        .iter()
        .map(|answer| {
            let json = serde_json::to_value(answer).expect("an answer is plain data");
            (
                json["annotator"]
                    .as_str()
                    .expect("an answer names its annotator")
                    .to_string(),
                json["args"].clone(),
                json["annotation"].clone(),
            )
        })
        .collect();
    let app = axum::Router::new().route(
        "/annotator",
        axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
            let answer = answers
                .iter()
                .find(|(name, args, _)| {
                    request["kind"] == "annotation" && request["name"] == *name && request["artifact"]["args"] == *args
                })
                .map(|(_, _, annotation)| annotation.clone());
            async move {
                match answer {
                    Some(annotation) => (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({ "version": 1, "answer": annotation })),
                    ),
                    None => (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({}))),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// The emails the sink holds after one episode.
fn sent(sink: &Path) -> Vec<String> {
    match std::fs::read_dir(sink.join(System::Email.dir_name())) {
        Ok(entries) => entries
            .map(|entry| std::fs::read_to_string(entry.expect("the sink lists").path()).expect("an email reads"))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("the sink lists: {error}"),
    }
}

struct Run {
    outcome: Outcome,
    released: bool,
    sent: Vec<String>,
}

async fn share(policy: &Policy, to: &str, provider: &Provider) -> Run {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let data = dir.path().join("data");
    let sink = dir.path().join("sink");
    std::fs::create_dir_all(data.join("finance")).expect("the corpus writes");
    std::fs::write(data.join("finance").join(policy.packet), format!("{MARKER}\n")).expect("the packet writes");

    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy.toml).expect("the policy writes");
    let mut config = Config::load(&path).expect("the policy is a loadable deployment");
    let compiled = appa_policy::Config::from_toml_str(
        &toml::to_string(config.policy_file().value()).expect("the policy re-renders"),
    )
    .expect("the policy compiles");
    if let Some(manifest) = policy.answers {
        let origin = annotator(manifest).await;
        for transport in config.externals.annotators.values_mut() {
            if let Transport::Url(endpoint) = transport
                && let Some(path) = endpoint.url.strip_prefix("http://127.0.0.1:0")
            {
                endpoint.url = format!("{origin}{path}");
            }
        }
    }

    let address = shim::serve(CorpWorld {
        data_root: data,
        sink_root: sink.clone(),
        enabled: System::ALL.into_iter().collect(),
    })
    .await
    .expect("the shim binds");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    let agent = Agent::new(
        Arc::clone(&runtime),
        OpenAiCompatible::with_http_client(
            OpenAiConfig::new(provider.clone().serve().await, "fixture/model", "test-key"),
            appa_example_agent::HttpClient::loopback(),
        ),
        ToolShim::new(format!("http://{address}{}", shim::TOOLS_PATH)),
        ToolCatalogue::new(catalogue::advertised(&compiled, false))
            .expect("the policy's tools leave the control name to the runtime"),
    )
    .with_head(TranscriptHead::new(vec![WireMessage::system(
        "You are a corporate assistant.",
    )]))
    .with_limits(Limits::default());

    let outcome = agent
        .run(
            root(),
            &format!("share {} with {to}", policy.packet),
            Default::default(),
        )
        .await;
    let released = runtime
        .audit(&root())
        .expect("the audit reads")
        .iter()
        .any(|entry| matches!(&entry.event, AuditEvent::Released { tool, .. } if tool == "share_legal_packet"));
    Run {
        outcome,
        released,
        sent: sent(&sink),
    }
}

/// The agent proposes the share, accepts any narrowing offered, and proposes
/// it again byte-for-byte (`RUL-5`).
fn sharing(packet: &str, to: &str) -> Provider {
    let provider = Provider::default();
    let arguments = serde_json::json!({ "file": packet, "to": to });
    provider
        .calls("share_legal_packet", arguments.clone())
        .pursues_first_or_says("refused")
        .calls("share_legal_packet", arguments)
        .says("done");
    provider
}

async fn refused(policy: &Policy, outsider: &str) {
    let run = share(policy, outsider, &sharing(policy.packet, outsider)).await;
    assert!(
        matches!(run.outcome, Outcome::Answer(_)),
        "{}: {:?}",
        policy.name,
        run.outcome
    );
    assert!(!run.released, "{}: the outsider share must not dispatch", policy.name);
    assert!(
        run.sent.is_empty(),
        "{}: no email may leave: {:?}",
        policy.name,
        run.sent
    );
}

async fn delivered(policy: &Policy, recipient: &str) {
    let run = share(policy, recipient, &sharing(policy.packet, recipient)).await;
    assert_eq!(run.outcome, Outcome::Answer("done".to_string()), "{}", policy.name);
    assert!(run.released, "{}: the authorized share dispatches", policy.name);
    match run.sent.as_slice() {
        [email] => assert!(
            email.starts_with(&format!("To: {recipient}\n")) && email.contains(MARKER),
            "{}: the packet reaches its recipient: {email}",
            policy.name,
        ),
        other => panic!("{}: exactly one email leaves, not {other:?}", policy.name),
    }
}

#[tokio::test]
async fn the_corp_policy_sends_the_finance_packet_only_to_its_readers() {
    refused(&CORP, OUTSIDE_COUNSEL).await;
    delivered(&CORP, "finance-lead@northwind.example").await;
}

#[tokio::test]
async fn the_share_legal_packet_scenario_sends_the_packet_only_to_legal() {
    refused(&SHARE_LEGAL_PACKET, OUTSIDE_COUNSEL).await;
    delivered(&SHARE_LEGAL_PACKET, "legal@northwind.example").await;
}

#[tokio::test]
async fn the_joint_merger_brief_scenario_sends_the_packet_only_to_finance_readers() {
    refused(&JOINT_MERGER_BRIEF, OUTSIDE_COUNSEL).await;
    delivered(&JOINT_MERGER_BRIEF, "cfo@northwind.example").await;
}

#[tokio::test]
async fn the_route_project_packet_annotator_routes_the_packet_only_to_its_acl() {
    refused(&ROUTE_PROJECT_PACKET, "finance-all@northwind.example").await;
    delivered(&ROUTE_PROJECT_PACKET, "onyx-steering@northwind.example").await;
}
