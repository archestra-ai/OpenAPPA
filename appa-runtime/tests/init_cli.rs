#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

mod common;
#[path = "common/init_fixture.rs"]
mod init_fixture;
use common::repo_root;
use init_fixture::{Fixture, Installed, default_policy_key, runtime_fingerprint, shipped_default_config};

/// The release workflow proves a released binary ignores `APPA_ENDPOINT` by
/// running activation against a config that does not exist: the endpoint is
/// settled first, so a build that reads the seam refuses the value, and one
/// that ignores it fails on the config. Home and directory variables are
/// removed so nothing outside the fixture is reached either way.
#[test]
fn release_override_probe_reaches_endpoint_before_deployment_paths() {
    let fixture = Fixture::new();
    for endpoint in ["http://127.0.0.1:0", "http://127.0.0.1:8787"] {
        let mut command = Command::new(&fixture.appa);
        command
            .current_dir(&fixture.root)
            .arg("activate-claude")
            .arg("--config")
            .arg("./no-such-dir/appa.toml")
            .arg("--archive")
            .arg("./no-such-archive.tar.gz");
        for variable in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "APPA_INSTALL_DIR",
            "APPA_CONFIG_DIR",
            "APPA_DATA_DIR",
            "CLAUDE_CONFIG_DIR",
        ] {
            command.env_remove(variable);
        }
        let output = command.env("APPA_ENDPOINT", endpoint).output().expect("probe runs");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected = if cfg!(debug_assertions) && endpoint.ends_with(":0") {
            "is not a usable runtime endpoint"
        } else {
            "does not load"
        };
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!fixture.data.exists());
        assert!(!fixture.root.join("claude.log").exists());
        assert!(!fixture.root.join("no-such-dir").exists());
    }
}

/// The template URL an install registers for the MCP server at `url`.
fn mcp_template(url: &str) -> String {
    format!("${{APPA_RUNTIME_URL:-{url}}}/mcp")
}

/// The MCP registration an earlier install left, as the fake `claude` holds it.
fn register_mcp(fixture: &Fixture, url: &str) {
    fs::write(
        fixture.claude.join("mcp-appa"),
        serde_json::json!({"type": "http", "url": url}).to_string(),
    )
    .expect("the registration is written");
}

/// An install a previous activation left, with bytes of its own in every file
/// and registration a later activation rewrites, so a restore that merely
/// reinstalls this build is told apart from one that puts the previous state back.
fn previous_install(fixture: &Fixture) -> Installed {
    fixture.successful_activation();
    fs::write(fixture.deployed_binary(), b"the previous build").expect("the previous binary is written");
    let settings = fs::read_to_string(fixture.settings()).expect("settings are readable");
    fs::write(fixture.settings(), settings.replacen('{', "{\"userSetting\": true,", 1))
        .expect("the previous settings are written");
    register_mcp(fixture, &mcp_template("http://127.0.0.1:1"));
    fs::write(
        fixture.skill(),
        "---\nname: appa-guide\ndescription: an earlier install\n---\n",
    )
    .expect("the previous skill is written");
    Installed::of(fixture)
}

#[test]
fn launcher_uses_the_install_directory_not_the_source_binary_cache() {
    let fixture = Fixture::new();
    let launchers = fixture.root.join("launchers");
    let output = fixture.activate().env("APPA_INSTALL_DIR", &launchers).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(launchers.join("clappa").is_file());
    assert!(!fixture.bin.join("clappa").exists());
}

/// The MCP registration fails after the binary and the hook entries have
/// already been replaced: both come back, and so does the registration the
/// earlier install had made.
#[test]
fn a_failure_at_the_mcp_registration_puts_the_previous_install_back() {
    let fixture = Fixture::new();
    let before = previous_install(&fixture);

    let failed = fixture
        .activate()
        .env("FAKE_CLAUDE_FAIL_ONCE", "mcp-add")
        .output()
        .expect("appa activates");

    assert!(!failed.status.success());
    assert_eq!(Installed::of(&fixture), before);
    assert!(!fixture.deployed_binary().with_extension("prev").exists());
    assert!(
        fixture.launcher_is_armed(),
        "the previous install's launcher is re-armed"
    );
}

