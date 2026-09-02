//! `appa replay` over the real hook path: traces run against an in-memory deployment, and
//! the shipped examples stay green. Every test declares the one policy it needs, next to it.

mod common;
use common::{repo_root, serve};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use appa_runtime::api::{OfferKind, Runtime};
use appa_runtime::config::Config;
use appa_runtime::replay::{
    Expect, Got, StepOutcome, Summary, Trace, TraceReport, Verdict, collect, parse, render, run,
};
use axum::Router;
use axum::routing::post;

const EXTERNALS: &str = "\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n";

/// A deployment over the given `[policy]` body and any `[externals.*]` bindings after the
/// shared root settings.
fn runtime(dir: &tempfile::TempDir, policy: &str, bindings: &str) -> Runtime {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, format!("[policy]\nversion = 2\n{policy}{EXTERNALS}{bindings}")).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    Runtime::open_in_memory(config, None).expect("the deployment opens in memory")
}

fn trace(name: &str, text: &str) -> Trace {
    parse(Path::new(name), text).expect("the trace parses")
}

fn outcomes(report: &TraceReport) -> Vec<(usize, &StepOutcome)> {
    report.steps.iter().map(|step| (step.line, &step.outcome)).collect()
}

fn rendered(reports: &[TraceReport]) -> (String, Summary) {
    let mut out = Vec::new();
    let summary = render(reports, false, &mut out).expect("renders");
    (String::from_utf8(out).expect("utf-8"), summary)
}

const PASSED: StepOutcome = StepOutcome::Passed { taken: None };

fn taken(kind: OfferKind) -> StepOutcome {
    StepOutcome::Passed { taken: Some(kind) }
}

/// An email leaves the company; a deploy needs a backup that never comes.
const ALLOW_AND_DENY_POLICY: &str = r#"
[[policy.tool]]
name = "Email"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "Deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}
"#;

#[tokio::test]
async fn one_allow_and_one_deny_pass() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, ALLOW_AND_DENY_POLICY, "");
    let traces = [
        trace("allow.appa", "Email {\n  to: \"x@other.com\"\n}\nexpect allow\n"),
        trace("deny.appa", "Deploy {}\nexpect deny\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(outcomes(&reports[0]), vec![(1, &PASSED)]);
    assert_eq!(outcomes(&reports[1]), vec![(1, &PASSED)]);
    let (text, summary) = rendered(&reports);
    assert_eq!(
        text,
        "ok    allow.appa\nok    deny.appa\n2 files: 2 ok, 0 failed, 0 could not run\n"
    );
    assert_eq!(summary.exit_code(), std::process::ExitCode::SUCCESS);
}

/// HR reads narrow the audience to `hr`; any other read restricts nothing; an email leaves
/// the company; a post reaches HR.
const NARROWING_POLICY: &str = r#"
[[policy.tool]]
name = "Read(path:/hr/*)"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "Read"
delta = {}

[[policy.tool]]
name = "Email"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "Post"
requires = { audience = { contains = ["hr"] } }
delta = {}
"#;

/// The trace the command exists for: a read narrows the trajectory, and the email that was
/// fine before it is denied after it. The read is allowed the way the model gets it allowed,
/// by accepting the offered narrowing.
#[tokio::test]
async fn a_narrowing_read_denies_the_later_email() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, NARROWING_POLICY, "");
    let traces = [
        trace(
            "leak.appa",
            "Read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nEmail {\n  to: \"x@other.com\"\n}\nexpect deny\n\nPost {\n  text: \"reviewed\"\n}\nexpect allow\n",
        ),
        trace("no-read.appa", "Email {\n  to: \"x@other.com\"\n}\nexpect allow\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![(1, &taken(OfferKind::Accept)), (6, &PASSED), (11, &PASSED)]
    );
    assert_eq!(reports[1].verdict(), Verdict::Ok);
}

/// A wipe needs a person's sign-off; a deploy needs a backup that never comes.
const ATTENTION_POLICY: &str = r#"
[[policy.tool]]
name = "Wipe"
requires = { attention = ["signoff"] }
delta = {}

[[policy.tool]]
name = "Deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}

[[policy.authority]]
name = "approver"

[policy.authority.permits]
attention = ["signoff"]
"#;

fn approver() -> OfferKind {
    OfferKind::Authority {
        names: vec!["approver".into()],
    }
}

