//! `appa replay` over the real hook path: traces run against an in-memory deployment, and
//! the shipped examples stay green. Every test declares the one policy it needs, next to it.

mod common;
use common::{repo_root, serve};

use std::collections::BTreeSet;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Command;

use appa_runtime::api::{OfferKind, Remedies, Runtime};
use appa_runtime::config::Config;
use appa_runtime::replay::{
    Expect, Got, StepOutcome, Summary, Taken, Trace, TraceReport, Verdict, collect, parse, render, render_json, run,
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
    Runtime::open_in_memory(config, None, Remedies::StandIn).expect("the deployment opens in memory")
}

/// [`runtime`], serving `appa/do_something` as `appa replay` does.
fn serving(dir: &tempfile::TempDir, policy: &str, bindings: &str) -> Runtime {
    let path = dir.path().join("appa.toml");
    std::fs::write(&path, format!("[policy]\nversion = 2\n{policy}{EXTERNALS}{bindings}")).expect("the fixture writes");
    let config = Config::load(&path).expect("the fixture validates");
    Runtime::open_in_memory(config.with_do_something(), None, Remedies::StandIn)
        .expect("the deployment opens in memory")
}

/// An `appa/do_something` step whose `requires` is the given JSON object.
fn do_something(requires: &str, expect: &str) -> String {
    format!("appa/do_something {{\n  requires: {requires}\n}}\nexpect {expect}\n\n")
}

fn trace(name: &str, text: &str) -> Trace {
    parse(Path::new(name), text).expect("the trace parses")
}

fn outcomes(report: &TraceReport) -> Vec<(usize, &StepOutcome)> {
    report.steps.iter().map(|step| (step.line, &step.outcome)).collect()
}

fn rendered(reports: &[TraceReport]) -> (String, Summary) {
    let mut out = Vec::new();
    let summary = render(reports, Remedies::StandIn, false, &mut out).expect("renders");
    (String::from_utf8(out).expect("utf-8"), summary)
}

const ALLOWED: StepOutcome = StepOutcome::Passed {
    got: Got::Allowed,
    taken: None,
};
const DENIED: StepOutcome = StepOutcome::Passed {
    got: Got::Blocked(BTreeSet::new()),
    taken: None,
};

fn taken(kind: OfferKind) -> StepOutcome {
    StepOutcome::Passed {
        got: Got::Blocked(BTreeSet::from([kind.clone()])),
        taken: Some(Taken {
            offer: kind,
            rewritten: None,
        }),
    }
}

/// A step that passed on an offer it did not take.
fn offered(kind: OfferKind) -> StepOutcome {
    StepOutcome::Passed {
        got: Got::Blocked(BTreeSet::from([kind])),
        taken: None,
    }
}

/// An email leaves the company; a deploy needs a backup that never comes.
const ALLOW_AND_DENY_POLICY: &str = r#"
[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "mcp/ops/deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}
"#;

#[tokio::test]
async fn one_allow_and_one_deny_pass() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, ALLOW_AND_DENY_POLICY, "");
    let traces = [
        trace(
            "allow.appa",
            "mcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect allow\n",
        ),
        trace("deny.appa", "mcp/ops/deploy {}\nexpect deny\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(outcomes(&reports[0]), vec![(1, &ALLOWED)]);
    assert_eq!(outcomes(&reports[1]), vec![(1, &DENIED)]);
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
name = "mcp/files/read(path:/hr/*)"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "mcp/files/read"
delta = {}

[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "mcp/slack/post_message"
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
            "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nmcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect deny\n\nmcp/slack/post_message {\n  text: \"reviewed\"\n}\nexpect allow\n",
        ),
        trace(
            "no-read.appa",
            "mcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect allow\n",
        ),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![(1, &taken(OfferKind::Accept)), (6, &DENIED), (11, &ALLOWED)]
    );
    assert_eq!(reports[1].verdict(), Verdict::Ok);
}

fn observed(got: Got, taken: Option<OfferKind>) -> StepOutcome {
    StepOutcome::Observed {
        got,
        taken: taken.map(|offer| Taken { offer, rewritten: None }),
    }
}

