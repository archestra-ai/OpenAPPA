//! One isolated activation: private home, install, config, data and Claude
//! directories, fake `claude` and `curl` first on PATH, the fixture starter in
//! place of the deployed binary's own runtime start, and the config the
//! marketplace would have written, holding the shipped default.
#![cfg(unix)]
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use appa_engine::profile::PolicyFileKey;
use appa_runtime::config::Config;
use sha2::{Digest, Sha256};

use crate::common::{repo_root, stage_bundle};

/// The key of the policy the fixture's config carries: the shipped default,
/// which composes to the same bytes wherever it is loaded from.
pub fn default_policy_key() -> String {
    let config = Config::load(&default_config_path()).expect("the shipped default loads");
    PolicyFileKey::of(config.policy_file().bytes()).as_str().to_owned()
}

fn default_config_path() -> PathBuf {
    repo_root().join("marketplace/plugins/claude-code/default.appa.toml")
}

/// The shipped default config, byte for byte: what the marketplace writes on a
/// first install, and what the fixture's config holds.
pub fn shipped_default_config() -> String {
    fs::read_to_string(default_config_path()).expect("the shipped default is readable")
}

pub fn executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fixture is executable");
}

/// The staged plugin tree of this checkout, packed as the archive a development
/// build's generation carries and its activation verifies.
fn pack_bundle(root: &Path) -> PathBuf {
    let staged = stage_bundle(root);
    let archive = root.join("plugin-source.tar.gz");
    let file = fs::File::create(&archive).expect("the archive is created");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::fast()));
    builder.append_dir_all(".", &staged).expect("the staged tree packs");
    builder
        .into_inner()
        .and_then(flate2::write::GzEncoder::finish)
        .expect("the archive is finished");
    archive
}