#[test]
fn a_failure_at_the_start_puts_the_previous_profile_back() {
    let fixture = Fixture::new();
    let before = previous_install(&fixture);

    let failed = fixture
        .activate()
        .env("FAKE_STARTER_FAILS", "1")
        .output()
        .expect("appa activates");

    assert!(!failed.status.success());
    assert_eq!(Installed::of(&fixture), before);
    assert!(!fixture.deployed_binary().with_extension("prev").exists());
    assert!(
        fixture.launcher_is_armed(),
        "the previous install's launcher is re-armed"
    );
}

/// A runtime that does not answer for its policy inside the probe's deadline
/// cannot be reconciled, so activation fails and binds nothing to it rather than
/// reporting it healthy.
#[test]
fn a_policy_key_timeout_fails_activation() {
    let fixture = Fixture::new();

    let failed = fixture
        .activate()
        .env("FAKE_POLICY_KEY_TIMEOUT", "1")
        .output()
        .expect("appa activates");

    assert!(!failed.status.success());
    assert_eq!(Installed::of(&fixture), Installed::nothing());
}

/// A first install that fails after its runtime is up leaves nothing behind:
/// the runtime it started is stopped, and no file or registration it wrote
/// survives to bind a session to a runtime whose policy activation could not settle.
#[test]
fn a_failure_after_the_start_stops_the_runtime_activation_started() {
    let fixture = Fixture::new();
    let stand_in = fixture.root.join("stand-in");

    let failed = fixture
        .activate()
        .env("FAKE_RUNTIME_STAND_IN", &stand_in)
        .env_remove("FAKE_POLICY_KEY")
        .output()
        .expect("appa activates");

    assert!(!failed.status.success());
    let pid: i32 = fs::read_to_string(stand_in.join("pid"))
        .expect("the starter recorded the runtime it started")
        .trim()
        .parse()
        .expect("the recorded pid parses");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && unsafe { libc::kill(pid, 0) } == 0 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the runtime activation started is still running"
    );
    assert_eq!(Installed::of(&fixture), Installed::nothing());
}

#[test]
fn a_first_activation_writes_the_profile_and_arms_the_launcher() {
    let fixture = Fixture::new();
    let reloads = fixture.root.join("reloads");
    let output = fixture
        .activate()
        .env("FAKE_RELOADS", &reloads)
        .output()
        .expect("appa activates");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    // The harness binary lands on an appa-private path, not on PATH, and the
    // copy that is on PATH is left alone.
    assert!(fixture.deployed_binary().is_file());
    assert!(fixture.bin.join("appa").is_file());
    assert!(fixture.launcher_is_armed());
    assert_eq!(
        fs::read_to_string(fixture.config.join("appa.toml")).ok(),
        Some(shipped_default_config())
    );

    // Every event of a protected session names the deployed binary, and the
    // session's first entry carries everything the runtime start needs.
    let entries = fixture.owned_hook_entries();
    let mut events: Vec<&str> = entries.iter().map(|(event, _)| event.as_str()).collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "PostToolUse",
            "PostToolUseFailure",
            "PreToolUse",
            "SessionStart",
            "SessionStart",
            "Stop",
            "StopFailure",
            "SubagentStart",
            "SubagentStop",
            "UserPromptSubmit",
        ]
    );
    let deployment_url = "http://127.0.0.1:8787";
    let session_start = entries
        .iter()
        .find(|(event, hook)| event == "SessionStart" && hook["args"][0] == "hook")
        .map(|(_, hook)| hook.clone())
        .expect("the session start entry posts");
    assert_eq!(
        session_start["args"],
        serde_json::json!([
            "hook",
            "--deployment-url",
            deployment_url,
            "--ensure-runtime",
            "--config",
            fixture.config.join("appa.toml"),
            "--data-dir",
            fixture.data,
        ])
    );
    let statusline = fixture.settings_value()["statusLine"]["command"]
        .as_str()
        .expect("the status line is a command")
        .to_owned();
    assert!(
        statusline.contains(fixture.deployed_binary().to_str().unwrap()),
        "{statusline}"
    );
    assert_eq!(
        fixture.mcp_registration(),
        Some(serde_json::json!({"type": "http", "url": mcp_template(deployment_url)}))
    );
    let guide = repo_root().join("integrations/appa-guide");
    assert_eq!(
        fs::read_to_string(fixture.skill()).expect("the skill is written"),
        format!(
            "{}\n\n{}",
            fs::read_to_string(guide.join("SKILL.md")).unwrap(),
            fs::read_to_string(guide.join("references/claude-code.md")).unwrap()
        )
    );
    assert_eq!(
        fs::read_to_string(fixture.contracts_guide()).expect("the policy-review guide is written"),
        fs::read_to_string(repo_root().join("website/content/docs/contracts.md")).unwrap()
    );
    // The runtime reports serving the key the fixture computed for the shipped
    // default, and activation found nothing to reconcile: that key is the real one.
    assert!(
        !reloads.exists(),
        "a first install reloaded a runtime already serving its policy"
    );
}

