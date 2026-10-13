//! The huggingface battery's base and target policies, composed the way each deployment does.
mod common;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use appa_package::BatteryTarget;
use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Json, Router, extract::Path as Segments, http::StatusCode, routing::get};
use common::{actor, offer_of, propose, ran, raw, repo_root, root, serve};

const ROOT: &str = r#"
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[externals]
timeout_ms = 30000
max_body_bytes = 1048576
"#;

fn battery_dir() -> std::path::PathBuf {
    repo_root().join("marketplace/batteries/huggingface")
}

fn policy(file: &str) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(battery_dir().join(file)).unwrap()).unwrap()
}

/// A Hub stand-in whose token belongs to `acme`, naming `acme/private` a private model of
/// acme's, served for the rest of this test binary; every helper the runtime spawns
/// inherits `HF_ENDPOINT` pointing at it.
fn hub_api() {
    static API: OnceLock<()> = OnceLock::new();
    API.get_or_init(|| {
        let router = Router::new()
            .route(
                "/api/whoami-v2",
                get(|| async { Json(serde_json::json!({"type": "user", "name": "acme"})) }),
            )
            .route(
                "/api/models/{owner}/{name}",
                get(|Segments((owner, name)): Segments<(String, String)>| async move {
                    match (owner.as_str(), name.as_str()) {
                        ("acme", "private") => Ok(Json(
                            serde_json::json!({"id": "acme/private", "author": "acme", "private": true, "gated": false}),
                        )),
                        _ => Err(StatusCode::NOT_FOUND),
                    }
                }),
            );
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the stand-in's runtime builds")
                .block_on(async move {
                    sender.send(serve(router).await).expect("the url is received");
                    std::future::pending::<()>().await
                });
        });
        let url = receiver.recv().expect("the stand-in serves");
        // SAFETY: set once, before any deployment of this binary loads; nothing else in
        // this binary writes the environment.
        unsafe {
            std::env::set_var("HF_ENDPOINT", url);
            std::env::set_var("APPA_PROVIDER_HUGGINGFACE_TOKEN", "hf_fixture");
        }
    });
}

/// The battery installed beside a root config that includes its base.
fn installed(dir: &Path) -> std::path::PathBuf {
    let target = dir.join("huggingface");
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(battery_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
    let path = dir.join("appa.toml");
    std::fs::write(&path, format!("include = [\"huggingface/appa.toml\"]\n{ROOT}")).unwrap();
    path
}

async fn opened(config: Config) -> Arc<Runtime> {
    let store =
        Arc::new(appa_eventlog::LogStore::open(appa_eventlog::Backend::Memory).expect("an in-memory log opens"));
    let runtime = Arc::new(Runtime::open_with_store(config, store, None).expect("the deployment opens"));
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
            }
        )
        .await,
        HookDecision::Ack
    );
    runtime
}

async fn deployed(target: BatteryTarget) -> (tempfile::TempDir, Arc<Runtime>) {
    hub_api();
    let dir = tempfile::tempdir().unwrap();
    let config = Config::load_for_target(&installed(dir.path()), &[], target).expect("the composition loads");
    let runtime = opened(config).await;
    (dir, runtime)
}

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/huggingface/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

fn private_read() -> ProposedCall {
    call(
        "hub_repo_details",
        serde_json::json!({"repo_ids": ["acme/private"], "repo_type": "model"}),
    )
}

fn private_write() -> ProposedCall {
    call(
        "hf_fs_write",
        serde_json::json!({"cmd": "put", "args": ["hf://models/acme/private/notes.md"], "content": "x"}),
    )
}

/// The label the runtime admitted after `read` ran, the narrowing it asks for accepted
/// first, as the audit records it.
async fn admitted(runtime: &Arc<Runtime>, read: ProposedCall) -> (String, String) {
    if let decision @ HookDecision::DenyCall { .. } = propose(runtime, read.clone()).await {
        let outcome = runtime.execute_remedy(&actor(), offer_of(&decision)).await;
        assert!(matches!(outcome, RemedyOutcome::Authorized { .. }), "{outcome:?}");
    }
    assert_eq!(
        propose(runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(runtime, read).await;
    let labels: Vec<_> = runtime
        .audit(&actor().root)
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some((label.trust, label.audience)),
            _ => None,
        })
        .collect();
    let [label] = labels.as_slice() else {
        panic!("one read admits one label: {labels:?}");
    };
    label.clone()
}