/// Steps without `expect` report what they got and never end the file. The HR read is
/// accepted as `expect allow` would; the email it denies takes nothing; the post still runs.
/// A denied call never ran, so its narrowing `delta` does not land on the trajectory.
#[tokio::test]
async fn observed_steps_report_and_never_end_the_file() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let policy = format!(
        "{NARROWING_POLICY}\n[[policy.tool]]\nname = \"mcp/ops/deploy\"\nrequires = {{ effects = {{ contains = [\"backup.completed\"] }} }}\ndelta = {{ audience = [\"hr\"] }}\n"
    );
    let runtime = runtime(&dir, &policy, "");
    let email = "mcp/mail/send {\n  to: \"x@other.com\"\n}\n";
    let traces = [
        trace(
            "observe.appa",
            &format!(
                "mcp/files/read {{\n  path: \"/hr/salaries.csv\"\n}}\n{email}mcp/slack/post_message {{\n  text: \"reviewed\"\n}}\n{email}expect deny\n"
            ),
        ),
        trace("unlanded.appa", &format!("mcp/ops/deploy {{}}\n{email}")),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![
            (
                1,
                &observed(
                    Got::Blocked(BTreeSet::from([OfferKind::Accept])),
                    Some(OfferKind::Accept)
                )
            ),
            (4, &observed(Got::Blocked(BTreeSet::new()), None)),
            (7, &observed(Got::Allowed, None)),
            (10, &DENIED),
        ]
    );
    assert_eq!(
        outcomes(&reports[1]),
        vec![
            (1, &observed(Got::Blocked(BTreeSet::new()), None)),
            (2, &observed(Got::Allowed, None)),
        ]
    );
    let (_, summary) = rendered(&reports);
    assert_eq!((summary.ok, summary.exit_code()), (2, std::process::ExitCode::SUCCESS));
}

/// A wipe needs a person's sign-off; a deploy needs a backup that never comes.
const ATTENTION_POLICY: &str = r#"
[[policy.tool]]
name = "mcp/ops/wipe"
requires = { attention = ["signoff"] }
delta = {}

[[policy.tool]]
name = "mcp/ops/deploy"
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
        trace(
            "wipe.appa",
            "mcp/ops/wipe {}\nexpect authority\n\nmcp/ops/deploy {}\nexpect deny\n",
        ),
        trace("wipe-named.appa", "mcp/ops/wipe {}\nexpect authority approver\n"),
        trace("wipe-denied.appa", "mcp/ops/wipe {}\nexpect deny\n"),
        trace("wipe-other.appa", "mcp/ops/wipe {}\nexpect authority cto\n"),
    ];
    let reports = run(&runtime, &traces).await;
    assert_eq!(outcomes(&reports[0]), vec![(1, &taken(approver())), (4, &DENIED)]);
    assert_eq!(outcomes(&reports[1]), vec![(1, &taken(approver()))]);
    assert!(matches!(
        &reports[2].steps[0].outcome,
        StepOutcome::Mismatch { got: Got::Blocked(kinds), want: Expect::Deny, .. }
            if kinds == &BTreeSet::from([approver()])
    ));
    let (text, _) = rendered(&reports);
    assert!(
        text.contains("wipe-denied.appa:1: mcp/ops/wipe: got authority approver, want deny\n"),
        "{text}"
    );
    assert!(
        text.contains("wipe-other.appa:1: mcp/ops/wipe: got authority approver, want authority cto\n"),
        "{text}"
    );
}

/// HR reads narrow the audience; an export needs `public`; the redactor is the declared
/// way from `hr` to `public`.
const REDACTION_POLICY: &str = r#"
[[policy.tool]]
name = "mcp/files/read(path:/hr/*)"
delta = { audience = ["hr"] }

[[policy.tool]]
name = "mcp/reports/export"
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
        "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nmcp/reports/export {\n  body: \"salaries\"\n}\nexpect sanitizer\n\nmcp/reports/export {\n  body: \"again\"\n}\nexpect sanitizer redactor\n",
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

