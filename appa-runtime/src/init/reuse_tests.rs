//! Subprocess isolation keeps the native install environment out of parallel tests.
#![cfg(unix)]

use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn prepared_registration_reuse_preserves_native_state_on_success_and_failure() {
    let executable = env::current_exe().unwrap();
    for outcome in ["success", "failure"] {
        let root = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let config = root_path.join("config/appa.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        create_default_config(&config).unwrap();
        let ComposedPolicy::Key(key) = verify_config(&config).unwrap() else {
            panic!("fixture policy is known")
        };
        let mut command = Command::new(&executable);
        command
            .args([
                "--exact",
                "init::reuse_tests::prepared_registration_reuse_child",
                "--nocapture",
            ])
            .env("APPA_REUSE_TEST_ROOT", root.path())
            .env("APPA_REUSE_TEST_OUTCOME", outcome)
            .env("HOME", &root_path)
            .env("APPA_INSTALL_DIR", root_path.join("bin"))
            .env("APPA_CONFIG_DIR", root_path.join("config"))
            .env("APPA_DATA_DIR", root_path.join("data"))
            .env("CLAUDE_CONFIG_DIR", root_path.join("claude"))
            .env("FAKE_CLAUDE_HOME", root_path.join("claude"))
            .env("FAKE_CLAUDE_LOG", root_path.join("claude.log"))
            .env("FAKE_RUNTIME_CONFIG", &config)
            .env(
                "PATH",
                format!("{}:{}", root_path.join("bin").display(), env::var("PATH").unwrap()),
            )
            .env("APPA_ENDPOINT", "http://127.0.0.1:48799")
            .env(
                "FAKE_RUNTIME_FINGERPRINT",
                format!("{:x}", Sha256::digest(fs::read(&executable).unwrap())),
            )
            .env("FAKE_POLICY_KEY", key)
            .env_remove("FAKE_STARTER_FAILS");
        if outcome == "failure" {
            command.env("FAKE_STARTER_FAILS", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{outcome}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn prepared_registration_reuse_child() {
    let Some(root) = env::var_os("APPA_REUSE_TEST_ROOT") else {
        return;
    };
    let root = fs::canonicalize(root).unwrap();
    let failure = env::var("APPA_REUSE_TEST_OUTCOME").unwrap() == "failure";
    let paths = DeploymentPaths {
        install_dir: root.join("bin"),
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        claude_dir: root.join("claude"),
    };
    for directory in [
        &paths.install_dir,
        &paths.config_dir,
        &paths.data_dir,
        &paths.claude_dir.join("plugins"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (source, target) in [("fake-claude.sh", "claude"), ("fake-curl.sh", "curl")] {
        let target = paths.install_dir.join(target);
        fs::copy(fixtures.join(source), &target).unwrap();
        fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = paths.config_dir.join("appa.toml");
    create_default_config(&config).unwrap();
    let appa = env::current_exe().unwrap();
    let deployed_appa = paths.data_dir.join("bin/appa");
    fs::create_dir_all(deployed_appa.parent().unwrap()).unwrap();
    fs::copy(&appa, &deployed_appa).unwrap();
    let source = root.join("source");
    plugin_bundle::stage_repository(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(), &source).unwrap();
    fs::copy(
        fixtures.join("fake-ensure-runtime.sh"),
        source.join("plugin/hooks/ensure-runtime.sh"),
    )
    .unwrap();
    let endpoint = Endpoint::parse("http://127.0.0.1:48799").unwrap();
    let deployment = plugin_bundle::materialize(
        Population::Tree(&source),
        &paths.data_dir.join("deployments"),
        &deployed_appa,
        &config,
        &paths.data_dir,
        &endpoint,
    )
    .unwrap();
    let plugin = deployment.root.join("plugin");
    let registry = paths.claude_dir.join("plugins/installed_plugins.json");
    let marketplace = paths.claude_dir.join("plugins/known_marketplaces.json");
    let settings = paths.claude_dir.join("settings.json");
    for (path, value) in [
        (
            &registry,
            serde_json::json!({"version":2,"plugins":{(PLUGIN):[{"scope":"user","installPath":plugin}]}}),
        ),
        (
            &marketplace,
            serde_json::json!({(MARKETPLACE):{"source":{"source":"directory","path":deployment.root}}}),
        ),
        (&settings, serde_json::json!({"enabledPlugins":{(PLUGIN):true}})),
    ] {
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut compensation = Compensation::default();
    install_statusline(&plugin, &paths, &mut compensation).unwrap();
    compensation.commit();
    let launcher = install_clappa(&paths.install_dir).unwrap();
    let statusline = paths.install_dir.join("appa-statusline.sh");
    if failure {
        fs::write(&statusline, "previous statusline").unwrap();
    }
    let preserved = [&registry, &marketplace, &settings, &launcher, &deployed_appa];
    let sentinel = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567890);
    let before: Vec<_> = preserved
        .iter()
        .map(|path| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(sentinel)
                .unwrap();
            fs::read(path).unwrap()
        })
        .collect();
    let statusline_before = fs::read(&statusline).unwrap();
    let log = root.join("claude.log");
    let result = install_claude(
        PluginSource::Explicit(source),
        Configuration::Prepared {
            config,
            previous_binary: None,
        },
    );
    if failure {
        assert!(matches!(result, Err(InitError::Starter(_))), "{result:?}");
    } else {
        assert!(result.is_ok(), "{result:?}");
    }
    assert_eq!(
        fs::read_to_string(log).unwrap().lines().collect::<Vec<_>>(),
        ["plugin marketplace list"]
    );
    for (path, bytes) in preserved.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), bytes, "{}", path.display());
        // A repaired statusline records and restores settings bytes on failure;
        // compensation does not promise to restore file timestamps.
        if !failure || *path != &settings {
            assert_eq!(
                fs::metadata(path).unwrap().modified().unwrap(),
                sentinel,
                "{}",
                path.display()
            );
        }
    }
    assert_eq!(fs::read(statusline).unwrap(), statusline_before);
}
