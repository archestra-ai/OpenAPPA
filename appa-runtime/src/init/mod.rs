//! Native deployment activation. The marketplace installs machine state through
//! this module; harness skills only author policy.
//!
//! The only host-side code of a protected session is the deployed binary: the
//! user's Claude Code settings name it in every hook entry and in the status
//! line, the runtime's MCP server is registered through the `claude` CLI, and
//! the appa-guide skill is written from bytes compiled into the binary.

use crate::config::ConfigError;
use crate::installation::archive::{self, ArchiveError, VerifiedArchive};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thiserror::Error;

mod config;
pub(crate) mod endpoint;
mod mcp;
pub(crate) mod paths;
mod receipt;
mod removal;
pub(crate) mod settings;
mod skill;

pub use self::paths::installed_config_path;
pub use self::removal::claude_code_remove;

use self::config::{ComposedPolicy, discard_file, verify_config};
use self::endpoint::{
    Endpoint, RuntimeOutcome, clear_stale_endpoint, endpoint_health, reconcile_policy, stop_owned_appa_runtime,
    verify_runtime_deployment,
};
#[cfg(windows)]
use self::paths::windows_identity;
use self::paths::{DeploymentPaths, appa_filename, deployment_paths, same_file};
use self::receipt::{Receipt, Style};
use self::settings::HookTarget;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("cannot find the current executable: {0}")]
    CurrentExecutable(std::io::Error),
    #[error("cannot find a home directory; set HOME or the relevant APPA directory variables")]
    MissingHome,
    #[error("cannot make the directory override {path} absolute: {source}")]
    AbsolutePath { path: PathBuf, source: std::io::Error },
    #[error("{path} must be valid UTF-8 to be named in a Claude Code hook entry")]
    UnportablePath { path: PathBuf },
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
    #[error("cannot change APPA integration state at {path}: {message}")]
    NativeState { path: PathBuf, message: String },
    #[error(
        "Claude Code has an MCP server named `{}` at {url} that no APPA install wrote; remove or rename it, then rerun the install",
        mcp::SERVER
    )]
    McpConflict { url: String },
    #[error("{path} is not the appa-guide skill an APPA install writes; move it aside, then rerun the install")]
    SkillConflict { path: PathBuf },
    #[error("{value} is not a usable runtime endpoint: {reason}")]
    MalformedEndpoint { value: String, reason: String },
    #[error("the deployed binary could not bring `appa runtime` up: {0}")]
    Starter(String),
    #[error("the runtime endpoint {endpoint} is taken: {message}")]
    RuntimeIdentity { endpoint: String, message: String },
    #[error(
        "the appa runtime (pid {pid}) still answers {endpoint} after being stopped. Stop it, then rerun appa plugin install claude-code."
    )]
    RuntimeSurvived { pid: i32, endpoint: String },
    #[error("the runtime at {endpoint} does not answer for its policy: {message}")]
    PolicyKey { endpoint: String, message: String },
    #[error("the runtime at {endpoint} refused to serve {path}: {message}")]
    ReloadRefused {
        endpoint: String,
        path: PathBuf,
        message: String,
    },
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("{operation}; restoring the previous installation also failed: {recovery}")]
    Recovery {
        operation: Box<InitError>,
        recovery: Box<InitError>,
    },
}

/// Activate a validated deployment: this binary, the config the marketplace
/// wrote, and the Claude Code profile that binds a protected session to them.
///
/// `appa plugin install claude-code` runs this after the generation is
/// retained. Nothing here asks a question or fetches a package. The sequence is
/// ordered so that the profile is refused before anything is written when it
/// holds foreign state under APPA's names, and so that the endpoint is settled
/// before the profile is switched over. Directories and the deployed binary's
/// parent are written before that settling; both are additive and neither is
/// what Claude reads.
pub fn activate_claude_code(
    config: &Path,
    archive: &Path,
    previous_binary: Option<&Path>,
) -> Result<String, InitError> {
    // The endpoint is settled before anything is read: a release build ignores
    // the environment here, and the release check proves it on this refusal.
    let endpoint = Endpoint::resolve()?;
    let config = std::path::absolute(config).map_err(|source| InitError::AbsolutePath {
        path: config.to_owned(),
        source,
    })?;
    crate::config::Config::load(&config).map_err(|source| InitError::UnloadableConfig {
        path: config.clone(),
        source: Box::new(source),
    })?;
    let source = VerifiedArchive::of(archive)?;
    install_claude(&source.label(), endpoint, config, previous_binary)
}

