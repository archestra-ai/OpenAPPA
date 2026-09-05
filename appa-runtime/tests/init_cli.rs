#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use appa_engine::profile::PolicyFileKey;
use appa_runtime::config::Config;
use sha2::{Digest, Sha256};

mod common;
use common::{repo_root, stage_bundle};

/// The key of the policy a first install writes: the shipped default, which
/// composes to the same bytes wherever it is loaded from.
fn default_policy_key() -> String {
    let example = repo_root().join("marketplace/adapters/claude-code/default.appa.toml");
    let config = Config::load(&example).expect("the shipped default loads");
    PolicyFileKey::of(config.policy_file().bytes()).as_str().to_owned()
}

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

/// Where init deploys the harness binary: private to appa, never on PATH.
fn deployed_binary(data: &Path) -> std::path::PathBuf {
    data.join("bin/appa")
}

/// One isolated install: private home, install, config, data and Claude
/// directories, fake `claude` and `curl` first on PATH, and the installed-plugin
/// directory the fake registry points at, carrying the fixture starter.
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    config: PathBuf,
    data: PathBuf,
    claude: PathBuf,
    plugin: PathBuf,
    appa: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().expect("a resolved root");
        let bin = root.join("bin");
        let claude = root.join("claude");
        let plugin = root.join("installed-plugin");
        fs::create_dir_all(&bin).expect("bin directory");
        fs::create_dir_all(plugin.join("hooks")).expect("plugin hooks");
        fs::create_dir_all(&claude).expect("Claude directory");
        let appa = install_test_binaries(&bin);
        install_fake_curl(&bin);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for (fixture, target) in [
            ("fake-claude.sh", bin.join("claude")),
            ("fake-ensure-runtime.sh", plugin.join("hooks/ensure-runtime.sh")),
        ] {
            fs::copy(fixtures.join(fixture), &target).expect("the fixture is copied");
            executable(&target);
        }
        fs::write(plugin.join("statusline.sh"), "#!/bin/sh\nexit 0\n").expect("statusline");
        let source = stage_bundle(&root);
        Self {
            _directory: directory,
            config: root.join("config"),
            data: root.join("data"),
            root,
            bin,
            claude,
            plugin,
            appa,
            source,
        }
    }

    /// `appa init claude-code` against this fixture, with the endpoint answering
    /// as this deployment's own healthy runtime serving the policy init writes.
    /// A test overrides the `FAKE_*` variables for the case it reproduces.
    fn init(&self) -> Command {
        let mut command = Command::new(&self.appa);
        command
            .current_dir(&self.root)
            .args(["init", "claude-code", "--plugin-source"])
            .arg(&self.source)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default()),
            )
            .env("HOME", &self.root)
            .env("APPA_INSTALL_DIR", &self.bin)
            .env("APPA_CONFIG_DIR", &self.config)
            .env("APPA_DATA_DIR", &self.data)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("FAKE_CLAUDE_HOME", &self.claude)
            .env("FAKE_CLAUDE_LOG", self.root.join("claude.log"))
            .env("FAKE_PLUGIN_ROOT", &self.plugin)
            .env("FAKE_RUNTIME_FINGERPRINT", runtime_fingerprint(&self.appa))
            .env("FAKE_RUNTIME_CONFIG", self.config.join("appa.toml"))
            .env("FAKE_POLICY_KEY", default_policy_key());
        command
    }

    fn deployed_binary(&self) -> PathBuf {
        deployed_binary(&self.data)
    }

    fn statusline(&self) -> PathBuf {
        self.bin.join("appa-statusline.sh")
    }

    fn settings(&self) -> PathBuf {
        self.claude.join("settings.json")
    }

    fn registry(&self) -> Option<serde_json::Value> {
        let bytes = fs::read(self.claude.join("plugins/installed_plugins.json")).ok()?;
        Some(serde_json::from_slice(&bytes).expect("the registry is JSON"))
    }
}

