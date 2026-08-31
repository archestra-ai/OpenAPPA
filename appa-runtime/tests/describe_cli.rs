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
