mod common;
use common::raw;

use std::sync::Arc;

use appa_runtime::api::{OpenError, Runtime};
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};

fn policy_with(tools: &str) -> String {
    format!("[policy]\nversion = 1\n{tools}\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 4096\n")
}

fn with_notes() -> String {
    policy_with("[[policy.tool]]\nname = \"notes\"\n")
}

fn without_notes() -> String {
    policy_with("")
}

fn unloadable() -> String {
    policy_with("[[policy.tool]]\nname = \"notes\"\nrequires = { nonesuch = { contains = [\"x\"] } }\n")
}

fn unknown_builtin() -> String {
    policy_with(
        r#"[[policy.tool]]
name = "notes"

[[policy.authority]]
name = "security"
permits = { attention = ["security-signoff"] }

[externals.authorities.security]
builtin = "no-such-module"
"#,
    )
}

struct Deployment {
    dir: tempfile::TempDir,
    runtime: Arc<Runtime>,
}

impl Deployment {
    #[allow(clippy::result_large_err)]
    fn open(policy: &str) -> Result<Deployment, OpenError> {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let config = write_config(&dir, policy);
        let runtime = Runtime::open(config, dir.path().join("appa.db"), None)?;
        Ok(Deployment {
            dir,
            runtime: Arc::new(runtime),
        })
    }

    #[allow(clippy::result_large_err)]
    fn reload(&self, policy: &str) -> Result<bool, OpenError> {
        let config = write_config(&self.dir, policy);
        self.runtime.reload(config).map(|reloaded| reloaded.changed)
    }
}

fn write_config(dir: &tempfile::TempDir, policy: &str) -> Config {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy).expect("the fixture writes");
    Config::load(&path).expect("the fixture validates")
}

fn notes() -> ProposedCall {
    ProposedCall {
        tool: "notes".to_string(),
        arguments: raw(serde_json::json!({"file": "alice.md"})),
    }
}

fn actor(root: &TrajectoryId) -> Actor {
    Actor {
        root: root.clone(),
        child: None,
    }
}

async fn start(runtime: &Arc<Runtime>, root: &TrajectoryId) {
    let started = hooks::handle(runtime, HookEvent::SessionStart { root: root.clone() }).await;
    assert_eq!(started, HookDecision::Ack);
}

async fn propose_notes(runtime: &Arc<Runtime>, root: &TrajectoryId) -> HookDecision {
    let decision = hooks::handle(
        runtime,
        HookEvent::ToolCall {
            actor: actor(root),
            call: notes(),
            spawn: false,
        },
    )
    .await;
    if decision == (HookDecision::AllowCall { spawn: None }) {
        hooks::handle(
            runtime,
            HookEvent::ToolResult {
                actor: actor(root),
                call: notes(),
                outcome: ToolOutcome::Success {
                    body: OutcomeBody::Available("alice's notes".to_string()),
                },
            },
        )
        .await;
    }
    decision
}

fn allowed(decision: &HookDecision) -> bool {
    matches!(decision, HookDecision::AllowCall { .. })
}

/// After a swap removes the declaration, nothing covers `notes`: the refusal is
/// typed and names the tool.
fn refused_undeclared(decision: &HookDecision) -> bool {
    matches!(decision, HookDecision::Refuse { detail } if detail.contains("notes"))
}

#[tokio::test]
async fn a_running_root_keeps_its_opening_policy_and_a_new_root_binds_the_new_file() {
    let deployment = Deployment::open(&with_notes()).expect("the fixture opens");
    let before = TrajectoryId("reload:before".to_string());
    start(&deployment.runtime, &before).await;
    assert!(
        allowed(&propose_notes(&deployment.runtime, &before).await),
        "the opening file declares notes"
    );

    assert!(deployment.reload(&without_notes()).expect("the edit loads"));

    assert!(
        allowed(&propose_notes(&deployment.runtime, &before).await),
        "a root already running keeps the policy it opened with"
    );

    let after = TrajectoryId("reload:after".to_string());
    start(&deployment.runtime, &after).await;
    assert!(
        refused_undeclared(&propose_notes(&deployment.runtime, &after).await),
        "a root opened after the swap binds the new file"
    );
}

#[tokio::test]
async fn reloading_an_unchanged_file_reports_no_change() {
    let deployment = Deployment::open(&with_notes()).expect("the fixture opens");
    assert!(!deployment.reload(&with_notes()).expect("the unchanged file loads"));
    assert!(deployment.reload(&without_notes()).expect("the edit loads"));
}

