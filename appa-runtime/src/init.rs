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

use crate::plugin_bundle::{
    self, Deployment, Endpoint, Population, PluginBundleError, PluginSource,
};

const MARKETPLACE: &str = "appa";
const PLUGIN: &str = "appa-runtime@appa";
const RECOVERY_PREFIX: &str = ".appa-init-recovery-";
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
    #[error("the runtime at {endpoint} is not this installed build: {message}")]
    RuntimeIdentity { endpoint: String, message: String },
    #[error(
        "a previous appa runtime (pid {pid}) is still executing {path}; stop it and rerun init"
    )]
    RuntimeSurvived { pid: i32, path: PathBuf },
    #[error(transparent)]
    PluginBundle(#[from] PluginBundleError),
    #[error("{operation}; restoring the previous Claude Code plugin also failed: {recovery}")]
    PluginRecovery {
        operation: Box<InitError>,
        recovery: Box<InitError>,
    },
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

/// Install the plugin belonging to this binary's own release into Claude Code,
/// together with this binary, as one bundle.
///
/// The sequence is ordered so that nothing outside a temporary file changes
/// until the plugin source has been resolved and verified, and so that the
/// endpoint is cleared before any mutation rather than after Claude has been
/// switched over.
pub fn claude_code(explicit_source: Option<&str>) -> Result<String, InitError> {
    let appa = env::current_exe().map_err(InitError::CurrentExecutable)?;
    let endpoint = Endpoint::resolve()?;

    // 1. Resolve and verify the source. Nothing outside a temp file has changed.
    let source = PluginSource::resolve(explicit_source)?;
    let paths = deployment_paths()?;
    let installations = installed_plugin_installations(&paths.claude_dir)?;
    let marketplaces = run_claude(["plugin", "marketplace", "list"])?;

    // 2. Directories, and the config that survives every upgrade.
    for directory in [&paths.install_dir, &paths.config_dir, &paths.data_dir] {
        fs::create_dir_all(directory).map_err(|source| InitError::WriteFile {
            path: directory.clone(),
            source,
        })?;
    }
    let deployed_appa = paths.data_dir.join("bin").join(appa_filename());
    fs::create_dir_all(deployed_appa.parent().expect("the deployed binary has a parent"))
        .map_err(|source| InitError::InstallRuntime {
            path: deployed_appa.clone(),
            source,
        })?;
    let config = paths.config_dir.join("appa.toml");
    let config_created = create_default_config(&config)?;

    // 3. Materialize the deployment, or validate and reuse an existing one.
    let cache_dir = paths.data_dir.join("cache").join("plugin");
    let archive = match &source {
        PluginSource::Explicit(_) => None,
        PluginSource::Release(digest) => Some(plugin_bundle::ensure_archive(
            *digest,
            env!("CARGO_PKG_VERSION"),
            &cache_dir,
            &plugin_bundle::release_base_url(),
        )?),
    };
    let population = match (&source, &archive) {
        (PluginSource::Explicit(path), _) => Population::Tree(path),
        (_, Some(archive)) => Population::Archive(archive),
        (PluginSource::Release(_), None) => unreachable!("a release source always resolves an archive"),
    };
    let deployment = plugin_bundle::materialize(
        population,
        &paths.data_dir.join("deployments"),
        &deployed_appa,
        &config,
        &paths.data_dir,
        &endpoint,
    )?;

    // 4. Clear the endpoint before anything is mutated. A verified runtime at a
    //    retired install path that will not stop aborts init here, rather than
    //    leaving a new plugin registered against an old runtime that a rerun
    //    cannot dislodge.
    clear_retired_runtime(&paths)?;

    // 5. Snapshot for recovery and disarm the launcher.
    let launcher_dir = appa.parent().unwrap_or(&paths.install_dir);
    let recovery = prepare_plugin_recovery(&installations, &paths.data_dir)?;
    if recovery.is_some() {
        install_disabled_clappa(launcher_dir)?;
    }

    // 6. The Claude switch and the binary, with the existing rollback.
    let replacement = replace_plugin(&deployment.root, &marketplaces, &installations)
        .and_then(|()| install_runtime(&appa, &deployed_appa));
    if let Err(operation) = replacement {
        if let Some(recovery) = recovery.as_ref()
            && let Err(recovery_error) = restore_plugin(recovery).and_then(|()| install_clappa(launcher_dir).map(drop))
        {
            return Err(InitError::PluginRecovery {
                operation: Box::new(operation),
                recovery: Box::new(recovery_error),
            });
        }
        return Err(operation);
    }

    remove_legacy_runtime(&appa, &paths)?;

    // 7. Launcher, statusline, runtime, and the fingerprint backstop.
    let plugin_root = installed_plugin_root(&paths.claude_dir)?;
    install_clappa(launcher_dir)?;
    install_statusline(&plugin_root, &paths, &endpoint)?;
    start_runtime(&plugin_root, &deployed_appa, &endpoint)?;
    cleanup_plugin_recoveries(&paths.data_dir);

    // 8. Anything left on PATH that this init did not deploy is named, never
    //    removed: it is the user's file to keep or drop.
    let stale_path_copy = stale_path_copy(&paths, &deployed_appa);

    Ok(render_receipt(
        &source_label(&source, &deployment),
        &config,
        config_created,
        stale_path_copy.as_deref(),
        std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none(),
    ))
}

fn source_label(source: &PluginSource, deployment: &Deployment) -> String {
    let origin = match source {
        PluginSource::Explicit(path) => format!("{} (development source)", friendly_path(path)),
        PluginSource::Release(_) => format!("appa {} release plugin", env!("CARGO_PKG_VERSION")),
    };
    format!("{origin} -> {}", friendly_path(&deployment.root))
}

/// A copy of `appa` at the retired install path, which earlier versions
/// deployed to and which may still shadow this build on PATH.
fn stale_path_copy(paths: &DeploymentPaths, deployed: &Path) -> Option<PathBuf> {
    let retired = paths.install_dir.join(appa_filename());
    (retired.is_file() && !same_file(&retired, deployed)).then_some(retired)
}

fn render_receipt(
    adapter: &str,
    config: &Path,
    config_created: bool,
    stale_path_copy: Option<&Path>,
    color: bool,
) -> String {
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
    let mut receipt = format!(
        "{title}\n\n  {} {adapter}\n  {} {PLUGIN}\n  {} healthy\n  {} {} ({})\n  {} clappa\n",
        label("Adapter"),
        label("Plugin"),
        label("Runtime"),
        label("Config"),
        friendly_path(config),
        if config_created { "created" } else { "kept" },
        label("Launcher"),
    );
    // A session loads its hooks at session start, and the hook wire carries no
    // version, so a session running across an upgrade keeps talking to the
    // runtime it started with.
    receipt.push_str("\nRestart any running `clappa` session to pick this up.\n");
    if let Some(stale) = stale_path_copy {
        receipt.push_str(&format!(
            "\nA previous appa remains at {}. It is not used any more and may shadow\nthis build on PATH; remove it when you are ready.\n",
            friendly_path(stale),
        ));
    }
    receipt.push_str("\nNext: run `clappa`, then `/appa-guide init`.\n");
    receipt
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
    env::var_os("HOME").map(PathBuf::from).or({
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
        // A Unix process can keep running after Cargo has unlinked its executable. Scan the
        // two exact retired install paths even when no directory entry remains, then remove any
        // file that is left. Windows keeps the file present while the process is running, so a
        // missing path cannot be a live legacy runtime.
        if cfg!(windows) && !target.exists() {
            continue;
        }
        stop_legacy_runtime_at(&target)?;
        if !target.exists() {
            continue;
        }
        #[cfg(unix)]
        fs::remove_file(&target).map_err(|source| InitError::InstallRuntime {
            path: target.clone(),
            source,
        })?;
        #[cfg(windows)]
        fs::remove_file(&target).map_err(|source| InitError::InstallRuntime { path: target, source })?;
    }
    Ok(())
}

#[cfg(unix)]
fn stop_legacy_runtime_at(target: &Path) -> Result<(), InitError> {
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return Ok(());
    };
    if !output.status.success() {
        // Process discovery is a migration convenience. Restricted environments may deny ps;
        // continue the install and let runtime identity verification reject a surviving daemon.
        return Ok(());
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && stopped.iter().any(|pid| unsafe { libc::kill(*pid, 0) == 0 }) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn stop_legacy_runtime_at(target: &Path) -> Result<(), InitError> {
    stop_windows_process_at(target, "appa-runtime")
}

/// Stop any runtime executing the retired install path before anything is
/// mutated.
///
/// The stop set is exact: `<install_dir>/appa`, the path an init with the
/// environment resolving as it does now would have deployed to. Managed
/// stopping and default-endpoint ownership are bounded to that. A runtime left
/// by an init run under a different `APPA_INSTALL_DIR` or `APPA_DATA_DIR`
/// executes a path this init never computes, so it is not in the stop set;
/// `verify_runtime_binary` reports it as a foreign responder and leaves it
/// running.
///
/// A verified target that survives termination aborts init, because the
/// fingerprint backstop runs after the Claude switch: proceeding would register
/// the new plugin against an old runtime, and a rerun would find the same
/// surviving process and do the same thing again.
fn clear_retired_runtime(paths: &DeploymentPaths) -> Result<(), InitError> {
    let retired = paths.install_dir.join(appa_filename());
    match stop_processes_executing(&retired)?.first() {
        Some(&pid) => Err(InitError::RuntimeSurvived { pid, path: retired }),
        None => Ok(()),
    }
}

/// Terminate every process whose executable *is* `target`, and return those
/// still alive afterwards.
///
/// Discovery starts from `ps`, whose `command` column is argv and therefore
/// spoofable, so every candidate is verified against the operating system's own
/// answer for that pid before it is signalled. A pid whose executable cannot be
/// read is skipped and reported, never killed on argv alone.
#[cfg(unix)]
fn stop_processes_executing(target: &Path) -> Result<Vec<i32>, InitError> {
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        // Restricted environments may deny ps. Continue, and let the fingerprint
        // check report whatever is answering.
        return Ok(Vec::new());
    }
    let Some(identity) = file_identity(target) else {
        return Ok(Vec::new());
    };

    // init itself commonly runs from the retired path -- that is what a user
    // typing `~/.local/bin/appa init claude-code` does -- and it is in the stop
    // set by every other measure, so it is excluded by pid.
    let own = std::process::id() as i32;
    let mut signalled = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((pid, _)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == own {
            continue;
        }
        match executable_of(pid) {
            Some(executable) if file_identity(&executable) == Some(identity) => {}
            // Not this executable, or unreadable: report and skip.
            _ => continue,
        }
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(InitError::InstallRuntime {
                    path: target.to_path_buf(),
                    source: error,
                });
            }
            continue;
        }
        signalled.push(pid);
    }

    if signalled.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        signalled.retain(|pid| unsafe { libc::kill(*pid, 0) == 0 });
        if signalled.is_empty() {
            return Ok(Vec::new());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    signalled.retain(|pid| unsafe { libc::kill(*pid, 0) == 0 });
    Ok(signalled)
}

/// OS file identity, so a runtime launched through a symlinked install path is
/// not wrongly excluded. On Unix `(dev, ino)` is exact, and it identifies hard
/// links to one file as that file.
#[cfg(unix)]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

/// The executable a pid is actually running, from the operating system rather
/// than from its own argv.
#[cfg(target_os = "linux")]
fn executable_of(pid: i32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn executable_of(pid: i32) -> Option<PathBuf> {
    // PROC_PIDPATHINFO_MAXSIZE
    const MAX: usize = 4 * libc::PATH_MAX as usize;

    let mut buffer = vec![0u8; MAX];
    let written =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if written <= 0 {
        return None;
    }
    buffer.truncate(written as usize);
    Some(PathBuf::from(String::from_utf8(buffer).ok()?))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn executable_of(_pid: i32) -> Option<PathBuf> {
    // No portable primitive here: report and skip rather than kill on argv.
    None
}

#[cfg(windows)]
fn stop_processes_executing(target: &Path) -> Result<Vec<i32>, InitError> {
    stop_windows_process_at(target, "appa")
}

/// Whether two paths name the same file, resolving symlinks.
fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        match (file_identity(left), file_identity(right)) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
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
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "$target = $env:APPA_RUNTIME_REPLACE_TARGET; $name = $env:APPA_RUNTIME_REPLACE_NAME; Get-Process -Name $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target } | Stop-Process -Force -ErrorAction SilentlyContinue; exit 0",
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
    // Process discovery is a migration convenience. Continue the install and let
    // runtime identity verification reject a surviving daemon.
    Ok(())
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

#[derive(Clone)]
struct PluginInstallation {
    scope: String,
    project_path: Option<PathBuf>,
    install_path: Option<PathBuf>,
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
    entries
        .iter()
        .filter_map(|entry| {
            let scope = entry.get("scope")?.as_str()?.to_owned();
            let project_path = entry.get("projectPath").and_then(Value::as_str).map(PathBuf::from);
            let install_path = entry.get("installPath").and_then(Value::as_str).map(PathBuf::from);
            Some(PluginInstallation {
                scope,
                project_path,
                install_path,
            })
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
        .collect::<Result<Vec<_>, _>>()
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

struct PluginRecovery {
    marketplace: PathBuf,
    installations: Vec<PluginInstallation>,
}

fn replace_plugin(
    deployment: &Path,
    marketplaces: &Output,
    installations: &[PluginInstallation],
) -> Result<(), InitError> {
    for installation in installations {
        run_claude_in(
            ["plugin", "uninstall", PLUGIN, "--scope", &installation.scope, "--yes"],
            installation.project_path.as_deref(),
        )?;
    }
    if output_text(marketplaces).lines().any(is_appa_marketplace_line) {
        run_claude(["plugin", "marketplace", "remove", MARKETPLACE])?;
    }
    run_claude_os([
        OsStr::new("plugin"),
        OsStr::new("marketplace"),
        OsStr::new("add"),
        deployment.as_os_str(),
    ])?;
    run_claude(["plugin", "install", PLUGIN, "--scope", "user"])?;
    Ok(())
}

fn prepare_plugin_recovery(
    installations: &[PluginInstallation],
    data_dir: &Path,
) -> Result<Option<PluginRecovery>, InitError> {
    if installations.is_empty() {
        return Ok(None);
    }
    let source = installations
        .iter()
        .filter_map(|installation| installation.install_path.as_deref())
        .find(|path| path.is_dir())
        .ok_or(InitError::MissingPlugin)?;
    let marketplace = data_dir.join(format!("{RECOVERY_PREFIX}{}", std::process::id()));
    fs::create_dir_all(marketplace.join(".claude-plugin")).map_err(|source| InitError::WriteFile {
        path: marketplace.clone(),
        source,
    })?;
    copy_directory(source, &marketplace.join("plugin"))?;
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "name": MARKETPLACE,
        "description": "Temporary rollback source created by appa init.",
        "owner": { "name": "Archestra" },
        "plugins": [{ "name": "appa-runtime", "source": "./plugin" }]
    }))
    .expect("the recovery marketplace is valid JSON");
    let manifest_path = marketplace_manifest(&marketplace);
    fs::write(&manifest_path, manifest).map_err(|source| InitError::WriteFile {
        path: manifest_path,
        source,
    })?;
    Ok(Some(PluginRecovery {
        marketplace,
        installations: installations.to_vec(),
    }))
}

fn copy_directory(source_path: &Path, target: &Path) -> Result<(), InitError> {
    fs::create_dir_all(target).map_err(|source| InitError::WriteFile {
        path: target.to_path_buf(),
        source,
    })?;
    for entry in fs::read_dir(source_path).map_err(|source| InitError::WriteFile {
        path: source_path.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| InitError::WriteFile {
            path: source_path.to_path_buf(),
            source,
        })?;
        let destination = target.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|source| InitError::WriteFile {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            copy_directory(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), &destination).map_err(|source| InitError::WriteFile {
                path: destination,
                source,
            })?;
        }
    }
    Ok(())
}

fn restore_plugin(recovery: &PluginRecovery) -> Result<(), InitError> {
    for installation in &recovery.installations {
        let _ = run_claude_in(
            ["plugin", "uninstall", PLUGIN, "--scope", &installation.scope, "--yes"],
            installation.project_path.as_deref(),
        );
    }
    let _ = run_claude(["plugin", "marketplace", "remove", MARKETPLACE]);
    run_claude_os([
        OsStr::new("plugin"),
        OsStr::new("marketplace"),
        OsStr::new("add"),
        recovery.marketplace.as_os_str(),
    ])?;
    for installation in &recovery.installations {
        run_claude_in(
            ["plugin", "install", PLUGIN, "--scope", &installation.scope],
            installation.project_path.as_deref(),
        )?;
    }
    Ok(())
}

fn cleanup_plugin_recoveries(data_dir: &Path) {
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(RECOVERY_PREFIX) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
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

fn install_disabled_clappa(install_dir: &Path) -> Result<(), InitError> {
    #[cfg(windows)]
    let (path, contents) = (
        install_dir.join("clappa.cmd"),
        "@echo off\r\necho appa init did not complete; rerun appa init claude-code 1>&2\r\nexit /b 1\r\n",
    );
    #[cfg(not(windows))]
    let (path, contents) = (
        install_dir.join("clappa"),
        "#!/bin/sh\nprintf 'appa init did not complete; rerun appa init claude-code\\n' >&2\nexit 1\n",
    );
    fs::write(&path, contents).map_err(|source| InitError::WriteFile {
        path: path.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|source| InitError::WriteFile { path, source })?;
    }
    Ok(())
}

fn install_statusline(
    plugin_root: &Path,
    paths: &DeploymentPaths,
    endpoint: &Endpoint,
) -> Result<(), InitError> {
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

    // The copy lives outside the deployment tree, so the endpoint is rendered
    // into it here rather than during materialization.
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
    // The copy lands outside the deployment tree, so materialization never sees
    // it and the endpoint is rendered into it here.
    plugin_bundle::render_endpoint_in_file(&target, endpoint)?;
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

fn start_runtime(plugin_root: &Path, runtime: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
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
    // Every path and the endpoint reach the starter through the appa-paths file
    // rendered into the deployment beside it. APPA_RUNTIME_URL is removed rather
    // than set: to a starter it means "the user runs their own runtime here",
    // and setting it would suppress managed replacement permanently.
    let output = command
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .map_err(|error| InitError::Starter(error.to_string()))?;
    if output.status.success() {
        return verify_runtime_binary(runtime, endpoint);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(InitError::Starter(if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }))
}

fn verify_runtime_binary(runtime: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
    let expected = Sha256::digest(fs::read(runtime).map_err(|source| InitError::InstallRuntime {
        path: runtime.to_path_buf(),
        source,
    })?);
    let expected: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "2"])
        .arg(endpoint.join("/binary-fingerprint"))
        .output()
        .map_err(|error| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "the answering process exposes no binary fingerprint; stop it and rerun init".to_owned(),
        });
    }
    let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if actual == expected {
        return Ok(());
    }
    // Managed stopping covers only the paths this environment resolves to. A
    // runtime left by an init run under a different APPA_INSTALL_DIR or
    // APPA_DATA_DIR is a foreign responder: named, never killed.
    Err(InitError::RuntimeIdentity {
        endpoint: endpoint.url().to_owned(),
        message: "a different appa build is answering; stop that process and rerun init".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in the stop set can actually find and signal.
    ///
    /// macOS kills a copied platform binary outright -- a copy of `/bin/sh` or
    /// `/bin/sleep` dies with SIGKILL before it runs a single instruction -- so
    /// a test built on one would pass without ever exercising the stop set,
    /// because a killed process is also a stopped one. `perl` copies and runs,
    /// and can be told to ignore SIGTERM, which is the case that must abort
    /// init rather than be assumed gone.
    #[cfg(unix)]
    const STAND_IN: &str = "/usr/bin/perl";

    /// A process whose executable really *is* `at`, so verification finds it
    /// there rather than taking a spoofable argv on trust.
    ///
    /// The stand-in is reaped on its own thread. A dead child that nobody has
    /// waited for is a zombie, and `kill(pid, 0)` still succeeds on one, so
    /// without the reaper these tests could not tell a stopped process from a
    /// running one. In production the retired runtime is never init's child.
    #[cfg(unix)]
    fn process_executing(at: &Path, ignores_sigterm: bool) -> Option<i32> {
        use std::os::unix::fs::PermissionsExt;

        if !Path::new(STAND_IN).is_file() {
            return None;
        }
        fs::create_dir_all(at.parent().expect("a parent")).expect("the directory exists");
        fs::copy(STAND_IN, at).expect("the stand-in executable is copied");
        fs::set_permissions(at, fs::Permissions::from_mode(0o755)).expect("the stand-in is executable");

        // The stand-in announces itself by creating a file. Waiting on a signal
        // it sends is the only reliable liveness proof here: a copied binary the
        // platform refuses to run leaves a zombie that `kill(pid, 0)` still
        // reports as alive, so checking the pid would accept a process that
        // never ran.
        let ready = at.with_extension("ready");
        let disposition = if ignores_sigterm {
            "$SIG{TERM} = 'IGNORE'; "
        } else {
            ""
        };
        let script = format!(
            "{disposition}open(my $f, '>', $ARGV[0]) or die; close $f; sleep 30"
        );
        let mut child = Command::new(at)
            .args(["-e", &script])
            .arg(&ready)
            .spawn()
            .expect("the stand-in process starts");
        let pid = child.id() as i32;
        std::thread::spawn(move || child.wait());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if ready.is_file() {
                return Some(pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // This platform will not run the stand-in at all. Skip rather than
        // report a pass that exercised nothing.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        None
    }

    /// Whether a pid is still running, once its reaper has had a moment.
    #[cfg(unix)]
    fn still_running(pid: i32) -> bool {
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_at_the_retired_path_is_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let retired = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&retired, false) else {
            return;
        };

        let survivors = stop_processes_executing(&retired).expect("the stop set runs");

        assert!(survivors.is_empty(), "a stoppable runtime was reported as surviving");
        assert!(
            !still_running(pid),
            "the runtime at the retired path is still running",
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_at_another_path_is_left_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // What an init run under a different APPA_INSTALL_DIR or APPA_DATA_DIR
        // leaves behind: a path this environment never computes.
        let elsewhere = directory.path().join("other-install/appa");
        let Some(pid) = process_executing(&elsewhere, false) else {
            return;
        };
        let retired = directory.path().join("bin/appa");
        fs::create_dir_all(retired.parent().expect("a parent")).expect("the retired directory");
        fs::copy(STAND_IN, &retired).expect("the retired binary exists but runs nothing");

        let survivors = stop_processes_executing(&retired).expect("the stop set runs");

        assert!(survivors.is_empty());
        assert!(
            still_running(pid),
            "a runtime outside the stop set must be left running, not killed",
        );
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    #[cfg(unix)]
    #[test]
    fn a_verified_runtime_that_refuses_to_die_is_reported() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let retired = directory.path().join("bin/appa");
        // Ignores SIGTERM, which is exactly the case that must abort init
        // rather than let a new plugin bind to an old runtime.
        let Some(pid) = process_executing(&retired, true) else {
            return;
        };

        let survivors = stop_processes_executing(&retired).expect("the stop set runs");

        assert_eq!(
            survivors,
            vec![pid],
            "a verified target that outlived SIGTERM must be reported, not assumed gone",
        );
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_stops_an_unlinked_legacy_runtime() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("appa-runtime");
        let Some(pid) = process_executing(&target, false) else {
            return;
        };
        fs::remove_file(target.with_extension("ready")).expect("the readiness marker is removed");
        fs::remove_file(&target).expect("Cargo unlinks the installed legacy executable");

        stop_legacy_runtime_at(&target).expect("legacy cleanup succeeds");

        assert!(!still_running(pid), "cleanup must stop the unlinked process");
    }

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
        let receipt = render_receipt("current checkout", &config, false, None, false);

        assert!(receipt.starts_with("OpenAPPA initialized for Claude Code\n\n"));
        assert!(receipt.contains("Adapter   current checkout"));
        assert!(receipt.contains("Runtime   healthy"));
        assert!(receipt.contains("Launcher  clappa"));
        assert!(receipt.contains("Next: run `clappa`, then `/appa-guide init`."));
        assert!(!receipt.contains("Statusline"));
        assert!(!receipt.contains("appa-runtime (healthy)"));
        assert!(!receipt.contains("\u{1b}["));

        let colored = render_receipt("current checkout", &config, false, None, true);
        assert!(colored.starts_with("\u{1b}[1;32m✓ OpenAPPA initialized\u{1b}[0m"));
    }
}
