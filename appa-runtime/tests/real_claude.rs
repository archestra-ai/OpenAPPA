#![cfg(unix)]
//! Claude Code's own semantics, captured against the real CLI.
//!
//! These are the facts activation depends on, and none of them is stated
//! anywhere in this repository: they were established by running `claude`
//! against an isolated `CLAUDE_CONFIG_DIR`. They are pinned here so a change in
//! Claude's behaviour surfaces as a failing test rather than as a broken
//! install.
//!
//! Ignored by default: they need a real `claude` on PATH and, for the hook
//! entries, its credentials and one model turn. Run with
//! `cargo test --test real_claude -- --ignored`.

use std::fs;
use std::path::Path;
use std::process::Command;

fn claude(config: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new("claude")
        .args(arguments)
        .env("CLAUDE_CONFIG_DIR", config)
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .expect("the claude CLI runs")
}

/// The `system`/`init` line of a `stream-json` session, which Claude Code
/// prints before its first model call, credentials or not.
fn session_init(output: &std::process::Output) -> serde_json::Value {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|line: &serde_json::Value| line["type"] == "system" && line["subtype"] == "init")
        .expect("the session announces itself")
}

/// In an isolated profile, without credentials: the MCP template is stored
/// verbatim, a duplicate name is refused, removal reports what activation's
/// rollback reads, and a skill under the user skills directory is loaded as
/// a slash command.
#[test]
#[ignore = "needs a real claude CLI"]
fn real_claude_keeps_the_mcp_template_and_loads_the_user_skill() {
    let directory = tempfile::tempdir().expect("temporary directory");
    // A path with a space: every registration must survive one.
    let root = directory.path().join("a space");
    let config = root.join("claude config");
    fs::create_dir_all(&config).expect("the isolated Claude config directory");

    let skill = config.join("skills/appa-guide/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).expect("the skill directory");
    fs::write(&skill, "---\nname: appa-guide\ndescription: probe\n---\nSay probe.\n").expect("the skill is written");

    // A dead port, so the session reaches no runtime of this machine.
    let template = "${APPA_RUNTIME_URL:-http://127.0.0.1:1}/mcp";
    let server = serde_json::json!({"type": "http", "url": template}).to_string();
    let added = claude(&config, &["mcp", "add-json", "--scope", "user", "appa", &server]);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    let duplicate = claude(&config, &["mcp", "add-json", "--scope", "user", "appa", &server]);
    assert!(!duplicate.status.success(), "add-json now replaces an existing name");
    let reported = claude(&config, &["mcp", "get", "appa"]);
    assert!(reported.status.success());
    let url = String::from_utf8_lossy(&reported.stdout)
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("URL:").map(str::trim).map(str::to_owned))
        .expect("`claude mcp get` reports a URL line");
    assert_eq!(url, template, "the template is no longer stored verbatim");

    // The session has no credentials here and ends at its first model call;
    // the skills it loaded are announced before that.
    let session = Command::new("claude")
        .args([
            "-p",
            "Reply with the single word ok.",
            "--setting-sources",
            "user",
            "--tools",
            "",
            "--max-turns",
            "1",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
        ])
        .current_dir(&root)
        .env("CLAUDE_CONFIG_DIR", &config)
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .expect("claude runs headless");
    let init = session_init(&session);
    assert!(
        init["skills"]
            .as_array()
            .is_some_and(|skills| skills.iter().any(|skill| skill == "appa-guide")),
        "the user skill is not loaded: {init}"
    );

    let removed = claude(&config, &["mcp", "remove", "appa", "--scope", "user"]);
    assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stderr));
    for arguments in [
        ["mcp", "get", "appa", "", ""],
        ["mcp", "remove", "appa", "--scope", "user"],
    ] {
        let arguments: Vec<&str> = arguments.into_iter().filter(|argument| !argument.is_empty()).collect();
        let absent = claude(&config, &arguments);
        assert!(!absent.status.success());
        assert!(
            String::from_utf8_lossy(&absent.stderr).contains("No MCP server named"),
            "the absent-server message of `claude {}` changed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&absent.stderr)
        );
    }
}

/// With the user's own credentials and one model turn: exec-form entries run
/// with no shell, in parallel within an event, with the hook JSON on stdin.
/// The profile is left alone: the entries come from `--settings` and nothing
/// of the session is persisted.
#[test]
#[ignore = "needs a real claude CLI, its credentials and one model turn"]
fn real_claude_runs_exec_form_hook_entries() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path().join("a space");
    fs::create_dir_all(&root).expect("the working root");
    let marker = |name: &str| root.join(name).to_str().expect("UTF-8 path").to_owned();
    let entry = |name: &str| serde_json::json!({"type": "command", "command": "/usr/bin/tee", "args": ["-a", marker(name)], "timeout": 5});
    let settings = root.join("settings.json");
    fs::write(
        &settings,
        serde_json::json!({"hooks": {
            "SessionStart": [{"hooks": [entry("session-start-1"), entry("session-start-2")]}],
            "UserPromptSubmit": [{"hooks": [entry("prompt")]}],
            "Stop": [{"hooks": [entry("stop")]}],
        }})
        .to_string(),
    )
    .expect("the settings are written");

    let session = Command::new("claude")
        .args([
            "-p",
            "Reply with the single word ok.",
            "--setting-sources",
            "",
            "--settings",
            settings.to_str().expect("UTF-8 path"),
            "--tools",
            "",
            "--model",
            "haiku",
            "--max-turns",
            "1",
            "--output-format",
            "json",
            "--no-session-persistence",
        ])
        .current_dir(&root)
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .expect("claude runs headless");
    assert!(session.status.success(), "{}", String::from_utf8_lossy(&session.stderr));
    for name in ["session-start-1", "session-start-2", "prompt", "stop"] {
        let fired = fs::read_to_string(marker(name)).unwrap_or_default();
        assert!(
            fired.contains("\"hook_event_name\""),
            "the {name} entry did not receive its hook JSON: {fired:?}"
        );
    }
}
