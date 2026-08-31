#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

fn executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fixture is executable");
}

fn install_test_binaries(bin: &Path) -> std::path::PathBuf {
    let appa = bin.join("appa");
    fs::copy(env!("CARGO_BIN_EXE_appa"), &appa).expect("appa is copied");
    executable(&appa);
    appa
}

fn install_fake_curl(bin: &Path) {
    let curl = bin.join("curl");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-curl.sh"),
        &curl,
    )
    .expect("fake curl is copied");
    executable(&curl);
}

fn runtime_fingerprint(deployed: &Path) -> String {
    let digest = Sha256::digest(fs::read(deployed).expect("runtime bytes"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The marketplace root a developer passes to `--plugin-source`, staged by the
/// same script the release runs.
fn stage_bundle(into: &Path) -> std::path::PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let staged = into.join("plugin-source");
    let status = Command::new("sh")
        .arg(repository.join("scripts/appa-stage-plugin-bundle.sh"))
        .arg(&staged)
        .status()
        .expect("the staging script runs");
    assert!(status.success(), "the staging script failed");
    staged
}

/// Where init deploys the harness binary: private to appa, never on PATH.
fn deployed_binary(data: &Path) -> std::path::PathBuf {
    data.join("bin/appa")
}

#[test]
fn init_installs_one_local_adapter_and_is_safe_to_run_again() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let bin = directory.path().join("bin");
    let config = directory.path().join("config");
    let data = directory.path().join("data");
    let claude = directory.path().join("claude");
    let plugin = directory.path().join("installed-plugin");
    fs::create_dir_all(&bin).expect("bin directory");
    fs::create_dir_all(plugin.join("hooks")).expect("plugin hooks");
    let appa = install_test_binaries(&bin);
    fs::copy(&appa, bin.join("appa-runtime")).expect("legacy runtime fixture is copied");
    executable(&bin.join("appa-runtime"));
    install_fake_curl(&bin);
    let source = stage_bundle(directory.path());
    let fingerprint = runtime_fingerprint(&appa);

    let fake_claude = bin.join("claude");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh"),
        &fake_claude,
    )
    .expect("fake claude is copied");
    executable(&fake_claude);

    let starter = plugin.join("hooks/ensure-runtime.sh");
    fs::write(&starter, "#!/bin/sh\nexit 0\n").expect("fake starter");
    executable(&starter);
    fs::write(plugin.join("statusline.sh"), "#!/bin/sh\nprintf APPA\n").expect("statusline");

    let log = directory.path().join("claude.log");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let run = |reported_fingerprint: &str, fail_once: Option<&str>| {
        let mut command = Command::new(&appa);
        // Run from a directory that holds no marketplace: default resolution
        // must not consult the working directory, and an explicit source is an
        // ordinary path argument.
        command
            .current_dir(directory.path())
            .args(["init", "claude-code", "--plugin-source"])
            .arg(&source)
            .env("PATH", &path)
            .env("HOME", directory.path())
            .env("APPA_INSTALL_DIR", &bin)
            .env("APPA_CONFIG_DIR", &config)
            .env("APPA_DATA_DIR", &data)
            .env("CLAUDE_CONFIG_DIR", &claude)
            .env("FAKE_CLAUDE_HOME", &claude)
            .env("FAKE_CLAUDE_LOG", &log)
            .env("FAKE_PLUGIN_ROOT", &plugin)
            .env("FAKE_RUNTIME_FINGERPRINT", reported_fingerprint);
        if let Some(failure) = fail_once {
            command.env("FAKE_CLAUDE_FAIL_ONCE", failure);
        }
        command.output().expect("appa init runs")
    };

    let first = run(&fingerprint, None);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let first_stdout = String::from_utf8(first.stdout).expect("UTF-8 output");
    assert!(first_stdout.starts_with("OpenAPPA initialized for Claude Code"));
    assert!(first_stdout.contains("development source"));
    assert!(first_stdout.contains("(created)"));
    // The harness binary lands on an appa-private path, not on PATH, and the
    // copy that is on PATH is named rather than removed.
    assert!(deployed_binary(&data).is_file());
    assert!(bin.join("appa").is_file());
    assert!(first_stdout.contains("A previous appa remains at"));
    assert!(!bin.join("appa-runtime").exists());
    assert!(bin.join("clappa").is_file());
    assert!(bin.join("appa-statusline.sh").is_file());
    assert!(config.join("appa.toml").is_file());

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(claude.join("settings.json")).expect("Claude settings"))
            .expect("settings JSON");
    assert!(
        settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|command| command.ends_with("appa-statusline.sh"))
    );

    let second = run(&fingerprint, None);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert!(String::from_utf8_lossy(&second.stdout).contains("(kept)"));

    let calls = fs::read_to_string(&log).expect("Claude invocation log");
    assert_eq!(
        calls.matches("plugin install appa-runtime@appa --scope user").count(),
        2
    );
    assert_eq!(
        calls
            .matches("plugin uninstall appa-runtime@appa --scope user --yes")
            .count(),
        1
    );
    assert_eq!(calls.matches("plugin marketplace remove appa").count(), 1);

    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(claude.join("plugins/installed_plugins.json")).expect("plugin registry"))
            .expect("registry JSON");
    assert_eq!(
        registry["plugins"]["appa-runtime@appa"].as_array().map(Vec::len),
        Some(1)
    );

    fs::copy(&appa, bin.join("appa-runtime")).expect("legacy runtime is restored for the failed-upgrade fixture");
    executable(&bin.join("appa-runtime"));
    for failure in ["marketplace-add", "plugin-install"] {
        let failed = run(&fingerprint, Some(failure));
        assert!(!failed.status.success(), "the injected {failure} failure must surface");
        let registry: serde_json::Value = serde_json::from_slice(
            &fs::read(claude.join("plugins/installed_plugins.json")).expect("restored plugin registry"),
        )
        .expect("restored registry JSON");
        assert_eq!(
            registry["plugins"]["appa-runtime@appa"].as_array().map(Vec::len),
            Some(1),
            "{failure} must restore the prior plugin",
        );
        assert!(
            claude.join("marketplace-appa").is_file(),
            "{failure} must restore the marketplace"
        );
        assert!(
            bin.join("appa-runtime").is_file(),
            "{failure} must leave the previous runtime available",
        );
        assert!(
            !fs::read_to_string(bin.join("clappa"))
                .expect("restored launcher")
                .contains("init did not complete"),
            "{failure} must restore the protected launcher",
        );
    }

    let wrong_runtime = run("not-this-build", None);
    assert!(!wrong_runtime.status.success());
    assert!(String::from_utf8_lossy(&wrong_runtime.stderr).contains("different appa build"));

    let unrecoverable = run(&fingerprint, Some("plugin-install-always"));
    assert!(!unrecoverable.status.success());
    assert!(String::from_utf8_lossy(&unrecoverable.stderr).contains("restoring the previous Claude Code plugin"));
    assert!(
        fs::read_to_string(bin.join("clappa"))
            .expect("fail-closed launcher")
            .contains("init did not complete"),
        "a failed rollback must leave clappa refusing to launch an unprotected session",
    );

    let repaired = run(&fingerprint, None);
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
}

