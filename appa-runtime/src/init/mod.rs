//! Native deployment bootstrap. The CLI installs machine state; harness skills only author policy.

use crate::config::ConfigError;
use crate::plugin_bundle::{self, Endpoint, PluginBundleError, PluginSource, Population};
use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
// Only the PowerShell helpers below spawn a process; nothing else in this module does.
#[cfg(windows)]
use std::process::Command;
use thiserror::Error;

mod claude;
mod config;
mod endpoint;
mod paths;
mod receipt;

pub use self::paths::installed_config_path;

use self::claude::{
    cleanup_plugin_recovery, install_statusline, installed_plugin_installations, installed_plugin_root,
    prepare_plugin_recovery, replace_plugin, run_claude, start_runtime, undo_plugin_switch,
};
use self::config::{
    ComposedPolicy, ConfigOutcome, create_default_config, discard_file, offer_config_rewrite, verify_config,
};
use self::endpoint::{
    RuntimeOutcome, clear_foreign_endpoint, clear_stale_endpoint, endpoint_health, reconcile_policy,
    stop_owned_appa_runtime, verify_runtime_deployment,
};
#[cfg(windows)]
use self::paths::windows_identity;
use self::paths::{DeploymentPaths, appa_filename, deployment_paths, same_file};
use self::receipt::{Receipt, Style, source_label};

const MARKETPLACE: &str = "appa";

const PLUGIN: &str = "appa-runtime@appa";

const RECOVERY_PREFIX: &str = ".appa-init-recovery-";

