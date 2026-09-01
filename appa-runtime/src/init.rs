//! Native deployment bootstrap. The CLI installs machine state; harness skills only author policy.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::plugin_bundle::{self, Deployment, Endpoint, PluginBundleError, PluginSource, Population};
use crate::runtime_cli::{Adapter, refuse_unobservable_returns};

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
    #[error("the deployment config {path} does not load: {source}")]
    UnloadableConfig { path: PathBuf, source: Box<ConfigError> },
    #[error("the deployment config {path} cannot serve Claude Code: {reason}")]
    UnusableConfig { path: PathBuf, reason: String },
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
    #[error("a previous appa runtime (pid {pid}) is still executing {path}; stop it and rerun init")]
    RuntimeSurvived { pid: i32, path: PathBuf },
    #[error("the runtime at {endpoint} refused to serve {path}: {message}")]
    ReloadRefused {
        endpoint: String,
        path: PathBuf,
        message: String,
    },
    #[error(transparent)]
    PluginBundle(#[from] PluginBundleError),
    #[error("{operation}; restoring the previous Claude Code plugin also failed: {recovery}")]
    PluginRecovery {
        operation: Box<InitError>,
        recovery: Box<InitError>,
    },
}

/// What this init did to the config file, as the receipt reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigOutcome {
    /// Seeded from this build's default, because no config was there.
    Created,
    /// Left exactly as it was found.
    Kept,
    /// Replaced with this build's default, at the user's word, the previous
    /// file beside it.
    Rewritten,
}

impl ConfigOutcome {
    fn as_str(self) -> &'static str {
        match self {
            ConfigOutcome::Created => "created",
            ConfigOutcome::Kept => "kept",
            ConfigOutcome::Rewritten => "rewritten",
        }
    }
}

/// What this init did about the policy the running runtime serves.
///
/// The starter leaves an already-healthy runtime alone, and that process keeps serving the
/// policy it loaded at startup. A restart loads this file itself, so the keys agree and
/// there is nothing to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeOutcome {
    /// Serving the configuration this init validated, with nothing to reconcile.
    Healthy,
    /// Serving an older policy until it was reloaded, at the user's word.
    Reloaded,
    /// Still serving an older policy, because the user declined the reload.
    OlderPolicy,
}

impl RuntimeOutcome {
    fn as_str(self) -> &'static str {
        match self {
            RuntimeOutcome::Healthy => "healthy",
            RuntimeOutcome::Reloaded => "healthy (policy reloaded)",
            RuntimeOutcome::OlderPolicy => "healthy (serving an older policy)",
        }
    }
}

struct DeploymentPaths {
    install_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    claude_dir: PathBuf,
}

/// A directory override made absolute, without touching the filesystem.
///
/// Overrides are rendered into a deployment's hooks and hashed into its
/// identity, and a hook runs from whatever working directory Claude was
/// launched in. A relative override would therefore resolve somewhere else
/// entirely at hook time, so it is made absolute here, once, before any of that.
/// This is lexical: no `canonicalize`, no case folding, consistent with
/// refusing rather than normalizing elsewhere.
fn absolute_directory(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
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
    progress("resolving the matching plugin");
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
    fs::create_dir_all(deployed_appa.parent().expect("the deployed binary has a parent")).map_err(|source| {
        InitError::InstallRuntime {
            path: deployed_appa.clone(),
            source,
        }
    })?;
    let config = paths.config_dir.join("appa.toml");
    let config_outcome = match create_default_config(&config)? {
        true => ConfigOutcome::Created,
        false => offer_config_rewrite(&config)?,
    };
    let composed_policy = verify_config(&config)?;

    // 3. Materialize the deployment, or validate and reuse an existing one.
    progress("preparing the plugin bundle");
    let deployments = paths.data_dir.join("deployments");
    let deploy = |population| {
        plugin_bundle::materialize(
            population,
            &deployments,
            &deployed_appa,
            &config,
            &paths.data_dir,
            &endpoint,
        )
    };
    let deployment = match &source {
        PluginSource::Explicit(path) => deploy(Population::Tree(path))?,
        PluginSource::Release { reference, digest } => {
            let archive = plugin_bundle::ensure_archive(
                *digest,
                reference,
                env!("CARGO_PKG_VERSION"),
                &paths.data_dir.join("cache").join("plugin"),
                &plugin_bundle::release_base_url(),
            )?;
            deploy(Population::Archive(&archive))?
        }
        PluginSource::Commit { commit, digest } => {
            let archive = plugin_bundle::ensure_commit_archive(
                commit,
                *digest,
                &paths.data_dir.join("cache").join("plugin"),
                &plugin_bundle::source_archive_base_url(),
            )?;
            deploy(Population::Archive(&archive))?
        }
        PluginSource::Local { root, digest } => deploy(Population::Repository {
            root,
            expected: *digest,
        })?,
    };

    // 4. Clear the endpoint before anything is mutated. A verified runtime at a
    //    retired install path that will not stop aborts init here, rather than
    //    leaving a new plugin registered against an old runtime that a rerun
    //    cannot dislodge.
    progress("checking the runtime endpoint");
    clear_retired_runtime(&paths)?;
    //    A previous install may have been unlinked before init ran. Its process
    //    still owns the endpoint, and its authenticated health answer names the
    //    stale pid even though no pathname remains for the retired-path scan.
    clear_stale_endpoint(&endpoint)?;
    //    A healthy runtime from another build is stopped only after an explicit
    //    confirmation and only when it identifies a same-user appa pid.
    clear_foreign_endpoint(&appa, &endpoint)?;

    // 5. Snapshot for recovery and disarm the launcher.
    let launcher_dir = appa.parent().unwrap_or(&paths.install_dir);
    let recovery = prepare_plugin_recovery(&installations, &paths.data_dir)?;
    if recovery.is_some() {
        install_disabled_clappa(launcher_dir)?;
    }

    // 6. The Claude switch, the binary, and the runtime this plugin is being
    //    bound to: one transaction. Verification is inside it, because a plugin
    //    left registered against a runtime that failed verification is exactly
    //    the skew this bundle exists to prevent.
    progress("updating the Claude Code plugin");
    let switch = replace_plugin(&deployment.root, &marketplaces, &installations)
        .and_then(|()| install_runtime(&appa, &deployed_appa))
        .and_then(|()| remove_legacy_runtime(&appa, &paths))
        .and_then(|()| installed_plugin_root(&paths.claude_dir))
        .and_then(|plugin_root| {
            install_statusline(&plugin_root, &paths)?;
            progress("starting the runtime");
            start_runtime(&plugin_root, &deployed_appa, &endpoint)
        });
    if let Err(operation) = switch {
        if let Err(recovery_error) = undo_plugin_switch(recovery.as_ref(), launcher_dir) {
            return Err(InitError::PluginRecovery {
                operation: Box::new(operation),
                recovery: Box::new(recovery_error),
            });
        }
        return Err(operation);
    }

    // 7. Only now is the launcher armed. Every earlier return leaves `clappa`
    //    absent on a first install and disabled on an upgrade, so a session
    //    started against a half-installed bundle cannot be a protected one.
    install_clappa(launcher_dir)?;
    cleanup_plugin_recoveries(&paths.data_dir);

    // 8. The plugin and the launcher now point at this configuration; a runtime the
    //    starter left running may not. Asked last, because a decline still leaves a
    //    complete install — only the policy in memory lags.
    let runtime_outcome = reconcile_policy(&endpoint, &config, composed_policy)?;

    // 9. Anything left on PATH that this init did not deploy is named, never
    //    removed: it is the user's file to keep or drop.
    let stale_path_copy = stale_path_copy(&paths, &deployed_appa);

    Ok(render_receipt(
        &source_label(&source, &deployment),
        &config,
        config_outcome,
        runtime_outcome,
        stale_path_copy.as_deref(),
        std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none(),
    ))
}