#[test]
fn a_rerun_keeps_the_config_and_rewrites_nothing_it_already_wrote() {
    let fixture = Fixture::new();
    fixture.successful_activation();
    let config = fixture.config.join("appa.toml");
    let authored = format!(
        "{}\n# an edit activation keeps\n",
        fs::read_to_string(&config).expect("the config is readable")
    );
    fs::write(&config, &authored).expect("the edit is written");
    let written = Installed::of(&fixture);

    fixture.successful_activation();

    assert_eq!(fs::read_to_string(&config).ok(), Some(authored));
    assert_eq!(Installed::of(&fixture), written);
    let calls = fixture.claude_calls();
    assert_eq!(calls.matches("mcp get appa").count(), 2);
    assert_eq!(calls.matches("mcp add-json").count(), 1);
    assert_eq!(calls.matches("mcp remove").count(), 0);
}

/// A foreign runtime already owning the endpoint is refused before the profile
/// is touched at all, so the installation it would have replaced is still the
/// one that is registered and running. Another build is one way to be foreign;
/// this build serving another deployment's configuration is the other, and it
/// is the one a digest alone cannot see.
#[test]
fn a_foreign_runtime_is_refused_before_the_profile_is_touched() {
    let fixture = Fixture::new();
    fixture.successful_activation();
    let fingerprint = runtime_fingerprint(&fixture.appa);
    let mine = fixture.config.join("appa.toml");
    let before = Installed::of(&fixture);

    for (build, serving) in [
        ("not-this-build", mine.as_path()),
        (fingerprint.as_str(), Path::new("/somewhere/else/appa.toml")),
    ] {
        let calls_before = fixture.claude_calls().lines().count();
        let refused = fixture
            .activate()
            .env("FAKE_RUNTIME_FINGERPRINT", build)
            .env("FAKE_RUNTIME_CONFIG", serving)
            .output()
            .expect("appa activates");
        assert!(
            !refused.status.success(),
            "a runtime claiming build {build} at {} must be refused",
            serving.display()
        );
        let mutating: Vec<String> = fixture
            .claude_calls()
            .lines()
            .skip(calls_before)
            .filter(|line| !line.starts_with("mcp get appa"))
            .map(str::to_owned)
            .collect();
        assert!(
            mutating.is_empty(),
            "a refused endpoint must not reach a single mutating claude call: {mutating:?}",
        );
        assert_eq!(Installed::of(&fixture), before);
        assert!(
            fixture.launcher_is_armed(),
            "a refused endpoint must leave the working launcher armed"
        );
    }
}

