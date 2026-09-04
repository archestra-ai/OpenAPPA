//! Everything that goes through the `claude` CLI: the plugin registry, the
//! marketplace this init publishes, and the rollback source it keeps.

use serde_json::{Map, Value};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::paths::DeploymentPaths;
use super::{Compensation, InitError, MARKETPLACE, PLUGIN, RECOVERY_PREFIX, Undo, file_before, install_clappa};

/// Run one `claude` command, from `directory` when a project-scoped plugin
/// installation names one, and answer with its output only when it succeeded.
pub(super) fn run_claude<A: AsRef<OsStr>>(
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
pub(super) struct PluginInstallation {
    pub(super) scope: String,
    pub(super) project_path: Option<PathBuf>,
    pub(super) install_path: Option<PathBuf>,
}

pub(super) fn installed_plugin_installations(claude_dir: &Path) -> Result<Vec<PluginInstallation>, InitError> {
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

pub(super) fn installed_plugin_root(claude_dir: &Path) -> Result<PathBuf, InitError> {
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

pub(super) struct PluginRecovery {
    pub(super) marketplace: PathBuf,
    pub(super) installations: Vec<PluginInstallation>,
}

pub(super) fn replace_plugin(
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

pub(super) fn prepare_plugin_recovery(
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
pub(super) fn undo_plugin_switch(recovery: Option<&PluginRecovery>, launcher_dir: &Path) -> Result<(), InitError> {
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
pub(super) fn cleanup_plugin_recovery(recovery: Option<&PluginRecovery>) {
    if let Some(recovery) = recovery
        && let Err(error) = fs::remove_dir_all(&recovery.marketplace)
    {
        tracing::warn!(path = %recovery.marketplace.display(), %error, "cannot remove the rollback source");
    }
}

/// Install the platform statusline and point Claude's settings at it, unless a
/// statusline that is not APPA's is configured, which is left alone.
pub(super) fn install_statusline(
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
pub(super) fn start_runtime(plugin_root: &Path) -> Result<(), InitError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_line_matches_only_the_named_marketplace() {
        assert!(is_appa_marketplace_line("  ❯ appa"));
        assert!(!is_appa_marketplace_line("  ❯ appa-other"));
        assert!(!is_appa_marketplace_line("Source: GitHub (appa)"));
    }
}