fn progress(message: &str) {
    eprintln!("appa init: {message}...");
}

fn source_label(source: &PluginSource, deployment: &Deployment) -> String {
    let origin = match source {
        PluginSource::Explicit(path) => format!("{} (development source)", friendly_path(path)),
        PluginSource::Release { reference, .. } => format!("appa {reference} release plugin"),
        PluginSource::Commit { commit, .. } => format!("OpenAPPA commit {}", &commit[..commit.len().min(12)]),
        PluginSource::Local { root, .. } => format!("{} (dirty development source)", friendly_path(root)),
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
    config_outcome: ConfigOutcome,
    runtime_outcome: RuntimeOutcome,
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
        "{title}\n\n  {} {adapter}\n  {} {PLUGIN}\n  {} {}\n  {} {} ({})\n  {} clappa\n",
        label("Adapter"),
        label("Plugin"),
        label("Runtime"),
        runtime_outcome.as_str(),
        label("Config"),
        friendly_path(config),
        config_outcome.as_str(),
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
        absolute_directory(PathBuf::from(path))
    } else if cfg!(windows) {
        data_dir.join("bin")
    } else {
        home.as_ref().ok_or(InitError::MissingHome)?.join(".local/bin")
    };
    let claude_dir = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .map(absolute_directory)
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
        return Some(absolute_directory(PathBuf::from(path)));
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
        return Some(absolute_directory(PathBuf::from(path)));
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
    // Legacy cleanup removes the file next, and a surviving process surfaces
    // there as a failed removal.
    stop_windows_processes_at(target, "appa-runtime").map(drop)
}

/// Stop any runtime executing the retired install path before anything is
/// mutated.
///
/// The pathname stop set is exact: `<install_dir>/appa`, the path an init with
/// the environment resolving as it does now would have deployed to. A runtime
/// whose executable was already unlinked cannot match that path; the subsequent
/// endpoint check reclaims it only when `/health` explicitly answers
/// `stale <pid>` and the pid passes the starter's ownership/name check. A
/// healthy runtime from another install remains foreign and is never stopped.
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

/// The subcommand a managed runtime is started with, by every starter and by
/// init itself. It is what distinguishes a runtime from any other invocation of
/// the same binary.
#[cfg(unix)]
const RUNTIME_SUBCOMMAND: &str = "runtime";

