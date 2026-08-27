//! Native deployment bootstrap. The CLI installs machine state; harness skills only author policy.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MARKETPLACE: &str = "appa";
const PLUGIN: &str = "appa-runtime@appa";
const REMOTE_MARKETPLACE: &str = "archestra-ai/OpenAPPA";
const DEFAULT_CONFIG: &str = include_str!("../../integrations/claude-code/examples/claude-code.appa.toml");

#[derive(Debug, Error)]
pub enum InitError {
    #[error("cannot find the current executable: {0}")]
    CurrentExecutable(std::io::Error),
    #[error("cannot find a home directory; set HOME or the relevant APPA directory variables")]
    MissingHome,
    #[error("the `claude` command is unavailable: {0}")]
    ClaudeUnavailable(std::io::Error),
    #[error("`claude {command}` failed: {message}")]
    ClaudeCommand { command: String, message: String },
    #[error("the local Claude Code marketplace at {0} is incomplete")]
    InvalidMarketplace(PathBuf),
    #[error("cannot install the runtime at {path}: {source}")]
    InstallRuntime { path: PathBuf, source: std::io::Error },
    #[error("cannot initialize {path}: {source}")]
    WriteFile { path: PathBuf, source: std::io::Error },
    #[error("Claude's plugin registry at {path} is invalid: {message}")]
    PluginRegistry { path: PathBuf, message: String },
    #[error("Claude installed {PLUGIN}, but its installed plugin directory is unavailable")]
    MissingPlugin,
    #[error("Claude reports {count} installed copies of {PLUGIN}; initialization requires exactly one user copy")]
    PluginMultiplicity { count: usize },
    #[error(
        "Claude reports {PLUGIN} in {scope} scope for missing project {path}; remove that stale plugin entry first"
    )]
    MissingPluginProject { scope: String, path: PathBuf },
    #[error("the installed Claude plugin is missing {0}")]
    MissingPluginFile(PathBuf),
    #[error("the installed Claude plugin could not start `appa runtime`: {0}")]
    Starter(String),
    #[error("the runtime at 127.0.0.1:8787 is not this installed build: {0}")]
    RuntimeIdentity(String),
}

#[derive(Clone)]
enum MarketplaceSource {
    Local(PathBuf),
    Remote(String),
}

impl MarketplaceSource {
    fn command_argument(&self) -> &OsStr {
        match self {
            Self::Local(path) => path.as_os_str(),
            Self::Remote(source) => source.as_ref(),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Local(path) if env::current_dir().is_ok_and(|current| current.starts_with(path)) => {
                "current checkout".to_owned()
            }
            Self::Local(path) => friendly_path(path),
            Self::Remote(source) => source.clone(),
        }
    }
}

struct DeploymentPaths {
    install_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    claude_dir: PathBuf,
}

/// The platform config file used by installed deployments and `appa describe`.
pub fn installed_config_path() -> PathBuf {
    installed_config_dir().map_or_else(|| PathBuf::from("appa.toml"), |dir| dir.join("appa.toml"))
}