#[test]
fn init_keeps_a_custom_statusline() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let bin = directory.path().join("bin");
    let config = directory.path().join("config");
    let data = directory.path().join("data");
    let claude = directory.path().join("claude");
    let plugin = directory.path().join("installed-plugin");
    fs::create_dir_all(&bin).expect("bin directory");
    fs::create_dir_all(plugin.join("hooks")).expect("plugin hooks");
    fs::create_dir_all(&claude).expect("Claude directory");
    let appa = install_test_binaries(&bin);
    install_fake_curl(&bin);
    let source = stage_bundle(directory.path());
    let fingerprint = runtime_fingerprint(&appa);

    let fake_claude = bin.join("claude");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh"),
        &fake_claude,
    )
    .expect("fake claude is copied");
    executable(&fake_claude);
    let starter = plugin.join("hooks/ensure-runtime.sh");
    fs::write(&starter, "#!/bin/sh\nexit 0\n").expect("fake starter");
    executable(&starter);
    fs::write(plugin.join("statusline.sh"), "#!/bin/sh\nexit 0\n").expect("statusline");
    fs::write(
        claude.join("settings.json"),
        r#"{"statusLine":{"type":"command","command":"my-status"}}"#,
    )
    .expect("custom settings");

    let output = Command::new(appa)
        .current_dir(directory.path())
        .args(["init", "claude-code", "--plugin-source"])
        .arg(&source)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .env("HOME", directory.path())
        .env("APPA_INSTALL_DIR", &bin)
        .env("APPA_CONFIG_DIR", &config)
        .env("APPA_DATA_DIR", &data)
        .env("CLAUDE_CONFIG_DIR", &claude)
        .env("FAKE_CLAUDE_HOME", &claude)
        .env("FAKE_CLAUDE_LOG", directory.path().join("claude.log"))
        .env("FAKE_PLUGIN_ROOT", &plugin)
        .env("FAKE_RUNTIME_FINGERPRINT", fingerprint)
        .output()
        .expect("appa init runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Statusline"));
    let settings = fs::read_to_string(claude.join("settings.json")).expect("settings remain");
    assert!(settings.contains("my-status"));
    assert!(!bin.join("appa-statusline.sh").exists());
}