/// Terminate every managed runtime whose executable *is* `target`, and return
/// those still alive afterwards.
///
/// Two conditions, and a candidate needs both. The executable must be the
/// retired path, verified against the operating system's own answer for that
/// pid, because `ps` reports argv and argv is spoofable. And argv must name the
/// `runtime` subcommand, because the retired binary is also what a concurrent
/// `appa init` or an in-flight `appa hook` is executing, and terminating those
/// would interrupt work that has nothing to do with the runtime being replaced.
/// Argv is only ever narrowing here: it can excuse a process from the stop set,
/// never admit one the executable check rejected.
///
/// Windows applies the executable condition alone. Reading another process's
/// command line there needs a CIM query rather than `Get-Process`, and the same
/// helper serves legacy cleanup, whose binary had no subcommand at all.
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
        let Some((pid, arguments)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == own {
            continue;
        }
        // A whole token, so a path that merely contains the word does not match.
        // The starter runs `<binary> runtime --listen <addr>`, and a binary path
        // carrying spaces splits into tokens that are all still not `runtime`.
        if !arguments.split_whitespace().any(|token| token == RUNTIME_SUBCOMMAND) {
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
    let written = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
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
    stop_windows_processes_at(target, "appa")
}

/// The comparison operand on Windows: the fully resolved path, folded for the
/// case-insensitive filesystem.
///
/// Both sides are canonicalized, rather than trusting `Get-Process.Path` to
/// return one particular form. `fs::canonicalize` yields the Win32 final path,
/// whose extended-length prefix is stripped so the two sides can be compared as
/// written. This is equality of resolved final paths, not file-ID identity, so
/// two hard links to one file at different paths compare unequal -- a gap named
/// in the deployment documentation rather than papered over.
#[cfg(windows)]
fn windows_identity(path: &Path) -> Option<String> {
    let canonical = fs::canonicalize(path).ok()?;
    let text = canonical.to_str()?;
    Some(text.strip_prefix(r"\\?\").unwrap_or(text).to_lowercase())
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
        stop_windows_processes_at(target, "appa")?;
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

/// Terminate every `process_name` process whose resolved executable is
/// `target`, and answer with those still alive afterwards.
///
/// PowerShell only enumerates and stops. The comparison happens here, so
/// Windows and Unix apply the same rule and neither swallows a discovery or
/// termination failure the way `-ErrorAction SilentlyContinue` did.
#[cfg(windows)]
fn stop_windows_processes_at(target: &Path, process_name: &str) -> Result<Vec<i32>, InitError> {
    let Some(identity) = windows_identity(target) else {
        // A path that will not resolve is reported and skipped, never killed.
        return Ok(Vec::new());
    };

    let listed = powershell(
        "Get-Process -Name $env:APPA_STOP_NAME -ErrorAction SilentlyContinue | \
         ForEach-Object { \"$($_.Id)`t$($_.Path)\" }",
        [("APPA_STOP_NAME", process_name.to_owned())],
    )?;

    let own = std::process::id() as i32;
    let mut targets = Vec::new();
    for line in listed.lines() {
        let Some((pid, path)) = line.trim_end().split_once('\t') else {
            continue;
        };
        let Ok(pid) = pid.trim().parse::<i32>() else {
            continue;
        };
        // init commonly runs from the retired path itself.
        if pid == own {
            continue;
        }
        // An empty or access-denied path is reported and skipped.
        if path.is_empty() {
            tracing::debug!(pid, "skipping a process whose executable path is unreadable");
            continue;
        }
        if windows_identity(Path::new(path)).as_deref() == Some(identity.as_str()) {
            targets.push(pid);
        }
    }
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let ids = targets.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
    let survivors = powershell(
        "$ids = $env:APPA_STOP_IDS -split ',' | ForEach-Object { [int]$_ }; \
         foreach ($id in $ids) { Stop-Process -Id $id -Force -ErrorAction Stop }; \
         $deadline = (Get-Date).AddSeconds(10); \
         while ((Get-Date) -lt $deadline) { \
           $alive = @($ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }); \
           if ($alive.Count -eq 0) { break }; \
           Start-Sleep -Milliseconds 200 \
         }; \
         $ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }",
        [("APPA_STOP_IDS", ids)],
    )?;

    Ok(survivors
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .collect())
}

/// Run one PowerShell command, surfacing its failure rather than exiting 0.
#[cfg(windows)]
fn powershell<const N: usize>(command: &str, environment: [(&str, String); N]) -> Result<String, InitError> {
    let mut process = Command::new("powershell.exe");
    process.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        command,
    ]);
    for (name, value) in environment {
        process.env(name, value);
    }
    let output = process
        .output()
        .map_err(|error| InitError::Starter(error.to_string()))?;
    if !output.status.success() {
        return Err(InitError::Starter(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

/// The policy version this build's default config declares.
fn template_policy_version() -> i64 {
    policy_version(DEFAULT_CONFIG).expect("the bundled default config declares an integer policy version")
}

/// The `[policy] version` of one config's own text, before any include composes.
fn policy_version(text: &str) -> Option<i64> {
    toml::from_str::<toml::Value>(text)
        .ok()?
        .get("policy")?
        .get("version")?
        .as_integer()
}

/// Offer to replace a config authored against an older policy model.
///
/// The config is the user's, and init keeps it across every upgrade. A policy
/// version below this build's is the one mechanical signal that it was authored
/// against a model this build no longer writes, so it is also the only drift
/// init asks about. Only a terminal is asked, and the answer defaults to no: a
/// rewrite discards every edit the file carries, the include lines that bind
/// batteries included, and keeps them only in the backup.
fn offer_config_rewrite(path: &Path) -> Result<ConfigOutcome, InitError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(ConfigOutcome::Kept);
    }
    let stderr = std::io::stderr();
    offer_config_rewrite_with(path, &mut stdin.lock(), &mut stderr.lock())
}

fn offer_config_rewrite_with(
    path: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<ConfigOutcome, InitError> {
    let template = template_policy_version();
    let text = fs::read_to_string(path).map_err(|source| InitError::WriteFile {
        path: path.to_path_buf(),
        source,
    })?;
    // A config whose version is missing or unreadable is not old, it is broken:
    // `verify_config` refuses it next, naming the fault it actually has.
    match policy_version(&text) {
        Some(found) if found < template => {}
        _ => return Ok(ConfigOutcome::Kept),
    }
    let backup = path.with_extension("toml.bak");
    if !confirm_rewrite(path, &backup, input, output)? {
        return Ok(ConfigOutcome::Kept);
    }
    // The original moves aside whole, so nothing here can leave a half-written
    // policy in place: the new file is written under `create_new` and removed
    // again if that write fails, and a failure puts the original back.
    fs::rename(path, &backup).map_err(|source| InitError::WriteFile {
        path: backup.clone(),
        source,
    })?;
    match create_default_config(path) {
        Ok(_) => Ok(ConfigOutcome::Rewritten),
        Err(written) => match fs::rename(&backup, path) {
            Ok(()) => Err(written),
            Err(source) => Err(InitError::WriteFile {
                path: path.to_path_buf(),
                source,
            }),
        },
    }
}

fn confirm_rewrite(
    path: &Path,
    backup: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, InitError> {
    let prompt = |source| InitError::WriteFile {
        path: path.to_path_buf(),
        source,
    };
    write!(
        output,
        "appa: {} was authored against an older policy model than this build writes.\n\
         Rewrite it from this build's default? Your file, its include lines and every edit\n\
         in it, is kept only at {}, replacing whatever is there. [y/N] ",
        friendly_path(path),
        friendly_path(backup),
    )
    .and_then(|()| output.flush())
    .map_err(prompt)?;
    let mut answer = String::new();
    if input.read_line(&mut answer).map_err(prompt)? == 0 {
        return Ok(false);
    }
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// The config the runtime will be started against, put through the runtime's
/// own startup refusals first.
///
/// A config kept across upgrades drifts: an included battery moves ahead of the
/// policy version an earlier init wrote, an include is edited to an absolute
/// path, a hand-edited `Agent` row stops pinning the argument that keeps a
/// subagent's return observable. The runtime refuses each of those at startup,
/// which init can report only as an endpoint that never became healthy. Running
/// both refusals here names the file and the fault, before anything outside
/// this file has changed.
/// Answers with the policy key this file composes to, or `None` when the file resolves
/// only where the runtime runs — the caller then has nothing to compare a serving runtime
/// against.
fn verify_config(path: &Path) -> Result<Option<String>, InitError> {
    let config = match Config::load(path) {
        Ok(config) => config,
        // A `token_env` resolves where the runtime runs, not here. A hook starts
        // it with the session's environment, which carries variables this
        // terminal does not, so a secret this process cannot see is not init's
        // to refuse: the start that follows is what proves the token reachable.
        Err(ConfigError::MissingSecret { .. }) => return Ok(None),
        Err(source) => {
            return Err(InitError::UnloadableConfig {
                path: path.to_path_buf(),
                source: Box::new(source),
            });
        }
    };
    refuse_unobservable_returns(Adapter::ClaudeCode, config.policy_file().value()).map_err(|reason| {
        InitError::UnusableConfig {
            path: path.to_path_buf(),
            reason,
        }
    })?;
    Ok(Some(crate::engine::policy_file_key(config.policy_file().bytes())))
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

/// Undo a switch that has already reached Claude.
///
/// With a snapshot the previous plugin is restored and its launcher re-armed.
/// Without one there was no APPA plugin before this init, so the new one is
/// removed outright rather than left pointing Claude at a runtime this init
/// could not verify. Both errors reach the caller, which reports them beside
/// the failure that caused the undo.
fn undo_plugin_switch(recovery: Option<&PluginRecovery>, launcher_dir: &Path) -> Result<(), InitError> {
    match recovery {
        Some(recovery) => restore_plugin(recovery).and_then(|()| install_clappa(launcher_dir).map(drop)),
        None => {
            run_claude(["plugin", "uninstall", PLUGIN, "--scope", "user", "--yes"])?;
            run_claude(["plugin", "marketplace", "remove", MARKETPLACE])?;
            Ok(())
        }
    }
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

/// Who is answering the endpoint, as far as one probe can establish.
///
/// A healthy runtime left by an init under a different `APPA_INSTALL_DIR` or
/// `APPA_DATA_DIR` is foreign: named, never killed. Stale runtimes are cleared
/// before this classification through their separate health protocol.
#[derive(Debug, PartialEq, Eq)]
enum EndpointOwner {
    /// Nothing answered, or what answered serves no fingerprint. Before the
    /// start this is the ordinary case; after it, it is a failure.
    Unidentified,
    /// The binary whose bytes were offered for comparison.
    Deployment,
    /// A different build. New runtimes name their pid; an older runtime may
    /// return only its digest and remains ineligible for automatic stopping.
    Foreign { pid: Option<i32> },
}

fn endpoint_health(endpoint: &Endpoint) -> Result<Option<String>, InitError> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "2"])
        .arg(endpoint.join("/health"))
        .output()
        .map_err(|error| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn positive_pid(pid: &str) -> Option<i32> {
    if pid.is_empty() || pid.starts_with('0') || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

fn stale_pid(answer: &str) -> Option<i32> {
    positive_pid(answer.strip_prefix("stale ")?)
}

/// Stop the exact stale APPA runtime named by the endpoint before classifying
/// any remaining responder as foreign.
///
/// This covers an unlinked Unix executable: pathname identity is unavailable,
/// but the runtime's own health protocol still names its pid. The pid is not
/// trusted by itself; init applies the same same-user/process-name check as the
/// shipped starter before sending a signal. An `ok`, malformed, or absent
/// health answer never grants shutdown authority.
fn clear_stale_endpoint(endpoint: &Endpoint) -> Result<(), InitError> {
    let Some(answer) = endpoint_health(endpoint)? else {
        return Ok(());
    };
    let Some(pid) = stale_pid(&answer) else {
        return Ok(());
    };
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!(
                "the endpoint names stale pid {pid}, but it is not this user's appa runtime; not stopping it"
            ),
        });
    }
    // Close the validation-to-signal race: the process must still be the one
    // answering with the same stale pid immediately before it is terminated.
    match endpoint_health(endpoint)? {
        None => return Ok(()),
        Some(ref current) if current == "ok" => return Ok(()),
        Some(ref current) if stale_pid(current) == Some(pid) => {}
        Some(_) => {
            return Err(InitError::RuntimeIdentity {
                endpoint: endpoint.url().to_owned(),
                message: format!("the endpoint changed ownership before stale pid {pid} could be stopped"),
            });
        }
    }
    terminate_appa_pid(pid)?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(ref current) if current == "ok" => return Ok(()),
            Some(ref current) if stale_pid(current) == Some(pid) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Some(_) => {
                return Err(InitError::RuntimeIdentity {
                    endpoint: endpoint.url().to_owned(),
                    message: format!("the endpoint changed ownership while stale pid {pid} was stopping"),
                });
            }
        }
    }
    #[cfg(unix)]
    let path = executable_of(pid).unwrap_or_else(|| PathBuf::from(format!("pid {pid} at {}", endpoint.url())));
    #[cfg(windows)]
    let path = PathBuf::from(format!("pid {pid} at {}", endpoint.url()));
    Err(InitError::RuntimeSurvived { pid, path })
}

#[cfg(unix)]
fn is_owned_appa_runtime(pid: i32) -> Result<bool, InitError> {
    if pid == std::process::id() as i32 {
        return Ok(false);
    }
    let query = |field: &str| -> Option<String> {
        let output = Command::new("ps")
            .args(["-o", field, "-p", &pid.to_string()])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(uid) = query("uid=").and_then(|uid| uid.parse::<u32>().ok()) else {
        return Ok(false);
    };
    if uid != unsafe { libc::geteuid() } {
        return Ok(false);
    }
    let Some(command_name) = query("comm=") else {
        return Ok(false);
    };
    if Path::new(&command_name).file_name().and_then(OsStr::to_str) != Some("appa") {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(unix)]
fn terminate_appa_pid(pid: i32) -> Result<(), InitError> {
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let source = std::io::Error::last_os_error();
    if source.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(InitError::InstallRuntime {
        path: executable_of(pid).unwrap_or_else(|| PathBuf::from(format!("pid {pid}"))),
        source,
    })
}

#[cfg(windows)]
const WINDOWS_APPA_IDENTITY_SCRIPT: &str = r#"
$appaPid = [int]$env:APPA_STALE_PID
$appaProcess = Get-Process -Id $appaPid -ErrorAction SilentlyContinue
$appaState = 'missing'
if ($null -ne $appaProcess) {
    $appaState = 'foreign'
    $appaCim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $appaPid" -ErrorAction SilentlyContinue
    $appaOwner = if ($null -ne $appaCim) {
        Invoke-CimMethod -InputObject $appaCim -MethodName GetOwnerSid -ErrorAction SilentlyContinue
    } else {
        $null
    }
    $appaCallerSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($appaProcess.ProcessName -ieq 'appa' -and
        $null -ne $appaCim -and $appaCim.Name -ieq 'appa.exe' -and
        $null -ne $appaOwner -and $appaOwner.ReturnValue -eq 0 -and
        $appaOwner.Sid -eq $appaCallerSid) {
        $appaState = 'owned'
    }
}
"#;

#[cfg(windows)]
fn is_owned_appa_runtime(pid: i32) -> Result<bool, InitError> {
    let command = format!("{WINDOWS_APPA_IDENTITY_SCRIPT}\n$appaState");
    let answer = powershell(&command, [("APPA_STALE_PID", pid.to_string())])?;
    Ok(answer.trim() == "owned")
}

#[cfg(windows)]
fn terminate_appa_pid(pid: i32) -> Result<(), InitError> {
    // Resolve the process object first and stop that object, not a freshly
    // looked-up PID. The SID/name checks are repeated in this same PowerShell
    // invocation so an elevated init never turns a forged health answer into
    // authority to terminate another user's process.
    let command = format!(
        "{WINDOWS_APPA_IDENTITY_SCRIPT}\n\
         if ($appaState -eq 'missing') {{ exit 0 }}\n\
         if ($appaState -ne 'owned') {{ throw 'pid is not this user''s appa runtime' }}\n\
         if (-not $appaProcess.HasExited) {{ \
             Stop-Process -InputObject $appaProcess -Force -ErrorAction Stop \
         }}"
    );
    powershell(&command, [("APPA_STALE_PID", pid.to_string())]).map(drop)
}

fn endpoint_owner(binary: &Path, endpoint: &Endpoint) -> Result<EndpointOwner, InitError> {
    let expected = Sha256::digest(fs::read(binary).map_err(|source| InitError::InstallRuntime {
        path: binary.to_path_buf(),
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
        return Ok(EndpointOwner::Unidentified);
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    Ok(classify_endpoint_owner(&expected, &answer))
}

fn classify_endpoint_owner(expected: &str, answer: &str) -> EndpointOwner {
    let mut fields = answer.split_whitespace();
    let actual = fields.next().unwrap_or_default();
    let pid = fields.next().and_then(positive_pid);
    if actual == expected {
        EndpointOwner::Deployment
    } else {
        EndpointOwner::Foreign { pid }
    }
}

fn confirm_stop(pid: i32, endpoint: &Endpoint) -> Result<bool, InitError> {
    let stdin = std::io::stdin();
    let stderr = std::io::stderr();
    confirm_stop_with(pid, endpoint, &mut stdin.lock(), &mut stderr.lock())
}

fn confirm_stop_with(
    pid: i32,
    endpoint: &Endpoint,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, InitError> {
    write!(
        output,
        "appa: a different appa build (pid {pid}) owns {}. Stop it and continue? [Y/n] ",
        endpoint.url()
    )
    .and_then(|()| output.flush())
    .map_err(|source| InitError::RuntimeIdentity {
        endpoint: endpoint.url().to_owned(),
        message: format!("cannot ask permission to stop pid {pid}: {source}"),
    })?;
    let mut answer = String::new();
    if input
        .read_line(&mut answer)
        .map_err(|source| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("cannot read permission to stop pid {pid}: {source}"),
        })?
        == 0
    {
        return Ok(false);
    }
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"))
}

/// Clear a foreign owner while Claude and the launcher are still untouched.
///
/// Silence is accepted because that is a first install. A foreign process is
/// eligible only when the APPA identity response names a same-user `appa`
/// process and the user confirms the stop. The identity is checked again
/// immediately before signalling to close the prompt-to-kill race.
fn clear_foreign_endpoint(binary: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
    match endpoint_owner(binary, endpoint)? {
        EndpointOwner::Deployment | EndpointOwner::Unidentified => Ok(()),
        EndpointOwner::Foreign { pid: None } => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "a different appa build owns this endpoint, but this older runtime does not identify its pid; stop it and rerun init"
                .to_owned(),
        }),
        EndpointOwner::Foreign { pid: Some(pid) } => {
            clear_confirmed_foreign_with(binary, endpoint, pid, confirm_stop)
        }
    }
}

fn clear_confirmed_foreign_with(
    binary: &Path,
    endpoint: &Endpoint,
    pid: i32,
    confirm: impl FnOnce(i32, &Endpoint) -> Result<bool, InitError>,
) -> Result<(), InitError> {
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!(
                "a different build names pid {pid}, but it is not this user's appa runtime; not stopping it"
            ),
        });
    }
    if !confirm(pid, endpoint)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("a different appa build (pid {pid}) still owns this endpoint; init cancelled"),
        });
    }
    match endpoint_owner(binary, endpoint)? {
        EndpointOwner::Unidentified => return Ok(()),
        EndpointOwner::Deployment => return Ok(()),
        EndpointOwner::Foreign { pid: Some(current) } if current == pid => {}
        EndpointOwner::Foreign { .. } => {
            return Err(InitError::RuntimeIdentity {
                endpoint: endpoint.url().to_owned(),
                message: "the endpoint changed ownership after approval; not stopping either process".to_owned(),
            });
        }
    }
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("pid {pid} changed identity after approval; not stopping it"),
        });
    }
    terminate_appa_pid(pid)?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    #[cfg(unix)]
    let path = executable_of(pid).unwrap_or_else(|| PathBuf::from(format!("pid {pid} at {}", endpoint.url())));
    #[cfg(windows)]
    let path = PathBuf::from(format!("pid {pid} at {}", endpoint.url()));
    Err(InitError::RuntimeSurvived { pid, path })
}