fn install_claude(
    origin: &str,
    endpoint: Endpoint,
    config: PathBuf,
    previous_binary: Option<&Path>,
) -> Result<String, InitError> {
    let appa = env::current_exe().map_err(InitError::CurrentExecutable)?;
    let paths = deployment_paths()?;
    let _profile_lock = lock_claude_profile(&paths.claude_dir)?;

    // 1. Directories, and the config that survives every upgrade.
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
    let composed_policy = verify_config(&config)?;

    // 2. What the profile holds under APPA's names. A server or a skill that
    //    no install wrote is refused here, with the profile untouched.
    progress("reading the Claude Code profile");
    let registered = mcp::current()?;
    skill::verify(&paths.claude_dir)?;
    settings::verify(&paths)?;

    // 3. Settle the endpoint before the profile is switched over. A runtime
    //    that will not stop aborts here, rather than leaving hooks registered
    //    against an old runtime that a rerun cannot dislodge.
    progress("checking the runtime endpoint");
    //    A runtime whose binary an install replaced on disk still owns the
    //    endpoint, and its health answer names the stale pid.
    clear_stale_endpoint(&endpoint)?;
    if endpoint_health(&endpoint)?.is_some() {
        // Nobody is asked on behalf of a different deployment. An update stops
        // the runtime of the binary it replaces, once that runtime proves to be
        // serving this config; any other owner is refused with its pid named.
        if let Err(current_error) = verify_runtime_deployment(&appa, &config, &endpoint) {
            let Some(previous) = previous_binary else {
                return Err(current_error);
            };
            let pid = verify_runtime_deployment(previous, &config, &endpoint)?;
            stop_owned_appa_runtime(pid, &endpoint)?;
        }
    }

    // 4. The launcher an earlier install armed is disarmed while the profile is
    //    between two states. The install that completes re-arms it; so does a
    //    rollback that put everything back, and nothing else does.
    let launcher = paths.install_dir.join(CLAPPA.0);
    let launcher_before = file_before(&launcher)?;
    if launcher_before.is_some() {
        install_disabled_clappa(&paths.install_dir)?;
    }

    // 5. The binary, the profile, and the runtime the profile is being bound
    //    to: one transaction. Verification is inside it, because hooks left
    //    registered against a runtime that failed verification is exactly the
    //    skew this sequence exists to prevent. Every step records what it
    //    changed, and a failure unwinds those changes in reverse.
    progress("updating the Claude Code profile");
    let target = HookTarget {
        binary: &deployed_appa,
        url: endpoint.url(),
        config: &config,
        data_dir: &paths.data_dir,
    };
    let mut compensation = Compensation::default();
    let switch = switch_over(
        &appa,
        &target,
        &composed_policy,
        &endpoint,
        &paths,
        &registered,
        &mut compensation,
    );
    let runtime_outcome = match switch {
        Ok(outcome) => {
            compensation.commit();
            outcome
        }
        Err(operation) => {
            let restored = compensation.unwind().and_then(|()| match &launcher_before {
                Some(bytes) => fs::write(&launcher, bytes).map_err(|source| InitError::WriteFile {
                    path: launcher.clone(),
                    source,
                }),
                None => Ok(()),
            });
            if let Err(recovery) = restored {
                return Err(InitError::Recovery {
                    operation: Box::new(operation),
                    recovery: Box::new(recovery),
                });
            }
            return Err(operation);
        }
    };

    // Arm the launcher only after verification.
    install_clappa(&paths.install_dir)?;

    Ok(Receipt {
        adapter: origin.to_owned(),
        hooks: settings::path(&paths),
        config,
        runtime_outcome,
    }
    .render(Style::of_stdout()))
}

