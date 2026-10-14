//! Battery integration: shipped examples, write requirements/effects, cross-provider
//! confidentiality, and the battery's base and target policies composed the way each
//! deployment does.
mod common;

use appa_package::BatteryTarget;
use appa_runtime::api::{AuditEvent, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};
use axum::{Router, extract::State, routing::post};
use common::{actor, members_source, offer_of, propose, ran, raw, repo_root, root, serve, session_runtime};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn call(tool: &str, args: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: format!("mcp/linear/{tool}"),
        arguments: raw(args),
        cwd: None,
    }
}

/// A loopback `linear` audience source recording each collection it was asked for. Every
/// collection holds one reader, except the full members, who are two: a team is narrower
/// than `internal`.
#[derive(Clone, Default)]
struct Reads(Arc<Mutex<Vec<String>>>);

async fn serve_reads() -> (String, Reads) {
    let reads = Reads::default();
    let router = Router::new()
        .route(
            "/audience",
            post(|State(reads): State<Reads>, body: String| async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("the request is JSON");
                let selector = request["artifact"]["selector"].as_str().unwrap_or_default().to_string();
                let members = match selector.as_str() {
                    "full-members" => vec!["alice@corp.example", "bob@corp.example"],
                    _ => vec!["alice@corp.example"],
                };
                reads.0.lock().unwrap().push(selector);
                serde_json::json!({ "version": 1, "answer": { "members": members } }).to_string()
            }),
        )
        .with_state(reads.clone());
    (format!("{}/audience", serve(router).await), reads)
}

/// The documented example over the shipped batteries; `source` swaps the linear battery's
/// resolver process for a loopback source.
async fn runtime(dir: &tempfile::TempDir, extra: &str, source: Option<&str>) -> Arc<Runtime> {
    for battery in ["linear", "github"] {
        let origin = repo_root().join("marketplace/batteries").join(battery);
        let target = dir.path().join("marketplace/batteries").join(battery);
        std::fs::create_dir_all(&target).unwrap();
        // The whole package, so a battery with target policies composes its single-user one.
        for entry in std::fs::read_dir(&origin).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
            }
        }
        if let (Some(url), "linear") = (source, battery) {
            let policy = std::fs::read_to_string(origin.join("single-user.toml")).unwrap();
            let binding =
                "command = [\"python3\", \"audience-source.py\"]\ntoken_env = \"APPA_PROVIDER_LINEAR_TOKEN\"\n";
            assert!(
                policy.contains(binding),
                "the linear battery binds its resolver as documented"
            );
            std::fs::write(
                target.join("single-user.toml"),
                policy.replace(binding, &format!("url = \"{url}\"\n")),
            )
            .unwrap();
        }
    }
    // Use the documented configuration; substitute a deterministic review authority.
    let text = std::fs::read_to_string(repo_root().join("examples/live-replays/linear/appa.toml"))
        .unwrap()
        .replace("../../../marketplace/", "marketplace/")
        .replace("builtin = \"hitl\"", "builtin = \"approve\"");
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, format!("{text}\n{extra}")).unwrap();
    session_runtime(dir.path(), &path).await
}

#[tokio::test]
async fn linear_write_requires_review_and_records_its_effect() {
    for (tool, args) in [
        (
            "save_comment",
            serde_json::json!({"issueId":"ENG-1","body":"reviewed text"}),
        ),
        ("save_issue", serde_json::json!({"id":"ENG-2","title":"reviewed text"})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(&dir, "", None).await;
        let write = call(tool, args);
        let decision = propose(&runtime, write.clone()).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }));
        let outcome = runtime.execute_remedy(&actor(), offer_of(&decision)).await;
        // A public trajectory covers either destination symbolically. Reviewing the
        // write must not require a directory lookup before accepting its restriction.
        assert!(matches!(outcome, RemedyOutcome::Authorized { .. }), "{outcome:?}");
        assert_eq!(
            propose(&runtime, write.clone()).await,
            HookDecision::AllowCall { spawn: None }
        );
        ran(&runtime, write).await;
        let effects: Vec<_> = runtime
            .audit(&root())
            .unwrap()
            .into_iter()
            .filter_map(|entry| match entry.event {
                AuditEvent::Released { tool, effects, .. } if tool.starts_with("mcp/linear/") => Some(effects),
                _ => None,
            })
            .collect();
        assert_eq!(effects, vec![vec!["linear.changed"]]);
    }
}