/// Reconcile the policy a surviving runtime serves with the file this init validated.
///
/// The starter replaces a runtime whose executable changed, and that fresh process loads
/// this file itself. A runtime it left running does not: it still serves what it loaded at
/// startup, and a config written since is on disk only. Comparing the two keys keeps the
/// question to the case that has one — an install that changed nothing asks nothing.
fn reconcile_policy(endpoint: &Endpoint, config: &Path, composed: Option<String>) -> Result<RuntimeOutcome, InitError> {
    if !policy_diverged(composed.as_deref(), serving_policy_key(endpoint).as_deref()) {
        return Ok(RuntimeOutcome::Healthy);
    }
    if !confirm_reload(config)? {
        return Ok(RuntimeOutcome::OlderPolicy);
    }
    reload_policy(endpoint, config)?;
    Ok(RuntimeOutcome::Reloaded)
}

/// Whether a serving runtime lags the file this init validated.
///
/// Absent either key there is nothing to compare: a config that resolves only where the
/// runtime runs gives init no key of its own, and a runtime that does not answer for its
/// policy gives none either. Init never guesses at a divergence, so both cases are quiet.
fn policy_diverged(composed: Option<&str>, serving: Option<&str>) -> bool {
    matches!((composed, serving), (Some(composed), Some(serving)) if composed != serving)
}

