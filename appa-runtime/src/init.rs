//! Native deployment bootstrap. The CLI installs machine state; harness skills only author policy.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Map, Value};
use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::default_config;
use crate::plugin_bundle::{self, Deployment, Endpoint, PluginBundleError, PluginSource, Population};

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

/// The policy key of the config file this init validated.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposedPolicy {
    /// The key, comparable against the one a runtime serves.
    Key(String),
    /// A `token_env` resolves only where the runtime runs, so this process cannot compose
    /// the file at all. Not knowing is never the same as agreeing: the runtime may be
    /// serving anything, and only the person running init can settle it.
    Unknowable,
}

/// Why a running runtime may not be answering under the file this init validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divergence {
    /// It serves a different policy than this file composes to.
    Serving,
    /// Whether it serves this file cannot be established here.
    Unestablished,
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
fn absolute_directory(path: PathBuf) -> Result<PathBuf, InitError> {
    std::path::absolute(&path).map_err(|source| InitError::AbsolutePath { path, source })
}

/// The platform config file used by installed deployments and `appa describe`.
pub fn installed_config_path() -> PathBuf {
    match installed_config_dir() {
        Ok(Some(directory)) => directory.join("appa.toml"),
        Ok(None) => PathBuf::from("appa.toml"),
        Err(error) => {
            tracing::warn!(%error, "falling back to the working directory for the config path");
            PathBuf::from("appa.toml")
        }
    }
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

fn source_label(source: &PluginSource, deployment: &Deployment) -> String {
    let origin = match source {
        PluginSource::Explicit(path) => format!("{} (development source)", friendly_path(path)),
        PluginSource::Release { reference, .. } => format!("appa {reference} release plugin"),
        PluginSource::Commit { commit, .. } => format!("OpenAPPA commit {}", &commit[..commit.len().min(12)]),
        PluginSource::Local { root, .. } => format!("{} (dirty development source)", friendly_path(root)),
    };
    format!("{origin} -> {}", friendly_path(&deployment.root))
}

/// Whether the receipt carries terminal escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Plain,
    Colored,
}

impl Style {
    fn of_stdout() -> Self {
        if std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none() {
            Style::Colored
        } else {
            Style::Plain
        }
    }
}

/// What an init decided, before any of it is words.
///
/// Everything the receipt can report is a field here, so what init keeps out of
/// its summary — install paths, deployment digests, the files it wrote — is
/// absent by construction rather than by a rendering step that must remember to
/// leave it out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Receipt {
    /// Where the plugin came from, as the user would name it.
    adapter: String,
    config: PathBuf,
    config_outcome: ConfigOutcome,
    runtime_outcome: RuntimeOutcome,
}

impl Receipt {
    fn render(&self, style: Style) -> String {
        let colored = style == Style::Colored;
        let title = if colored {
            "\u{1b}[1;32m✓ OpenAPPA initialized\u{1b}[0m \u{1b}[2mfor Claude Code\u{1b}[0m"
        } else {
            "OpenAPPA initialized for Claude Code"
        };
        let label = |name: &str| {
            if colored {
                format!("\u{1b}[1;36m{name:<9}\u{1b}[0m")
            } else {
                format!("{name:<9}")
            }
        };
        let mut receipt = format!(
            "{title}\n\n  {} {}\n  {} {PLUGIN}\n  {} {}\n  {} {} ({})\n  {} clappa\n",
            label("Adapter"),
            self.adapter,
            label("Plugin"),
            label("Runtime"),
            self.runtime_outcome.as_str(),
            label("Config"),
            friendly_path(&self.config),
            self.config_outcome.as_str(),
            label("Launcher"),
        );
        // A session loads its hooks at session start, and the hook wire carries no
        // version, so a session running across an upgrade keeps talking to the
        // runtime it started with.
        receipt.push_str("\nRestart any running `clappa` session to pick this up.\n");
        receipt.push_str("\nNext: run `clappa`, then `/appa-guide init`.\n");
        receipt
    }
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

fn deployment_paths() -> Result<DeploymentPaths, InitError> {
    let home = user_home();
    let config_dir = installed_config_dir()?.ok_or(InitError::MissingHome)?;
    let data_dir = installed_data_dir()?.ok_or(InitError::MissingHome)?;
    let install_dir = if let Some(path) = env::var_os("APPA_INSTALL_DIR") {
        absolute_directory(PathBuf::from(path))?
    } else if cfg!(windows) {
        data_dir.join("bin")
    } else {
        home.as_ref().ok_or(InitError::MissingHome)?.join(".local/bin")
    };
    let claude_dir = match env::var_os("CLAUDE_CONFIG_DIR") {
        Some(path) => absolute_directory(PathBuf::from(path))?,
        None => home.ok_or(InitError::MissingHome)?.join(".claude"),
    };
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

fn installed_config_dir() -> Result<Option<PathBuf>, InitError> {
    if let Some(path) = env::var_os("APPA_CONFIG_DIR") {
        return absolute_directory(PathBuf::from(path)).map(Some);
    }
    #[cfg(target_os = "macos")]
    return Ok(env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa")));
    #[cfg(target_os = "windows")]
    return Ok(env::var_os("APPDATA").map(PathBuf::from).map(|path| path.join("appa")));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Ok(env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config/appa"))
        }))
}

fn installed_data_dir() -> Result<Option<PathBuf>, InitError> {
    if let Some(path) = env::var_os("APPA_DATA_DIR") {
        return absolute_directory(PathBuf::from(path)).map(Some);
    }
    #[cfg(target_os = "macos")]
    return Ok(env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/appa")));
    #[cfg(target_os = "windows")]
    return Ok(env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("appa")));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Ok(env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("appa"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share/appa"))
        }))
}