#[tokio::test]
async fn restricted_linear_content_can_reach_a_github_destination_with_the_same_readers() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(
        &dir,
        r#"
[[policy.tool]]
name = "mcp/github/issue_write(repo:private)"
delta = {}
requires = { trust = "trusted", audience = { contains = ["alice@corp.example"] } }
"#,
        None,
    )
    .await;
    let read = call("get_issue", serde_json::json!({"id":"ENG-1"}));
    let offer = offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;
    let trusts: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Admitted { label } => Some(label.trust),
            _ => None,
        })
        .collect();
    assert_eq!(trusts, ["trusted"], "workspace members write Linear issues");
    let write = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(serde_json::json!({
        "owner":"example","repo":"private","title":"Reviewed summary","body":"Restricted fixture content"})),
        cwd: None,
    };
    assert_eq!(
        propose(&runtime, write.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, write).await;
    let public = ProposedCall {
        tool: "mcp/github/issue_write".into(),
        arguments: raw(serde_json::json!({
        "owner":"example","repo":"public","title":"Leak","body":"Restricted fixture content"})),
        cwd: None,
    };
    assert!(!matches!(
        propose(&runtime, public).await,
        HookDecision::AllowCall { .. }
    ));
}

/// Moving an issue into another team writes where that team reads: the runtime asks the
/// source for the destination team's readers, not for the issue's current readers.
#[tokio::test]
async fn moving_an_issue_is_bounded_by_the_destination_teams_readers() {
    let dir = tempfile::tempdir().unwrap();
    let (url, reads) = serve_reads().await;
    let runtime = runtime(&dir, "", Some(&url)).await;

    // Narrow the trajectory to Alice first: a `contains` on the public trajectory reads nothing.
    let read = call("get_issue", serde_json::json!({"id":"ENG-1"}));
    let offer = offer_of(&propose(&runtime, read.clone()).await);
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;
    reads.0.lock().unwrap().clear();

    let mv = call(
        "save_issue",
        serde_json::json!({"id":"ENG-2","team":"SEC","title":"moved"}),
    );
    let decision = propose(&runtime, mv.clone()).await;
    assert!(!matches!(decision, HookDecision::AllowCall { .. }));
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, mv.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, mv).await;

    let asked = reads.0.lock().unwrap().clone();
    assert!(asked.contains(&"team/SEC/readers".to_string()), "{asked:?}");
    assert!(!asked.contains(&"issue/ENG-2/readers".to_string()), "{asked:?}");
}

/// A private team's issues are read by the team, a narrower audience than `internal`. The
/// shipped read requires `contains internal` of what the call sends — the session's label —
/// and its `delta` only labels the result, so the read proceeds once the agent accepts the
/// narrowing to the team's readers.
#[tokio::test]
async fn a_private_team_read_narrows_to_the_team_without_failing_its_requirement() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _) = serve_reads().await;
    let runtime = runtime(&dir, "", Some(&url)).await;

    let read = call("list_issues", serde_json::json!({"team":"SEC"}));
    let decision = propose(&runtime, read.clone()).await;
    let HookDecision::DenyCall { review, .. } = &decision else {
        panic!("the read waits for its narrowing to be accepted: {decision:?}")
    };
    assert!(review.is_empty(), "accepting a narrowing consults no human");
    assert!(matches!(
        runtime.execute_remedy(&actor(), offer_of(&decision)).await,
        RemedyOutcome::Authorized { .. }
    ));
    assert_eq!(
        propose(&runtime, read.clone()).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, read).await;

    let narrowed: Vec<_> = runtime
        .audit(&root())
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            AuditEvent::Narrowed { to, .. } => Some(to.audience),
            _ => None,
        })
        .collect();
    assert_eq!(narrowed, ["@linear:team/SEC/readers"]);
}

fn battery_dir() -> std::path::PathBuf {
    repo_root().join("marketplace/batteries/linear")
}

fn target_policy(file: &str) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(battery_dir().join(file)).unwrap()).unwrap()
}