/// What a write into `acme/private` decides once the trajectory holds trusted data only
/// the session's own person may see, the account read by `hf_whoami`.
async fn write_after_whoami(runtime: &Arc<Runtime>) -> HookDecision {
    assert_eq!(
        admitted(runtime, call("hf_whoami", serde_json::json!({}))).await,
        ("trusted".to_string(), "self".to_string())
    );
    propose(runtime, private_write()).await
}

/// Single-user: a private repository of the token's owner stays with the viewer, and a
/// write into it takes data only the viewer may see.
#[tokio::test]
async fn single_user_writes_the_viewers_own_private_repository() {
    let (_dir, runtime) = deployed(BatteryTarget::SingleUser).await;
    assert_eq!(
        admitted(&runtime, private_read()).await,
        ("suspicious".to_string(), "self".to_string())
    );
    let (_dir, runtime) = deployed(BatteryTarget::SingleUser).await;
    assert_eq!(
        write_after_whoami(&runtime).await,
        HookDecision::AllowCall { spawn: None }
    );
}

/// Multi-user: a private repository's content stays with the session's own person, and a
/// write into it, its owner unknown, needs data everyone may see.
#[tokio::test]
async fn multi_user_writes_a_private_repository_only_with_public_data() {
    let (_dir, runtime) = deployed(BatteryTarget::MultiUser).await;
    assert_eq!(
        admitted(&runtime, private_read()).await,
        ("suspicious".to_string(), "self".to_string())
    );
    let (_dir, runtime) = deployed(BatteryTarget::MultiUser).await;
    assert_eq!(
        propose(&runtime, private_write()).await,
        HookDecision::AllowCall { spawn: None }
    );
    let (_dir, runtime) = deployed(BatteryTarget::MultiUser).await;
    assert!(!matches!(
        write_after_whoami(&runtime).await,
        HookDecision::AllowCall { .. }
    ));
}

/// The base alone, as a host composes it with no target: it loads without a helper, a
/// repository read stays with the viewer, a write needs public data, and creating a
/// repository waits for a person.
#[tokio::test]
async fn the_base_alone_keeps_reads_with_the_viewer_and_writes_public() {
    let base = std::fs::read_to_string(battery_dir().join("appa.toml")).unwrap();
    let compose = || {
        Config::hosted_composed(
            "[policy]\nversion = 2\ntrust_chain = [\"suspicious\", \"trusted\"]\n",
            &[HostedBattery {
                name: "huggingface",
                base: &base,
                target: None,
                token_env: &[],
            }],
            HostDefaults::new(Duration::from_secs(5), 65_536),
            |_| None,
        )
        .expect("the base composes alone")
    };
    let runtime = opened(compose()).await;
    assert_eq!(
        admitted(&runtime, private_read()).await,
        ("suspicious".to_string(), "self".to_string())
    );
    let runtime = opened(compose()).await;
    let create = call("create_repo", serde_json::json!({"uri": "hf://models/acme/new"}));
    assert!(!matches!(
        propose(&runtime, create).await,
        HookDecision::AllowCall { .. }
    ));
    assert!(!matches!(
        write_after_whoami(&runtime).await,
        HookDecision::AllowCall { .. }
    ));
}

/// The two target policies declare the same rules; multi-user binds the same annotators
/// and no audience source.
#[test]
fn the_target_policies_declare_the_same_rules() {
    let single = policy("single-user.toml");
    let multi = policy("multi-user.toml");
    assert_eq!(single["policy"], multi["policy"]);
    let sections =
        |document: &toml::Value| -> Vec<String> { document["externals"].as_table().unwrap().keys().cloned().collect() };
    assert_eq!(sections(&single), ["annotators", "audience"]);
    assert_eq!(sections(&multi), ["annotators"]);
    let annotators = |document: &toml::Value| -> Vec<String> {
        document["externals"]["annotators"]
            .as_table()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(annotators(&single), annotators(&multi));
}