/// Everything a failed upgrade must leave as it found it.
#[derive(Debug, PartialEq, Eq)]
struct Installed {
    binary: Option<Vec<u8>>,
    statusline: Option<Vec<u8>>,
    settings: Option<Vec<u8>>,
    registry: Option<serde_json::Value>,
}

impl Installed {
    fn of(fixture: &Fixture) -> Self {
        Self {
            binary: fs::read(fixture.deployed_binary()).ok(),
            statusline: fs::read(fixture.statusline()).ok(),
            settings: fs::read(fixture.settings()).ok(),
            registry: fixture.registry(),
        }
    }
}

/// An install a previous init left, with bytes of its own in every file a later
/// init rewrites, so a restore that merely reinstalls this build is told apart
/// from one that puts the previous files back.
fn previous_install(fixture: &Fixture) -> Installed {
    assert!(fixture.init().output().expect("appa init runs").status.success());
    fs::write(fixture.deployed_binary(), b"the previous build").expect("the previous binary is written");
    fs::write(fixture.statusline(), b"the previous statusline").expect("the previous statusline is written");
    let settings = fs::read_to_string(fixture.settings()).expect("settings are readable");
    fs::write(fixture.settings(), settings.replacen('{', "{\"userSetting\": true,", 1))
        .expect("the previous settings are written");
    Installed::of(fixture)
}

fn launcher_is_armed(fixture: &Fixture) -> bool {
    fs::read_to_string(fixture.bin.join("clappa")).is_ok_and(|launcher| !launcher.contains("init did not complete"))
}

#[test]
fn a_failure_before_the_statusline_puts_the_previous_binary_back() {
    let fixture = Fixture::new();
    let before = previous_install(&fixture);
    // The statusline install refuses a plugin without its statusline, after the
    // binary has already been replaced.
    fs::remove_file(fixture.plugin.join("statusline.sh")).expect("the plugin statusline is removed");

    let failed = fixture.init().output().expect("appa init runs");

    assert!(!failed.status.success());
    assert_eq!(Installed::of(&fixture), before);
    assert!(!fixture.deployed_binary().with_extension("prev").exists());
    assert!(
        launcher_is_armed(&fixture),
        "the previous install's launcher is re-armed"
    );
}

#[test]
fn a_failure_at_the_start_puts_the_previous_statusline_back() {
    let fixture = Fixture::new();
    let before = previous_install(&fixture);

    let failed = fixture
        .init()
        .env("FAKE_STARTER_FAILS", "1")
        .output()
        .expect("appa init runs");

    assert!(!failed.status.success());
    assert_eq!(Installed::of(&fixture), before);
    assert!(!fixture.deployed_binary().with_extension("prev").exists());
    assert!(
        launcher_is_armed(&fixture),
        "the previous install's launcher is re-armed"
    );
}

