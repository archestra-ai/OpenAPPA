//! One isolated activation: private home, install, config, data and Claude
//! directories, a fake `claude` first on PATH, a stand-in runtime answering
//! the deployment's endpoint, the fixture starter in place of the deployed
//! binary's own runtime start, and the config the marketplace would have
//! written, holding the shipped default.
#![cfg(unix)]
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use appa_engine::profile::PolicyFileKey;
use appa_runtime::config::{Config, ConfigError};
use sha2::{Digest, Sha256};

use crate::common::repo_root;

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

pub fn runtime_fingerprint(deployed: &Path) -> String {
    let digest = Sha256::digest(fs::read(deployed).expect("runtime bytes"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A config naming a secret this process cannot see has no key here; the
/// stand-in runtime then serves a placeholder, as a runtime that could see it would.
fn policy_key_of(config: &Path) -> String {
    match Config::load(config) {
        Ok(config) => PolicyFileKey::of(config.policy_file().bytes()).as_str().to_owned(),
        Err(ConfigError::MissingSecret { .. }) => "composed-where-the-secret-is".to_owned(),
        Err(error) => panic!("the fixture policy loads: {error}"),
    }
}

/// What `/policy-key` answers.
pub enum PolicyRoute {
    /// The key of the fixture's config as it is on disk when asked.
    OfConfig,
    Serves(String),
    /// The route is missing, which activation refuses.
    Missing,
    /// Never answers inside activation's deadline.
    Stalls,
}

/// What the stand-in runtime at the fixture's endpoint answers, route by route.
/// A test changes a field for the case it reproduces.
pub struct Answers {
    /// The build `/binary-fingerprint` names.
    pub fingerprint: String,
    /// When set, what every `/binary-fingerprint` after the first names: init
    /// probes the endpoint once before it mutates anything and once after the
    /// start, and the two answers differing is how a runtime arriving
    /// mid-install is reproduced.
    pub fingerprint_later: Option<String>,
    /// The configuration `/binary-fingerprint` says it serves.
    pub config: PathBuf,
    pub policy: PolicyRoute,
    /// What `/policy-key` answers once a reload was asked for: a runtime that
    /// loaded the file it was asked to.
    pub policy_after_reload: Option<String>,
    /// A directory the fixture starter starts a stand-in process in: until its
    /// pid file exists nothing answers, and afterwards the stand-in's pid is the
    /// one every identity answer names. Without it the answering pid is that of
    /// an exited process, which no ownership check accepts.
    pub stand_in: Option<PathBuf>,
    /// How many reloads were asked for.
    pub reloads: usize,
    /// When set, receives the request line of every request answered.
    pub recorder: Option<mpsc::Sender<String>>,
    fingerprints: usize,
}

enum Reply {
    Answer(&'static str, String),
    Nothing,
    Stall,
}

/// The stand-in runtime: every connection answered on its own thread, so one
/// that stalls holds up nothing else.
fn serve(listener: TcpListener, answers: Arc<Mutex<Answers>>, policy_file: PathBuf, exited_pid: u32) {
    for connection in listener.incoming() {
        let Ok(connection) = connection else {
            return;
        };
        let answers = Arc::clone(&answers);
        let policy_file = policy_file.clone();
        std::thread::spawn(move || answer(connection, &answers, &policy_file, exited_pid));
    }
}

fn answer(mut connection: TcpStream, answers: &Mutex<Answers>, policy_file: &Path, exited_pid: u32) {
    let mut reader = BufReader::new(connection.try_clone().expect("the stream clones"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) if header == "\r\n" => break,
            Ok(_) => {}
        }
    }
    let request = request.trim_end().to_owned();
    let reply = {
        let mut answers = answers.lock().expect("the answers are never poisoned");
        if let Some(recorder) = &answers.recorder {
            let _ = recorder.send(request.clone());
        }
        reply(&mut answers, &request, policy_file, exited_pid)
    };
    match reply {
        Reply::Answer(status, body) => {
            let _ = connection.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
        Reply::Nothing => {}
        Reply::Stall => std::thread::sleep(Duration::from_secs(5)),
    }
}

fn reply(answers: &mut Answers, request: &str, policy_file: &Path, exited_pid: u32) -> Reply {
    let pid = match &answers.stand_in {
        None => exited_pid.to_string(),
        Some(stand_in) => match fs::read_to_string(stand_in.join("pid")) {
            Ok(pid) => pid.trim().to_owned(),
            Err(_) => return Reply::Nothing,
        },
    };
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    match path {
        "/health" => Reply::Answer("200 OK", "ok".to_owned()),
        "/policy-key" => match (&answers.policy, &answers.policy_after_reload) {
            (PolicyRoute::Stalls, _) => Reply::Stall,
            (_, Some(key)) if answers.reloads > 0 => Reply::Answer("200 OK", key.clone()),
            (PolicyRoute::Missing, _) => Reply::Answer("404 Not Found", String::new()),
            (PolicyRoute::Serves(key), _) => Reply::Answer("200 OK", key.clone()),
            (PolicyRoute::OfConfig, _) => Reply::Answer("200 OK", policy_key_of(policy_file)),
        },
        "/reload" => {
            answers.reloads += 1;
            Reply::Answer("200 OK", String::new())
        }
        "/binary-fingerprint" => {
            answers.fingerprints += 1;
            let build = match &answers.fingerprint_later {
                Some(later) if answers.fingerprints > 1 => later,
                _ => &answers.fingerprint,
            };
            Reply::Answer("200 OK", format!("{build} {pid}\n{}", answers.config.display()))
        }
        "/hook" => Reply::Answer("200 OK", r#"{"protocol":1,"decision":"ack"}"#.to_owned()),
        _ => Reply::Answer("404 Not Found", String::new()),
    }
}

/// The pid of a process that ran and is gone.
fn exited_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("`true` runs");
    let pid = child.id();
    child.wait().expect("`true` exits");
    pid
}

pub struct Fixture {
    _directory: tempfile::TempDir,
    pub root: PathBuf,
    pub bin: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub claude: PathBuf,
    pub appa: PathBuf,
    /// The deployment's endpoint, where the stand-in runtime answers.
    pub url: String,
    answers: Arc<Mutex<Answers>>,
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
            ("fake-claude.sh", bin.join("claude")),
            ("fake-ensure-runtime.sh", root.join("fake-starter.sh")),
        ] {
            fs::copy(fixtures.join(fixture), &target).expect("the fixture is copied");
            executable(&target);
        }
        let config = root.join("config");
        fs::create_dir_all(&config).expect("config directory");
        fs::write(config.join("appa.toml"), shipped_default_config()).expect("the config is written");

        let listener = TcpListener::bind("127.0.0.1:0").expect("the stand-in runtime binds");
        let url = format!("http://{}", listener.local_addr().expect("the bound address"));
        let answers = Arc::new(Mutex::new(Answers {
            fingerprint: runtime_fingerprint(&appa),
            fingerprint_later: None,
            config: config.join("appa.toml"),
            policy: PolicyRoute::OfConfig,
            policy_after_reload: None,
            stand_in: None,
            reloads: 0,
            recorder: None,
            fingerprints: 0,
        }));
        let served = Arc::clone(&answers);
        let policy_file = config.join("appa.toml");
        let exited = exited_pid();
        std::thread::spawn(move || serve(listener, served, policy_file, exited));

        Self {
            _directory: directory,
            config,
            data: root.join("data"),
            root,
            bin,
            claude,
            appa,
            url,
            answers,
        }
    }

    /// What the stand-in runtime answers, for a test to change.
    pub fn answers(&self) -> MutexGuard<'_, Answers> {
        self.answers.lock().expect("the answers are never poisoned")
    }

    /// `appa activate-claude` against this fixture, as `appa plugin install
    /// claude-code` runs it once the generation is retained, with the endpoint
    /// answering as this deployment's own healthy runtime serving the fixture's
    /// policy. A test changes [`Fixture::answers`] for the case it reproduces.
    pub fn activate(&self) -> Command {
        self.bridge("activate-claude")
    }

    /// `appa remove-claude` against this fixture, as `appa plugin remove
    /// claude-code` runs it.
    pub fn remove(&self) -> Command {
        self.bridge("remove-claude")
    }

    /// `appa plugin remove claude-code --purge` against this fixture, with the
    /// endpoint the purge stops named by the caller.
    pub fn purge(&self, endpoint: &str) -> Command {
        let mut command = Command::new(&self.appa);
        command
            .current_dir(&self.root)
            .args(["plugin", "remove", "claude-code", "--purge"]);
        self.environment(&mut command);
        command.env("APPA_ENDPOINT", endpoint).env_remove("APPA_CONFIG");
        command
    }

    fn bridge(&self, subcommand: &str) -> Command {
        let mut command = Command::new(&self.appa);
        command
            .current_dir(&self.root)
            .arg(subcommand)
            .arg("--config")
            .arg(self.config.join("appa.toml"));
        self.environment(&mut command);
        command
    }

    /// The starter reads the stand-in directory from its environment, so a
    /// command built after [`Answers::stand_in`] is set starts the stand-in.
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
            .env("APPA_ENDPOINT", &self.url)
            .env_remove("APPA_RUNTIME_URL");
        match &self.answers().stand_in {
            Some(stand_in) => command.env("FAKE_RUNTIME_STAND_IN", stand_in),
            None => command.env_remove("FAKE_RUNTIME_STAND_IN"),
        };
    }

    /// The key of the fixture's config as it is on disk now.
    pub fn policy_key(&self) -> String {
        policy_key_of(&self.config.join("appa.toml"))
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

    pub fn contracts_guide(&self) -> PathBuf {
        self.claude.join("skills/appa-guide/references/contracts.md")
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