#[test]
fn a_failed_rollback_disarms_the_launcher_until_an_activation_completes() {
    let fixture = Fixture::new();
    previous_install(&fixture);

    let unrecoverable = fixture
        .activate()
        .env("FAKE_CLAUDE_FAIL_ONCE", "mcp-add-always")
        .output()
        .expect("appa activates");
    assert_eq!(unrecoverable.status.code(), Some(3));
    assert!(
        !fixture.launcher_is_armed(),
        "a failed rollback must leave clappa refusing to launch an unprotected session",
    );

    fixture.successful_activation();
    assert!(fixture.launcher_is_armed());
}

/// A runtime of this deployment that survived the install keeps serving the policy it
/// loaded at startup, and only the install can notice. Nothing is asked: the reconcile
/// reloads, and the runtime must then answer with the policy it was asked to load.
#[test]
fn activation_reloads_a_surviving_runtime_that_serves_an_older_policy() {
    let fixture = Fixture::new();
    let reloads = fixture.root.join("reloads");
    let output = fixture
        .activate()
        // This deployment's own runtime, serving a policy that is not the file the
        // marketplace wrote: the one state a reload is for.
        .env("FAKE_POLICY_KEY", "a-policy-this-activation-did-not-compose")
        .env("FAKE_POLICY_KEY_AFTER_RELOAD", default_policy_key())
        .env("FAKE_RELOADS", &reloads)
        .output()
        .expect("appa activates");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        reloads.exists(),
        "a diverged runtime of this deployment must be reloaded, not left serving its older policy",
    );
}

#[test]
fn activation_keeps_a_custom_statusline() {
    let fixture = Fixture::new();
    let custom = serde_json::json!({"type": "command", "command": "my-status"});
    fs::write(
        fixture.settings(),
        serde_json::json!({"statusLine": custom}).to_string(),
    )
    .expect("custom settings");

    fixture.successful_activation();

    assert_eq!(fixture.settings_value()["statusLine"], custom);
}

/// Everything the profile held before is there after activation and after
/// removal, as the value it was: entries of other hooks, in their place, and
/// settings this install never names.
#[test]
fn foreign_settings_survive_activation_and_removal() {
    let fixture = Fixture::new();
    let original = serde_json::json!({
        "theme": "dark",
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "lint"}]}],
            "Notification": [{"hooks": [{"type": "command", "command": "notify"}]}]
        }
    });
    fs::write(fixture.settings(), original.to_string()).expect("the settings are written");

    fixture.successful_activation();
    let activated = fixture.settings_value();
    assert_eq!(activated["theme"], "dark");
    assert_eq!(activated["hooks"]["Notification"], original["hooks"]["Notification"]);
    assert_eq!(
        activated["hooks"]["PreToolUse"][0], original["hooks"]["PreToolUse"][0],
        "the foreign entry keeps its place"
    );
    assert_eq!(activated["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(2));

    let removed = fixture.remove().output().expect("appa removes");
    assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stderr));
    assert_eq!(fixture.settings_value(), original);
    assert_eq!(fixture.mcp_registration(), None);
    assert!(
        !fixture.skill().parent().unwrap().exists(),
        "the skill directory goes with its files"
    );
    assert!(!fixture.launcher().exists());
    assert!(
        fixture.deployed_binary().is_file(),
        "removal takes back the profile, not the runtime"
    );

    // Removal is replayable: a second run finds nothing of its own and succeeds.
    let again = fixture.remove().output().expect("appa removes");
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(fixture.settings_value(), original);
}