/// The stand-in never rewrites, and the report never credits the redactor with a rewrite.
/// Under `Remedies::Live` the bound redactor answers: the call is released with its rewritten
/// arguments, and the report shows them. An authority nothing binds then has nobody to answer.
#[tokio::test]
async fn the_stand_in_owns_its_answers_and_live_remedies_rewrite() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let policy = format!("{REDACTION_POLICY}{ATTENTION_POLICY}");
    let path = dir.path().join("appa.toml");
    std::fs::write(
        &path,
        format!("[policy]\nversion = 2\n{policy}{EXTERNALS}{REDACTOR_BINDING}"),
    )
    .expect("the fixture writes");
    let open = |remedies| {
        Runtime::open_in_memory(Config::load(&path).expect("the fixture validates"), None, remedies)
            .expect("the deployment opens in memory")
    };
    let text = "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nmcp/reports/export {\n  body: \"write to bob@example.com today\"\n}\nexpect sanitizer redactor\n";
    let traces = [
        trace("export.appa", text),
        trace("wipe.appa", "mcp/ops/wipe {}\nexpect authority approver\n"),
    ];
    let render_text = |reports: &[TraceReport], remedies| {
        let mut out = Vec::new();
        render(reports, remedies, true, &mut out).expect("renders");
        String::from_utf8(out).expect("utf-8")
    };

    let stood_in = run(&open(Remedies::StandIn), &traces).await;
    assert_eq!(outcomes(&stood_in[0])[1], (6, &taken(redactor())));
    let shown = render_text(&stood_in, Remedies::StandIn);
    assert!(
        shown.contains("export.appa:6 mcp/reports/export sanitizer redactor (the stand-in answered for redactor: value unchanged)\n"),
        "{shown}"
    );
    assert!(
        shown.contains("wipe.appa:1 mcp/ops/wipe authority approver (the stand-in approved for approver)\n"),
        "{shown}"
    );
    assert!(
        shown.contains("note: a stand-in answered 2 authority or sanitizer consults"),
        "{shown}"
    );
    assert!(
        !shown.contains("rewrote") && !shown.contains("after approver"),
        "{shown}"
    );

    let live = run(&open(Remedies::Live), &traces).await;
    let (line, outcome) = outcomes(&live[0])[1];
    let StepOutcome::Passed {
        got: Got::Blocked(blocked),
        taken: Some(Taken {
            offer,
            rewritten: Some(rewritten),
        }),
    } = outcome
    else {
        panic!("the live redactor rewrites the export: {outcome:?}");
    };
    assert_eq!((line, blocked, offer), (6, &BTreeSet::from([redactor()]), &redactor()));
    // What the redactor writes in place of the address is its own choice; the address is gone.
    let body = rewritten["body"].as_str().expect("the rewrite keeps the body a string");
    assert_eq!(rewritten, &serde_json::json!({"body": body}));
    assert!(!body.contains("bob@example.com"), "{rewritten}");
    assert!(matches!(&live[1].steps[0].outcome, StepOutcome::CannotRun(_)));
    let shown = render_text(&live, Remedies::Live);
    assert!(
        shown.contains(&format!(
            "export.appa:6 mcp/reports/export sanitizer redactor (after redactor rewrote it)\n    rewritten arguments: {rewritten}\n"
        )),
        "{shown}"
    );
    assert!(!shown.contains("stand-in"), "{shown}");

    let mut out = Vec::new();
    render_json(&live, Remedies::Live, &mut out).expect("renders");
    let export: serde_json::Value = serde_json::from_str(
        String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .nth(1)
            .expect("the export step"),
    )
    .expect("one JSON value");
    assert_eq!(
        (&export["answered_by"], &export["rewritten"]),
        (&serde_json::json!("live"), rewritten)
    );
}

fn redactor() -> OfferKind {
    OfferKind::Sanitizer {
        name: "redactor".into(),
    }
}

/// A read that restricts nothing; an email leaves the company; a deploy needs a backup
/// that never comes.
const MISMATCH_POLICY: &str = r#"
[[policy.tool]]
name = "mcp/files/read"
delta = {}

