use std::process::Command;

#[test]
fn describe_succeeds_for_a_missing_config_without_creating_files() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("missing.toml");

    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("describe")
        .args(["--config"])
        .arg(&config)
        .output()
        .expect("describe runs");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let description = String::from_utf8(output.stdout).expect("text description");
    assert!(description.starts_with("OpenAPPA world"));
    assert!(description.contains(&format!("Config: {} (missing)", config.display())));
    assert!(!config.exists(), "describe must not create a default config");
}

#[test]
fn describe_reports_malformed_config_without_echoing_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("appa.toml");
    let secret = "do-not-print-this-value";
    std::fs::write(&config, format!("token = \\\"{secret}")).expect("malformed config");

    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("describe")
        .args(["--config"])
        .arg(&config)
        .output()
        .expect("describe runs");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("(unparsable)"));
    assert!(!stdout.contains(secret));
}

#[test]
fn bare_describe_uses_the_installed_config_directory() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let expected = directory.path().join("appa.toml");

    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .env("APPA_CONFIG_DIR", directory.path())
        .arg("describe")
        .output()
        .expect("describe runs");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let description = String::from_utf8(output.stdout).expect("text description");
    assert!(description.contains(&format!("Config: {} (missing)", expected.display())));
    assert!(!expected.exists());
}

#[test]
fn describe_has_one_text_interface_and_no_json_mode() {
    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["describe", "--json"])
        .output()
        .expect("describe runs");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--json'"));
}

/// A deploy needs a backup no tool records and no authority waives.
const UNSATISFIABLE: &str = r#"[policy]
version = 2

[[policy.tool]]
name = "mcp/ops/deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 65536
"#;

/// The same policy with a tool that records the backup.
const SATISFIABLE: &str = r#"[policy]
version = 2

[[policy.tool]]
name = "mcp/ops/backup"
effects = ["backup.completed"]
delta = {}

[[policy.tool]]
name = "mcp/ops/deploy"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}

[externals]
timeout_ms = 1000
max_body_bytes = 65536
"#;

fn checked(policy: &str) -> std::process::ExitStatus {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("appa.toml");
    std::fs::write(&config, policy).expect("config");
    Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["describe", "--check", "--config"])
        .arg(&config)
        .output()
        .expect("describe runs")
        .status
}

#[test]
fn describe_check_fails_on_a_requirement_nothing_can_meet() {
    assert!(!checked(UNSATISFIABLE).success());
    assert!(checked(SATISFIABLE).success());
}

/// The check is an authoring check: the policy still loads, and its call is refused with no
/// plan.
#[tokio::test]
async fn a_requirement_nothing_can_meet_loads_and_hard_blocks_its_call() {
    use crate::common::{propose, raw, session_runtime};
    use appa_runtime_api::{HookDecision, Outcomes, ProposedCall};

    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("appa.toml");
    std::fs::write(&config, UNSATISFIABLE).expect("config");
    let runtime = session_runtime(directory.path(), &config).await;
    let decision = propose(
        &runtime,
        ProposedCall {
            tool: "mcp/ops/deploy".to_string(),
            arguments: raw(serde_json::json!({})),
            cwd: None,
        },
    )
    .await;
    let HookDecision::DenyCall { offers, outcomes, .. } = decision else {
        panic!("the call is denied, got {decision:?}");
    };
    assert!(offers.is_empty());
    assert_eq!(outcomes, Some(Outcomes::HardBlock));
}
