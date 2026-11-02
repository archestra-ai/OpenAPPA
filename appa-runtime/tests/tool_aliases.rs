//! A battery writes its rules once for a canonical tool, `tool/<namespace>/<tool>`, and
//! another battery's `[aliases.tool]` map binds that tool to each client spelling of it.
//! The client's call is evaluated as sent: only the policy's rules are respelled.
mod common;

use std::path::Path;

use appa_runtime::config::{Config, ConfigError};
use appa_runtime_api::{HookDecision, ProposedCall};
use common::{propose, ran, raw, session_runtime};

const CODING: &str = r#"
[policy]
version = 2

[[policy.tool]]
name = "tool/coding/shell(command:ls*)"
delta = {}
"#;

const CLIENTS: &str = r#"
[aliases.tool.coding]
shell = [
  { name = "host/claude-code/Bash", args = { command = "command" } },
  { name = "host/claude-code/exec_command", args = { command = "cmd" } },
]

[policy]
version = 2
"#;

fn write_deployment(dir: &Path, batteries: &[(&str, &str)]) -> std::path::PathBuf {
    for (name, text) in batteries {
        let battery = dir.join("batteries").join(name);
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("appa.toml"), text).unwrap();
    }
    let include: Vec<String> = batteries
        .iter()
        .map(|(name, _)| format!("\"batteries/{name}/appa.toml\""))
        .collect();
    let root = dir.join("appa.toml");
    std::fs::write(
        &root,
        format!(
            "include = [{}]\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n",
            include.join(", ")
        ),
    )
    .unwrap();
    root
}

fn call(tool: &str, arguments: serde_json::Value) -> ProposedCall {
    ProposedCall {
        tool: tool.to_string(),
        arguments: raw(arguments),
        cwd: None,
    }
}

#[tokio::test]
async fn a_canonical_rule_matches_each_client_spelling_of_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_deployment(dir.path(), &[("coding", CODING), ("clients", CLIENTS)]);
    let loaded = Config::load(&root).unwrap();
    let stored: toml::Value = toml::from_str(std::str::from_utf8(loaded.policy_file().bytes()).unwrap()).unwrap();
    assert!(stored.get("aliases").is_none(), "the stored policy carries no map");
    let names: Vec<&str> = stored["policy"]["tool"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|rule| rule["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "host/claude-code/Bash(command:ls*)",
            "host/claude-code/exec_command(cmd:ls*)"
        ]
    );
    let runtime = session_runtime(dir.path(), &root).await;

    for allowed in [
        call("host/claude-code/Bash", serde_json::json!({ "command": "ls -la" })),
        call("host/claude-code/exec_command", serde_json::json!({ "cmd": "ls -la" })),
    ] {
        let decision = propose(&runtime, allowed.clone()).await;
        assert!(matches!(decision, HookDecision::AllowCall { .. }), "{decision:?}");
        ran(&runtime, allowed).await;
    }
    for refused in [
        call(
            "host/claude-code/exec_command",
            serde_json::json!({ "command": "ls -la" }),
        ),
        call(
            "host/claude-code/exec_command",
            serde_json::json!({ "cmd": "rm -rf /" }),
        ),
        call("host/claude-code/Bash", serde_json::json!({ "cmd": "ls -la" })),
    ] {
        let decision = propose(&runtime, refused).await;
        assert!(!matches!(decision, HookDecision::AllowCall { .. }), "{decision:?}");
    }
}

#[test]
fn a_map_without_the_battery_it_maps_is_refused_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let root = write_deployment(dir.path(), &[("clients", CLIENTS)]);
    let refused = Config::load(&root).unwrap_err();
    assert!(matches!(&refused, ConfigError::ToolAlias(_)), "{refused}");
}