[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "mcp/ops/deploy"
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
            "mcp/files/read {\n  path: \"/docs/readme.md\"\n}\nexpect allow\n\nmcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect deny\n\nmcp/ops/deploy {}\nexpect deny\n",
        ),
        trace("right.appa", "mcp/ops/deploy {}\nexpect deny\n"),
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
        "wrong.appa:6: mcp/mail/send: got allow, want deny\nFAIL  wrong.appa\nok    right.appa\n2 files: 1 ok, 1 failed, 0 could not run\n"
    );
    assert_eq!(summary.exit_code(), std::process::ExitCode::from(1));
}

/// A deploy needs a backup that never comes.
const DEPLOY_POLICY: &str = r#"
[[policy.tool]]
name = "mcp/ops/deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}
"#;

#[tokio::test]
async fn a_deny_mismatch_prints_the_feedback() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, DEPLOY_POLICY, "");
    let traces = [trace("deploy.appa", "mcp/ops/deploy {}\nexpect allow\n")];
    let reports = run(&runtime, &traces).await;
    let (text, _) = rendered(&reports);
    assert!(
        text.starts_with("deploy.appa:1: mcp/ops/deploy: got deny, want allow\n    [appa] Blocked"),
        "{text}"
    );
}

#[tokio::test]
async fn an_undeclared_tool_cannot_run_and_is_never_a_deny() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, DEPLOY_POLICY, "");
    let traces = [trace("ghost.appa", "mcp/ops/ghost {}\nexpect deny\n")];
    let reports = run(&runtime, &traces).await;
    assert!(matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(_)));
    assert_eq!(reports[0].verdict(), Verdict::CannotRun);
    let (text, summary) = rendered(&reports);
    assert!(text.starts_with("ghost.appa:1: mcp/ops/ghost: cannot run: "), "{text}");
    assert_eq!(summary.exit_code(), std::process::ExitCode::from(2));
}

/// Files run at once, each in its own trajectory: one file's narrowing is invisible to the
/// other, whichever finishes first.
#[tokio::test]
async fn files_do_not_share_trajectory_state() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, NARROWING_POLICY, "");
    let narrowing = "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\nmcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect deny\n";
    let fresh = "mcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect allow\n";
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
name = "mcp/web/fetch"
annotator = "classifier"

[[policy.tool]]
name = "mcp/mail/send"
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
    let text = "mcp/web/fetch {\n  url: \"https://example.com\"\n}\nexpect allow\n\nmcp/mail/send {}\nexpect deny\n";

    let reports = run(&runtime, &[trace("annotated.appa", text)]).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![(1, &taken(OfferKind::Accept)), (6, &DENIED)]
    );

    *answer.lock().unwrap() = false;
    let reports = run(&runtime, &[trace("down.appa", text)]).await;
    assert!(
        matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(detail) if detail.contains("classifier")),
        "{:?}",
        reports[0]
    );
}

/// `appa/do_something` asks what the trajectory allows: public before the HR read, and only
/// HR after it. The steps that ask change nothing, so the real email after them is judged
/// as if they had not been proposed.
#[tokio::test]
async fn do_something_asks_what_the_trajectory_allows() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = serving(&dir, NARROWING_POLICY, "");
    let public = r#"{"audience": {"contains": ["public"]}}"#;
    let text = [
        do_something(public, "allow"),
        "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\n".to_string(),
        do_something(public, "deny"),
        do_something(r#"{"audience": {"contains": ["hr"]}}"#, "allow"),
        "mcp/mail/send {\n  to: \"x@other.com\"\n}\nexpect deny\n".to_string(),
    ]
    .concat();
    let reports = run(&runtime, &[trace("private.appa", &text)]).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![
            (1, &ALLOWED),
            (6, &taken(OfferKind::Accept)),
            (11, &DENIED),
            (16, &ALLOWED),
            (21, &DENIED),
        ]
    );

    let regressed = serving(
        &tempfile::tempdir().expect("a temp dir"),
        &NARROWING_POLICY.replace("delta = { audience = [\"hr\"] }", "delta = {}"),
        "",
    );
    let reports = run(&regressed, &[trace("private.appa", &text)]).await;
    let (rendered, _) = rendered(&reports);
    assert!(
        rendered.contains("private.appa:11: appa/do_something: got allow, want deny\n"),
        "{rendered}"
    );
}