#[tokio::test]
async fn a_refused_edit_changes_nothing_and_the_gate_keeps_deciding() {
    let deployment = Deployment::open(&with_notes()).expect("the fixture opens");
    let root = TrajectoryId("reload:refused".to_string());
    start(&deployment.runtime, &root).await;

    let dialect: fn(&OpenError) -> bool = |error| matches!(error, OpenError::Policy(_));
    let registry: fn(&OpenError) -> bool = |error| matches!(error, OpenError::Modules(_));
    for (case, policy, gate) in [
        ("a file the dialect refuses", unloadable(), dialect),
        ("a builtin the registry does not hold", unknown_builtin(), registry),
    ] {
        let refusal = deployment
            .reload(&policy)
            .expect_err(&format!("{case} must refuse the reload"));
        assert!(gate(&refusal), "{case} refused at the wrong gate: {refusal}");

        assert!(
            allowed(&propose_notes(&deployment.runtime, &root).await),
            "{case}: the running deployment must keep serving"
        );
        let fresh = TrajectoryId(format!("reload:refused:{}", case.len()));
        start(&deployment.runtime, &fresh).await;
        assert!(
            allowed(&propose_notes(&deployment.runtime, &fresh).await),
            "{case}: a fresh root must still bind the file that was serving"
        );
    }
}

/// The `notes` tool beside one annotator that names `builtin` on its declaration,
/// over the given `[externals]` entries.
fn declaring(builtin: &str, externals: &str) -> String {
    policy_with(&format!(
        "[[policy.tool]]\nname = \"notes\"\n[[policy.annotator]]\nname = \"classify\"\nbuiltin = \"{builtin}\"\n"
    )) + externals
}

/// A declared builtin the deployment cannot serve refuses to open, refuses to reload, and
/// a refused reload leaves the running deployment serving.
#[tokio::test]
async fn a_declared_builtin_the_deployment_cannot_serve_refuses_open_and_reload() {
    let bound: fn(&OpenError) -> bool =
        |error| matches!(error, OpenError::BoundBuiltinAnnotator(name) if name == "classify");
    let no_profile: fn(&OpenError) -> bool =
        |error| matches!(error, OpenError::LlmNotConfigured(name) if name == "classify");
    let no_platform: fn(&OpenError) -> bool =
        |error| matches!(error, OpenError::UnsupportedClaudeCodePlatform(name) if name == "classify");
    let mut cases = vec![
        (
            "a builtin annotator that is also bound",
            declaring(
                "claude-code",
                "[externals.annotators.classify]\nurl = \"https://classify.internal\"\n",
            ),
            bound,
        ),
        (
            "an llm annotator with no [externals.llm]",
            declaring("llm", ""),
            no_profile,
        ),
    ];
    if !cfg!(unix) {
        cases.push((
            "a claude-code annotator off Unix",
            declaring("claude-code", ""),
            no_platform,
        ));
    }

    let deployment = Deployment::open(&with_notes()).expect("the fixture opens");
    let root = TrajectoryId("reload:unservable".to_string());
    start(&deployment.runtime, &root).await;
    for (case, policy, gate) in cases {
        let Err(refusal) = Deployment::open(&policy) else {
            panic!("{case} must refuse to open");
        };
        assert!(gate(&refusal), "{case} refused opening at the wrong gate: {refusal}");

        let refusal = deployment
            .reload(&policy)
            .expect_err(&format!("{case} must refuse the reload"));
        assert!(gate(&refusal), "{case} refused the reload at the wrong gate: {refusal}");
        assert!(
            allowed(&propose_notes(&deployment.runtime, &root).await),
            "{case}: the running deployment must keep serving"
        );
        let fresh = TrajectoryId(format!("reload:unservable:{}", case.len()));
        start(&deployment.runtime, &fresh).await;
        assert!(
            allowed(&propose_notes(&deployment.runtime, &fresh).await),
            "{case}: a fresh root must still bind the file that was serving"
        );
    }
}

#[tokio::test]
async fn reload_rereads_includes_and_a_broken_include_refuses() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let root_path = dir.path().join("appa.toml");
    let battery_path = dir.path().join("battery.toml");
    std::fs::write(
        &root_path,
        "include = [\"battery.toml\"]\n[policy]\nversion = 1\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 4096\n",
    )
    .expect("the root config writes");
    std::fs::write(
        &battery_path,
        "[policy]\nversion = 1\n[[policy.tool]]\nname = \"notes\"\n",
    )
    .expect("the battery writes");
    let runtime = Arc::new(
        Runtime::open(
            Config::load(&root_path).expect("the composed config loads"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the runtime opens"),
    );

    std::fs::write(&battery_path, "[policy]\nversion = 1\nlimits = {}\n").expect("the broken battery writes");
    assert!(Config::load(&root_path).is_err(), "a broken include must refuse");

    std::fs::write(&battery_path, "[policy]\nversion = 1\n").expect("the edited battery writes");
    let reloaded = runtime
        .reload(Config::load(&root_path).expect("the edited include loads"))
        .expect("the composed deployment reloads");
    assert!(reloaded.changed);
    let after = TrajectoryId("reload:included-edit".to_string());
    start(&runtime, &after).await;
    assert!(
        refused_undeclared(&propose_notes(&runtime, &after).await),
        "a fresh root sees the edited include"
    );
}