/// The rollback source under the data directory belongs to the init that wrote
/// it. A concurrent init, or one that crashed mid-switch, may still need its own,
/// so a successful init removes only the one it made.
#[test]
fn another_inits_rollback_source_survives_a_successful_init() {
    let fixture = Fixture::new();
    let other = fixture.data.join(".appa-init-recovery-424242");
    fs::create_dir_all(other.join("plugin")).expect("the other rollback source is written");
    assert!(fixture.init().output().expect("appa init runs").status.success());
    let upgrade = fixture.init().output().expect("appa init runs");
    assert!(upgrade.status.success(), "{}", String::from_utf8_lossy(&upgrade.stderr));

    assert!(
        other.join("plugin").is_dir(),
        "the other init's rollback source was removed"
    );
    let leftovers: Vec<_> = fs::read_dir(&fixture.data)
        .expect("the data directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".appa-init-recovery-"))
        .collect();
    assert_eq!(
        leftovers,
        [".appa-init-recovery-424242"],
        "this init's own rollback source was left"
    );
}

/// A runtime that does not answer for its policy inside the probe's deadline
/// cannot be reconciled, so init fails and binds nothing to it rather than
/// reporting it healthy.
#[test]
fn a_policy_key_timeout_fails_init() {
    let fixture = Fixture::new();

    let failed = fixture
        .init()
        .env("FAKE_POLICY_KEY_TIMEOUT", "1")
        .output()
        .expect("appa init runs");

    assert!(!failed.status.success());
    assert_eq!(
        Installed::of(&fixture),
        Installed {
            binary: None,
            statusline: None,
            settings: None,
            registry: Some(serde_json::json!({"version": 2, "plugins": {}})),
        }
    );
    assert!(!fixture.bin.join("clappa").exists());
}

/// A first install that fails after its runtime is up leaves nothing behind:
/// the runtime it started is stopped, and no file or registration it wrote
/// survives to bind a session to a runtime whose policy init could not settle.
#[test]
fn a_failure_after_the_start_stops_the_runtime_init_started() {
    let fixture = Fixture::new();
    let stand_in = fixture.root.join("stand-in");

    let failed = fixture
        .init()
        .env("FAKE_RUNTIME_STAND_IN", &stand_in)
        .env_remove("FAKE_POLICY_KEY")
        .output()
        .expect("appa init runs");

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
        "the runtime init started is still running"
    );
    assert_eq!(
        Installed::of(&fixture),
        Installed {
            binary: None,
            statusline: None,
            settings: None,
            registry: Some(serde_json::json!({"version": 2, "plugins": {}})),
        }
    );
    assert!(!fixture.claude.join("marketplace-appa").exists());
    assert!(!fixture.bin.join("clappa").exists());
}

#[test]
fn the_plugin_source_override_is_hidden_from_normal_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["init", "claude-code", "--help"])
        .output()
        .expect("appa help runs");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("plugin-source"));
}

/// The shipped default config, byte for byte: what a first init seeds.
fn shipped_default_config() -> String {
    fs::read_to_string(repo_root().join("marketplace/adapters/claude-code/default.appa.toml"))
        .expect("the shipped default is readable")
}

/// Every `claude` invocation the fixture answered, one line each, in order.
fn claude_calls(fixture: &Fixture) -> String {
    fs::read_to_string(fixture.root.join("claude.log")).expect("the Claude invocation log is readable")
}

/// How many copies of the plugin Claude's registry holds, or `None` when it
/// holds no entry for it at all.
fn registered_copies(fixture: &Fixture) -> Option<usize> {
    fixture.registry()?["plugins"]["appa-runtime@appa"]
        .as_array()
        .map(Vec::len)
}

fn successful_init(fixture: &Fixture) {
    let output = fixture.init().output().expect("appa init runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn a_first_init_installs_the_bundle_and_registers_one_plugin() {
    let fixture = Fixture::new();
    let reloads = fixture.root.join("reloads");
    let output = fixture
        .init()
        .env("FAKE_RELOADS", &reloads)
        .output()
        .expect("appa init runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    // The harness binary lands on an appa-private path, not on PATH, and the
    // copy that is on PATH is left alone.
    assert!(fixture.deployed_binary().is_file());
    assert!(fixture.bin.join("appa").is_file());
    assert!(launcher_is_armed(&fixture));
    assert!(fixture.statusline().is_file());
    assert_eq!(
        fs::read_to_string(fixture.config.join("appa.toml")).ok(),
        Some(shipped_default_config())
    );

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.settings()).expect("Claude settings")).expect("settings JSON");
    assert!(
        settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|command| command.ends_with("appa-statusline.sh"))
    );
    assert_eq!(registered_copies(&fixture), Some(1));
    // The runtime reports serving the key the fixture computed for the shipped
    // default, and init found nothing to reconcile: that key is the real one.
    assert!(
        !reloads.exists(),
        "a first install reloaded a runtime already serving its policy"
    );
}