fn appa_filename() -> &'static str {
    if cfg!(windows) { "appa.exe" } else { "appa" }
}

/// The comparison operand on Windows: the fully resolved path, folded for the
/// case-insensitive filesystem.
///
/// Both sides are canonicalized, rather than trusting `Get-Process.Path` to
/// return one particular form. `fs::canonicalize` yields the Win32 final path,
/// whose extended-length prefix is stripped so the two sides can be compared as
/// written. This is equality of resolved final paths, not file-ID identity, so
/// two hard links to one file at different paths compare unequal: such a pair is
/// reported as two deployments rather than one.
#[cfg(windows)]
fn windows_identity(path: &Path) -> Option<String> {
    let canonical = fs::canonicalize(path).ok()?;
    let text = canonical.to_str()?;
    Some(text.strip_prefix(r"\\?\").unwrap_or(text).to_lowercase())
}

/// Whether two paths name the same existing file, resolving symlinks. On Unix
/// `(dev, ino)` identity also names hard links to one file as that file.
fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
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

/// Remove a file this init wrote and abandons. Nothing it protects is lost
/// with it, so a failure is noted beside the error being returned.
fn discard_file(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "cannot remove a file init abandoned");
    }
}

/// Seed the config from this build's default, or keep the one already there.
fn create_default_config(path: &Path) -> Result<ConfigOutcome, InitError> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(ConfigOutcome::Kept),
        Err(source) => {
            return Err(InitError::WriteFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if let Err(source) = file
        .write_all(default_config::text().as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        discard_file(path);
        return Err(InitError::WriteFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(ConfigOutcome::Created)
}

/// The policy version this build's default config declares.
fn template_policy_version() -> i64 {
    policy_version(&default_config::text()).expect("the bundled default config declares an integer policy version")
}

/// The `[policy] version` of one config's own text, before any include composes.
fn policy_version(text: &str) -> Option<i64> {
    toml::from_str::<toml::Value>(text)
        .ok()?
        .get("policy")?
        .get("version")?
        .as_integer()
}

/// Find a backup name without replacing an earlier backup.
fn available_backup_path(path: &Path) -> PathBuf {
    let backup = path.with_extension("toml.bak");
    if !backup.exists() {
        return backup;
    }
    for number in 1.. {
        let candidate = path.with_extension(format!("toml.bak.{number}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unsigned integer always has another value")
}

/// Offer to replace a config authored against an older policy model.
///
/// The config is the user's, and init keeps it across every upgrade. A policy
/// version below this build's is the one mechanical signal that it was authored
/// against an older model than this build writes, so it is also the only drift
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
    let backup = available_backup_path(path);
    let rewrite = Confirmation {
        question: format!(
            "appa: {} uses an older policy format. This version of Appa uses policy version {template}.\n\
             Replace it with the new default policy? Your existing file will be backed up to {},\n\
             without replacing any existing backup.",
            friendly_path(path),
            friendly_path(&backup),
        ),
        default: Answer::No,
    };
    let prompt = |source| InitError::WriteFile {
        path: path.to_path_buf(),
        source,
    };
    if rewrite.ask(input, output).map_err(prompt)? == Answer::No {
        return Ok(ConfigOutcome::Kept);
    }
    // Reserve this unused name before moving the user's file. `rename` can replace a
    // destination, so reserving it makes that replacement safe and prevents an
    // existing backup from being lost.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .map_err(|source| InitError::WriteFile {
            path: backup.clone(),
            source,
        })?;
    // The original moves aside whole, so nothing here can leave a half-written
    // policy in place: the new file is written under `create_new` and removed
    // again if that write fails, and a failure puts the original back.
    if let Err(source) = fs::rename(path, &backup) {
        discard_file(&backup);
        return Err(InitError::WriteFile {
            path: backup.clone(),
            source,
        });
    }
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
/// Answers with the policy key this file composes to, or [`ComposedPolicy::Unknowable`]
/// when the file resolves only where the runtime runs.
fn verify_config(path: &Path) -> Result<ComposedPolicy, InitError> {
    let config = match Config::load(path) {
        Ok(config) => config,
        // A `token_env` resolves where the runtime runs, not here. A hook starts
        // it with the session's environment, which carries variables this
        // terminal does not, so a secret this process cannot see is not init's
        // to refuse: the start that follows is what proves the token reachable.
        Err(ConfigError::MissingSecret { .. }) => return Ok(ComposedPolicy::Unknowable),
        Err(source) => {
            return Err(InitError::UnloadableConfig {
                path: path.to_path_buf(),
                source: Box::new(source),
            });
        }
    };
    Ok(ComposedPolicy::Key(crate::engine::policy_file_key(
        config.policy_file().bytes(),
    )))
}

/// Run one `claude` command, from `directory` when a project-scoped plugin
/// installation names one, and answer with its output only when it succeeded.
fn run_claude<A: AsRef<OsStr>>(
    arguments: impl IntoIterator<Item = A>,
    directory: Option<&Path>,
) -> Result<Output, InitError> {
    let arguments: Vec<A> = arguments.into_iter().collect();
    let command = arguments
        .iter()
        .map(|argument| argument.as_ref().to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let mut process = Command::new("claude");
    process.args(&arguments);
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
        run_claude(
            ["plugin", "uninstall", PLUGIN, "--scope", &installation.scope, "--yes"],
            installation.project_path.as_deref(),
        )?;
    }
    let listed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&marketplaces.stdout),
        String::from_utf8_lossy(&marketplaces.stderr)
    );
    if listed.lines().any(is_appa_marketplace_line) {
        run_claude(["plugin", "marketplace", "remove", MARKETPLACE], None)?;
    }
    run_claude(
        [
            OsStr::new("plugin"),
            OsStr::new("marketplace"),
            OsStr::new("add"),
            deployment.as_os_str(),
        ],
        None,
    )?;
    run_claude(["plugin", "install", PLUGIN, "--scope", "user"], None)?;
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
    let manifest_path = marketplace.join(".claude-plugin/marketplace.json");
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
            run_claude(["plugin", "uninstall", PLUGIN, "--scope", "user", "--yes"], None)?;
            run_claude(["plugin", "marketplace", "remove", MARKETPLACE], None)?;
            Ok(())
        }
    }
}