pub fn runtime_fingerprint(deployed: &Path) -> String {
    let digest = Sha256::digest(fs::read(deployed).expect("runtime bytes"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub struct Fixture {
    _directory: tempfile::TempDir,
    pub root: PathBuf,
    pub bin: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub claude: PathBuf,
    pub appa: PathBuf,
    pub archive: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().expect("a resolved root");
        let bin = root.join("bin");
        let claude = root.join("claude");
        fs::create_dir_all(&bin).expect("bin directory");
        fs::create_dir_all(&claude).expect("Claude directory");
        let appa = bin.join("appa");
        fs::copy(env!("CARGO_BIN_EXE_appa"), &appa).expect("appa is copied");
        executable(&appa);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for (fixture, target) in [
            ("fake-curl.sh", bin.join("curl")),
            ("fake-claude.sh", bin.join("claude")),
            ("fake-ensure-runtime.sh", root.join("fake-starter.sh")),
        ] {
            fs::copy(fixtures.join(fixture), &target).expect("the fixture is copied");
            executable(&target);
        }
        let config = root.join("config");
        fs::create_dir_all(&config).expect("config directory");
        fs::write(config.join("appa.toml"), shipped_default_config()).expect("the config is written");
        let archive = pack_bundle(&root);
        Self {
            _directory: directory,
            config,
            data: root.join("data"),
            root,
            bin,
            claude,
            appa,
            archive,
        }
    }

    /// `appa activate-claude` against this fixture, as `appa plugin install
    /// claude-code` runs it once the generation is retained, with the endpoint
    /// answering as this deployment's own healthy runtime serving the fixture's
    /// policy. A test overrides the `FAKE_*` variables for the case it reproduces.
    pub fn activate(&self) -> Command {
        let mut command = Command::new(&self.appa);
        command
            .current_dir(&self.root)
            .arg("activate-claude")
            .arg("--config")
            .arg(self.config.join("appa.toml"))
            .arg("--archive")
            .arg(&self.archive);
        self.environment(&mut command);
        command
    }

    /// `appa remove-claude` against this fixture, as `appa plugin remove
    /// claude-code` runs it.
    pub fn remove(&self) -> Command {
        let mut command = Command::new(&self.appa);
        command
            .current_dir(&self.root)
            .arg("remove-claude")
            .arg("--config")
            .arg(self.config.join("appa.toml"))
            .arg("--archive")
            .arg(&self.archive);
        self.environment(&mut command);
        command
    }

    fn environment(&self, command: &mut Command) {
        command
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default()),
            )
            .env("HOME", &self.root)
            .env("APPA_INSTALL_DIR", &self.bin)
            .env("APPA_CONFIG_DIR", &self.config)
            .env("APPA_DATA_DIR", &self.data)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("APPA_RUNTIME_STARTER", self.root.join("fake-starter.sh"))
            .env("FAKE_CLAUDE_HOME", &self.claude)
            .env("FAKE_CLAUDE_LOG", self.root.join("claude.log"))
            .env("FAKE_RUNTIME_FINGERPRINT", runtime_fingerprint(&self.appa))
            .env("FAKE_RUNTIME_CONFIG", self.config.join("appa.toml"))
            .env("FAKE_POLICY_KEY", default_policy_key())
            .env_remove("APPA_ENDPOINT")
            .env_remove("APPA_RUNTIME_URL");
    }

    /// Where activation deploys the harness binary: private to appa, never on PATH.
    pub fn deployed_binary(&self) -> PathBuf {
        self.data.join("bin/appa")
    }

    pub fn settings(&self) -> PathBuf {
        self.claude.join("settings.json")
    }

    pub fn settings_value(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.settings()).expect("Claude settings")).expect("settings JSON")
    }

    /// The JSON the fake `claude mcp add-json` was given for the `appa` server,
    /// or `None` when no server is registered.
    pub fn mcp_registration(&self) -> Option<serde_json::Value> {
        let bytes = fs::read(self.claude.join("mcp-appa")).ok()?;
        Some(serde_json::from_slice(&bytes).expect("the registered server is JSON"))
    }

    pub fn skill(&self) -> PathBuf {
        self.claude.join("skills/appa-guide/SKILL.md")
    }

    pub fn launcher(&self) -> PathBuf {
        self.bin.join("clappa")
    }

    pub fn launcher_is_armed(&self) -> bool {
        fs::read_to_string(self.launcher()).is_ok_and(|launcher| !launcher.contains("did not complete"))
    }

    /// Every `claude` invocation the fixture answered, one line each, in order.
    pub fn claude_calls(&self) -> String {
        fs::read_to_string(self.root.join("claude.log")).expect("the Claude invocation log is readable")
    }

    pub fn successful_activation(&self) {
        let output = self.activate().output().expect("appa activates");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    /// Every entry of the settings file that names the deployed binary, with the
    /// event it is registered under.
    pub fn owned_hook_entries(&self) -> Vec<(String, serde_json::Value)> {
        let settings = self.settings_value();
        let binary = self.deployed_binary();
        let mut entries = Vec::new();
        for (event, groups) in settings["hooks"].as_object().expect("hooks is an object") {
            for group in groups.as_array().expect("each event carries groups") {
                for hook in group["hooks"].as_array().expect("each group carries hooks") {
                    if hook["command"].as_str() == binary.to_str() {
                        entries.push((event.clone(), hook.clone()));
                    }
                }
            }
        }
        entries
    }
}

/// Everything a failed upgrade must leave as it found it.
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub binary: Option<Vec<u8>>,
    pub settings: Option<Vec<u8>>,
    pub mcp: Option<serde_json::Value>,
    pub skill: Option<Vec<u8>>,
    pub launcher: Option<Vec<u8>>,
}

impl Installed {
    pub fn of(fixture: &Fixture) -> Self {
        Self {
            binary: fs::read(fixture.deployed_binary()).ok(),
            settings: fs::read(fixture.settings()).ok(),
            mcp: fixture.mcp_registration(),
            skill: fs::read(fixture.skill()).ok(),
            launcher: fs::read(fixture.launcher()).ok(),
        }
    }

    pub fn nothing() -> Self {
        Self {
            binary: None,
            settings: None,
            mcp: None,
            skill: None,
            launcher: None,
        }
    }
}