/// Install this build and this checkout's adapter into Claude Code.
pub fn claude_code(explicit_source: Option<&str>) -> Result<String, InitError> {
    let appa = env::current_exe().map_err(InitError::CurrentExecutable)?;
    let source = resolve_marketplace_source(explicit_source, &appa)?;
    let paths = deployment_paths()?;
    let installations = installed_plugin_installations(&paths.claude_dir)?;
    let marketplaces = run_claude(["plugin", "marketplace", "list"])?;

    remove_legacy_runtime(&appa, &paths)?;
    fs::create_dir_all(&paths.install_dir).map_err(|source| InitError::InstallRuntime {
        path: paths.install_dir.clone(),
        source,
    })?;
    let deployed_appa = paths.install_dir.join(appa_filename());
    install_runtime(&appa, &deployed_appa)?;

    fs::create_dir_all(&paths.config_dir).map_err(|source| InitError::WriteFile {
        path: paths.config_dir.clone(),
        source,
    })?;
    fs::create_dir_all(&paths.data_dir).map_err(|source| InitError::WriteFile {
        path: paths.data_dir.clone(),
        source,
    })?;
    let config = paths.config_dir.join("appa.toml");
    let config_created = create_default_config(&config)?;

    for installation in installations {
        run_claude_in(
            ["plugin", "uninstall", PLUGIN, "--scope", &installation.scope, "--yes"],
            installation.project_path.as_deref(),
        )?;
    }

    if output_text(&marketplaces).lines().any(is_appa_marketplace_line) {
        run_claude(["plugin", "marketplace", "remove", MARKETPLACE])?;
    }

    run_claude_os([
        OsStr::new("plugin"),
        OsStr::new("marketplace"),
        OsStr::new("add"),
        source.command_argument(),
    ])?;
    run_claude(["plugin", "install", PLUGIN, "--scope", "user"])?;

    let plugin_root = installed_plugin_root(&paths.claude_dir)?;
    activate_platform_hooks(&plugin_root)?;
    let launcher_dir = appa.parent().unwrap_or(&paths.install_dir);
    install_clappa(launcher_dir)?;
    install_statusline(&plugin_root, &paths)?;
    start_runtime(&plugin_root, &paths, &deployed_appa)?;

    Ok(render_receipt(
        &source.label(),
        &config,
        config_created,
        std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none(),
    ))
}

fn render_receipt(adapter: &str, config: &Path, config_created: bool, color: bool) -> String {
    let title = if color {
        "\u{1b}[1;32m✓ OpenAPPA initialized\u{1b}[0m \u{1b}[2mfor Claude Code\u{1b}[0m"
    } else {
        "OpenAPPA initialized for Claude Code"
    };
    let label = |name: &str| {
        if color {
            format!("\u{1b}[1;36m{name:<9}\u{1b}[0m")
        } else {
            format!("{name:<9}")
        }
    };
    format!(
        "{title}\n\n  {} {adapter}\n  {} {PLUGIN}\n  {} healthy\n  {} {} ({})\n  {} clappa\n\nNext: run `clappa`, then `/appa-guide init`.\n",
        label("Adapter"),
        label("Plugin"),
        label("Runtime"),
        label("Config"),
        friendly_path(config),
        if config_created { "created" } else { "kept" },
        label("Launcher"),
    )
}

fn friendly_path(path: &Path) -> String {
    user_home().and_then(|home| path.strip_prefix(home).ok()).map_or_else(
        || path.display().to_string(),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            }
        },
    )
}

fn resolve_marketplace_source(explicit: Option<&str>, appa: &Path) -> Result<MarketplaceSource, InitError> {
    if let Some(source) = explicit {
        let path = PathBuf::from(source);
        if path.exists() {
            return local_marketplace(path);
        }
        return Ok(MarketplaceSource::Remote(source.to_owned()));
    }

    if let Ok(current) = env::current_dir() {
        let mut fallback = None;
        for ancestor in current.ancestors() {
            for candidate in [ancestor.to_path_buf(), ancestor.join("integrations/claude-code")] {
                if marketplace_manifest(&candidate).is_file() {
                    if candidate.join("batteries").is_dir()
                        && candidate.join("website/content/docs/contracts.md").is_file()
                    {
                        return local_marketplace(candidate);
                    }
                    fallback.get_or_insert(candidate);
                }
            }
        }
        if let Some(candidate) = fallback {
            return local_marketplace(candidate);
        }
    }

    if let Some(parent) = appa.parent() {
        let packaged = parent.join("claude-code");
        if marketplace_manifest(&packaged).is_file() {
            return local_marketplace(packaged);
        }
    }

    Ok(MarketplaceSource::Remote(REMOTE_MARKETPLACE.to_owned()))
}