/// Put the installation this init replaced back from its rollback source.
/// Clearing whatever the failed switch left registered is best effort: what
/// matters is that the add and the installs that follow succeed.
fn restore_plugin(recovery: &PluginRecovery) -> Result<(), InitError> {
    for installation in &recovery.installations {
        let cleared = run_claude(
            ["plugin", "uninstall", PLUGIN, "--scope", &installation.scope, "--yes"],
            installation.project_path.as_deref(),
        );
        if let Err(error) = cleared {
            tracing::warn!(scope = %installation.scope, %error, "cannot clear the plugin before restoring it");
        }
    }
    if let Err(error) = run_claude(["plugin", "marketplace", "remove", MARKETPLACE], None) {
        tracing::warn!(%error, "cannot clear the marketplace before restoring it");
    }
    run_claude(
        [
            OsStr::new("plugin"),
            OsStr::new("marketplace"),
            OsStr::new("add"),
            recovery.marketplace.as_os_str(),
        ],
        None,
    )?;
    for installation in &recovery.installations {
        run_claude(
            ["plugin", "install", PLUGIN, "--scope", &installation.scope],
            installation.project_path.as_deref(),
        )?;
    }
    Ok(())
}

/// Remove this invocation's rollback source. Another init's, live or crashed,
/// is not this one's to judge.
fn cleanup_plugin_recovery(recovery: Option<&PluginRecovery>) {
    if let Some(recovery) = recovery
        && let Err(error) = fs::remove_dir_all(&recovery.marketplace)
    {
        tracing::warn!(path = %recovery.marketplace.display(), %error, "cannot remove the rollback source");
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

/// Install the platform statusline and point Claude's settings at it, unless a
/// statusline that is not APPA's is configured, which is left alone.
fn install_statusline(
    plugin_root: &Path,
    paths: &DeploymentPaths,
    compensation: &mut Compensation,
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
    for path in [&target, &settings_path] {
        compensation.record(Undo::File {
            path: path.clone(),
            before: file_before(path)?,
        });
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

/// Run the installed plugin's starter, which brings up the deployed runtime
/// when nothing healthy answers the endpoint.
fn start_runtime(plugin_root: &Path) -> Result<(), InitError> {
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
        return Ok(());
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
/// `APPA_DATA_DIR` is foreign: it is named, and stopped only after the user
/// confirms it and only when it identifies a same-user `appa` pid. Stale
/// runtimes are cleared before this classification through their separate
/// health protocol.
#[derive(Debug, PartialEq, Eq)]
enum EndpointOwner {
    /// Nothing answered, or what answered serves no fingerprint. Before the
    /// start this is the ordinary case; after it, it is a failure.
    Unidentified,
    /// The binary whose bytes were offered for comparison, serving this
    /// configuration, in the process it names.
    Deployment { pid: i32 },
    /// A different build or a different configuration, naming the pid that
    /// serves it.
    Foreign { pid: i32 },
}

/// How long init waits for a runtime it asked to stop, and how often it looks.
///
/// Every wait for a stopping runtime uses the same budget: a runtime that outlives
/// one of them has outlived all of them, and `RuntimeSurvived` means the same thing
/// wherever it is raised.
const STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Every question init asks a runtime goes out through here, so the flags that
/// decide how long init waits and whether curl reports its own failure have one
/// definition rather than one per question.
fn ask_endpoint(endpoint: &Endpoint, path: &str, arguments: &[&str]) -> std::io::Result<Output> {
    Command::new("curl").args(arguments).arg(endpoint.join(path)).output()
}

fn endpoint_health(endpoint: &Endpoint) -> Result<Option<String>, InitError> {
    let output = ask_endpoint(endpoint, "/health", &["--fail", "--silent", "--max-time", "2"]).map_err(|error| {
        InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: error.to_string(),
        }
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
/// The runtime's own health protocol names its pid. The pid is not trusted by
/// itself; init applies the same same-user/process-name check as the shipped
/// starter before sending a signal. An `ok`, malformed, or absent health answer
/// never grants shutdown authority.
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
    terminate_owned_appa_runtime(pid, endpoint)?;

    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(ref current) if current == "ok" => return Ok(()),
            Some(ref current) if stale_pid(current) == Some(pid) => {
                std::thread::sleep(STOP_POLL);
            }
            Some(_) => {
                return Err(InitError::RuntimeIdentity {
                    endpoint: endpoint.url().to_owned(),
                    message: format!("the endpoint changed ownership while stale pid {pid} was stopping"),
                });
            }
        }
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

/// Signal `pid`, confirming immediately before that it is still this user's
/// appa runtime: the check-to-signal window is what a forged answer would use.
fn terminate_owned_appa_runtime(pid: i32, endpoint: &Endpoint) -> Result<(), InitError> {
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("pid {pid} is not this user's appa runtime; not stopping it"),
        });
    }
    terminate_appa_pid(pid)
}

/// Stop a runtime this init started, and wait for its process to go.
fn stop_owned_appa_runtime(pid: i32, endpoint: &Endpoint) -> Result<(), InitError> {
    terminate_owned_appa_runtime(pid, endpoint)?;
    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        if !process_exists(pid) {
            return Ok(());
        }
        std::thread::sleep(STOP_POLL);
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
fn process_exists(pid: i32) -> bool {
    powershell(
        "if (Get-Process -Id $env:APPA_STALE_PID -ErrorAction SilentlyContinue) { 'alive' }",
        [("APPA_STALE_PID", pid.to_string())],
    )
    .is_ok_and(|answer| answer.trim() == "alive")
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
        path: PathBuf::from(format!("pid {pid}")),
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

/// Who owns the endpoint: this deployment, another one, or a process that will not say.
///
/// A deployment is a build *and* the configuration it serves. Comparing builds alone makes
/// every install of one build look like the same deployment, which is how an install ends
/// up reloading, and reporting on, a runtime that is not its own.
fn endpoint_owner(binary: &Path, config: &Path, endpoint: &Endpoint) -> Result<EndpointOwner, InitError> {
    let expected = crate::runtime_cli::binary_digest(binary).map_err(|source| InitError::InstallRuntime {
        path: binary.to_path_buf(),
        source,
    })?;
    let output = ask_endpoint(
        endpoint,
        "/binary-fingerprint",
        &["--fail", "--silent", "--max-time", "2"],
    )
    .map_err(|error| InitError::RuntimeIdentity {
        endpoint: endpoint.url().to_owned(),
        message: error.to_string(),
    })?;
    if !output.status.success() {
        return Ok(EndpointOwner::Unidentified);
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    classify_endpoint_owner(&expected, config, endpoint, &answer)
}

/// A process is this deployment only when it names both this build and this configuration.
/// Anything else — a different build, a different config, or an answer that names no
/// config at all — is another deployment, to be stopped before this install proceeds.
/// Either way the answer must name the pid that serves it: a runtime that cannot be
/// stopped by pid can be neither cleared nor rolled back.
fn classify_endpoint_owner(
    expected: &str,
    config: &Path,
    endpoint: &Endpoint,
    answer: &str,
) -> Result<EndpointOwner, InitError> {
    let (identity, rest) = answer.split_once('\n').unwrap_or((answer, ""));
    let mut fields = identity.split_whitespace();
    let actual = fields.next().unwrap_or_default();
    let pid = fields
        .next()
        .and_then(positive_pid)
        .ok_or_else(|| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "the answering runtime does not identify its pid; stop it and rerun init".to_owned(),
        })?;
    // Everything after the first newline is the path, less the one the transport appends:
    // a config path may itself hold a newline, and splitting again would truncate it.
    let serves = rest.strip_suffix('\n').unwrap_or(rest);
    if actual == expected && !serves.is_empty() && Path::new(serves) == config {
        Ok(EndpointOwner::Deployment { pid })
    } else {
        Ok(EndpointOwner::Foreign { pid })
    }
}

fn confirm_stop(pid: i32, endpoint: &Endpoint) -> Result<Answer, InitError> {
    let stop = Confirmation {
        question: format!(
            "appa: another appa deployment (pid {pid}) owns {}. Stop it and continue?",
            endpoint.url()
        ),
        default: Answer::Yes,
    };
    let stdin = std::io::stdin();
    let stderr = std::io::stderr();
    stop.ask(&mut stdin.lock(), &mut stderr.lock())
        .map_err(|source| InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("cannot ask permission to stop pid {pid}: {source}"),
        })
}

/// Clear a foreign owner while Claude and the launcher are still untouched.
///
/// Silence is accepted because that is a first install. A foreign process is
/// eligible only when the APPA identity response names a same-user `appa`
/// process and the user confirms the stop. The identity is checked again
/// immediately before signalling to close the prompt-to-kill race.
fn clear_foreign_endpoint(binary: &Path, config: &Path, endpoint: &Endpoint) -> Result<(), InitError> {
    match endpoint_owner(binary, config, endpoint)? {
        EndpointOwner::Deployment { .. } | EndpointOwner::Unidentified => Ok(()),
        EndpointOwner::Foreign { pid } => clear_confirmed_foreign_with(binary, config, endpoint, pid, confirm_stop),
    }
}

fn clear_confirmed_foreign_with(
    binary: &Path,
    config: &Path,
    endpoint: &Endpoint,
    pid: i32,
    confirm: impl FnOnce(i32, &Endpoint) -> Result<Answer, InitError>,
) -> Result<(), InitError> {
    if !is_owned_appa_runtime(pid)? {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!(
                "a different build names pid {pid}, but it is not this user's appa runtime; not stopping it"
            ),
        });
    }
    if confirm(pid, endpoint)? == Answer::No {
        return Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: format!("another appa deployment (pid {pid}) still owns this endpoint; init cancelled"),
        });
    }
    match endpoint_owner(binary, config, endpoint)? {
        EndpointOwner::Unidentified => return Ok(()),
        EndpointOwner::Deployment { .. } => return Ok(()),
        EndpointOwner::Foreign { pid: current } if current == pid => {}
        EndpointOwner::Foreign { .. } => {
            return Err(InitError::RuntimeIdentity {
                endpoint: endpoint.url().to_owned(),
                message: "the endpoint changed ownership after approval; not stopping either process".to_owned(),
            });
        }
    }
    terminate_owned_appa_runtime(pid, endpoint)?;

    let deadline = std::time::Instant::now() + STOP_DEADLINE;
    while std::time::Instant::now() < deadline {
        match endpoint_health(endpoint)? {
            None => return Ok(()),
            Some(_) => std::thread::sleep(STOP_POLL),
        }
    }
    Err(InitError::RuntimeSurvived {
        pid,
        endpoint: endpoint.url().to_owned(),
    })
}