/// A call that needs a person: the block offers the authority, the stand-in approves, and
/// the trace goes on. The expectation may name the authority. Expecting `deny` or another
/// name there is a mismatch that names the offer.
#[tokio::test]
async fn an_authority_offer_is_taken_as_approved() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, ATTENTION_POLICY, "");
    let traces = [
        trace("wipe.appa", "Wipe {}\nexpect authority\n\nDeploy {}\nexpect deny\n"),
        trace("wipe-named.appa", "Wipe {}\nexpect authority approver\n"),
        trace("wipe-denied.appa", "Wipe {}\nexpect deny\n"),
        trace("wipe-other.appa", "Wipe {}\nexpect authority cto\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(outcomes(&reports[0]), vec![(1, &taken(approver())), (4, &PASSED)]);
    assert_eq!(outcomes(&reports[1]), vec![(1, &taken(approver()))]);
    assert!(matches!(
        &reports[2].steps[0].outcome,
        StepOutcome::Mismatch { got: Got::Blocked(kinds), want: Expect::Deny, .. }
            if kinds == &BTreeSet::from([approver()])
    ));
    let (text, _) = rendered(&reports);
    assert!(
        text.contains("wipe-denied.appa:1: Wipe: got authority approver, want deny\n"),
        "{text}"
    );
    assert!(
        text.contains("wipe-other.appa:1: Wipe: got authority approver, want authority cto\n"),
        "{text}"
    );
}

/// HR reads narrow the audience; an export needs `public`; the redactor is the declared
/// way from `hr` to `public`.
const REDACTION_POLICY: &str = r#"
[[policy.tool]]
name = "Read(path:/hr/*)"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "Export"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.sanitizer]]
name = "redactor"
on = ["tool_input"]

[policy.sanitizer.permits]
audience = { from = ["hr"], to = ["public"] }
"#;

const REDACTOR_BINDING: &str = "\n[externals.sanitizers.redactor]\nbuiltin = \"redact-email\"\n";

/// A value that must pass the sanitizer: the block offers it, the stand-in returns the
/// arguments unchanged, the call runs, and the trajectory stays narrowed.
#[tokio::test]
async fn a_sanitizer_offer_is_taken_with_the_value_unchanged() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, REDACTION_POLICY, REDACTOR_BINDING);
    let traces = [trace(
        "export.appa",
        "Read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nExport {\n  body: \"salaries\"\n}\nexpect sanitizer\n\nExport {\n  body: \"again\"\n}\nexpect sanitizer redactor\n",
    )];
    let reports = run(&runtime, &traces).await;
    let redactor = OfferKind::Sanitizer {
        name: "redactor".into(),
    };
    assert_eq!(
        outcomes(&reports[0]),
        vec![
            (1, &taken(OfferKind::Accept)),
            (6, &taken(redactor.clone())),
            (11, &taken(redactor)),
        ]
    );
}

/// A read that restricts nothing; an email leaves the company; a deploy needs a backup
/// that never comes.
const MISMATCH_POLICY: &str = r#"
[[policy.tool]]
name = "Read"
delta = {}

[[policy.tool]]
name = "Email"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "Deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}
"#;

#[tokio::test]
async fn a_mismatch_stops_its_file_and_the_other_files_still_run() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, MISMATCH_POLICY, "");
    let traces = [
        trace(
            "wrong.appa",
            "Read {\n  path: \"/docs/readme.md\"\n}\nexpect allow\n\nEmail {\n  to: \"x@other.com\"\n}\nexpect deny\n\nDeploy {}\nexpect deny\n",
        ),
        trace("right.appa", "Deploy {}\nexpect deny\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(reports[0].steps.len(), 2, "the file stopped at the mismatch");
    assert!(matches!(
        &reports[0].steps[1].outcome,
        StepOutcome::Mismatch {
            got: Got::Allowed,
            want: Expect::Deny,
            feedback: None,
        }
    ));
    assert_eq!(reports[1].verdict(), Verdict::Ok);
    let (text, summary) = rendered(&reports);
    assert_eq!(
        text,
        "wrong.appa:6: Email: got allow, want deny\nFAIL  wrong.appa\nok    right.appa\n2 files: 1 ok, 1 failed, 0 could not run\n"
    );
    assert_eq!(summary.exit_code(), std::process::ExitCode::from(1));
}

/// A deploy needs a backup that never comes.
const DEPLOY_POLICY: &str = r#"
[[policy.tool]]
name = "Deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}
"#;

#[tokio::test]
async fn a_deny_mismatch_prints_the_feedback() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, DEPLOY_POLICY, "");
    let traces = [trace("deploy.appa", "Deploy {}\nexpect allow\n")];
    let reports = run(&runtime, &traces).await;
    let (text, _) = rendered(&reports);
    assert!(
        text.starts_with("deploy.appa:1: Deploy: got deny, want allow\n    [appa] Blocked"),
        "{text}"
    );
}

#[tokio::test]
async fn an_undeclared_tool_cannot_run_and_is_never_a_deny() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, DEPLOY_POLICY, "");
    let traces = [trace("ghost.appa", "Ghost {}\nexpect deny\n")];
    let reports = run(&runtime, &traces).await;
    assert!(matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(_)));
    assert_eq!(reports[0].verdict(), Verdict::CannotRun);
    let (text, summary) = rendered(&reports);
    assert!(text.starts_with("ghost.appa:1: Ghost: cannot run: "), "{text}");
    assert_eq!(summary.exit_code(), std::process::ExitCode::from(2));
}

