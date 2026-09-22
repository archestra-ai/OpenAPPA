use std::io::Write;
use std::process::{Command, Stdio};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.annotator]]
name = "classifier"
ranks = ["suspicious", "trusted"]
audiences = []
marks = []

[[policy.tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = { type = "object", properties = { url = { type = "string" } }, required = ["url"] }
annotator = "classifier"

[[policy.tool]]
name = "clock"
delta = {}

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.annotators.classifier]
command = ["/bin/sh", "classifier.sh"]
"#;

/// Answers inside its mandate for every call but one naming `secrets`, which it narrows to
/// an audience its mandate does not admit.
const CLASSIFIER: &str = r#"
case "$(cat)" in
  *secrets*) echo '{"version":1,"answer":{"delta":{"audience":["self"]},"requires":{"history":[],"attention":[]},"emits":[]}}' ;;
  *) echo '{"version":1,"answer":{"delta":{"trust":"suspicious"},"requires":{"history":[],"attention":[]},"emits":[]}}' ;;
esac
"#;

const CALLS: &str = r#"{"id": "page", "tool": "fetch", "arguments": {"url": "https://a.example"}}
{"id": "secrets", "tool": "fetch", "arguments": {"url": "https://a.example/secrets"}}
{"id": "clock", "tool": "clock", "arguments": {}}
{"id": "unknown", "tool": "deploy", "arguments": {}}
"#;

fn annotate(repeat: &str) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(dir.path().join("appa.toml"), POLICY).expect("the policy is written");
    std::fs::write(dir.path().join("classifier.sh"), CLASSIFIER).expect("the annotator is written");
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["runtime", "annotate", "--repeat", repeat, "--config"])
        .arg(dir.path().join("appa.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("appa starts");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(CALLS.as_bytes())
        .expect("the calls are written");
    let output = child.wait_with_output().expect("appa exits");
    assert!(output.status.success());
    let mut rows: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .expect("the answers are text")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every answer is one JSON line"))
        .collect();
    rows.sort_by_key(|row| (row["id"].as_str().map(str::to_string), row["repeat"].as_u64()));
    rows
}

#[test]
fn every_call_reports_what_a_fresh_proposal_would_get_from_its_annotator() {
    let rows = annotate("1");
    let outcomes: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row["id"].as_str().unwrap(), row["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("clock", "static"),
            ("page", "answer"),
            ("secrets", "outside_mandate"),
            ("unknown", "unresolved"),
        ]
    );
    let page = &rows[1];
    assert_eq!(page["annotator"], "classifier");
    assert_eq!(page["answer"]["delta"]["trust"], "suspicious");
}

#[test]
fn a_call_is_asked_once_per_repeat() {
    let rows = annotate("3");
    let repeats: Vec<u64> = rows
        .iter()
        .filter(|row| row["id"] == "page")
        .map(|row| row["repeat"].as_u64().unwrap())
        .collect();
    assert_eq!(repeats, [0, 1, 2]);
}