/// Reconcile the policy a surviving runtime serves with the file this init validated.
///
/// The starter replaces a runtime whose executable changed, and that fresh process loads
/// this file itself. A runtime it left running does not: it still serves what it loaded at
/// startup, and a config written since is on disk only. Comparing the two keys keeps the
/// question to the case that has one — an install that changed nothing asks nothing.
fn reconcile_policy(
    endpoint: &Endpoint,
    config: &Path,
    composed: &ComposedPolicy,
) -> Result<RuntimeOutcome, InitError> {
    let Some(divergence) = policy_divergence(composed, &serving_policy_key(endpoint)?) else {
        return Ok(RuntimeOutcome::Healthy);
    };
    if confirm_reload(config, divergence)? == Answer::No {
        return Ok(RuntimeOutcome::OlderPolicy);
    }
    reload_policy(endpoint, config)?;
    Ok(RuntimeOutcome::Reloaded)
}

/// Why a serving runtime may not be answering under the file this init validated, or
/// `None` when it demonstrably is.
///
/// A config init cannot compose is not settled by assumption: the runtime can be asked,
/// and only a person can decide, so the question is put. The reload itself resolves the
/// secret where the runtime runs, which is the environment that has it.
fn policy_divergence(composed: &ComposedPolicy, serving: &str) -> Option<Divergence> {
    match composed {
        ComposedPolicy::Key(key) if key == serving => None,
        ComposedPolicy::Key(_) => Some(Divergence::Serving),
        ComposedPolicy::Unknowable => Some(Divergence::Unestablished),
    }
}