/// Different deployment configs may target one Claude profile. Serialize the
/// native mutation on that shared profile, not only on each config's store.
fn lock_claude_profile(directory: &Path) -> Result<fs::File, InitError> {
    fs::create_dir_all(directory).map_err(|source| InitError::WriteFile {
        path: directory.to_owned(),
        source,
    })?;
    let path = directory.join(".appa-install.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&path).map_err(|source| InitError::WriteFile {
        path: path.clone(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| InitError::WriteFile {
            path: path.clone(),
            source,
        })?
        .is_file()
    {
        return Err(InitError::NativeState {
            path,
            message: "profile lock must be a regular file".into(),
        });
    }
    file.try_lock().map_err(|error| InitError::NativeState {
        path,
        message: format!("cannot lock Claude profile; another APPA operation may be running: {error}"),
    })?;
    Ok(file)
}

/// The steps of the switch, each recording what it changed.
///
/// The runtime the profile is bound to must also be serving this deployment's
/// policy, so the reconcile is inside the transaction: a refusal there means the
/// endpoint belongs to someone else, and hooks left registered against it are
/// the same skew as hooks left registered against a runtime that failed
/// verification.
fn switch_over(
    appa: &Path,
    target: &HookTarget<'_>,
    composed_policy: &ComposedPolicy,
    endpoint: &Endpoint,
    paths: &DeploymentPaths,
    registered: &mcp::Registered,
    compensation: &mut Compensation,
) -> Result<RuntimeOutcome, InitError> {
    install_runtime(appa, target.binary, compensation)?;
    settings::install_hooks(paths, target, compensation)?;
    mcp::register(registered, target.url, compensation)?;
    skill::install(&paths.claude_dir, compensation)?;
    settings::install_statusline(paths, target, compensation)?;
    progress("starting the runtime");
    // A runtime answering `ok` here was running before this install and stays
    // the user's; anything the start brings up after silence is ours to stop.
    let running_before = endpoint_health(endpoint)?.is_some_and(|answer| answer == "ok");
    start_runtime(target)?;
    let pid = verify_runtime_deployment(target.binary, target.config, endpoint)?;
    if !running_before {
        compensation.record(Undo::Runtime {
            pid,
            endpoint: endpoint.clone(),
        });
    }
    reconcile_policy(endpoint, target.config, composed_policy)
}

/// Bring the deployed runtime up through the deployed binary itself, the start
/// every protected SessionStart performs; a healthy runtime is left as it is.
fn start_runtime(target: &HookTarget<'_>) -> Result<(), InitError> {
    let mut command = match archive::debug_override("APPA_RUNTIME_STARTER") {
        Some(starter) => {
            let mut command = Command::new("sh");
            command.arg(starter);
            command
        }
        None => {
            let mut command = Command::new(target.binary);
            command
                .args(["runtime", "ensure", "--deployment-url", target.url])
                .arg("--config")
                .arg(target.config)
                .arg("--data-dir")
                .arg(target.data_dir);
            command
        }
    };
    // APPA_RUNTIME_URL is removed rather than set: to the start it means "the
    // user runs their own runtime here", and setting it would suppress managed
    // replacement permanently.
    let output = command
        .env_remove("APPA_RUNTIME_URL")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| InitError::Starter(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(InitError::Starter(if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }))
}

/// What the switch has changed on disk, in Claude's profile and in process
/// state, so a failure can put each change back in reverse order.
#[derive(Default)]
struct Compensation {
    done: Vec<Undo>,
}

enum Undo {
    /// The deployed binary's bytes before install_runtime replaced them, copied
    /// aside to `previous`; `None` when no binary was deployed.
    Binary { target: PathBuf, previous: Option<PathBuf> },
    /// A file the switch rewrote, with its bytes from before; `None` when it
    /// did not exist.
    File { path: PathBuf, before: Option<Vec<u8>> },
    /// The MCP registration the switch replaced: an earlier install's template
    /// URL, or `None` when there was none.
    Mcp { previous: Option<String> },
    /// A runtime this install started and verified as this deployment's.
    Runtime { pid: i32, endpoint: Endpoint },
}

impl Compensation {
    fn record(&mut self, undo: Undo) {
        self.done.push(undo);
    }

    /// Put back every recorded change, last first. Every step is attempted; the
    /// first failure is the one reported.
    fn unwind(self) -> Result<(), InitError> {
        let mut first_failure = None;
        for undo in self.done.into_iter().rev() {
            if let Err(error) = undo.apply() {
                tracing::warn!(%error, "an init rollback step failed");
                first_failure.get_or_insert(error);
            }
        }
        first_failure.map_or(Ok(()), Err)
    }

    /// The install stands: drop the binary snapshot.
    fn commit(self) {
        for undo in self.done {
            if let Undo::Binary {
                previous: Some(previous),
                ..
            } = undo
                && let Err(error) = fs::remove_file(&previous)
            {
                tracing::warn!(path = %previous.display(), %error, "cannot remove the binary snapshot");
            }
        }
    }
}

impl Undo {
    fn apply(self) -> Result<(), InitError> {
        match self {
            Undo::Binary { target, previous } => {
                let install = |source| InitError::InstallRuntime {
                    path: target.clone(),
                    source,
                };
                match previous {
                    Some(previous) => {
                        #[cfg(windows)]
                        if target.exists() {
                            stop_windows_processes_at(&target)?;
                            fs::remove_file(&target).map_err(install)?;
                        }
                        fs::rename(&previous, &target).map_err(install)
                    }
                    None => remove_if_present(&target).map_err(install),
                }
            }
            Undo::File { path, before } => match before {
                Some(bytes) => write_state(&path, &bytes),
                None => remove_if_present(&path).map_err(|source| InitError::WriteFile { path, source }),
            },
            Undo::Mcp { previous } => mcp::restore(previous),
            Undo::Runtime { pid, endpoint } => stop_owned_appa_runtime(pid, &endpoint),
        }
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The bytes at `path` before init rewrites it, or `None` when it is absent.
/// Anything but a regular file or nothing is refused.
fn file_before(path: &Path) -> Result<Option<Vec<u8>>, InitError> {
    crate::installation::optional_bytes(path).map_err(|error| InitError::NativeState {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

/// Replace `path` atomically, creating its directory.
fn write_state(path: &Path, bytes: &[u8]) -> Result<(), InitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| InitError::WriteFile {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    crate::installation::atomic_write(path, bytes).map_err(|error| InitError::NativeState {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn progress(message: &str) {
    eprintln!("appa: {message}...");
}

/// Copy the binary to its deployed path, keeping the bytes it replaces beside it
/// as `appa.prev` until the install stands.
fn install_runtime(source: &Path, target: &Path, compensation: &mut Compensation) -> Result<(), InitError> {
    if same_file(source, target) || runtime_contents_match(source, target)? {
        return Ok(());
    }
    let previous = if target.exists() {
        let snapshot = target.with_extension("prev");
        fs::copy(target, &snapshot).map_err(|source| InitError::InstallRuntime {
            path: snapshot.clone(),
            source,
        })?;
        Some(snapshot)
    } else {
        None
    };
    compensation.record(Undo::Binary {
        target: target.to_path_buf(),
        previous,
    });
    #[cfg(windows)]
    if target.exists() {
        stop_windows_processes_at(target)?;
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
        discard_file(&temporary);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary, target) {
        discard_file(&temporary);
        return Err(InitError::InstallRuntime {
            path: target.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn runtime_contents_match(source: &Path, target: &Path) -> Result<bool, InitError> {
    let target_metadata = match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(InitError::InstallRuntime {
                path: target.to_owned(),
                source,
            });
        }
    };
    // current_exe may name the invocation symlink on some platforms. Resolve
    // that source only; the managed destination must remain a regular file.
    let resolved_source = fs::canonicalize(source).map_err(|error| InitError::InstallRuntime {
        path: source.to_owned(),
        source: error,
    })?;
    let source = resolved_source.as_path();
    let source_metadata = fs::metadata(source).map_err(|error| InitError::InstallRuntime {
        path: source.to_owned(),
        source: error,
    })?;
    if source_metadata.len() != target_metadata.len() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if source_metadata.permissions().mode() & 0o111 != target_metadata.permissions().mode() & 0o111 {
            return Ok(false);
        }
    }
    let digest = |path: &Path| {
        let file = crate::installation::open_regular(path).map_err(|error| InitError::NativeState {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        appa_package::generation::ArtifactDigest::of_reader(file, 512 * 1024 * 1024).map_err(|source| {
            InitError::InstallRuntime {
                path: path.to_owned(),
                source,
            }
        })
    };
    Ok(digest(source)? == digest(target)?)
}

/// Terminate every `appa` process whose resolved executable is `target`, and
/// answer with those still alive afterwards.
///
/// PowerShell only enumerates and stops. The comparison happens here, so a
/// discovery or termination failure surfaces instead of being swallowed.
#[cfg(windows)]
fn stop_windows_processes_at(target: &Path) -> Result<Vec<i32>, InitError> {
    let Some(identity) = windows_identity(target) else {
        // A path that will not resolve is reported and skipped, never killed.
        return Ok(Vec::new());
    };

    let listed = powershell(
        "Get-Process -Name appa -ErrorAction SilentlyContinue | \
         ForEach-Object { \"$($_.Id)`t$($_.Path)\" }",
        [],
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
        // init may itself be running from the target path.
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

#[cfg(windows)]
const CLAPPA: (&str, &str) = ("clappa.cmd", "@echo off\r\nset APPA_GATE=1\r\nclaude %*\r\n");
#[cfg(not(windows))]
const CLAPPA: (&str, &str) = ("clappa", "#!/bin/sh\nexec env APPA_GATE=1 claude \"$@\"\n");

fn install_clappa(install_dir: &Path) -> Result<PathBuf, InitError> {
    let path = install_dir.join(CLAPPA.0);
    let existing = crate::installation::optional_bytes(&path).map_err(|error| InitError::NativeState {
        path: path.clone(),
        message: error.to_string(),
    })?;
    if existing.as_deref() == Some(CLAPPA.1.as_bytes()) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&path).map_err(|source| InitError::WriteFile {
                path: path.clone(),
                source,
            })?;
            if metadata.permissions().mode() & 0o111 == 0o111 {
                return Ok(path);
            }
        }
        #[cfg(not(unix))]
        return Ok(path);
    }
    fs::write(&path, CLAPPA.1).map_err(|source| InitError::WriteFile {
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
        "@echo off\r\necho appa plugin install did not complete; rerun appa plugin install claude-code 1>&2\r\nexit /b 1\r\n",
    );
    #[cfg(not(windows))]
    let (path, contents) = (
        install_dir.join("clappa"),
        "#!/bin/sh\nprintf 'appa plugin install did not complete; rerun appa plugin install claude-code\\n' >&2\nexit 1\n",
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

#[cfg(test)]
mod tests {
    #[test]
    fn identical_runtime_copies_do_not_replace_files_or_create_undo_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("installed");
        std::fs::write(&source, b"same runtime").unwrap();
        std::fs::write(&target, b"same runtime").unwrap();
        let backup = target.with_extension("prev");
        std::fs::write(&backup, b"retained backup").unwrap();
        let before = std::fs::metadata(&target).unwrap().modified().unwrap();
        let mut compensation = super::Compensation::default();
        super::install_runtime(&source, &target, &mut compensation).unwrap();
        assert!(compensation.done.is_empty());
        assert_eq!(std::fs::read(&backup).unwrap(), b"retained backup");
        assert_eq!(std::fs::metadata(&target).unwrap().modified().unwrap(), before);
        std::fs::write(&target, b"different!!!").unwrap();
        assert!(!super::runtime_contents_match(&source, &target).unwrap());
        assert!(!super::runtime_contents_match(&source, &root.path().join("missing")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_comparison_accepts_the_executables_source_symlink() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("executable");
        let source = root.path().join("command");
        let target = root.path().join("installed");
        std::fs::write(&executable, b"same runtime").unwrap();
        std::fs::write(&target, b"same runtime").unwrap();
        std::os::unix::fs::symlink(&executable, &source).unwrap();
        assert!(super::runtime_contents_match(&source, &target).unwrap());
        let target_alias = root.path().join("target-alias");
        std::os::unix::fs::symlink(&target, &target_alias).unwrap();
        assert!(!super::runtime_contents_match(&source, &target_alias).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn identical_runtime_bytes_still_require_executable_permission_repair() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("installed");
        for path in [&source, &target] {
            std::fs::write(path, b"same runtime").unwrap();
        }
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!super::runtime_contents_match(&source, &target).unwrap());
        let mut compensation = super::Compensation::default();
        super::install_runtime(&source, &target, &mut compensation).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o111, 0o111);
        compensation.commit();
    }

    use super::*;

    #[test]
    fn launcher_reuse_preserves_mtime_but_repairs_disabled_contents_and_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = install_clappa(root.path()).unwrap();
        let sentinel = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567890);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(sentinel)
            .unwrap();
        install_clappa(root.path()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), sentinel);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            install_clappa(root.path()).unwrap();
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
        }
        install_disabled_clappa(root.path()).unwrap();
        assert_ne!(fs::read(&path).unwrap(), CLAPPA.1.as_bytes());
        install_clappa(root.path()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), CLAPPA.1.as_bytes());
    }

    #[test]
    fn native_profile_lock_serializes_different_deployment_operations() {
        let root = tempfile::tempdir().unwrap();
        let first = lock_claude_profile(root.path()).unwrap();
        assert!(lock_claude_profile(root.path()).is_err());
        drop(first);
        assert!(root.path().join(".appa-install.lock").is_file());
        assert!(lock_claude_profile(root.path()).is_ok());
    }
}
