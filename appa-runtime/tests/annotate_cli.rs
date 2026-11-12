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
    annotate_with(POLICY, CLASSIFIER, repeat)
}

fn annotate_with(policy: &str, classifier: &str, repeat: &str) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(dir.path().join("appa.toml"), policy).expect("the policy is written");
    std::fs::write(dir.path().join("classifier.sh"), classifier).expect("the annotator is written");
    std::fs::write(dir.path().join("owner.sh"), OWNER).expect("the context provider is written");
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

/// A context provider that knows who owns what `fetch` reads, and declines every other call.
const OWNER: &str = r#"
case "$(cat)" in
  *'"tool":"fetch"'*) echo '{"version":1,"answer":{"owner":"etl"}}' ;;
  *) echo '{"version":1,"answer":null}' ;;
esac
"#;

/// Trusts a call only when its context carries the owner's answer and the failed
/// provider's error beside it.
const CONTEXT_CLASSIFIER: &str = r#"
input="$(cat)"
case "$input" in
  *'"broken":{"error":'*'"owner":{"answer":{"owner":"etl"}}'*) trust=trusted ;;
  *) trust=suspicious ;;
esac
echo "{\"version\":1,\"answer\":{\"delta\":{\"trust\":\"$trust\"},\"requires\":{\"history\":[],\"attention\":[]},\"emits\":[]}}"
"#;

#[test]
fn the_annotator_asked_by_the_cli_sees_what_the_context_providers_answered() {
    let policy = format!(
        "{POLICY}\n[externals.context.owner]\ncommand = [\"/bin/sh\", \"owner.sh\"]\n\n\
         [externals.context.broken]\ncommand = [\"/bin/sh\", \"-c\", \"exit 1\"]\n"
    );
    let rows = annotate_with(&policy, CONTEXT_CLASSIFIER, "1");
    let page = rows
        .iter()
        .find(|row| row["id"] == "page")
        .expect("the page call is reported");
    assert_eq!(
        page["outcome"], "answer",
        "a provider that fails does not refuse the call"
    );
    assert_eq!(page["answer"]["delta"]["trust"], "trusted");
}

/// Trusts a call only when its declaration carries the deployment hint and the hints of
/// exactly the providers with an entry in its context: `quiet` declines the call.
const HINTED_CLASSIFIER: &str = r#"
input="$(cat)"
case "$input" in
  *'"context_hints":{"broken":"broken hint","owner":"owner hint"}'*'"deployment_hint":"deployment hint"'*) trust=trusted ;;
  *) trust=suspicious ;;
esac
echo "{\"version\":1,\"answer\":{\"delta\":{\"trust\":\"$trust\"},\"requires\":{\"history\":[],\"attention\":[]},\"emits\":[]}}"
"#;

#[test]
fn the_annotator_asked_by_the_cli_reads_the_deployer_hints() {
    let providers = r#"
[externals.context.owner]
command = ["/bin/sh", "owner.sh"]
hint = "owner hint"

[externals.context.broken]
command = ["/bin/sh", "-c", "exit 1"]
hint = "broken hint"

[externals.context.quiet]
command = ["/bin/sh", "-c", 'cat > /dev/null; printf "%s" "{\"version\":1,\"answer\":null}"']
hint = "quiet hint"

[policy.deployment]
hint = "deployment hint"
"#;
    let policy = format!("{POLICY}{providers}");
    let rows = annotate_with(&policy, HINTED_CLASSIFIER, "1");
    let page = rows
        .iter()
        .find(|row| row["id"] == "page")
        .expect("the page call is reported");
    assert_eq!(page["outcome"], "answer");
    assert_eq!(page["answer"]["delta"]["trust"], "trusted");
}

#[test]
fn zero_concurrency_is_refused_rather_than_hanging() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    std::fs::write(dir.path().join("appa.toml"), POLICY).expect("the policy is written");
    std::fs::write(dir.path().join("classifier.sh"), CLASSIFIER).expect("the annotator is written");
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["runtime", "annotate", "--concurrency", "0", "--config"])
        .arg(dir.path().join("appa.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("appa starts");
    // A refused flag exits before reading, so the write may meet a closed pipe.
    let _ = child.stdin.take().expect("stdin is piped").write_all(CALLS.as_bytes());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("appa is waitable") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            child.kill().expect("a hung appa is killable");
            child.wait().expect("a killed appa is reaped");
            panic!("annotate with zero concurrency never exited");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(!status.success());
}

#[test]
fn a_closed_reader_ends_the_run_before_the_queued_calls_are_asked() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let asked = dir.path().join("asked.log");
    std::fs::write(dir.path().join("appa.toml"), POLICY).expect("the policy is written");
    // Slow enough that the reader closes before the second answer is written.
    let classifier = format!(
        "cat > /dev/null; echo asked >> '{}'; sleep 0.2\n{CLASSIFIER}",
        asked.display()
    );
    std::fs::write(dir.path().join("classifier.sh"), classifier).expect("the annotator is written");
    let calls: String = (0..8)
        .map(|n| {
            format!("{{\"id\": \"{n}\", \"tool\": \"fetch\", \"arguments\": {{\"url\": \"https://a.example/{n}\"}}}}\n")
        })
        .collect();
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["runtime", "annotate", "--concurrency", "1", "--config"])
        .arg(dir.path().join("appa.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("appa starts");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(calls.as_bytes())
        .expect("the calls are written");
    let mut first = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(child.stdout.take().expect("stdout is piped")),
        &mut first,
    )
    .expect("the first answer is read");
    assert!(first.contains("\"outcome\":\"answer\""), "{first}");
    let status = child.wait().expect("appa exits");
    let asked = std::fs::read_to_string(&asked)
        .expect("the annotator was asked")
        .lines()
        .count();
    assert!(asked < 8, "every queued call was still asked after the reader closed");
    assert!(
        !status.success(),
        "a run that could not write its answers reports a failure"
    );
}