/// A root that maps `self` and `internal` onto a loopback roster, so no read waits on Linear.
async fn target_root() -> String {
    let source = members_source().await;
    format!(
        r#"
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[policy.audience]
self = ["people:viewer"]
internal = ["people:members"]

[externals]
timeout_ms = 30000
max_body_bytes = 1048576

[externals.audience.people]
url = "{source}"
selectors = [{{ template = "viewer", feeds = "self" }}, {{ template = "members", feeds = "internal" }}]
"#
    )
}

/// The battery installed beside a root config that includes its base and then reads `root`,
/// each target policy's `linear` source bound to the loopback `source`.
fn installed(dir: &Path, root: &str, source: &str) -> std::path::PathBuf {
    let target = dir.join("linear");
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(battery_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
    for (file, helper) in [
        ("single-user.toml", "audience-source.py"),
        ("multi-user.toml", "audience-source-multi-user.py"),
    ] {
        let policy = std::fs::read_to_string(battery_dir().join(file)).unwrap();
        let binding = format!("command = [\"python3\", \"{helper}\"]\ntoken_env = \"APPA_PROVIDER_LINEAR_TOKEN\"\n");
        assert!(policy.contains(&binding), "{file} binds {helper}");
        std::fs::write(
            target.join(file),
            policy.replace(&binding, &format!("url = \"{source}\"\n")),
        )
        .unwrap();
    }
    let path = dir.join("appa.toml");
    std::fs::write(&path, format!("include = [\"linear/appa.toml\"]\n{root}")).unwrap();
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

/// Each target labels a read of one issue with that issue's readers, and asks the `linear`
/// source for them once a later call's requirement reads that label.
#[tokio::test]
async fn each_target_labels_an_issue_read_with_its_readers() {
    for target in [BatteryTarget::SingleUser, BatteryTarget::MultiUser] {
        let dir = tempfile::tempdir().unwrap();
        let (source, reads) = serve_reads().await;
        let config = Config::load_for_target(&installed(dir.path(), &target_root().await, &source), &[], target)
            .expect("the target composition loads");
        let runtime = opened(config).await;
        assert_eq!(
            admitted(&runtime, call("get_issue", serde_json::json!({"id": "ENG-1"}))).await,
            ("trusted".to_string(), "@linear:issue/ENG-1/readers".to_string()),
            "{target:?}"
        );
        propose(
            &runtime,
            call("save_comment", serde_json::json!({"issueId": "ENG-2", "body": "x"})),
        )
        .await;
        assert!(
            reads.0.lock().unwrap().contains(&"issue/ENG-1/readers".to_string()),
            "{target:?}"
        );
    }
}

/// The base alone, as a host composes it with no target: it loads without a helper, an
/// issue read stays with the viewer, and a comment of what it read is refused.
#[tokio::test]
async fn the_base_alone_keeps_issue_reads_with_the_viewer_and_refuses_their_writes() {
    let base = std::fs::read_to_string(battery_dir().join("appa.toml")).unwrap();
    let config = Config::hosted_composed(
        &target_root().await,
        &[HostedBattery {
            name: "linear",
            base: &base,
            target: None,
            token_env: &[],
        }],
        HostDefaults::new(Duration::from_secs(5), 65_536),
        |_| None,
    )
    .expect("the base composes alone");
    let runtime = opened(config).await;
    assert_eq!(
        admitted(&runtime, call("get_issue", serde_json::json!({"id": "ENG-1"}))).await,
        ("trusted".to_string(), "self".to_string())
    );
    let write = call(
        "save_comment",
        serde_json::json!({"issueId": "ENG-2", "body": "what ENG-1 says"}),
    );
    assert!(matches!(
        propose(&runtime, write).await,
        HookDecision::DenyCall { .. } | HookDecision::Refuse { .. }
    ));
}

/// The two target policies differ in their helper alone: the same rules and the same
/// externals bound, the multi-user one serving no token-owner selector.
#[test]
fn the_target_policies_declare_the_same_rules() {
    let single = target_policy("single-user.toml");
    let multi = target_policy("multi-user.toml");
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
    let templates = |document: &toml::Value| -> Vec<String> {
        document["externals"]["audience"]["linear"]["selectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|selector| selector["template"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(templates(&single).contains(&"viewer".to_owned()));
    assert!(!templates(&multi).contains(&"viewer".to_owned()));
}
