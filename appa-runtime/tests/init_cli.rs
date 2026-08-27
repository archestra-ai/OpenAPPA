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

fn runtime_fingerprint(bin: &Path) -> String {
    let digest = Sha256::digest(fs::read(bin.join("appa")).expect("runtime bytes"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
    let fingerprint = runtime_fingerprint(&bin);

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
    let run = |reported_fingerprint: &str| {
        Command::new(&appa)
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
            .args(["init", "claude-code"])
            .env("PATH", &path)
            .env("HOME", directory.path())
            .env("APPA_INSTALL_DIR", &bin)
            .env("APPA_CONFIG_DIR", &config)
            .env("APPA_DATA_DIR", &data)
            .env("CLAUDE_CONFIG_DIR", &claude)
            .env("FAKE_CLAUDE_HOME", &claude)
            .env("FAKE_CLAUDE_LOG", &log)
            .env("FAKE_PLUGIN_ROOT", &plugin)
            .env("FAKE_RUNTIME_FINGERPRINT", reported_fingerprint)
            .output()
            .expect("appa init runs")
    };

    let first = run(&fingerprint);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let first_stdout = String::from_utf8(first.stdout).expect("UTF-8 output");
    assert!(first_stdout.starts_with("OpenAPPA initialized for Claude Code"));
    assert!(first_stdout.contains("Adapter   current checkout"));
    assert!(first_stdout.contains("(created)"));
    assert!(bin.join("appa").is_file());
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

    let second = run(&fingerprint);
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

    let wrong_runtime = run("not-this-build");
    assert!(!wrong_runtime.status.success());
    assert!(String::from_utf8_lossy(&wrong_runtime.stderr).contains("different appa build"));
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
    let fingerprint = runtime_fingerprint(&bin);

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
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
        .args(["init", "claude-code"])
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