#[test]
fn a_rerun_keeps_the_config_and_replaces_the_plugin_once() {
    let fixture = Fixture::new();
    successful_init(&fixture);
    let config = fixture.config.join("appa.toml");
    let authored = format!(
        "{}\n# an edit init keeps\n",
        fs::read_to_string(&config).expect("the config is readable")
    );
    fs::write(&config, &authored).expect("the edit is written");

    successful_init(&fixture);

    assert_eq!(fs::read_to_string(&config).ok(), Some(authored));
    assert_eq!(registered_copies(&fixture), Some(1));
    let calls = claude_calls(&fixture);
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
}

#[test]
fn a_failed_switch_restores_the_registered_plugin_and_the_launcher() {
    let fixture = Fixture::new();
    successful_init(&fixture);
    let before = Installed::of(&fixture);

    for failure in ["marketplace-add", "plugin-install"] {
        let failed = fixture
            .init()
            .env("FAKE_CLAUDE_FAIL_ONCE", failure)
            .output()
            .expect("appa init runs");
        assert!(!failed.status.success(), "the injected {failure} failure must surface");
        assert_eq!(
            Installed::of(&fixture),
            before,
            "{failure} must restore the prior installation"
        );
        assert!(
            fixture.claude.join("marketplace-appa").is_file(),
            "{failure} must restore the marketplace"
        );
        assert!(
            launcher_is_armed(&fixture),
            "{failure} must restore the protected launcher"
        );
    }
}

/// A foreign runtime already owning the endpoint is refused before Claude is
/// touched at all, so the installation it would have replaced is still the one
/// that is registered and running. Another build is one way to be foreign; this
/// build serving another deployment's configuration is the other, and it is the
/// one a digest alone cannot see.
#[test]
fn a_foreign_runtime_is_refused_before_claude_is_touched() {
    let fixture = Fixture::new();
    successful_init(&fixture);
    let fingerprint = runtime_fingerprint(&fixture.appa);
    let mine = fixture.config.join("appa.toml");

    for (build, serving) in [
        ("not-this-build", mine.as_path()),
        (fingerprint.as_str(), Path::new("/somewhere/else/appa.toml")),
    ] {
        let before = claude_calls(&fixture).lines().count();
        let refused = fixture
            .init()
            .env("FAKE_RUNTIME_FINGERPRINT", build)
            .env("FAKE_RUNTIME_CONFIG", serving)
            .output()
            .expect("appa init runs");
        assert!(
            !refused.status.success(),
            "a runtime claiming build {build} at {} must be refused",
            serving.display()
        );
        let mutating: Vec<String> = claude_calls(&fixture)
            .lines()
            .skip(before)
            .filter(|line| !line.starts_with("plugin marketplace list"))
            .map(str::to_owned)
            .collect();
        assert!(
            mutating.is_empty(),
            "a refused endpoint must not reach a single mutating claude call: {mutating:?}",
        );
        assert!(
            launcher_is_armed(&fixture),
            "a refused endpoint must leave the working launcher armed"
        );
    }
}

#[test]
fn a_failed_rollback_disarms_the_launcher_until_an_init_completes() {
    let fixture = Fixture::new();
    successful_init(&fixture);

    let unrecoverable = fixture
        .init()
        .env("FAKE_CLAUDE_FAIL_ONCE", "plugin-install-always")
        .output()
        .expect("appa init runs");
    assert!(!unrecoverable.status.success());
    assert!(
        !launcher_is_armed(&fixture),
        "a failed rollback must leave clappa refusing to launch an unprotected session",
    );

    successful_init(&fixture);
    assert!(launcher_is_armed(&fixture));
}

