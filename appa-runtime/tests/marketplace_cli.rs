//! Black-box stream and filesystem contracts for marketplace commands.

use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(args)
        .current_dir(root)
        .env("HOME", root)
        .env("APPA_CONFIG_DIR", root.join("config"))
        .env_remove("APPA_CONFIG")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("marketplace command exceeded its test deadline: {output:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

/// Before any install, a listing shows what this build can install: the whole
/// catalog of its own tree, offline, none of it installed. It reads state only.
#[test]
fn local_list_shows_this_builds_catalog_and_does_not_initialize_an_installation() {
    let root = tempfile::tempdir().unwrap();
    for (kind, expected) in [("plugin", "claude-code"), ("battery", "github")] {
        let output = run(root.path(), &[kind, "list", "--json"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["status"], "ok");
        assert_eq!(document["operation"], format!("{kind}.list"));
        assert_eq!(document["result"]["deployment"], "absent");
        assert!(
            matches!(
                document["result"]["catalog"]["source"].as_str(),
                Some("build" | "checkout")
            ),
            "{document}"
        );
        let packages = document["result"]["packages"].as_array().unwrap();
        assert!(packages.iter().any(|package| package["name"] == expected), "{document}");
        for package in packages {
            assert_eq!(package["installed"], false);
            assert!(package["description"].as_str().is_some_and(|text| !text.is_empty()));
        }
        assert!(document.get("error").is_none());
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    let text = run(root.path(), &["plugin", "list"]);
    assert!(text.status.success());
    assert!(text.stderr.is_empty());
    assert!(String::from_utf8_lossy(&text.stdout).lines().count() > 1);
}

/// An install with no name orients instead of mutating: the exit is the parser's,
/// stdout stays empty in text mode and carries the usage envelope in JSON mode,
/// and no state is created.
#[test]
fn install_without_a_name_orients_and_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    for kind in ["plugin", "battery"] {
        let output = run(root.path(), &[kind, "install"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        let output = run(root.path(), &[kind, "install", "--json"]);
        assert_eq!(output.status.code(), Some(2));
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["error"]["code"], "usage");
        assert!(output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn list_names_the_deployment_state() {
    let root = tempfile::tempdir().unwrap();
    let document = |output: Output| -> serde_json::Value { serde_json::from_slice(&output.stdout).unwrap() };
    let absent = document(run(root.path(), &["battery", "list", "--json"]));
    assert_eq!(absent["result"]["deployment"], "absent");
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(root.path().join("config/appa.toml"), b"").unwrap();
    let unmanaged = document(run(root.path(), &["battery", "list", "--json"]));
    assert_eq!(unmanaged["result"]["deployment"], "unmanaged");
    assert!(
        unmanaged["result"]["packages"]
            .as_array()
            .is_some_and(|packages| packages.iter().all(|package| package["installed"] == false))
    );
}

#[test]
fn removing_an_unselected_plugin_is_read_only_and_idempotent() {
    let root = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let output = run(root.path(), &["plugin", "remove", "claude-code", "--json"]);
        assert!(output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["operation"], "plugin.remove");
        assert_eq!(result["result"]["state"], "unchanged");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn plugin_removal_reports_completed_recovery_before_an_idle_result() {
    for published_config in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let config = deployment(root.path());
        let state = root.path().join("config/.appa/appa.toml");
        let active = state.join("active.json");
        let selection: serde_json::Value = serde_json::from_slice(&std::fs::read(&active).unwrap()).unwrap();
        let after = std::fs::read(&config).unwrap();
        if !published_config {
            std::fs::remove_file(&config).unwrap();
            std::fs::remove_file(&active).unwrap();
        }
        let journal = state.join("transaction.json");
        std::fs::write(
            &journal,
            serde_json::to_vec(&serde_json::json!({
                "before":null,"after":after,"selection":selection,
                "activation":"none","previous":null
            }))
            .unwrap(),
        )
        .unwrap();
        let output = run(root.path(), &["plugin", "remove", "claude-code", "--json"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["result"]["state"], "recovered");
        assert!(!journal.exists());
        assert_eq!(config.exists(), published_config);
        assert_eq!(active.exists(), published_config);
        if published_config {
            assert_eq!(std::fs::read(&config).unwrap(), after);
        }
        let repeated = run(root.path(), &["plugin", "remove", "claude-code", "--json"]);
        assert!(repeated.status.success());
        let result: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
        assert_eq!(result["result"]["state"], "unchanged");
    }
}

#[test]
fn plugin_removal_requires_the_selected_native_artifact_before_changing_state() {
    let root = tempfile::tempdir().unwrap();
    let config = deployment(root.path());
    let active = root.path().join("config/.appa/appa.toml/active.json");
    let mut selection: serde_json::Value = serde_json::from_slice(&std::fs::read(&active).unwrap()).unwrap();
    selection["plugins"] = serde_json::json!(["claude-code"]);
    let selected = serde_json::to_vec(&selection).unwrap();
    std::fs::write(&active, &selected).unwrap();
    let before = std::fs::read(&config).unwrap();
    let output = run(root.path(), &["plugin", "remove", "claude-code", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert_eq!(std::fs::read(&active).unwrap(), selected);
    assert!(!active.parent().unwrap().join("transaction.json").exists());
}

#[test]
fn corrupt_selection_returns_one_error_envelope_and_preserves_bytes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("config/.appa/appa.toml");
    std::fs::create_dir_all(&state).unwrap();
    let active = state.join("active.json");
    std::fs::write(&active, b"broken").unwrap();
    let output = run(root.path(), &["plugin", "list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["status"], "error");
    assert_eq!(document["error"]["code"], "unreadable_state");
    assert!(document.get("result").is_none());
    assert_eq!(std::fs::read(active).unwrap(), b"broken");
    assert!(!state.join("install.lock").exists());
}

#[test]
fn pending_transaction_is_reported_as_recovery_not_a_successful_stale_list() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("config/.appa/appa.toml");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("transaction.json"), b"pending").unwrap();
    let output = run(root.path(), &["plugin", "list", "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["error"]["recovery_required"], true);
    assert_eq!(std::fs::read(state.join("transaction.json")).unwrap(), b"pending");
}

#[test]
fn help_works_without_a_config_and_unknown_options_do_not_mutate() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["plugin", "list", "--help"],
        vec!["battery", "list", "--help"],
        vec!["bundle", "--help"],
        vec!["plugin", "install", "--help"],
    ] {
        let output = run(root.path(), &args);
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    let output = run(root.path(), &["plugin", "list", "--nonsense"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let output = run(root.path(), &["plugin", "list", "--nonsense", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["error"]["code"], "usage");
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn invalid_install_input_is_refused_before_creating_state_or_contacting_a_host() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["plugin", "install", "claude-code", "--revision", "main", "--json"],
        vec!["plugin", "install", "claude-code", "--from", "missing.tar.gz", "--json"],
        vec![
            "plugin",
            "install",
            "claude-code",
            "--from",
            "missing.tar.gz",
            "--sha256",
            "bad",
            "--json",
        ],
    ] {
        let output = run(root.path(), &args);
        assert_eq!(output.status.code(), Some(2));
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["error"]["code"], "usage");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    std::fs::create_dir(root.path().join("config")).unwrap();
    std::fs::write(root.path().join("config/appa.toml"), b"broken = [").unwrap();
    let output = run(root.path(), &["plugin", "install", "claude-code", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["error"]["code"], "invalid_input");
    assert!(!root.path().join("config/.appa").exists());
}

fn deployment(root: &Path) -> std::path::PathBuf {
    deployment_for(root, None)
}

/// `kagent` stages the real kagent plugin beside the github battery; the slice
/// is the battery list its manifest includes.
fn deployment_for(root: &Path, kagent: Option<&[&str]>) -> std::path::PathBuf {
    use appa_package::generation::{ArtifactDigest, Generation, Image, Platform, REPOSITORY};
    use appa_runtime::installation::{Installation, Selection};
    use std::collections::BTreeMap;
    let source = root.join("source");
    let battery = source.join("batteries/github");
    std::fs::create_dir_all(&battery).unwrap();
    std::fs::write(
        battery.join("appa-package.toml"),
        "schema=1\nname='github'\ndescription='test'\n[battery]\npolicy='appa.toml'\nhosts=['claude-code','kagent']\n",
    )
    .unwrap();
    std::fs::write(
        battery.join("appa.toml"),
        "[policy]\nversion=2\n[[policy.tool]]\nname='mcp/github/read'\n",
    )
    .unwrap();
    let mut catalog = format!(
        "schema=1\nname='appa'\n[packages.battery.github]\npath='batteries/github'\ndigest='{}'\n",
        appa_package::TreeDigest::of_tree(&battery).unwrap()
    );
    if let Some(included) = kagent {
        let plugin_source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace/plugins/kagent");
        let plugin = source.join("plugins/kagent");
        std::fs::create_dir_all(&plugin).unwrap();
        for entry in appa_package::tree::walk(&plugin_source).unwrap() {
            let target = plugin.join(entry.portable);
            match entry.kind {
                appa_package::tree::EntryKind::Directory => std::fs::create_dir_all(target).unwrap(),
                appa_package::tree::EntryKind::File => {
                    std::fs::copy(entry.absolute, target).unwrap();
                }
            }
        }
        if !included.is_empty() {
            let manifest = plugin.join("appa-package.toml");
            let quoted: Vec<String> = included.iter().map(|name| format!("'{name}'")).collect();
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                &manifest,
                text.replace(
                    "[plugin]\n",
                    &format!("[plugin]\nbatteries = [{}]\n", quoted.join(", ")),
                ),
            )
            .unwrap();
        }
        catalog.push_str(&format!(
            "[packages.plugin.kagent]\npath='plugins/kagent'\ndigest='{}'\n",
            appa_package::TreeDigest::of_tree(&plugin).unwrap()
        ));
    }
    std::fs::write(source.join("marketplace.toml"), &catalog).unwrap();
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    archive.append_dir_all(".", &source).unwrap();
    let archive = archive.into_inner().unwrap().finish().unwrap();
    let digest = ArtifactDigest::of_bytes(&archive);
    let descriptor = serde_json::json!({"schema":1,"repository":REPOSITORY,"commit":"a".repeat(40),"release":"v1.0.0","protocol":appa_package::PROTOCOL,
        "catalog":ArtifactDigest::of_bytes(catalog.as_bytes()),"marketplace":digest,"batteries":digest,"runtime_chart":digest,
        "binaries":Platform::ALL.into_iter().map(|platform|(platform,digest.clone())).collect::<BTreeMap<_,_>>(),
        "images":Image::ALL.into_iter().map(|image|(image,serde_json::json!({"digest":digest,"platforms":{"linux/amd64":digest}}))).collect::<BTreeMap<_,_>>()});
    let generation = Generation::parse(&serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let config = root.join("config/appa.toml");
    let installation = Installation::open(&config).unwrap();
    installation.publish_packages(&source, &generation).unwrap();
    std::fs::create_dir_all(installation.state_path().join("artifacts")).unwrap();
    std::fs::write(installation.state_path().join("artifacts").join(digest.hex()), archive).unwrap();
    let selection = Selection::empty(generation, Platform::current().unwrap());
    let text = b"# authored policy\n[policy]\nversion=2\n[[policy.tool]]\nname='Custom'\n[externals]\ntimeout_ms=100\nmax_body_bytes=1024\n";
    installation.commit_config(None, text, &selection).unwrap();
    config
}

#[test]
fn kagent_prepares_updates_roundtrips_offline_and_removes_without_host_activation() {
    let source = tempfile::tempdir().unwrap();
    let config = deployment_for(source.path(), Some(&[]));
    let config_name = config.to_str().unwrap();
    let invoke = |args: &[&str]| {
        let output = run(source.path(), args);
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let installed = invoke(&[
        "plugin",
        "install",
        "kagent",
        "--config",
        config_name,
        "--runtime",
        "python",
        "--json",
    ]);
    assert_eq!(installed["result"]["state"], "prepared");
    assert_eq!(installed["result"]["cluster"], "unchanged");
    let original = std::path::PathBuf::from(installed["result"]["directory"].as_str().unwrap());
    assert!(original.join("agent-python.json").exists());
    assert!(!original.join("agent-go.json").exists());
    let repeated = invoke(&["plugin", "install", "kagent", "--config", config_name, "--json"]);
    assert_eq!(repeated["result"]["directory"], installed["result"]["directory"]);
    invoke(&["battery", "install", "github", "--config", config_name, "--json"]);
    assert!(!original.exists(), "battery update replaces old prepared policy");
    let refreshed = invoke(&["plugin", "install", "kagent", "--config", config_name, "--json"]);
    let prepared = Path::new(refreshed["result"]["directory"].as_str().unwrap());
    let values: serde_json::Value =
        serde_json::from_slice(&std::fs::read(prepared.join("runtime-values.json")).unwrap()).unwrap();
    assert!(
        values["config"]["contents"]
            .as_str()
            .unwrap()
            .contains("batteries/github/appa.toml")
    );
    let archive = source.path().join("offline.tar.gz");
    let exported = invoke(&[
        "bundle",
        "--config",
        config_name,
        "--output",
        archive.to_str().unwrap(),
        "--json",
    ]);
    let destination = tempfile::tempdir().unwrap();
    let replica = destination.path().join("replica.toml");
    let output = run(
        destination.path(),
        &[
            "plugin",
            "install",
            "kagent",
            "--config",
            replica.to_str().unwrap(),
            "--from",
            archive.to_str().unwrap(),
            "--sha256",
            exported["result"]["sha256"].as_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let imported: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let replica_files = Path::new(imported["result"]["directory"].as_str().unwrap());
    assert!(replica_files.join("agent-python.json").exists());
    assert!(!replica_files.join("agent-go.json").exists());
    let before = std::fs::read(&config).unwrap();
    invoke(&["plugin", "remove", "kagent", "--config", config_name, "--json"]);
    assert!(!prepared.exists());
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert_eq!(
        invoke(&["plugin", "remove", "kagent", "--config", config_name, "--json"])["result"]["state"],
        "unchanged"
    );
}

/// A first plugin install selects the batteries its manifest includes; a later
/// install respects the person's removal of one.
#[test]
fn a_first_plugin_install_includes_the_batteries_its_manifest_names_once() {
    let source = tempfile::tempdir().unwrap();
    let config = deployment_for(source.path(), Some(&["github"]));
    let config_name = config.to_str().unwrap();
    let invoke = |args: &[&str]| {
        let output = run(source.path(), args);
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let installed = invoke(&["plugin", "install", "kagent", "--config", config_name, "--json"]);
    assert_eq!(installed["result"]["batteries"], serde_json::json!(["github"]));
    let listed = invoke(&["battery", "list", "--config", config_name, "--json"]);
    assert_eq!(listed["result"]["packages"][0]["name"], "github");
    assert_eq!(listed["result"]["packages"][0]["installed"], true);
    let effective = appa_runtime::config::Config::load(&config).unwrap();
    assert!(
        effective.policy_file().value()["tool"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"].as_str() == Some("mcp/github/read"))
    );

    invoke(&["battery", "remove", "github", "--config", config_name, "--json"]);
    let again = invoke(&["plugin", "install", "kagent", "--config", config_name, "--json"]);
    assert_eq!(again["result"]["batteries"], serde_json::json!([]));
    let listed = invoke(&["battery", "list", "--config", config_name, "--json"]);
    assert_eq!(listed["result"]["packages"][0]["installed"], false);
}

#[test]
fn kagent_requires_explicit_config_and_rejects_inapplicable_runtime_before_writes() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["plugin", "install", "kagent", "--json"],
        vec!["plugin", "remove", "kagent", "--json"],
        vec!["plugin", "install", "claude-code", "--runtime", "go", "--json"],
    ] {
        let output = run(root.path(), &args);
        assert_eq!(output.status.code(), Some(1));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn explicit_custom_files_roundtrip_through_the_existing_bundle_commands() {
    let source = tempfile::tempdir().unwrap();
    let config = deployment(source.path());
    let original = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{original}\n[bundle]\nfiles=['helper.txt']\n")).unwrap();
    let bundle = source.path().join("bundle.tar.gz");
    // Missing declarations fail as one machine envelope, without publishing output.
    let missing = run(
        source.path(),
        &["bundle", "--output", bundle.to_str().unwrap(), "--json"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&missing.stdout).unwrap()["status"],
        "error"
    );
    assert!(!bundle.exists());
    std::fs::write(source.path().join("config/helper.txt"), "portable data").unwrap();
    std::fs::write(source.path().join("config/secret.txt"), "private neighbor").unwrap();
    assert!(
        run(source.path(), &["battery", "install", "github", "--json"])
            .status
            .success()
    );
    let exported = run(
        source.path(),
        &["bundle", "--output", bundle.to_str().unwrap(), "--json"],
    );
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stdout)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    let checksum = receipt["result"]["sha256"].as_str().unwrap();
    std::fs::remove_file(source.path().join("config/helper.txt")).unwrap();
    let replica = tempfile::tempdir().unwrap();
    let replica_config = replica.path().join("config/appa.toml");
    {
        use appa_runtime::installation::{Acquired, Installation, Selection};
        let acquired = Acquired::import(
            &bundle,
            &appa_package::generation::ArtifactDigest::parse(&format!("sha256:{checksum}")).unwrap(),
        )
        .unwrap();
        let target = Installation::open(&replica_config).unwrap();
        target.retain(&acquired).unwrap();
        let empty = Selection::empty(
            acquired.generation().clone(),
            appa_package::generation::Platform::current().unwrap(),
        );
        target.commit_config(None, original.as_bytes(), &empty).unwrap();
    }
    std::fs::write(replica.path().join("config/helper.txt"), "do not overwrite").unwrap();
    let imported = run(
        replica.path(),
        &[
            "battery",
            "install",
            "github",
            "--from",
            bundle.to_str().unwrap(),
            "--sha256",
            checksum,
            "--json",
        ],
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    let text = std::fs::read_to_string(&replica_config).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    let file = replica_config
        .parent()
        .unwrap()
        .join(parsed["bundle"]["files"][0].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(file).unwrap(), "portable data");
    assert_eq!(
        std::fs::read_to_string(replica.path().join("config/helper.txt")).unwrap(),
        "do not overwrite"
    );
    assert!(!replica.path().join("config/secret.txt").exists());
    let rebundle = replica.path().join("again.tar.gz");
    let output = run(
        replica.path(),
        &["bundle", "--output", rebundle.to_str().unwrap(), "--json"],
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        run(replica.path(), &["battery", "remove", "github", "--json"])
            .status
            .success()
    );
    assert!(appa_runtime::config::Config::load(&replica_config).is_ok());
}

#[test]
fn battery_install_and_remove_update_the_real_policy_without_network_or_host_registration() {
    let root = tempfile::tempdir().unwrap();
    let config = deployment(root.path());
    let original = std::fs::read_to_string(&config).unwrap();
    let output = run(
        root.path(),
        &["battery", "install", "github", "--server", "work-github", "--json"],
    );
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["result"]["state"], "installed");
    let after = std::fs::read_to_string(&config).unwrap();
    assert!(after.contains(&original));
    let effective = appa_runtime::config::Config::load(&config).unwrap();
    assert_eq!(
        effective.policy_file().value()["tool"][0]["name"].as_str(),
        Some("Custom")
    );
    assert_eq!(
        effective.policy_file().value()["tool"][1]["name"].as_str(),
        Some("mcp/github/read")
    );
    let repeated = run(root.path(), &["battery", "install", "github", "--json"]);
    assert!(repeated.status.success());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), after);
    let removed = run(root.path(), &["battery", "remove", "github", "--json"]);
    assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stdout));
    let effective = appa_runtime::config::Config::load(&config).unwrap();
    assert_eq!(effective.policy_file().value()["tool"].as_array().unwrap().len(), 1);
    assert!(std::fs::read_to_string(&config).unwrap().contains(&original));
}

#[test]
fn removal_refuses_a_manual_include_instead_of_claiming_the_battery_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let config = deployment(root.path());
    let original = std::fs::read_to_string(&config).unwrap();
    let text = format!(
        "include=['.appa/appa.toml/generations/{}/marketplace/batteries/github/appa.toml']\n{original}",
        "a".repeat(40)
    );
    std::fs::write(&config, &text).unwrap();
    assert!(
        run(root.path(), &["battery", "install", "github", "--json"])
            .status
            .success()
    );
    let before = std::fs::read(&config).unwrap();
    let removed = run(root.path(), &["battery", "remove", "github", "--json"]);
    assert_eq!(removed.status.code(), Some(1));
    assert_eq!(std::fs::read(&config).unwrap(), before);
    let listed = run(root.path(), &["battery", "list", "--json"]);
    let document: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(document["result"]["packages"][0]["name"], "github");
}

#[test]
fn human_battery_and_bundle_results_are_text_with_copyable_identifiers() {
    let root = tempfile::tempdir().unwrap();
    let config = deployment(root.path());
    let installed = run(root.path(), &["battery", "install", "github"]);
    assert!(installed.status.success());
    let text = String::from_utf8(installed.stdout).unwrap();
    assert!(text.contains("github"));
    assert!(text.contains(config.to_str().unwrap()));
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
    assert!(
        !installed.stderr.is_empty(),
        "artifact verification has a progress phase"
    );
    let archive = root.path().join("export.tar.gz");
    let exported = run(root.path(), &["bundle", "--output", archive.to_str().unwrap()]);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let text = String::from_utf8(exported.stdout).unwrap();
    assert!(text.contains(archive.to_str().unwrap()));
    let digest = appa_package::generation::ArtifactDigest::of_bytes(&std::fs::read(&archive).unwrap());
    assert!(text.contains(digest.hex()));
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
}