/// Files run at once, each in its own trajectory: one file's narrowing is invisible to the
/// other, whichever finishes first.
#[tokio::test]
async fn files_do_not_share_trajectory_state() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, NARROWING_POLICY, "");
    let narrowing =
        "Read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nEmail {\n  to: \"x@other.com\"\n}\nexpect deny\n";
    let fresh = "Email {\n  to: \"x@other.com\"\n}\nexpect allow\n";
    let mut traces = Vec::new();
    for index in 0..8 {
        traces.push(trace(&format!("narrowing-{index}.appa"), narrowing));
        traces.push(trace(&format!("fresh-{index}.appa"), fresh));
    }
    let reports = run(&runtime, &traces).await;
    for report in &reports {
        assert_eq!(report.verdict(), Verdict::Ok, "{report:?}");
    }
}

/// A fetch whose contract the classifier writes per call; a send that needs `trusted`.
const ANNOTATED_POLICY: &str = r#"
[[policy.annotator]]
name = "classifier"

[[policy.tool]]
name = "fetch"
annotator = "classifier"

[[policy.tool]]
name = "send"
requires = { trust = "trusted" }
delta = {}
"#;

/// An annotator bound in the deployment is consulted as in production; its answer decides
/// the step. A stub that fails makes the step "cannot run", never a deny.
#[tokio::test]
async fn a_bound_annotator_decides_the_step() {
    let answer = std::sync::Arc::new(std::sync::Mutex::new(true));
    let served = answer.clone();
    let router = Router::new().route(
        "/annotate",
        post(move |_body: String| {
            let up = *served.lock().unwrap();
            async move {
                if up {
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({
                            "version": 1,
                            "answer": {
                                "delta": { "trust": "suspicious" },
                                "requires": { "history": [], "attention": [] },
                                "emits": [],
                            }
                        })
                        .to_string(),
                    )
                } else {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string())
                }
            }
        }),
    );
    let url = format!("{}/annotate", serve(router).await);
    let binding = format!("\n[externals.annotators.classifier]\nurl = \"{url}\"\n");
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, ANNOTATED_POLICY, &binding);
    let text = "fetch {\n  url: \"https://example.com\"\n}\nexpect allow\n\nsend {}\nexpect deny\n";

    let reports = run(&runtime, &[trace("annotated.appa", text)]).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![(1, &taken(OfferKind::Accept)), (6, &PASSED)]
    );

    *answer.lock().unwrap() = false;
    let reports = run(&runtime, &[trace("down.appa", text)]).await;
    assert!(
        matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(detail) if detail.contains("classifier")),
        "{:?}",
        reports[0]
    );
}

fn example_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples/replay"))
        .expect("the examples directory is readable")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[tokio::test]
async fn every_shipped_example_passes() {
    let dirs = example_dirs();
    assert!(dirs.len() >= 6, "the shipped examples were found: {dirs:?}");
    for dir in dirs {
        let config = Config::load(&dir.join("appa.toml")).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let runtime =
            Runtime::open_in_memory(config, None).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let files = collect(std::slice::from_ref(&dir)).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let traces: Vec<Trace> = files
            .iter()
            .map(|file| {
                let text = std::fs::read_to_string(file).expect("the trace is readable");
                parse(file, &text).unwrap_or_else(|error| panic!("{error}"))
            })
            .collect();
        let reports = run(&runtime, &traces).await;
        let (text, summary) = rendered(&reports);
        assert_eq!(
            (summary.failed, summary.cannot_run),
            (0, 0),
            "{}:\n{text}",
            dir.display()
        );
    }
}

/// The command line end to end: the exit code tells a green run from a regression from a
/// broken setup.
#[test]
fn the_command_exits_0_1_and_2() {
    let example = repo_root().join("examples/replay/backup-before-deploy");
    let config = example.join("appa.toml");
    let appa = env!("CARGO_BIN_EXE_appa");

    let green = Command::new(appa)
        .args(["replay", "--config"])
        .arg(&config)
        .arg(&example)
        .output()
        .expect("the binary runs");
    assert_eq!(
        green.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&green.stderr)
    );
    assert!(
        String::from_utf8_lossy(&green.stdout).ends_with("1 file: 1 ok, 0 failed, 0 could not run\n"),
        "{}",
        String::from_utf8_lossy(&green.stdout)
    );

    let dir = tempfile::tempdir().expect("a temp dir");
    let wrong = dir.path().join("wrong.appa");
    std::fs::write(&wrong, "Deploy {}\nexpect allow\n").expect("the trace writes");
    let regression = Command::new(appa)
        .args(["replay", "--config"])
        .arg(&config)
        .arg(&wrong)
        .output()
        .expect("the binary runs");
    assert_eq!(regression.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&regression.stdout);
    assert!(
        stdout.contains("wrong.appa:1: Deploy: got deny, want allow"),
        "{stdout}"
    );

    let broken = dir.path().join("broken.appa");
    std::fs::write(&broken, "Deploy {\n  when: soon\n}\nexpect allow\n").expect("the trace writes");
    let refused = Command::new(appa)
        .args(["replay", "--config"])
        .arg(&config)
        .arg(&broken)
        .output()
        .expect("the binary runs");
    assert_eq!(refused.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("broken.appa:2: argument `when` is not one JSON value"),
        "{stderr}"
    );
}
