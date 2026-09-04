//! The shipped push-target example end to end: a `git push` proposed from a working
//! directory reaches the example's command annotator with that directory as
//! `artifact.cwd`, and the annotator asks Git there where the push goes.

#![cfg(unix)]

mod common;
use common::{propose, ran, raw, repo_root, root};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use appa_runtime::api::Runtime;
use appa_runtime::{config::Config, hooks};
use appa_runtime_api::{HookDecision, HookEvent, ProposedCall};

const SINK: &str = "https://github.com/archestra-ai/openappa-sink.git";
const OTHER: &str = "https://github.com/archestra-ai/OpenAPPA.git";

fn policy() -> String {
    let script = repo_root().join("examples/claude-code-battery/local/push-target.py");
    format!(
        r#"
[policy]
version = 2

[[policy.annotator]]
name = "local.push-target"
audiences = []
marks = ["hitl"]

[[policy.tool]]
name = "Bash(command:*git push*)"
annotator = "local.push-target"

[[policy.tool]]
name = "Bash"
requires = {{ attention = ["hitl"] }}
delta = {{}}

[[policy.authority]]
name = "hitl"
[policy.authority.permits]
attention = ["hitl"]

[externals]
timeout_ms = 10000
review_timeout_ms = 1000
max_body_bytes = 65536

[externals.annotators."local.push-target"]
command = ["python3", "{}"]

[externals.authorities.hitl]
builtin = "hitl"
"#,
        script.display()
    )
}

async fn open_runtime(dir: &tempfile::TempDir) -> Arc<Runtime> {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, policy()).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    let runtime = Arc::new(Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens"));
    assert_eq!(
        hooks::handle(&runtime, HookEvent::SessionStart { root: root() }).await,
        HookDecision::Ack
    );
    runtime
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

/// A fresh repository whose `origin` pushes to `push_url`.
fn repository(parent: &Path, name: &str, push_url: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("the repository directory is created");
    git(&dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
    git(&dir, &["remote", "add", "origin", push_url]);
    dir
}

fn push_from(command: &str, cwd: Option<&Path>) -> ProposedCall {
    ProposedCall {
        tool: "Bash".to_string(),
        arguments: raw(serde_json::json!({ "command": command })),
        cwd: cwd.map(Path::to_path_buf),
    }
}

#[tokio::test]
async fn a_push_to_the_allowed_repository_runs_and_any_other_asks_a_person() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir).await;
    let sink = repository(dir.path(), "sink", SINK);
    let other = repository(dir.path(), "other", OTHER);

    // The directory the harness reported is where the annotator asks Git.
    assert_eq!(
        propose(&runtime, push_from("git push origin main", Some(&sink))).await,
        HookDecision::AllowCall { spawn: None }
    );
    ran(&runtime, push_from("git push origin main", None)).await;

    // The same bytes from another repository go elsewhere: the annotation requires
    // `hitl`, and the block offers the person's review.
    match propose(&runtime, push_from("git push origin main", Some(&other))).await {
        HookDecision::DenyCall { feedback, review, .. } => {
            assert!(feedback.contains("hitl"), "the block names the mark: {feedback}");
            assert_eq!(review.len(), 1, "one offer consults the person");
        }
        other => panic!("a push elsewhere blocks, not {other:?}"),
    }
}

#[tokio::test]
async fn a_push_url_that_differs_from_the_fetch_url_is_what_counts() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir).await;
    let repo = repository(dir.path(), "mirror", SINK);
    git(&repo, &["remote", "set-url", "--push", "origin", OTHER]);

    assert!(
        matches!(
            propose(&runtime, push_from("git push origin main", Some(&repo))).await,
            HookDecision::DenyCall { .. }
        ),
        "the fetch URL names the sink; the push URL does not"
    );
}

#[tokio::test]
async fn a_push_spelled_to_dodge_the_directory_check_asks_a_person() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir).await;
    let sink = repository(dir.path(), "sink", SINK);

    for command in [
        "GIT_DIR=/elsewhere/.git git push origin main",
        "git push https://github.com/archestra-ai/openappa-sink.git main",
        "cd /elsewhere && git push origin main",
    ] {
        assert!(
            matches!(
                propose(&runtime, push_from(command, Some(&sink))).await,
                HookDecision::DenyCall { .. }
            ),
            "{command} is not a plain push"
        );
    }
}

#[tokio::test]
async fn a_push_with_no_reported_directory_is_refused_operationally() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = open_runtime(&dir).await;

    // The consult carries `cwd: null`; the script exits with an error, so the call is not
    // judged: an operational refusal naming the annotator, never a policy denial.
    match propose(&runtime, push_from("git push origin main", None)).await {
        HookDecision::Refuse { detail } => assert!(detail.contains("local.push-target"), "{detail}"),
        other => panic!("a consult without a directory refuses, not {other:?}"),
    }
}