fn local_marketplace(path: PathBuf) -> Result<MarketplaceSource, InitError> {
    let path = path.canonicalize().unwrap_or(path);
    let manifest = marketplace_manifest(&path);
    let marketplace = fs::read(&manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let plugin_source = marketplace
        .filter(|value| value.get("name").and_then(Value::as_str) == Some(MARKETPLACE))
        .and_then(|value| {
            value.get("plugins")?.as_array()?.iter().find_map(|plugin| {
                (plugin.get("name").and_then(Value::as_str) == Some("appa-runtime"))
                    .then(|| plugin.get("source").and_then(Value::as_str))
                    .flatten()
                    .map(str::to_owned)
            })
        })
        .map(|source| path.join(source.trim_start_matches("./")));
    let Some(plugin_source) = plugin_source else {
        return Err(InitError::InvalidMarketplace(path));
    };
    if !plugin_source.join(".claude-plugin/plugin.json").is_file() || !plugin_source.join("hooks/hooks.json").is_file()
    {
        return Err(InitError::InvalidMarketplace(path));
    }
    Ok(MarketplaceSource::Local(path))
}

fn marketplace_manifest(path: &Path) -> PathBuf {
    path.join(".claude-plugin/marketplace.json")
}

fn deployment_paths() -> Result<DeploymentPaths, InitError> {
    let home = user_home();
    let config_dir = installed_config_dir().ok_or(InitError::MissingHome)?;
    let data_dir = installed_data_dir().ok_or(InitError::MissingHome)?;
    let install_dir = if let Some(path) = env::var_os("APPA_INSTALL_DIR") {
        PathBuf::from(path)
    } else if cfg!(windows) {
        data_dir.join("bin")
    } else {
        home.as_ref().ok_or(InitError::MissingHome)?.join(".local/bin")
    };
    let claude_dir = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".claude")))
        .ok_or(InitError::MissingHome)?;
    Ok(DeploymentPaths {
        install_dir,
        config_dir,
        data_dir,
        claude_dir,
    })
}

fn user_home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).or_else(|| {
        #[cfg(windows)]
        {
            env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(windows))]
        {
            None
        }
    })
}

fn installed_config_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os("APPA_CONFIG_DIR") {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa"));
    #[cfg(target_os = "windows")]
    return env::var_os("APPDATA").map(PathBuf::from).map(|path| path.join("appa"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config/appa"))
        })
}

fn installed_data_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os("APPA_DATA_DIR") {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa"));
    #[cfg(target_os = "windows")]
    return env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("appa"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share/appa"))
        })
}

fn appa_filename() -> &'static str {
    if cfg!(windows) { "appa.exe" } else { "appa" }
}

fn legacy_runtime_filename() -> &'static str {
    if cfg!(windows) {
        "appa-runtime.exe"
    } else {
        "appa-runtime"
    }
}

fn remove_legacy_runtime(appa: &Path, paths: &DeploymentPaths) -> Result<(), InitError> {
    let mut targets = vec![paths.install_dir.join(legacy_runtime_filename())];
    if let Some(parent) = appa.parent() {
        let sibling = parent.join(legacy_runtime_filename());
        if sibling != targets[0] {
            targets.push(sibling);
        }
    }
    for target in targets {
        if !target.exists() {
            continue;
        }
        #[cfg(unix)]
        fs::remove_file(&target).map_err(|source| InitError::InstallRuntime {
            path: target.clone(),
            source,
        })?;
        stop_legacy_runtime_at(&target)?;
        #[cfg(windows)]
        fs::remove_file(&target).map_err(|source| InitError::InstallRuntime { path: target, source })?;
    }
    Ok(())
}

#[cfg(unix)]
fn stop_legacy_runtime_at(target: &Path) -> Result<(), InitError> {
    let health = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "1", "http://127.0.0.1:8787/health"])
        .output();
    let Ok(health) = health else {
        return Ok(());
    };
    if !health.status.success() || !String::from_utf8_lossy(&health.stdout).trim().starts_with("stale ") {
        return Ok(());
    }
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .map_err(|source| InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(InitError::InstallRuntime {
            path: target.to_path_buf(),
            source: std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        });
    }
    let written = target.to_string_lossy();
    let mut stopped = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let command = command.trim_start();
        let is_target =
            |path: &str| command == path || command.strip_prefix(path).is_some_and(|rest| rest.starts_with(' '));
        if !is_target(&written) {
            continue;
        }
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        // The exact executable path is one of APPA's two retired install locations.
        // A process owned by another user cannot be signalled by this process.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(InitError::InstallRuntime {
                    path: target.to_path_buf(),
                    source: error,
                });
            }
        }
        stopped.push(pid);
    }
    if !stopped.is_empty() {
        let address = "127.0.0.1:8787"
            .parse()
            .expect("the installed runtime address is valid");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(100)).is_ok()
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn stop_legacy_runtime_at(target: &Path) -> Result<(), InitError> {
    stop_windows_process_at(target, "appa-runtime")
}