/// An MCP server under APPA's name that no install wrote is someone else's:
/// activation refuses before it has written anything, and removal leaves it.
#[test]
fn a_foreign_mcp_server_under_appas_name_is_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let foreign = serde_json::json!({"type": "http", "url": "http://localhost:9/mcp"});
    fs::write(fixture.claude.join("mcp-appa"), foreign.to_string()).expect("the foreign server is registered");

    let refused = fixture.activate().output().expect("appa activates");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("http://localhost:9/mcp"),
        "the refusal names the foreign server"
    );
    assert_eq!(
        Installed::of(&fixture),
        Installed {
            mcp: Some(foreign.clone()),
            ..Installed::nothing()
        }
    );
    assert_eq!(fixture.claude_calls().trim(), "mcp get appa");

    let removal = fixture.remove().output().expect("appa removes");
    assert!(!removal.status.success());
    assert_eq!(fixture.mcp_registration(), Some(foreign));
}

/// A relative directory override must not reach the hook entries as written.
///
/// Hooks run from whatever working directory Claude was launched in, so a
/// relative `state/bin/appa` would resolve somewhere else entirely at hook time
/// and find no binary, config or database.
#[test]
fn relative_directory_overrides_are_rendered_absolute() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let output = fixture
        .activate()
        // Relative, resolved against the fixture's working directory and no other.
        .env("APPA_INSTALL_DIR", "bin")
        .env("APPA_CONFIG_DIR", "config")
        .env("APPA_DATA_DIR", "state")
        // The deployment the answering runtime claims: this init's own config.
        .env("FAKE_RUNTIME_CONFIG", root.join("config/appa.toml"))
        .output()
        .expect("appa activates");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let settings = fixture.settings_value();
    let mut rendered = Vec::new();
    for (_, groups) in settings["hooks"].as_object().expect("hooks is an object") {
        for group in groups.as_array().expect("groups") {
            for hook in group["hooks"].as_array().expect("hooks") {
                rendered.push(hook["command"].as_str().expect("a command").to_owned());
                let args = hook["args"].as_array().expect("args");
                for flag in ["--config", "--data-dir"] {
                    if let Some(position) = args.iter().position(|argument| argument == flag) {
                        rendered.push(args[position + 1].as_str().expect("a path").to_owned());
                    }
                }
            }
        }
    }
    assert!(rendered.len() > 10, "{rendered:?}");
    for value in rendered {
        let path = Path::new(&value);
        assert!(path.is_absolute(), "rendered relative: {value}");
        assert!(
            path.starts_with(root),
            "does not resolve under the working directory it was given: {value}",
        );
    }
}

/// A runtime that fails verification *after* the profile switch must take the
/// switch with it.
///
/// The preflight refuses a foreign owner that is already there, but one can
/// arrive between the preflight and the start. Leaving the hooks registered and
/// the launcher armed against it would be the exact skew activation exists to
/// prevent: Claude gated by hooks talking to a runtime nobody verified. There
/// was nothing here before, so undoing means removing what was just written.
#[test]
fn a_runtime_that_fails_verification_after_the_switch_undoes_it() {
    let fixture = Fixture::new();
    let output = fixture
        .activate()
        // The preflight sees this build; everything after it sees a stranger.
        .env("FAKE_CURL_CALLS", fixture.root.join("curl-calls"))
        .env("FAKE_RUNTIME_FINGERPRINT_LATER", "not-this-build")
        .output()
        .expect("appa activates");

    assert!(
        !output.status.success(),
        "verification against a foreign runtime must fail activation: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(Installed::of(&fixture), Installed::nothing());
}

/// A `clappa` under the install directory that no install wrote is the
/// user's: the activation refuses before it writes anything, and the file
/// stays as it was.
#[test]
fn a_launcher_of_the_users_own_refuses_the_activation() {
    let fixture = Fixture::new();
    let launcher = fixture.launcher();
    fs::create_dir_all(launcher.parent().unwrap()).expect("the install directory exists");
    let own = "#!/bin/sh\nexec claude --model opus \"$@\"\n";
    fs::write(&launcher, own).expect("the user's launcher is written");

    let output = fixture.activate().output().expect("appa activates");

    assert!(
        !output.status.success(),
        "a launcher of the user's own must refuse the activation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&launcher).unwrap(), own);
    assert!(!fixture.settings().exists(), "nothing of the profile is written");
}
