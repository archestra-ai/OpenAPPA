//! The github battery's base and target policies, composed the way each deployment does.
mod common;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use appa_package::BatteryTarget;
use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, ConfigError, HostDefaults, HostedBattery};
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
    repo_root().join("marketplace/batteries/github")
}

fn policy(file: &str) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(battery_dir().join(file)).unwrap()).unwrap()
}

/// A GitHub REST stand-in naming two repositories, `acme/website` public and `acme/secret`
/// private, served for the rest of this test binary; every helper the runtime spawns
/// inherits `GITHUB_API_URL` pointing at it.
fn github_api() {
    static API: OnceLock<()> = OnceLock::new();
    API.get_or_init(|| {
        let router = Router::new().route(
            "/repos/{owner}/{repo}",
            get(|Segments((owner, repo)): Segments<(String, String)>| async move {
                match (owner.as_str(), repo.as_str()) {
                    ("acme", "website") => Ok(Json(serde_json::json!({"visibility": "public", "fork": false}))),
                    ("acme", "secret") => Ok(Json(serde_json::json!({"visibility": "private", "fork": false}))),
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
            std::env::set_var("GITHUB_API_URL", url);
            std::env::set_var("APPA_PROVIDER_GITHUB_TOKEN", "ghp_fixture");
        }
    });
}

/// The battery installed beside a root config that includes its base and then reads `root`.
fn installed(dir: &Path, root: &str) -> std::path::PathBuf {
    let target = dir.join("github");
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(battery_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
    let path = dir.join("appa.toml");
    std::fs::write(&path, format!("include = [\"github/appa.toml\"]\n{root}")).unwrap();
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

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/github/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// The label the runtime admitted after `read` ran, the change it would make accepted
/// first when it asks for that, as the audit records it.
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

/// Single-user: a private repository's file narrows to its collaborators and keeps the
/// session's trust; a public one's stays public and enters `suspicious`.
#[tokio::test]
async fn single_user_reads_follow_the_repository_visibility() {
    github_api();
    for (repo, expected) in [
        ("secret", ("trusted", "@github:repo/acme/secret/collaborators")),
        ("website", ("suspicious", "public")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_for_target(&installed(dir.path(), ROOT), &[], BatteryTarget::SingleUser)
            .expect("the single-user composition loads");
        let runtime = opened(config).await;
        let (trust, audience) = admitted(
            &runtime,
            call(
                "get_file_contents",
                serde_json::json!({"owner": "acme", "repo": repo, "path": "README.md"}),
            ),
        )
        .await;
        assert_eq!((trust.as_str(), audience.as_str()), expected, "{repo}");
    }
}

/// The base alone, as a host composes it with no target: it loads without a helper, a
/// repository read stays with the viewer, and a write of what it read is refused.
#[tokio::test]
async fn the_base_alone_keeps_reads_with_the_viewer_and_refuses_their_writes() {
    let base = std::fs::read_to_string(battery_dir().join("appa.toml")).unwrap();
    let config = Config::hosted_composed(
        "[policy]\nversion = 2\ntrust_chain = [\"suspicious\", \"trusted\"]\n",
        &[HostedBattery {
            name: "github",
            base: &base,
            target: None,
            token_env: &[],
        }],
        HostDefaults::new(Duration::from_secs(5), 65_536),
        |_| None,
    )
    .expect("the base composes alone");
    let runtime = opened(config).await;
    let read = call(
        "get_file_contents",
        serde_json::json!({"owner": "acme", "repo": "secret", "path": "README.md"}),
    );
    assert_eq!(
        admitted(&runtime, read).await,
        ("suspicious".to_string(), "self".to_string())
    );
    let write = call(
        "create_or_update_file",
        serde_json::json!({"owner": "acme", "repo": "website", "path": "leak.md", "content": "x"}),
    );
    assert!(matches!(
        propose(&runtime, write).await,
        HookDecision::DenyCall { .. } | HookDecision::Refuse { .. }
    ));
}

/// Multi-user: the target policy routes member lookups to the root's `host-members`
/// roster, so the composition loads with one and is refused without it.
#[test]
fn multi_user_needs_the_root_host_members_roster() {
    let roster = "[externals.audience.host-members]\nreaders = { \"github:583231\" = \"ana@corp.example\" }\n";
    let with = tempfile::tempdir().unwrap();
    Config::load_for_target(
        &installed(with.path(), &format!("{ROOT}{roster}")),
        &[],
        BatteryTarget::MultiUser,
    )
    .expect("the multi-user composition loads with the roster");

    let without = tempfile::tempdir().unwrap();
    let refused = Config::load_for_target(&installed(without.path(), ROOT), &[], BatteryTarget::MultiUser);
    assert!(
        matches!(&refused, Err(ConfigError::UnknownLookupTarget { name, target }) if name == "github" && target == "host-members"),
        "{refused:?}"
    );
}

/// The two target policies differ in their helpers alone: the same rules, the same
/// declarations, and the same externals bound.
#[test]
fn the_target_policies_declare_the_same_rules() {
    let single = policy("single-user.toml");
    let multi = policy("multi-user.toml");
    assert_eq!(single["policy"], multi["policy"]);
    let bound = |document: &toml::Value| -> Vec<(String, Vec<String>)> {
        document["externals"]
            .as_table()
            .unwrap()
            .iter()
            .map(|(section, entries)| (section.clone(), entries.as_table().unwrap().keys().cloned().collect()))
            .collect()
    };
    assert_eq!(bound(&single), bound(&multi));
}