fn install_runtime(source: &Path, target: &Path) -> Result<(), InitError> {
    if source.canonicalize().ok() == target.canonicalize().ok() && target.exists() {
        return Ok(());
    }
    #[cfg(windows)]
    if target.exists() {
        stop_windows_process_at(target, "appa")?;
        fs::remove_file(target).map_err(|source| InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        })?;
    }
    let temporary = target.with_extension(format!("installing-{}", std::process::id()));
    fs::copy(source, &temporary).map_err(|source| InitError::InstallRuntime {
        path: target.to_path_buf(),
        source,
    })?;
    let permissions = fs::metadata(source)
        .and_then(|metadata| {
            let permissions = metadata.permissions();
            fs::set_permissions(&temporary, permissions)
        })
        .map_err(|source| InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        });
    if let Err(error) = permissions {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary, target) {
        let _ = fs::remove_file(&temporary);
        return Err(InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(windows)]
fn stop_windows_process_at(target: &Path, process_name: &str) -> Result<(), InitError> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "$target = $env:APPA_RUNTIME_REPLACE_TARGET; $name = $env:APPA_RUNTIME_REPLACE_NAME; Get-Process -Name $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target } | Stop-Process -Force",
        ])
        .env("APPA_RUNTIME_REPLACE_TARGET", target)
        .env("APPA_RUNTIME_REPLACE_NAME", process_name)
        .output()
        .map_err(|source| InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(InitError::InstallRuntime {
        path: target.to_path_buf(),
        source: std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    })
}