/// A matching offer passes the step without being taken, so the same question gets the same
/// offer again, and a real call afterwards still needs its own approval or rewrite.
#[tokio::test]
async fn do_something_never_takes_its_offer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = serving(&dir, ATTENTION_POLICY, "");
    let signoff = r#"{"attention": ["signoff"]}"#;
    let text = [
        do_something(signoff, "authority approver"),
        do_something(signoff, "authority"),
        "mcp/ops/wipe {}\nexpect authority approver\n".to_string(),
    ]
    .concat();
    let reports = run(&runtime, &[trace("signoff.appa", &text)]).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![
            (1, &offered(approver())),
            (6, &offered(approver())),
            (11, &taken(approver()))
        ]
    );

    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = serving(&dir, REDACTION_POLICY, REDACTOR_BINDING);
    let text = [
        "mcp/files/read {\n  path: \"/hr/salaries.csv\"\n}\nexpect allow\n\n".to_string(),
        do_something(r#"{"audience": {"contains": ["public"]}}"#, "sanitizer redactor"),
        "mcp/reports/export {\n  body: \"salaries\"\n}\nexpect sanitizer redactor\n".to_string(),
    ]
    .concat();
    let reports = run(&runtime, &[trace("export.appa", &text)]).await;
    let redactor = OfferKind::Sanitizer {
        name: "redactor".into(),
    };
    assert_eq!(
        outcomes(&reports[0]),
        vec![
            (1, &taken(OfferKind::Accept)),
            (6, &offered(redactor.clone())),
            (11, &taken(redactor))
        ]
    );
}

/// Trust is asked the same way: a `trusted` floor holds until suspicious content arrives.
#[tokio::test]
async fn do_something_asks_about_trust() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let policy = "\n[[policy.tool]]\nname = \"mcp/web/fetch\"\ndelta = { trust = \"suspicious\" }\n";
    let runtime = serving(&dir, policy, "");
    let trusted = r#"{"trust": "trusted"}"#;
    let text = [
        do_something(trusted, "allow"),
        "mcp/web/fetch {\n  url: \"https://example.com\"\n}\nexpect allow\n\n".to_string(),
        do_something(trusted, "deny"),
    ]
    .concat();
    let reports = run(&runtime, &[trace("trust.appa", &text)]).await;
    assert_eq!(
        outcomes(&reports[0]),
        vec![(1, &ALLOWED), (6, &taken(OfferKind::Accept)), (11, &DENIED)]
    );
}

/// A requirement nothing can lift is a plain deny, as for a declared tool.
#[tokio::test]
async fn do_something_is_denied_when_nothing_is_offered() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = serving(&dir, ATTENTION_POLICY, "");
    let text = do_something(r#"{"effects": {"contains": ["backup.completed"]}}"#, "deny");
    let reports = run(&runtime, &[trace("backup.appa", &text)]).await;
    assert_eq!(outcomes(&reports[0]), vec![(1, &DENIED)]);
}