/// The policy key the endpoint answers under. A runtime that does not answer for one
/// cannot be reconciled, and a plugin bound to it is the skew init exists to prevent.
fn serving_policy_key(endpoint: &Endpoint) -> Result<String, InitError> {
    let refused = |message: String| InitError::PolicyKey {
        endpoint: endpoint.url().to_owned(),
        message,
    };
    let output = ask_endpoint(
        endpoint,
        "/policy-key",
        &["--fail", "--silent", "--show-error", "--max-time", "2"],
    )
    .map_err(|error| refused(error.to_string()))?;
    if !output.status.success() {
        return Err(refused(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(refused("the answer names no policy key".to_owned()));
    }
    Ok(key)
}

/// Ask the running runtime to serve the configuration on disk.
///
/// The endpoint's owner is this deployment by the time this runs, so the reload reads this
/// deployment's file. The runtime validates it again before it swaps: a refusal here is a
/// fault worth naming, not a receipt footnote, because the older policy keeps serving.
fn reload_policy(endpoint: &Endpoint, config: &Path) -> Result<(), InitError> {
    let refused = |message: String| InitError::ReloadRefused {
        endpoint: endpoint.url().to_owned(),
        path: config.to_path_buf(),
        message,
    };
    let output = ask_endpoint(
        endpoint,
        "/reload",
        &["--fail", "--silent", "--show-error", "--max-time", "10", "-X", "POST"],
    )
    .map_err(|error| refused(error.to_string()))?;
    if !output.status.success() {
        return Err(refused(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
    }
    Ok(())
}

/// A terminal is asked; anything else reloads. A script that just wrote a config wants it
/// serving, and there is no one there to answer.
fn confirm_reload(config: &Path, divergence: Divergence) -> Result<Answer, InitError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(Answer::Yes);
    }
    let prompt = |source| InitError::WriteFile {
        path: config.to_path_buf(),
        source,
    };
    let config = friendly_path(config);
    // Each case states exactly what init established, and no more: one knows the running
    // runtime serves something else, the other knows only that it cannot tell.
    let established = match divergence {
        Divergence::Serving => {
            format!("appa: the running runtime still serves the policy it started with, not {config}.")
        }
        Divergence::Unestablished => format!(
            "appa: {config} resolves a secret only where the runtime runs, so this cannot tell\n\
             whether the running runtime already serves it."
        ),
    };
    let reload = Confirmation {
        question: format!(
            "{established}\nReload it now? Sessions open right now keep the deployment they started with."
        ),
        default: Answer::Yes,
    };
    let stderr = std::io::stderr();
    reload.ask(&mut stdin.lock(), &mut stderr.lock()).map_err(prompt)
}

/// The endpoint answers for this deployment: this build, serving this configuration.
/// Answers with the pid of the process serving it.
fn verify_runtime_deployment(runtime: &Path, config: &Path, endpoint: &Endpoint) -> Result<i32, InitError> {
    match endpoint_owner(runtime, config, endpoint)? {
        EndpointOwner::Deployment { pid } => Ok(pid),
        EndpointOwner::Unidentified => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "stop it, then run `appa init` again.".to_owned(),
        }),
        EndpointOwner::Foreign { .. } => Err(InitError::RuntimeIdentity {
            endpoint: endpoint.url().to_owned(),
            message: "another appa deployment is answering; stop that process and rerun init".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in the ownership check can find and the signal can reach.
    ///
    /// macOS kills a copied platform binary outright -- a copy of `/bin/sh` or
    /// `/bin/sleep` dies with SIGKILL before it runs a single instruction -- so
    /// a test built on one would pass without ever exercising the stop,
    /// because a killed process is also a stopped one. `perl` copies and runs.
    #[cfg(unix)]
    const STAND_IN: &str = "/usr/bin/perl";

    /// What every starter runs: the subcommand plus the endpoint it binds.
    #[cfg(unix)]
    const RUNTIME_ARGUMENTS: &[&str] = &["runtime", "--listen", "127.0.0.1:8787"];

    /// A process whose executable really *is* `at`, started with `arguments`, so
    /// the ownership check sees the process name it decides on.
    ///
    /// The stand-in is reaped on its own thread. A dead child that nobody has
    /// waited for is a zombie, and `kill(pid, 0)` still succeeds on one, so
    /// without the reaper these tests could not tell a stopped process from a
    /// running one. In production a stopped runtime is never init's child.
    #[cfg(unix)]
    fn process_executing(at: &Path, arguments: &[&str]) -> Option<i32> {
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
        let script = "open(my $f, '>', $ARGV[0]) or die; close $f; sleep 30";
        let mut child = Command::new(at)
            .args(["-e", script])
            .arg(&ready)
            .args(arguments)
            .spawn()
            .expect("the stand-in process starts");
        let pid = child.id() as i32;
        std::thread::spawn(move || child.wait());

        let deadline = std::time::Instant::now() + STOP_DEADLINE;
        while std::time::Instant::now() < deadline {
            if ready.is_file() {
                return Some(pid);
            }
            std::thread::sleep(STOP_POLL);
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

    /// A loopback fixture serving `answers` in turn, with the request lines it served. A
    /// probe's path is part of the contract it has with the runtime, so a test that cares
    /// which endpoint init asks reads them.
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
    fn only_this_build_serving_this_config_is_this_deployment() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8787").expect("the endpoint parses");
        let classify = |expected: &str, config: &Path, answer: &str| {
            classify_endpoint_owner(expected, config, &endpoint, answer).expect("the answer classifies")
        };
        let mine = Path::new("/home/user/config/appa.toml");
        assert_eq!(
            classify("same", mine, "same 42\n/home/user/config/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        // The build alone never settles it: one build serves as many deployments as there
        // are configurations, and each is a stranger to the others.
        assert_eq!(
            classify("same", mine, "same 42\n/home/other/config/appa.toml"),
            EndpointOwner::Foreign { pid: 42 }
        );
        // A path with spaces is one path, not two fields.
        let spaced = Path::new("/home/user/Application Support/appa.toml");
        assert_eq!(
            classify("same", spaced, "same 42\n/home/user/Application Support/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        // On Unix a directory name may hold a newline, so the path is read as the whole
        // remainder of the answer the runtime composes — and a transport that appends a
        // newline of its own does not turn one deployment into a stranger.
        let newlined = Path::new("/home/user/two\nlines/appa.toml");
        assert_eq!(
            classify("same", newlined, "same 42\n/home/user/two\nlines/appa.toml"),
            EndpointOwner::Deployment { pid: 42 }
        );
        assert_eq!(
            classify("same", mine, "same 42\n/home/user/config/appa.toml\n"),
            EndpointOwner::Deployment { pid: 42 }
        );
        assert_eq!(
            classify("same", mine, "different 42\n/home/user/config/appa.toml"),
            EndpointOwner::Foreign { pid: 42 }
        );
        // An answer that names no configuration cannot claim to be this deployment, and
        // one that names no pid cannot be stopped, so it is refused outright.
        assert_eq!(classify("same", mine, "same 42"), EndpointOwner::Foreign { pid: 42 });
        assert!(matches!(
            classify_endpoint_owner("same", mine, &endpoint, "different"),
            Err(InitError::RuntimeIdentity { .. })
        ));
    }

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

    #[test]
    fn a_serving_runtime_is_reconciled_only_when_agreement_is_not_established() {
        let key = |key: &str| ComposedPolicy::Key(key.to_string());
        assert_eq!(
            policy_divergence(&key("composed"), "serving"),
            Some(Divergence::Serving)
        );
        // An install that changed nothing must ask nothing.
        assert_eq!(policy_divergence(&key("same"), "same"), None);
        // A config this process cannot compose is unsettled, never settled: assuming
        // agreement here is what would leave an older policy serving unremarked.
        assert_eq!(
            policy_divergence(&ComposedPolicy::Unknowable, "serving"),
            Some(Divergence::Unestablished)
        );
    }

    #[test]
    fn a_serving_policy_key_is_read_from_the_policy_route() {
        let (endpoint, asked) = recorded_answers(vec!["c54f1509".to_string()]);
        assert_eq!(serving_policy_key(&endpoint).expect("the key reads"), "c54f1509");
        assert_eq!(
            asked.lock().expect("the request recorder is never poisoned").as_slice(),
            ["GET /policy-key HTTP/1.1".to_string()],
            "the probe reads the policy route, and reads it without mutating"
        );
    }

    /// A runtime that answers nothing usable, and a port nothing answers on, both
    /// refuse init: a plugin bound to a runtime whose policy cannot be established is
    /// the skew init exists to prevent.
    #[test]
    fn a_runtime_that_does_not_answer_for_its_policy_refuses_init() {
        let blank = recorded_answers(vec![String::new()]).0;
        assert!(matches!(serving_policy_key(&blank), Err(InitError::PolicyKey { .. })));
        let unbound = Endpoint::parse("http://127.0.0.1:1").expect("the endpoint parses");
        assert!(matches!(serving_policy_key(&unbound), Err(InitError::PolicyKey { .. })));
    }

    #[test]
    fn a_matching_policy_key_reconciles_without_asking_or_reloading() {
        // One answer is served: the key probe. A reload would need a second connection,
        // so reaching one at all would hang rather than pass.
        let endpoint = recorded_answers(vec!["agreed".to_string()]).0;
        let config = PathBuf::from("/home/user/config/appa.toml");
        assert_eq!(
            reconcile_policy(&endpoint, &config, &ComposedPolicy::Key("agreed".to_string()))
                .expect("the reconcile completes"),
            RuntimeOutcome::Healthy
        );
    }

    #[test]
    fn a_reload_that_installed_this_deployments_policy_is_reported_as_reloaded() {
        let (endpoint, _asked) = recorded_answers(vec![
            "older".to_string(),
            r#"{"policy_key":"this-deployment","policy_identity":"x","changed":true}"#.to_string(),
        ]);
        let config = PathBuf::from("/home/user/config/appa.toml");
        assert_eq!(
            reconcile_policy(&endpoint, &config, &ComposedPolicy::Key("this-deployment".to_string()))
                .expect("the reconcile completes"),
            RuntimeOutcome::Reloaded
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_approved_foreign_appa_runtime_is_stopped() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let foreign = directory.path().join("foreign/appa");
        let Some(pid) = process_executing(&foreign, RUNTIME_ARGUMENTS) else {
            return;
        };
        let candidate = directory.path().join("candidate-appa");
        fs::write(&candidate, "a different candidate build").expect("the candidate binary exists");
        let endpoint = recorded_answers(vec![format!("different-fingerprint {pid}")]).0;

        let config = directory.path().join("appa.toml");
        clear_confirmed_foreign_with(&candidate, &config, &endpoint, pid, |approved_pid, _| {
            assert_eq!(approved_pid, pid);
            Ok(Answer::Yes)
        })
        .expect("the approved foreign runtime stops");

        assert!(!still_running(pid), "the approved runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn init_reclaims_an_unlinked_runtime_named_by_its_stale_health_answer() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let replaced = directory.path().join("bin/appa");
        let Some(pid) = process_executing(&replaced, RUNTIME_ARGUMENTS) else {
            return;
        };
        fs::remove_file(&replaced).expect("the installed binary is unlinked while its runtime remains");
        let endpoint = recorded_answers(vec![format!("stale {pid}"), format!("stale {pid}")]).0;

        clear_stale_endpoint(&endpoint).expect("init stops its stale unlinked runtime");

        assert!(!still_running(pid), "the stale runtime still owns its process");
    }

    #[cfg(unix)]
    #[test]
    fn a_spoofed_stale_pid_does_not_grant_process_shutdown() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let other = directory.path().join("bin/not-appa");
        let Some(pid) = process_executing(&other, RUNTIME_ARGUMENTS) else {
            return;
        };
        let endpoint = recorded_answers(vec![format!("stale {pid}")]).0;

        let refused = clear_stale_endpoint(&endpoint);

        assert!(matches!(refused, Err(InitError::RuntimeIdentity { .. })));
        assert!(still_running(pid), "a non-appa process was terminated");
        unsafe { libc::kill(pid, libc::SIGKILL) };
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

        assert_eq!(
            create_default_config(&config).expect("the default config is written"),
            ConfigOutcome::Created
        );
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
        assert_eq!(
            fs::read_to_string(&config).ok(),
            Some(default_config::text().into_owned())
        );
        assert_eq!(fs::read_to_string(&backup).ok(), Some(authored));
        verify_config(&config).expect("the rewritten config composes");
    }

    #[test]
    fn a_current_config_is_never_offered_for_rewrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = directory.path().join("appa.toml");
        assert_eq!(
            create_default_config(&config).expect("the default config is written"),
            ConfigOutcome::Created
        );

        let (outcome, prompt) = answer_rewrite(&config, "y\n");
        assert!(prompt.is_empty(), "no offer is made");
        assert_eq!(outcome, ConfigOutcome::Kept);
        assert_eq!(
            fs::read_to_string(&config).ok(),
            Some(default_config::text().into_owned())
        );
        assert!(!directory.path().join("appa.toml.bak").exists());
    }

    /// A receipt reports a path relative to the home directory it is under, so a
    /// summary read aloud or pasted into an issue carries no account name.
    #[test]
    fn a_receipt_shortens_a_path_under_the_home_directory_and_leaves_others_whole() {
        let Some(home) = user_home() else {
            return;
        };

        assert_eq!(friendly_path(&home), "~");
        assert_eq!(friendly_path(&home.join("config/appa.toml")), "~/config/appa.toml");
        assert_eq!(friendly_path(Path::new("/etc/appa/appa.toml")), "/etc/appa/appa.toml");
    }

    /// `Style` is the only thing that decides whether escapes are emitted, and it
    /// decides it for the whole receipt rather than per line.
    #[test]
    fn only_a_colored_style_puts_escapes_in_a_receipt() {
        let receipt = Receipt {
            adapter: "current checkout".to_owned(),
            config: PathBuf::from("/etc/appa/appa.toml"),
            config_outcome: ConfigOutcome::Kept,
            runtime_outcome: RuntimeOutcome::Healthy,
        };

        assert!(!receipt.render(Style::Plain).contains('\u{1b}'));
        assert!(receipt.render(Style::Colored).contains('\u{1b}'));
    }

    /// Every outcome pair a run can end in renders, and no two of them render the
    /// same receipt: a user cannot be shown "kept" for a config that was rewritten.
    #[test]
    fn each_pair_of_outcomes_renders_a_distinct_receipt() {
        let mut seen = std::collections::HashSet::new();
        for config_outcome in [ConfigOutcome::Created, ConfigOutcome::Kept, ConfigOutcome::Rewritten] {
            for runtime_outcome in [
                RuntimeOutcome::Healthy,
                RuntimeOutcome::Reloaded,
                RuntimeOutcome::OlderPolicy,
            ] {
                let receipt = Receipt {
                    adapter: "current checkout".to_owned(),
                    config: PathBuf::from("/etc/appa/appa.toml"),
                    config_outcome,
                    runtime_outcome,
                };
                assert!(
                    seen.insert(receipt.render(Style::Plain)),
                    "{config_outcome:?} with {runtime_outcome:?} renders as another pair does",
                );
            }
        }
    }
}