/// A runtime of this deployment that survived the install keeps serving the policy it
/// loaded at startup, and only the install can notice. Nothing here is interactive, so the
/// reconcile reloads rather than asks.
#[test]
fn init_reloads_a_surviving_runtime_that_serves_an_older_policy() {
    let fixture = Fixture::new();
    let reloads = fixture.root.join("reloads");
    let output = fixture
        .init()
        // This deployment's own runtime, serving a policy that is not the file init
        // wrote: the one state a reload is for.
        .env("FAKE_POLICY_KEY", "a-policy-this-init-did-not-compose")
        .env("FAKE_RELOADS", &reloads)
        .output()
        .expect("appa init runs");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        reloads.exists(),
        "a diverged runtime of this deployment must be reloaded, not left serving its older policy",
    );
}

#[test]
fn init_keeps_a_custom_statusline() {
    let fixture = Fixture::new();
    let custom = serde_json::json!({"statusLine": {"type": "command", "command": "my-status"}});
    fs::write(fixture.settings(), custom.to_string()).expect("custom settings");

    successful_init(&fixture);

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.settings()).expect("settings remain")).expect("settings JSON");
    assert_eq!(settings, custom);
    assert!(!fixture.statusline().exists());
}

/// A relative directory override must not reach the rendered hooks as written.
///
/// Hooks run from whatever working directory Claude was launched in, so a
/// relative `state/bin/appa` would resolve somewhere else entirely at hook time
/// and find no binary, config or database.
#[test]
fn relative_directory_overrides_are_rendered_absolute() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let output = fixture
        .init()
        // Relative, resolved against the fixture's working directory and no other.
        .env("APPA_INSTALL_DIR", "bin")
        .env("APPA_CONFIG_DIR", "config")
        .env("APPA_DATA_DIR", "state")
        // The deployment the answering runtime claims: this init's own config.
        .env("FAKE_RUNTIME_CONFIG", root.join("config/appa.toml"))
        .output()
        .expect("appa init runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let deployment = fs::read_dir(root.join("state/deployments"))
        .expect("the deployment directory exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("plugin/hooks/appa-paths.sh").is_file())
        .expect("one materialized deployment");

    let rendered =
        fs::read_to_string(deployment.join("plugin/hooks/appa-paths.sh")).expect("the rendered paths file is readable");
    for name in ["APPA_BIN", "APPA_CONFIG", "APPA_DATA_DIR"] {
        let value = rendered
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}='")))
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap_or_else(|| panic!("{name} is missing from {rendered}"));
        assert!(Path::new(value).is_absolute(), "{name} was rendered relative: {value}",);
        assert!(
            Path::new(value).starts_with(root),
            "{name} does not resolve under the working directory it was given: {value}",
        );
    }
}

/// A runtime that fails verification *after* the Claude switch must take the
/// switch with it.
///
/// The preflight refuses a foreign owner that is already there, but one can
/// arrive between the preflight and the start. Leaving the new plugin
/// registered and the launcher armed against it would be the exact skew this
/// bundle exists to prevent: Claude gated by a plugin talking to a runtime
/// nobody verified. There was no plugin here before, so undoing means removing
/// the one just installed rather than restoring a predecessor.
#[test]
fn a_runtime_that_fails_verification_after_the_switch_undoes_it() {
    let fixture = Fixture::new();
    let output = fixture
        .init()
        // The preflight sees this build; everything after it sees a stranger.
        .env("FAKE_CURL_CALLS", fixture.root.join("curl-calls"))
        .env("FAKE_RUNTIME_FINGERPRINT_LATER", "not-this-build")
        .output()
        .expect("appa init runs");

    assert!(
        !output.status.success(),
        "verification against a foreign runtime must fail init: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        registered_copies(&fixture),
        None,
        "the plugin must not stay registered against a runtime that failed verification",
    );
    assert!(
        !fixture.claude.join("marketplace-appa").is_file(),
        "the marketplace must not stay registered either",
    );
    assert!(
        !fixture.bin.join("clappa").exists(),
        "the launcher must not be armed for a bundle whose runtime failed verification",
    );
}