/// A requirement the policy cannot state is a step that cannot run, never a deny that would
/// pass `expect deny`.
#[tokio::test]
async fn a_requirement_the_policy_cannot_state_cannot_run() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = serving(&dir, ATTENTION_POLICY, "");
    for (requires, says) in [
        (r#"{"trust": "vetted"}"#, "unknown trust rank \"vetted\""),
        (r#"{"attention": ["legal"]}"#, "no attention mark legal"),
        (r#"{"effects": {"contains": ["audited"]}}"#, "no effect audited"),
        (
            r#"{"audience": {"contains": ["@nobody"]}}"#,
            "names an audience the policy does not",
        ),
        (r#"{"audience": {"contains": ["$to"]}}"#, "names a call argument"),
        (r#"{"audiences": {"contains": ["public"]}}"#, "audiences"),
    ] {
        let reports = run(&runtime, &[trace("refused.appa", &do_something(requires, "deny"))]).await;
        assert!(
            matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(detail) if detail.contains(says)),
            "{requires}: {:?}",
            reports[0].steps[0].outcome
        );
    }
}

/// A deployment that does not serve `appa/do_something` refuses the name, even where a
/// wildcard covers every other undeclared call: the wildcard's annotator is never asked.
#[tokio::test]
async fn only_a_serving_deployment_decides_do_something() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = asked.clone();
    let router = Router::new().route(
        "/annotate",
        post(move |_body: String| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                serde_json::json!({
                    "version": 1,
                    "answer": { "delta": {}, "requires": { "history": [], "attention": [] }, "emits": [] }
                })
                .to_string()
            }
        }),
    );
    let url = format!("{}/annotate", serve(router).await);
    let policy =
        "\n[[policy.annotator]]\nname = \"classifier\"\n\n[[policy.tool]]\nname = \"*\"\nannotator = \"classifier\"\n";
    let binding = format!("\n[externals.annotators.classifier]\nurl = \"{url}\"\n");
    let dir = tempfile::tempdir().expect("a temp dir");
    let runtime = runtime(&dir, policy, &binding);
    let text = do_something(r#"{"audience": {"contains": ["public"]}}"#, "allow");
    let reports = run(&runtime, &[trace("unserved.appa", &text)]).await;
    assert!(
        matches!(&reports[0].steps[0].outcome, StepOutcome::CannotRun(detail) if detail.contains("appa/do_something")),
        "{:?}",
        reports[0].steps[0].outcome
    );
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// `appa/do_something` and its Annotator are the runtime's own: a policy declaring either is
/// refused, whether or not the deployment serves it.
#[test]
fn a_policy_cannot_declare_do_something() {
    for policy in [
        "\n[[policy.tool]]\nname = \"appa/do_something\"\ndelta = {}\n",
        "\n[[policy.annotator]]\nname = \"appa/do_something\"\n\n[[policy.tool]]\nname = \"mcp/web/fetch\"\nannotator = \"appa/do_something\"\n\n[externals.annotators.\"appa/do_something\"]\nurl = \"http://127.0.0.1:9/annotate\"\n",
    ] {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, format!("[policy]\nversion = 2\n{policy}{EXTERNALS}")).expect("the fixture writes");
        let refused = |config: Config| {
            Runtime::open_in_memory(config, None, Remedies::StandIn)
                .err()
                .map(|error| error.to_string())
        };
        let config = Config::load(&path).expect("the fixture validates");
        for opened in [refused(config.clone()), refused(config.with_do_something())] {
            assert!(
                opened
                    .as_deref()
                    .is_some_and(|error| error.contains("reserved") && error.contains("appa/do_something")),
                "{policy}: {opened:?}"
            );
        }
    }
}

#[cfg(unix)]
fn example_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples/tests"))
        .expect("the examples directory is readable")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[cfg(unix)]
#[tokio::test]
async fn every_shipped_example_passes() {
    let dirs = example_dirs();
    assert!(dirs.len() >= 7, "the shipped examples were found: {dirs:?}");
    for dir in dirs {
        let config = Config::load(&dir.join("appa.toml")).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let runtime = Runtime::open_in_memory(config.with_do_something(), None, Remedies::StandIn)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
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
    let example = repo_root().join("examples/tests/backup-before-deploy");
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
    std::fs::write(&wrong, "mcp/ops/deploy {}\nexpect allow\n").expect("the trace writes");
    let regression = Command::new(appa)
        .args(["replay", "--config"])
        .arg(&config)
        .arg(&wrong)
        .output()
        .expect("the binary runs");
    assert_eq!(regression.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&regression.stdout);
    assert!(
        stdout.contains("wrong.appa:1: mcp/ops/deploy: got deny, want allow"),
        "{stdout}"
    );

    let broken = dir.path().join("broken.appa");
    std::fs::write(&broken, "mcp/ops/deploy {\n  when: soon\n}\nexpect allow\n").expect("the trace writes");
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
