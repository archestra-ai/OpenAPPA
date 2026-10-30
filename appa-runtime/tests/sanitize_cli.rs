#![cfg(unix)]
//! `appa runtime sanitize`: a policy's sanitizers asked about listed values through the
//! production consult path, each answer reported as what a session would take from it.

use std::io::Write;
use std::process::{Command, Stdio};

const POLICY: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "search"
parameters = { type = "object", properties = { query = { type = "string" } }, required = ["query"], additionalProperties = false }
delta = {}

[[policy.tool]]
name = "fetch"
delta = { trust = "suspicious" }

[policy.deployment]
context_control = true
confined_results = ["fetch"]

[[policy.sanitizer]]
name = "redactor"
on = ["tool_output", "tool_input"]
hint = "Remove personal data."

[policy.sanitizer.permits]
audience = { from = ["self"], to = ["public"] }

[[policy.sanitizer]]
name = "output-only"
on = ["tool_output"]

[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.sanitizers.redactor]
command = ["/bin/sh", "redactor.sh"]

[externals.sanitizers.output-only]
command = ["/bin/sh", "redactor.sh"]
"#;

/// Echoes what it was asked in its answer, so the test reads the consult it received:
/// a tool result's declaration and body, or a call's canonical arguments. `silent` gets no
/// answer, and `reshape` gets a rewrite that is no longer the tool's arguments.
const REDACTOR: &str = r#"
input="$(cat)"
case "$input" in
  *silent*) exit 1 ;;
  *reshape*) echo '{"version":1,"answer":{"body":"no longer arguments"}}' ;;
  *'"on":"tool_input"'*) echo '{"version":1,"answer":{"body":"{\"query\":\"[redacted]\"}"}}' ;;
  *'"tool":"fetch"'*) echo '{"version":1,"answer":{"body":"fetch output, redacted"}}' ;;
  *) echo '{"version":1,"answer":{"body":"return, redacted"}}' ;;
esac
"#;

const VALUES: &str = r#"{"id": "result", "sanitizer": "redactor", "on": "tool_output", "tool": "fetch", "body": "Jane Doe, SSN 078-05-1120"}
{"id": "return", "sanitizer": "redactor", "on": "tool_output", "body": "a child's return"}
{"id": "call", "sanitizer": "redactor", "on": "tool_input", "tool": "search", "body": {"query": "Jane Doe salary"}}
{"id": "reshaped", "sanitizer": "redactor", "on": "tool_input", "tool": "search", "body": {"query": "reshape"}}
{"id": "silent", "sanitizer": "redactor", "on": "tool_output", "body": "silent"}
{"id": "unregistered", "sanitizer": "nobody", "on": "tool_output", "body": "x"}
{"id": "wrong-point", "sanitizer": "output-only", "on": "tool_input", "tool": "search", "body": {"query": "x"}}
{"id": "not-a-call", "sanitizer": "redactor", "on": "tool_input", "tool": "search", "body": {"q": "x"}}
"#;

fn sanitize(repeat: &str) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(dir.path().join("appa.toml"), POLICY).expect("the policy is written");
    std::fs::write(dir.path().join("redactor.sh"), REDACTOR).expect("the sanitizer is written");
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["runtime", "sanitize", "--repeat", repeat, "--config"])
        .arg(dir.path().join("appa.toml"))
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("appa starts");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(VALUES.as_bytes())
        .expect("the values are written");
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
fn every_value_reports_what_a_session_would_take_from_its_sanitizer() {
    let rows = sanitize("1");
    let outcomes: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row["id"].as_str().unwrap(), row["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("call", "derived"),
            ("not-a-call", "unresolved"),
            ("reshaped", "rejected"),
            ("result", "derived"),
            ("return", "derived"),
            ("silent", "no_answer"),
            ("unregistered", "unresolved"),
            ("wrong-point", "unresolved"),
        ]
    );
    let body = |id: &str| rows.iter().find(|row| row["id"] == id).unwrap()["body"].clone();
    // The producer reaches the sanitizer: a result names its tool, a child's return none.
    assert_eq!(body("result"), "fetch output, redacted");
    assert_eq!(body("return"), "return, redacted");
    assert_eq!(body("call"), r#"{"query":"[redacted]"}"#);
    let reshaped = rows.iter().find(|row| row["id"] == "reshaped").unwrap();
    assert_eq!(reshaped["answer"]["body"], "no longer arguments");
}

#[test]
fn a_value_is_asked_once_per_repeat() {
    let rows = sanitize("2");
    let repeats: Vec<u64> = rows
        .iter()
        .filter(|row| row["id"] == "result")
        .map(|row| row["repeat"].as_u64().unwrap())
        .collect();
    assert_eq!(repeats, [0, 1]);
}