fn create_default_config(path: &Path) -> Result<bool, InitError> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(source) => {
            return Err(InitError::WriteFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if let Err(source) = file.write_all(DEFAULT_CONFIG.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(InitError::WriteFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(true)
}

fn run_claude<const N: usize>(arguments: [&str; N]) -> Result<Output, InitError> {
    run_claude_in(arguments, None)
}

fn run_claude_in<const N: usize>(arguments: [&str; N], directory: Option<&Path>) -> Result<Output, InitError> {
    run_claude_os_in(arguments.map(OsStr::new), directory)
}

fn run_claude_os<const N: usize>(arguments: [&OsStr; N]) -> Result<Output, InitError> {
    run_claude_os_in(arguments, None)
}

fn run_claude_os_in<const N: usize>(arguments: [&OsStr; N], directory: Option<&Path>) -> Result<Output, InitError> {
    let command = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let mut process = Command::new("claude");
    process.args(arguments);
    if let Some(directory) = directory {
        process.current_dir(directory);
    }
    let output = process.output().map_err(InitError::ClaudeUnavailable)?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Err(InitError::ClaudeCommand {
        command,
        message: if stderr.is_empty() { stdout } else { stderr },
    })
}

fn output_text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn is_appa_marketplace_line(line: &str) -> bool {
    line.trim()
        .strip_prefix('❯')
        .is_some_and(|name| name.trim() == MARKETPLACE)
}

struct PluginInstallation {
    scope: String,
    project_path: Option<PathBuf>,
}

fn installed_plugin_installations(claude_dir: &Path) -> Result<Vec<PluginInstallation>, InitError> {
    let registry = plugin_registry(claude_dir)?;
    let Some(entries) = registry
        .get("plugins")
        .and_then(|plugins| plugins.get(PLUGIN))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let scope = entry.get("scope")?.as_str()?.to_owned();
            let project_path = entry.get("projectPath").and_then(Value::as_str).map(PathBuf::from);
            Some(PluginInstallation { scope, project_path })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|installation| {
            if installation.project_path.as_ref().is_some_and(|path| !path.is_dir()) {
                return Err(InitError::MissingPluginProject {
                    scope: installation.scope,
                    path: installation.project_path.expect("the missing path was present"),
                });
            }
            Ok(installation)
        })
        .collect::<Result<Vec<_>, _>>()?)
}

fn installed_plugin_root(claude_dir: &Path) -> Result<PathBuf, InitError> {
    let registry = plugin_registry(claude_dir)?;
    let entries = registry
        .get("plugins")
        .and_then(|plugins| plugins.get(PLUGIN))
        .and_then(Value::as_array)
        .ok_or(InitError::MissingPlugin)?;
    if entries.len() != 1 {
        return Err(InitError::PluginMultiplicity { count: entries.len() });
    }
    entries
        .first()
        .filter(|entry| entry.get("scope").and_then(Value::as_str) == Some("user"))
        .and_then(|entry| entry.get("installPath"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .ok_or(InitError::MissingPlugin)
}

fn plugin_registry(claude_dir: &Path) -> Result<Value, InitError> {
    let path = claude_dir.join("plugins/installed_plugins.json");
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let bytes = fs::read(&path).map_err(|source| InitError::WriteFile {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| InitError::PluginRegistry {
        path,
        message: error.to_string(),
    })
}

fn install_clappa(install_dir: &Path) -> Result<PathBuf, InitError> {
    #[cfg(windows)]
    let (path, contents) = (
        install_dir.join("clappa.cmd"),
        "@echo off\r\nset APPA_GATE=1\r\nclaude %*\r\n",
    );
    #[cfg(not(windows))]
    let (path, contents) = (
        install_dir.join("clappa"),
        "#!/bin/sh\nexec env APPA_GATE=1 claude \"$@\"\n",
    );
    fs::write(&path, contents).map_err(|source| InitError::WriteFile {
        path: path.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|source| InitError::WriteFile {
            path: path.clone(),
            source,
        })?;
    }
    Ok(path)
}

#[cfg(not(windows))]
fn activate_platform_hooks(_plugin_root: &Path) -> Result<(), InitError> {
    Ok(())
}

#[cfg(windows)]
fn activate_platform_hooks(plugin_root: &Path) -> Result<(), InitError> {
    let source = plugin_root.join("hooks/hooks.windows.json");
    let target = plugin_root.join("hooks/hooks.json");
    if !source.is_file() {
        return Err(InitError::MissingPluginFile(source));
    }
    fs::copy(&source, &target).map_err(|source| InitError::WriteFile { path: target, source })?;
    Ok(())
}

fn install_statusline(plugin_root: &Path, paths: &DeploymentPaths) -> Result<(), InitError> {
    #[cfg(windows)]
    let (source, target) = (
        plugin_root.join("statusline.ps1"),
        paths.install_dir.join("appa-statusline.ps1"),
    );
    #[cfg(not(windows))]
    let (source, target) = (
        plugin_root.join("statusline.sh"),
        paths.install_dir.join("appa-statusline.sh"),
    );
    if !source.is_file() {
        return Err(InitError::MissingPluginFile(source));
    }

    let settings_path = paths.claude_dir.join("settings.json");
    let mut settings = if settings_path.exists() {
        let bytes = fs::read(&settings_path).map_err(|source| InitError::WriteFile {
            path: settings_path.clone(),
            source,
        })?;
        serde_json::from_slice::<Value>(&bytes).map_err(|error| InitError::PluginRegistry {
            path: settings_path.clone(),
            message: error.to_string(),
        })?
    } else {
        Value::Object(Map::new())
    };
    let existing = settings
        .get("statusLine")
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str);
    if existing.is_some_and(|command| !command.contains("appa-statusline")) {
        return Ok(());
    }

    fs::copy(&source, &target).map_err(|source| InitError::WriteFile {
        path: target.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).map_err(|source| InitError::WriteFile {
            path: target.clone(),
            source,
        })?;
    }

    let object = settings.as_object_mut().ok_or_else(|| InitError::PluginRegistry {
        path: settings_path.clone(),
        message: "the root must be an object".to_owned(),
    })?;
    #[cfg(windows)]
    let statusline_command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        target.display()
    );
    #[cfg(not(windows))]
    let statusline_command = target.to_string_lossy().into_owned();
    object.insert(
        "statusLine".to_owned(),
        serde_json::json!({"type": "command", "command": statusline_command}),
    );
    fs::create_dir_all(&paths.claude_dir).map_err(|source| InitError::WriteFile {
        path: paths.claude_dir.clone(),
        source,
    })?;
    let encoded = serde_json::to_vec_pretty(&settings).expect("JSON values always encode");
    fs::write(&settings_path, encoded).map_err(|source| InitError::WriteFile {
        path: settings_path,
        source,
    })?;
    Ok(())
}

fn start_runtime(plugin_root: &Path, paths: &DeploymentPaths, runtime: &Path) -> Result<(), InitError> {
    #[cfg(windows)]
    let mut command = {
        let starter = plugin_root.join("hooks/hook.ps1");
        if !starter.is_file() {
            return Err(InitError::MissingPluginFile(starter));
        }
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-File"]);
        command.arg(starter);
        command.arg("-EnsureRuntime");
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let starter = plugin_root.join("hooks/ensure-runtime.sh");
        if !starter.is_file() {
            return Err(InitError::MissingPluginFile(starter));
        }
        let mut command = Command::new("sh");
        command.arg(starter);
        command
    };
    let output = command
        .env("APPA_INSTALL_DIR", &paths.install_dir)
        .env("APPA_CONFIG_DIR", &paths.config_dir)
        .env("APPA_DATA_DIR", &paths.data_dir)
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .map_err(|error| InitError::Starter(error.to_string()))?;
    if output.status.success() {
        return verify_runtime_binary(runtime);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(InitError::Starter(if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }))
}

fn verify_runtime_binary(runtime: &Path) -> Result<(), InitError> {
    let expected = Sha256::digest(fs::read(runtime).map_err(|source| InitError::InstallRuntime {
        path: runtime.to_path_buf(),
        source,
    })?);
    let expected: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--max-time",
            "2",
            "http://127.0.0.1:8787/binary-fingerprint",
        ])
        .output()
        .map_err(|error| InitError::RuntimeIdentity(error.to_string()))?;
    if !output.status.success() {
        return Err(InitError::RuntimeIdentity(
            "the answering process exposes no binary fingerprint; stop it and rerun init".to_owned(),
        ));
    }
    let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if actual == expected {
        return Ok(());
    }
    Err(InitError::RuntimeIdentity(
        "a different appa build is answering; stop it and rerun init".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_line_matches_only_the_named_marketplace() {
        assert!(is_appa_marketplace_line("  ❯ appa"));
        assert!(!is_appa_marketplace_line("  ❯ appa-other"));
        assert!(!is_appa_marketplace_line("Source: GitHub (appa)"));
    }

    #[test]
    fn receipt_is_compact_and_hides_installation_details() {
        let config = user_home()
            .unwrap_or_else(|| PathBuf::from("/home/user"))
            .join("config/appa.toml");
        let receipt = render_receipt("current checkout", &config, false, false);

        assert!(receipt.starts_with("OpenAPPA initialized for Claude Code\n\n"));
        assert!(receipt.contains("Adapter   current checkout"));
        assert!(receipt.contains("Runtime   healthy"));
        assert!(receipt.contains("Launcher  clappa"));
        assert!(receipt.contains("Next: run `clappa`, then `/appa-guide init`."));
        assert!(!receipt.contains("Statusline"));
        assert!(!receipt.contains("appa-runtime (healthy)"));
        assert!(!receipt.contains("\u{1b}["));

        let colored = render_receipt("current checkout", &config, false, true);
        assert!(colored.starts_with("\u{1b}[1;32m✓ OpenAPPA initialized\u{1b}[0m"));
    }
}