/// The policy key the endpoint answers under, or `None` when it does not answer for one.
fn serving_policy_key(endpoint: &Endpoint) -> Option<String> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "2"])
        .arg(endpoint.join("/policy-key"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!key.is_empty()).then_some(key)
}

/// Ask the running runtime to serve the configuration on disk.
///
/// The runtime validates the file again before it swaps: a refusal here is a fault worth
/// naming, not a receipt footnote, because the deployment keeps serving the older policy.
fn reload_policy(endpoint: &Endpoint, config: &Path) -> Result<(), InitError> {
    let refused = |message: String| InitError::ReloadRefused {
        endpoint: endpoint.url().to_owned(),
        path: config.to_path_buf(),
        message,
    };
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--max-time", "10", "-X", "POST"])
        .arg(endpoint.join("/reload"))
        .output()
        .map_err(|error| refused(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(refused(String::from_utf8_lossy(&output.stderr).trim().to_owned()))
}

/// A terminal is asked; anything else reloads. A script that just wrote a config wants it
/// serving, and there is no one there to answer.
fn confirm_reload(config: &Path) -> Result<bool, InitError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(true);
    }
    let stderr = std::io::stderr();
    confirm_reload_with(config, &mut stdin.lock(), &mut stderr.lock())
}

fn confirm_reload_with(config: &Path, input: &mut impl BufRead, output: &mut impl Write) -> Result<bool, InitError> {
    let prompt = |source| InitError::WriteFile {
        path: config.to_path_buf(),
        source,
    };
    write!(
        output,
        "appa: the running runtime still serves the policy it started with, not {}.\n\
         Reload it now? Sessions open right now keep the deployment they started with. [Y/n] ",
        friendly_path(config),
    )
    .and_then(|()| output.flush())
    .map_err(prompt)?;
    let mut answer = String::new();
    if input.read_line(&mut answer).map_err(prompt)? == 0 {
        return Ok(false);
    }
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes"))
}

