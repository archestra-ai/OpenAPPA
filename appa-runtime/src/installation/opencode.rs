//! Released AppenCode registration and operator-only setup. OpenCode loads the
//! retained JavaScript bundle; no package manager runs on the user's machine.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use appa_runtime_api::AdapterName;
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{InstallError, Installation, Selection, atomic_write, invalid, io, native, optional_bytes};
use crate::init::endpoint::{
    Endpoint, EndpointOwner, endpoint_health, endpoint_owner, reconcile_policy, verify_runtime_deployment,
};
use crate::runtime_start::{Deployment, ensure};
use crate::runtime_url::RuntimeTarget;

fn config_home() -> Result<PathBuf, InstallError> {
    let path = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .ok_or_else(|| invalid("no home directory; set XDG_CONFIG_HOME"))?;
    std::path::absolute(&path).map_err(|error| io("resolve config home", &path, error))
}

pub(super) fn config_path() -> Result<PathBuf, InstallError> {
    Ok(config_home()?.join("appa/opencode/appa.toml"))
}

fn settings_path() -> Result<PathBuf, InstallError> {
    let directory = std::env::var_os("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or(config_home()?.join("opencode"));
    let directory =
        std::path::absolute(&directory).map_err(|error| io("resolve OpenCode directory", &directory, error))?;
    if directory.join("opencode.jsonc").exists() {
        return Err(invalid(
            "global opencode.jsonc exists; merge AppenCode manually or convert it to JSON before installing; it will not be shadowed",
        ));
    }
    Ok(directory.join("opencode.json"))
}

fn endpoint() -> Result<Endpoint, InstallError> {
    Endpoint::parse(
        &super::archive::debug_override("APPA_OPENCODE_ENDPOINT").unwrap_or_else(|| "http://127.0.0.1:8789".into()),
    )
    .map_err(invalid)
}

fn quote(value: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    plugin: Value,
    guide: Value,
    mcp: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    config: PathBuf,
    current: Registration,
    previous: Option<Registration>,
}

struct Profile {
    path: PathBuf,
    before: Option<Vec<u8>>,
    settings: Value,
    receipt: Option<Receipt>,
}

impl Profile {
    fn read(config: &Path) -> Result<Self, InstallError> {
        let path = settings_path()?;
        let before = optional_bytes(&path)?;
        let settings: Value = before
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(invalid)?
            .unwrap_or_else(|| json!({}));
        if !settings.is_object()
            || settings.get("plugins").is_some_and(|value| !value.is_array())
            || settings.get("commands").is_some_and(|value| !value.is_object())
            || settings.get("mcp").is_some_and(|value| !value.is_object())
            || settings.pointer("/mcp/servers").is_some_and(|value| !value.is_object())
        {
            return Err(invalid(
                "OpenCode settings must contain an object, a plugins array, and object-valued commands and mcp.servers",
            ));
        }
        let receipt: Option<Receipt> = optional_bytes(&path.with_file_name(".appa-opencode.json"))?
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(invalid)?;
        if receipt.as_ref().is_some_and(|receipt| receipt.config != config) {
            return Err(invalid(
                "OpenCode is registered to another APPA deployment; remove that registration first",
            ));
        }
        Ok(Self {
            path,
            before,
            settings,
            receipt,
        })
    }

    fn owned(&self, value: &Value, field: fn(&Registration) -> &Value) -> bool {
        self.receipt.as_ref().is_some_and(|receipt| {
            field(&receipt.current) == value || receipt.previous.as_ref().is_some_and(|old| field(old) == value)
        })
    }

    fn plan(&mut self, config: &Path, plugin: &Path, url: &str) -> Result<Registration, InstallError> {
        let command = format!(
            "AppenCode policy setup runs in a separate operator session. From the project to inspect, ask the operator to run: appa opencode setup --config {}. Do not edit APPA policy from this coding session.",
            quote(&config.to_string_lossy())
        );
        let guide = json!({"description":"Set up or tune AppenCode policy", "template":command});
        let current_guide = &self.settings["commands"]["appa-guide"];
        if !current_guide.is_null()
            && *current_guide != guide
            && !self.owned(current_guide, |registration| &registration.guide)
        {
            return Err(invalid(
                "existing /appa-guide command is not installer-owned; preserve it and resolve the conflict",
            ));
        }
        let mut mcp = self.settings["mcp"]["servers"]["appa"].clone();
        if !mcp.is_null() && (!mcp.is_object() || mcp["url"] != format!("{url}/mcp")) {
            return Err(invalid(
                "existing appa MCP connection names a different runtime; reconcile it before installing",
            ));
        }
        if mcp.is_null() {
            mcp = json!({});
        }
        mcp["type"] = json!("remote");
        mcp["url"] = json!(format!("{url}/mcp"));
        mcp["oauth"] = json!(false);
        mcp["codemode"] = json!(false);
        let entry = json!({"package":plugin, "options":{"runtimeURL":url}});
        let mut plugins = self.settings["plugins"].as_array().cloned().unwrap_or_default();
        let mut matched = false;
        for value in &mut plugins {
            if self.owned(value, |registration| &registration.plugin) || *value == entry {
                if matched {
                    return Err(invalid(
                        "duplicate AppenCode registrations; reconcile OpenCode plugins first",
                    ));
                }
                *value = entry.clone();
                matched = true;
            } else if value.as_str() == plugin.to_str()
                || value["package"].as_str() == plugin.to_str()
                || self.receipt.as_ref().is_some_and(|receipt| {
                    std::iter::once(&receipt.current)
                        .chain(receipt.previous.iter())
                        .any(|registration| {
                            value.as_str() == registration.plugin["package"].as_str()
                                || (!value["package"].is_null() && value["package"] == registration.plugin["package"])
                        })
                })
            {
                return Err(invalid(
                    "AppenCode plugin options differ from the installer-owned registration",
                ));
            }
        }
        if !matched {
            plugins.push(entry.clone());
        }
        self.settings["plugins"] = json!(plugins);
        if self.settings.get("commands").is_none() {
            self.settings["commands"] = json!({});
        }
        self.settings["commands"]["appa-guide"] = guide.clone();
        if self.settings.get("mcp").is_none() {
            self.settings["mcp"] = json!({});
        }
        if self.settings["mcp"].get("servers").is_none() {
            self.settings["mcp"]["servers"] = json!({});
        }
        self.settings["mcp"]["servers"]["appa"] = mcp.clone();
        Ok(Registration {
            plugin: entry,
            guide,
            mcp,
        })
    }

    fn write(&self) -> Result<(), InstallError> {
        if optional_bytes(&self.path)? != self.before {
            return Err(InstallError::Changed(self.path.clone()));
        }
        let mut bytes = serde_json::to_vec_pretty(&self.settings).map_err(invalid)?;
        bytes.push(b'\n');
        atomic_write(&self.path, &bytes)
    }
}

fn profile_lock(path: &Path) -> Result<File, InstallError> {
    let directory = path.parent().expect("settings have a directory");
    fs::create_dir_all(directory).map_err(|error| io("create OpenCode directory", directory, error))?;
    let path = directory.join(".appa-install.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(&path)
        .map_err(|error| io("open OpenCode profile lock", &path, error))?;
    if !file
        .metadata()
        .map_err(|error| io("inspect profile lock", &path, error))?
        .is_file()
    {
        return Err(invalid("OpenCode profile lock must be a regular file"));
    }
    file.try_lock().map_err(|_| InstallError::Busy(path))?;
    Ok(file)
}

pub(super) fn verify(installation: &Installation, selection: &Selection) -> Result<(), InstallError> {
    let root = installation.version_marketplace(selection).join("plugins/opencode");
    Profile::read(&installation.config)?.plan(&installation.config, &root, endpoint()?.url())?;
    Ok(())
}

pub(super) fn activate(installation: &Installation, selection: &Selection) -> Result<(), InstallError> {
    let native = native::NativeArtifacts::prepare(installation, selection.generation(), selection.platform)?;
    let root = installation.version_marketplace(selection).join("plugins/opencode");
    let path = settings_path()?;
    let _lock = profile_lock(&path)?;
    let mut profile = Profile::read(&installation.config)?;
    let endpoint = endpoint()?;
    let registration = profile.plan(&installation.config, &root, endpoint.url())?;
    let composed = crate::init::config::verify_config(&installation.config).map_err(invalid)?;
    let target = RuntimeTarget {
        url: endpoint.url().to_owned(),
        user_owned: false,
    };
    // An occupied port is not permission to replace another deployment. Only
    // this config's runtime is restarted when upgrading its selected binary.
    if endpoint_health(&endpoint).is_some() {
        let probe = crate::loopback_http::Endpoint::parse(endpoint.url()).map_err(invalid)?;
        let answer = crate::loopback_http::get(
            &probe,
            "/binary-fingerprint",
            &crate::loopback_http::Deadline::spanning(Duration::from_secs(2)),
        )
        .map_err(invalid)?;
        let body = String::from_utf8_lossy(&answer.body);
        if !answer.is_success()
            || body.split_once('\n').map(|(_, config)| config.trim_end()) != installation.config.to_str()
        {
            return Err(invalid(
                "OpenCode runtime port belongs to another deployment; stop it explicitly before installing",
            ));
        }
        if !matches!(
            endpoint_owner(native.binary(), &installation.config, &endpoint).map_err(invalid)?,
            EndpointOwner::Deployment { .. }
        ) {
            crate::runtime_start::stop(&target).map_err(invalid)?;
        }
    }
    let deployment = Deployment {
        config: installation.config.clone(),
        data_dir: installation.config.parent().expect("config parent").to_path_buf(),
        adapter: AdapterName::OpenCode,
    };
    let withheld = [
        "APPA_OPENCODE_SETUP_CONFIG",
        "APPA_OPENCODE_SETUP_BINARY",
        "APPA_OPENCODE_BATTERIES",
    ]
    .map(Into::into);
    ensure(&target, &deployment, native.binary(), &withheld).map_err(invalid)?;
    verify_runtime_deployment(native.binary(), &installation.config, &endpoint).map_err(invalid)?;
    reconcile_policy(&endpoint, &installation.config, &composed).map_err(invalid)?;
    // Write ownership before registration. Recovery accepts either complete
    // profile after a crash between these two atomic writes.
    let receipt_path = path.with_file_name(".appa-opencode.json");
    if profile
        .receipt
        .as_ref()
        .is_none_or(|receipt| receipt.current != registration)
    {
        let receipt = Receipt {
            config: installation.config.clone(),
            current: registration,
            previous: profile.receipt.as_ref().map(|receipt| receipt.current.clone()),
        };
        atomic_write(&receipt_path, &serde_json::to_vec(&receipt).map_err(invalid)?)?;
    }
    profile.write()?;
    Ok(())
}

pub(super) fn remove(config: &Path) -> Result<(), InstallError> {
    let path = settings_path()?;
    let _lock = profile_lock(&path)?;
    let mut profile = Profile::read(config)?;
    let Some(receipt) = &profile.receipt else {
        return Ok(());
    };
    if let Some(plugins) = profile.settings["plugins"].as_array() {
        let kept: Vec<_> = plugins
            .iter()
            .filter(|entry| !profile.owned(entry, |registration| &registration.plugin))
            .cloned()
            .collect();
        profile.settings["plugins"] = json!(kept);
    }
    for (pointer, key, value) in [
        ("/commands", "appa-guide", &receipt.current.guide),
        ("/mcp/servers", "appa", &receipt.current.mcp),
    ] {
        if let Some(object) = profile.settings.pointer_mut(pointer).and_then(Value::as_object_mut)
            && object.get(key) == Some(value)
        {
            object.remove(key);
        }
    }
    profile.write()?;
    fs::remove_file(path.with_file_name(".appa-opencode.json"))
        .map_err(|error| io("remove OpenCode receipt", &path, error))
}

#[derive(Args)]
pub struct Setup {
    /// Deployment policy; user-global by default.
    #[arg(long, env = "APPA_CONFIG")]
    config: Option<PathBuf>,
    /// Project whose merged tool inventory to inspect; defaults to the current directory.
    #[arg(long)]
    project: Option<PathBuf>,
}

pub fn setup(args: Setup) -> ExitCode {
    let result = (|| {
        let config = args.config.map(Ok).unwrap_or_else(config_path)?;
        let installation = Installation::open(&config)?;
        installation.recover_config()?;
        let selection = installation
            .selection()?
            .filter(|selection| selection.plugins.contains("opencode"))
            .ok_or_else(|| invalid("run `appa plugin install opencode` before setup"))?;
        let acquired = super::Acquired::retained(&installation, &selection, selection.requirements())?;
        let native = native::NativeArtifacts::prepare(&installation, selection.generation(), selection.platform)?;
        let binary = native.binary().to_path_buf();
        let config = installation.config.clone();
        let project = args.project.unwrap_or(std::env::current_dir().map_err(invalid)?);
        let marketplace = acquired.marketplace().to_path_buf();
        drop(installation);
        let runtime = tokio::runtime::Runtime::new().map_err(invalid)?;
        runtime.block_on(operator(&binary, &config, &project, &marketplace))
    })();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("appa opencode setup: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn operator(binary: &Path, config: &Path, project: &Path, marketplace: &Path) -> Result<u8, InstallError> {
    let temporary = tempfile::tempdir().map_err(invalid)?;
    let policy = temporary.path().join("appa.toml");
    fs::write(&policy, "[policy]\nversion = 2\n[[policy.tool]]\nname = 'host/opencode/appa_setup'\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n").map_err(invalid)?;
    // The runtime itself allocates the port and prints its actual endpoint.
    let mut command = tokio::process::Command::new(binary);
    command
        .args([
            "runtime",
            "--adapter",
            "opencode",
            "--listen",
            "127.0.0.1:0",
            "--config",
        ])
        .arg(&policy)
        .arg("--db")
        .arg(temporary.path().join("appa.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(File::create(temporary.path().join("runtime.log")).map_err(invalid)?)
        .kill_on_drop(true);
    let mut runtime = crate::child_process::spawn_async(&mut command).map_err(invalid)?;
    let result = async {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut output = BufReader::new(runtime.stdout.take().expect("setup runtime stdout is piped"));
        let mut url = String::new();
        tokio::time::timeout(Duration::from_secs(20), output.read_line(&mut url)).await.map_err(invalid)?.map_err(invalid)?;
        let url = url.trim();
        Endpoint::parse(url).map_err(invalid)?;
        let root = marketplace.join("plugins/opencode/guide");
        let guide = ["SKILL.md", "references/core.md", "references/opencode.md"].into_iter()
            .map(|path| fs::read_to_string(root.join(path)).map_err(invalid)).collect::<Result<Vec<_>, _>>()?.join("\n\n");
        let settings = json!({
            "plugins":[{"package":marketplace.join("plugins/opencode"), "options":{"runtimeURL":url}}],
            "commands":{"appa-guide":{"template":guide}},
            "mcp":{"servers":{"appa":{"type":"remote", "url":format!("{url}/mcp"), "oauth":false, "codemode":false}}}
        });
        eprintln!("Operator setup session: configuration access only. Review the proposal before approving. Close it before protected coding.");
        let mut command = tokio::process::Command::new("opencode");
        command.arg("--standalone").arg(project).args(["--prompt", "Run /appa-guide init. Inspect and propose; wait for my approval before publishing."])
            .env("OPENCODE_CONFIG_PROJECT_DISABLE", "")
            .env("OPENCODE_CONFIG_CONTENT", settings.to_string())
            .env("APPA_OPENCODE_SETUP_CONFIG", config).env("APPA_OPENCODE_SETUP_BINARY", binary)
            .env("APPA_OPENCODE_BATTERIES", marketplace.join("batteries"))
            .env("APPA_OPENCODE_RUNTIME_URL", url).kill_on_drop(true);
        let mut session = crate::child_process::spawn_async(&mut command).map_err(invalid)?;
        let status = tokio::select! {
            result = session.wait() => result.map_err(invalid)?,
            _ = interrupted() => { session.kill().await.map_err(invalid)?; session.wait().await.map_err(invalid)? }
        };
        Ok(status.code().unwrap_or(130) as u8)
    }.await;
    let _ = runtime.kill().await;
    let _ = runtime.wait().await;
    eprintln!(
        "Setup closed. Run `appa plugin install opencode` to reload the protected runtime, then start a fresh OpenCode session."
    );
    result
}

async fn interrupted() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