#[derive(Debug, Error)]
pub enum InitError {
    #[error("cannot find the current executable: {0}")]
    CurrentExecutable(std::io::Error),
    #[error("cannot find a home directory; set HOME or the relevant APPA directory variables")]
    MissingHome,
    #[error("cannot make the directory override {path} absolute: {source}")]
    AbsolutePath { path: PathBuf, source: std::io::Error },
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
    #[error("a different Appa runtime is already running at {endpoint}; {message}")]
    RuntimeIdentity { endpoint: String, message: String },
    #[error("the appa runtime (pid {pid}) still answers {endpoint} after being stopped; stop it and rerun init")]
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
    PluginBundle(#[from] PluginBundleError),
    #[error("{operation}; restoring the previous installation also failed: {recovery}")]
    PluginRecovery {
        operation: Box<InitError>,
        recovery: Box<InitError>,
    },
}

/// Install the plugin belonging to this binary's own release into Claude Code,
/// together with this binary, as one bundle.
///
/// The sequence is ordered so that nothing outside a temporary file changes
/// until the plugin source has been resolved and verified, and so that the
/// endpoint is cleared before Claude is switched over. Directories, the config
/// and the deployment are written before that clearing; all three are additive
/// and none of them is what Claude reads.
pub fn claude_code(explicit_source: Option<&str>) -> Result<String, InitError> {
    let appa = env::current_exe().map_err(InitError::CurrentExecutable)?;
    let endpoint = Endpoint::resolve()?;

    // 1. Resolve and verify the source. Nothing outside a temp file has changed.
    progress("resolving the matching plugin");
    let source = PluginSource::resolve(explicit_source)?;
    let paths = deployment_paths()?;
    let installations = installed_plugin_installations(&paths.claude_dir)?;
    let marketplaces = run_claude(["plugin", "marketplace", "list"], None)?;

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
        ConfigOutcome::Kept => offer_config_rewrite(&config)?,
        created => created,
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

    // 4. Clear the endpoint before Claude is switched over. A runtime that will not
    //    stop aborts init here, rather than leaving a new plugin registered
    //    against an old runtime that a rerun cannot dislodge.
    progress("checking the runtime endpoint");
    //    A runtime whose binary an install replaced on disk still owns the
    //    endpoint, and its health answer names the stale pid.
    clear_stale_endpoint(&endpoint)?;
    //    A healthy runtime from another build is stopped only after an explicit
    //    confirmation and only when it identifies a same-user appa pid.
    clear_foreign_endpoint(&appa, &config, &endpoint)?;

    // 5. Snapshot for recovery and disarm the launcher.
    let launcher_dir = appa.parent().unwrap_or(&paths.install_dir);
    let recovery = prepare_plugin_recovery(&installations, &paths.data_dir)?;
    if recovery.is_some() {
        install_disabled_clappa(launcher_dir)?;
    }

    // 6. The Claude switch, the binary, and the runtime this plugin is being
    //    bound to: one transaction. Verification is inside it, because a plugin
    //    left registered against a runtime that failed verification is exactly
    //    the skew this bundle exists to prevent. Every step records what it
    //    changed, and a failure unwinds those changes in reverse before the
    //    plugin switch itself is undone.
    progress("updating the Claude Code plugin");
    let mut compensation = Compensation::default();
    let switch = replace_plugin(&deployment.root, &marketplaces, &installations).and_then(|()| {
        switch_over(
            &appa,
            &deployed_appa,
            &config,
            &composed_policy,
            &endpoint,
            &paths,
            &mut compensation,
        )
    });
    let runtime_outcome = match switch {
        Ok(outcome) => {
            compensation.commit();
            outcome
        }
        Err(operation) => {
            // Both recoveries are attempted; the first failure is the one reported.
            let unwound = compensation.unwind();
            let restored = undo_plugin_switch(recovery.as_ref(), launcher_dir);
            if let Err(recovery_error) = unwound.and(restored) {
                return Err(InitError::PluginRecovery {
                    operation: Box::new(operation),
                    recovery: Box::new(recovery_error),
                });
            }
            return Err(operation);
        }
    };

    // 7. Only now is the launcher armed. Every earlier return leaves `clappa`
    //    absent on a first install and disabled on an upgrade, so a session
    //    started against a half-installed bundle cannot be a protected one.
    install_clappa(launcher_dir)?;
    cleanup_plugin_recovery(recovery.as_ref());

    Ok(Receipt {
        adapter: source_label(&source, &deployment),
        config,
        config_outcome,
        runtime_outcome,
    }
    .render(Style::of_stdout()))
}

/// The steps after the Claude switch, each recording what it changed.
///
/// The runtime this plugin is bound to must also be serving this deployment's
/// policy, so the reconcile is inside the transaction: a refusal there means the
/// endpoint belongs to someone else, and a plugin left registered against it is
/// the same skew as a plugin left registered against a runtime that failed
/// verification. A decline is not a refusal: it answers `Ok` and the install
/// stands.
fn switch_over(
    appa: &Path,
    deployed_appa: &Path,
    config: &Path,
    composed_policy: &ComposedPolicy,
    endpoint: &Endpoint,
    paths: &DeploymentPaths,
    compensation: &mut Compensation,
) -> Result<RuntimeOutcome, InitError> {
    install_runtime(appa, deployed_appa, compensation)?;
    let plugin_root = installed_plugin_root(&paths.claude_dir)?;
    install_statusline(&plugin_root, paths, compensation)?;
    progress("starting the runtime");
    // A runtime answering `ok` here was running before this init and stays the
    // user's; anything the starter brings up after silence is init's to stop.
    let running_before = endpoint_health(endpoint)?.is_some_and(|answer| answer == "ok");
    start_runtime(&plugin_root)?;
    let pid = verify_runtime_deployment(deployed_appa, config, endpoint)?;
    if !running_before {
        compensation.record(Undo::Runtime {
            pid,
            endpoint: endpoint.clone(),
        });
    }
    reconcile_policy(endpoint, config, composed_policy)
}

/// What the switch has changed on disk and in process state, so a failure can
/// put each change back in reverse order. The plugin registration itself is
/// undone separately by [`undo_plugin_switch`].
#[derive(Default)]
struct Compensation {
    done: Vec<Undo>,
}

enum Undo {
    /// The deployed binary's bytes before install_runtime replaced them, copied
    /// aside to `previous`; `None` when no binary was deployed.
    Binary { target: PathBuf, previous: Option<PathBuf> },
    /// A file the statusline install rewrote, with its bytes from before; `None`
    /// when it did not exist.
    File { path: PathBuf, before: Option<Vec<u8>> },
    /// A runtime this init started and verified as this deployment's.
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
            Undo::File { path, before } => {
                let write = |source| InitError::WriteFile {
                    path: path.clone(),
                    source,
                };
                match before {
                    Some(bytes) => fs::write(&path, bytes).map_err(write),
                    None => remove_if_present(&path).map_err(write),
                }
            }
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
fn file_before(path: &Path) -> Result<Option<Vec<u8>>, InitError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InitError::WriteFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn progress(message: &str) {
    eprintln!("appa init: {message}...");
}

/// Copy the binary to its deployed path, keeping the bytes it replaces beside it
/// as `appa.prev` until the install stands.
fn install_runtime(source: &Path, target: &Path, compensation: &mut Compensation) -> Result<(), InitError> {
    if same_file(source, target) {
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

/// What a yes-or-no question resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
}

/// A yes-or-no question put to the person running init, with the answer an
/// empty line means; the prompt capitalizes that choice.
struct Confirmation {
    question: String,
    default: Answer,
}

impl Confirmation {
    /// Ask on `output` and read one line from `input`. End of input, where no
    /// one is there to answer, is a no whatever the default.
    fn ask(&self, input: &mut impl BufRead, output: &mut impl Write) -> std::io::Result<Answer> {
        let choices = match self.default {
            Answer::Yes => "[Y/n]",
            Answer::No => "[y/N]",
        };
        write!(output, "{} {choices} ", self.question)?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(Answer::No);
        }
        Ok(match answer.trim().to_ascii_lowercase().as_str() {
            "" => self.default,
            "y" | "yes" => Answer::Yes,
            _ => Answer::No,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_answer_takes_the_default_and_end_of_input_refuses() {
        let ask = |default: Answer, answer: &str| {
            let confirmation = Confirmation {
                question: "continue?".to_owned(),
                default,
            };
            confirmation
                .ask(&mut answer.as_bytes(), &mut Vec::new())
                .expect("the answer reads")
        };
        for default in [Answer::Yes, Answer::No] {
            for answer in ["y\n", "YES\n", " yes \n"] {
                assert_eq!(ask(default, answer), Answer::Yes, "{answer:?} under {default:?}");
            }
            for answer in ["n\n", "no\n", "anything else\n", ""] {
                assert_eq!(ask(default, answer), Answer::No, "{answer:?} under {default:?}");
            }
            assert_eq!(ask(default, "\n"), default);
        }
    }
}