fn verify_runtime_binary(runtime: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
    match endpoint_owner(runtime, endpoint)? {
        EndpointOwner::Deployment => Ok(()),
        EndpointOwner::Unidentified => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "the answering process exposes no binary fingerprint; stop it and rerun init".to_owned(),
        }),
        EndpointOwner::Foreign { .. } => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "a different appa build is answering; stop that process and rerun init".to_owned(),
        }),
    }
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

    /// What every starter runs: the subcommand plus the endpoint it binds.
    #[cfg(unix)]
    const RUNTIME_ARGUMENTS: &[&str] = &["runtime", "--listen", "127.0.0.1:8787"];

    /// A process whose executable really *is* `at`, started with `arguments`, so
    /// verification finds it there rather than taking a spoofable argv on trust
    /// and the stop set sees the argv it decides on.
    ///
    /// The stand-in is reaped on its own thread. A dead child that nobody has
    /// waited for is a zombie, and `kill(pid, 0)` still succeeds on one, so
    /// without the reaper these tests could not tell a stopped process from a
    /// running one. In production the retired runtime is never init's child.
    #[cfg(unix)]
    fn process_executing(at: &Path, ignores_sigterm: bool, arguments: &[&str]) -> Option<i32> {
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
        let disposition = if ignores_sigterm { "$SIG{TERM} = 'IGNORE'; " } else { "" };
        let script = format!("{disposition}open(my $f, '>', $ARGV[0]) or die; close $f; sleep 30");
        let mut child = Command::new(at)
            .args(["-e", &script])
            .arg(&ready)
            .args(arguments)
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
    fn health_answers(answers: Vec<String>) -> Endpoint {
        recorded_answers(answers).0
    }

    /// The same loopback fixture, with the request lines it served. A probe's path is part
    /// of the contract it has with the runtime, so a test that cares which endpoint init
    /// asks reads them; one that only cares how an answer parses takes `health_answers`.
    #[cfg(unix)]
    fn recorded_answers(answers: Vec<String>) -> (Endpoint, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback health fixture binds");
        let endpoint = Endpoint::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&asked);
        std::thread::spawn(move || {
            for answer in answers {
                let (mut connection, _) = listener.accept().expect("the health probe connects");
                let mut request = [0u8; 2048];
                let read = connection.read(&mut request).unwrap_or(0);
                let requested = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                recorder
                    .lock()
                    .expect("the request recorder is never poisoned")
                    .push(requested);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                connection
                    .write_all(response.as_bytes())
                    .expect("the health answer writes");
            }
        });
        (endpoint, asked)
    }

    #[test]
    fn only_canonical_stale_health_answers_grant_a_pid() {
        assert_eq!(stale_pid("stale 42"), Some(42));
        for answer in [
            "",
            "ok",
            "stale",
            "stale ",
            "stale 0",
            "stale 01",
            "stale -1",
            "stale 42 extra",
        ] {
            assert_eq!(stale_pid(answer), None, "accepted {answer:?}");
        }
    }

    #[test]
    fn runtime_identity_accepts_old_answers_but_only_new_answers_name_a_process() {
        assert_eq!(classify_endpoint_owner("same", "same 42"), EndpointOwner::Deployment);
        assert_eq!(
            classify_endpoint_owner("same", "different 42"),
            EndpointOwner::Foreign { pid: Some(42) }
        );
        assert_eq!(
            classify_endpoint_owner("same", "different"),
            EndpointOwner::Foreign { pid: None }
        );
    }

    #[test]
    fn stopping_a_foreign_runtime_requires_a_y_or_default_yes_answer() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8787").expect("the endpoint parses");
        for answer in ["y\n", "YES\n", "\n"] {
            let mut output = Vec::new();
            assert!(
                confirm_stop_with(42, &endpoint, &mut answer.as_bytes(), &mut output).expect("the answer reads"),
                "{answer:?} approves"
            );
            assert!(
                String::from_utf8(output)
                    .unwrap()
                    .contains("Stop it and continue? [Y/n]")
            );
        }
        for answer in ["n\n", "no\n", "anything else\n", ""] {
            assert!(
                !confirm_stop_with(42, &endpoint, &mut answer.as_bytes(), &mut Vec::new()).expect("the answer reads"),
                "{answer:?} refuses"
            );
        }
    }

    #[test]
    fn only_two_answerable_and_differing_policy_keys_are_a_divergence() {
        assert!(policy_diverged(Some("composed"), Some("serving")));
        // An install that changed nothing must ask nothing.
        assert!(!policy_diverged(Some("same"), Some("same")));
        // Neither side answering for a policy is silence, never an assumed divergence:
        // a config resolving only where the runtime runs, and a runtime that does not
        // report its policy at all.
        assert!(!policy_diverged(None, Some("serving")));
        assert!(!policy_diverged(Some("composed"), None));
        assert!(!policy_diverged(None, None));
    }

    #[test]
    fn reloading_a_lagging_runtime_requires_a_y_or_default_yes_answer() {
        let config = PathBuf::from("/home/user/config/appa.toml");
        for answer in ["y\n", "YES\n", "\n"] {
            assert!(
                confirm_reload_with(&config, &mut answer.as_bytes(), &mut Vec::new()).expect("the answer reads"),
                "{answer:?} approves"
            );
        }
        for answer in ["n\n", "no\n", "anything else\n", ""] {
            assert!(
                !confirm_reload_with(&config, &mut answer.as_bytes(), &mut Vec::new()).expect("the answer reads"),
                "{answer:?} refuses"
            );
        }
    }

    #[test]
    fn a_serving_policy_key_is_read_only_when_the_endpoint_answers_one() {
        let (endpoint, asked) = recorded_answers(vec!["c54f1509".to_string()]);
        assert_eq!(serving_policy_key(&endpoint).as_deref(), Some("c54f1509"));
        assert_eq!(
            asked.lock().expect("the request recorder is never poisoned").as_slice(),
            ["GET /policy-key HTTP/1.1".to_string()],
            "the probe reads the policy route, and reads it without mutating"
        );

        // A runtime predating the route answers nothing usable, and an unbound port
        // answers not at all. Both leave init with no key rather than a wrong one.
        let blank = health_answers(vec![String::new()]);
        assert_eq!(serving_policy_key(&blank), None);
        let unbound = Endpoint::parse("http://127.0.0.1:1").expect("the endpoint parses");
        assert_eq!(serving_policy_key(&unbound), None);
    }

    #[test]
    fn a_matching_policy_key_reconciles_without_asking_or_reloading() {
        // One answer is served: the key probe. A reload would need a second connection,
        // so reaching one at all would hang rather than pass.
        let endpoint = health_answers(vec!["agreed".to_string()]);
        let config = PathBuf::from("/home/user/config/appa.toml");
        assert_eq!(
            reconcile_policy(&endpoint, &config, Some("agreed".to_string())).expect("the reconcile completes"),
            RuntimeOutcome::Healthy
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_approved_foreign_appa_runtime_is_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let foreign = directory.path().join("foreign/appa");
        let Some(pid) = process_executing(&foreign, false, RUNTIME_ARGUMENTS) else {
            return;
        };
        let candidate = directory.path().join("candidate-appa");
        fs::write(&candidate, "a different candidate build").expect("the candidate binary exists");
        let endpoint = health_answers(vec![format!("different-fingerprint {pid}")]);

        clear_confirmed_foreign_with(&candidate, &endpoint, pid, |approved_pid, _| {
            assert_eq!(approved_pid, pid);
            Ok(true)
        })
        .expect("the approved foreign runtime stops");

        assert!(!still_running(pid), "the approved runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn init_reclaims_an_unlinked_runtime_named_by_its_stale_health_answer() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let retired = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&retired, false, RUNTIME_ARGUMENTS) else {
            return;
        };
        fs::remove_file(&retired).expect("the installed binary is unlinked while its runtime remains");
        let endpoint = health_answers(vec![format!("stale {pid}"), format!("stale {pid}")]);

        clear_stale_endpoint(&endpoint).expect("init stops its stale unlinked runtime");

        assert!(!still_running(pid), "the stale runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn a_spoofed_stale_pid_does_not_grant_process_shutdown() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let other = directory.path().join("bin/not-appa");
        let Some(pid) = process_executing(&other, false, RUNTIME_ARGUMENTS) else {
            return;
        };
        let endpoint = health_answers(vec![format!("stale {pid}")]);

        let refused = clear_stale_endpoint(&endpoint);

        assert!(matches!(refused, Err(InitError::RuntimeIdentity { .. })));
        assert!(still_running(pid), "a non-appa process was terminated");
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_at_the_retired_path_is_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let retired = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&retired, false, RUNTIME_ARGUMENTS) else {
            return;
        };

        let survivors = stop_processes_executing(&retired).expect("the stop set runs");

        assert!(survivors.is_empty(), "a stoppable runtime was reported as surviving");
        assert!(!still_running(pid), "the runtime at the retired path is still running",);
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_at_another_path_is_left_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // What an init run under a different APPA_INSTALL_DIR or APPA_DATA_DIR
        // leaves behind: a path this environment never computes.
        let elsewhere = directory.path().join("other-install/appa");
        let Some(pid) = process_executing(&elsewhere, false, RUNTIME_ARGUMENTS) else {
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

    /// What a second `appa init`, or an `appa hook` in flight, looks like: the
    /// retired executable, doing something that is not serving the endpoint.
    /// Terminating it would interrupt work unrelated to the runtime being
    /// replaced -- and killing a concurrent init mid-switch is the worst of
    /// them, because the Claude plugin replacement it is performing is not
    /// atomic.
    #[cfg(unix)]
    #[test]
    fn another_invocation_of_the_retired_binary_is_left_alone() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let retired = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&retired, false, &["init", "claude-code"]) else {
            return;
        };

        let survivors = stop_processes_executing(&retired).expect("the stop set runs");

        assert!(survivors.is_empty());
        assert!(
            still_running(pid),
            "the stop set signalled a process that is not a runtime",
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
        let Some(pid) = process_executing(&retired, true, RUNTIME_ARGUMENTS) else {
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
        // The retired daemon was its own binary and took no subcommand; legacy
        // cleanup matches on the executable path alone.
        let target = directory.path().join("appa-runtime");
        let Some(pid) = process_executing(&target, false, &[]) else {
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
    fn a_config_the_runtime_could_not_compose_stops_init() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");

        assert!(create_default_config(&config).expect("the default config is written"));
        verify_config(&config).expect("the config init writes composes");

        let ahead = template_policy_version() + 1;
        fs::write(
            directory.path().join("battery.toml"),
            format!("[policy]\nversion = {ahead}\n"),
        )
        .expect("battery written");
        let stale = fs::read_to_string(&config).expect("the config is readable");
        fs::write(&config, format!("include = [\"battery.toml\"]\n{stale}")).expect("include written");

        match verify_config(&config) {
            Err(InitError::UnloadableConfig { source, .. }) => {
                assert!(matches!(*source, ConfigError::IncludedVersion { .. }));
            }
            other => panic!("a battery ahead of the root policy version must stop init: {other:?}"),
        }
    }

    /// A loadable config plus whatever `body` declares, at the path init keeps.
    fn config_declaring(directory: &Path, body: &str) -> PathBuf {
        let config = directory.join("appa.toml");
        let version = template_policy_version();
        fs::write(
            &config,
            format!("[policy]\nversion = {version}\n{body}\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"),
        )
        .expect("the config is written");
        config
    }

    #[test]
    fn a_config_the_runtime_could_not_serve_stops_init() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = config_declaring(
            directory.path(),
            "[policy.deployment]\ncontext_control = true\n[[policy.tool]]\nname = \"Agent\"\ndelta = {}\n",
        );

        assert!(
            matches!(verify_config(&config), Err(InitError::UnusableConfig { .. })),
            "a subagent that can return unobserved must stop init, as it stops the runtime"
        );
    }

    #[test]
    fn a_token_this_process_cannot_see_is_left_to_the_runtime() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = config_declaring(
            directory.path(),
            "[externals.sanitizers.scrub]\nurl = \"https://scrub.internal\"\ntoken_env = \"APPA_UNSET_IN_THIS_PROCESS\"\n",
        );

        assert!(std::env::var_os("APPA_UNSET_IN_THIS_PROCESS").is_none());
        verify_config(&config).expect("init does not judge a secret it cannot reach");
    }

    /// A config one policy version behind the build, at the path init keeps.
    fn outdated_config(directory: &Path) -> PathBuf {
        let config = directory.join("appa.toml");
        let older = template_policy_version() - 1;
        fs::write(&config, format!("[policy]\nversion = {older}\n")).expect("the config is written");
        config
    }

    fn answer_rewrite(config: &Path, answer: &str) -> (ConfigOutcome, String) {
        let mut prompt = Vec::new();
        let outcome =
            offer_config_rewrite_with(config, &mut answer.as_bytes(), &mut prompt).expect("the offer completes");
        (outcome, String::from_utf8(prompt).expect("the prompt is text"))
    }

    #[test]
    fn an_outdated_config_is_rewritten_only_on_an_explicit_yes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = outdated_config(directory.path());
        let authored = fs::read_to_string(&config).expect("the config is readable");
        let backup = directory.path().join("appa.toml.bak");

        for declined in ["", "\n", "n\n", "no\n"] {
            let (outcome, prompt) = answer_rewrite(&config, declined);
            assert!(!prompt.is_empty(), "the offer is shown");
            assert_eq!(outcome, ConfigOutcome::Kept);
            assert_eq!(fs::read_to_string(&config).ok(), Some(authored.clone()));
            assert!(!backup.exists(), "a declined offer writes nothing");
        }

        assert_eq!(answer_rewrite(&config, "y\n").0, ConfigOutcome::Rewritten);
        assert_eq!(fs::read_to_string(&config).ok(), Some(DEFAULT_CONFIG.to_string()));
        assert_eq!(fs::read_to_string(&backup).ok(), Some(authored));
        verify_config(&config).expect("the rewritten config composes");
    }

    #[test]
    fn a_current_config_is_never_offered_for_rewrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");
        assert!(create_default_config(&config).expect("the default config is written"));

        let (outcome, prompt) = answer_rewrite(&config, "y\n");
        assert!(prompt.is_empty(), "no offer is made");
        assert_eq!(outcome, ConfigOutcome::Kept);
        assert_eq!(fs::read_to_string(&config).ok(), Some(DEFAULT_CONFIG.to_string()));
        assert!(!directory.path().join("appa.toml.bak").exists());
    }

    #[test]
    fn receipt_is_compact_and_hides_installation_details() {
        let config = user_home()
            .unwrap_or_else(|| PathBuf::from("/home/user"))
            .join("config/appa.toml");
        let receipt = render_receipt(
            "current checkout",
            &config,
            ConfigOutcome::Kept,
            RuntimeOutcome::Healthy,
            None,
            false,
        );

        assert!(receipt.starts_with("OpenAPPA initialized for Claude Code\n\n"));
        assert!(receipt.contains("Adapter   current checkout"));
        assert!(receipt.contains("Runtime   healthy"));
        assert!(receipt.contains("Launcher  clappa"));
        assert!(receipt.contains("Next: run `clappa`, then `/appa-guide init`."));
        assert!(!receipt.contains("Statusline"));
        assert!(!receipt.contains("appa-runtime (healthy)"));
        assert!(!receipt.contains("\u{1b}["));

        let colored = render_receipt(
            "current checkout",
            &config,
            ConfigOutcome::Kept,
            RuntimeOutcome::Healthy,
            None,
            true,
        );
        assert!(colored.starts_with("\u{1b}[1;32m✓ OpenAPPA initialized\u{1b}[0m"));
    }
}
