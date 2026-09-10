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

pub(super) fn plugin_registry(claude_dir: &Path) -> Result<Value, InitError> {
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

/// Reuse only a complete, enabled native registration of the verified tree.
/// Ordinary drift goes through installation's repair path; unreadable or
/// malformed state is not evidence that it is safe to replace a registration.
pub(super) fn registered_deployment_matches(claude_dir: &Path, deployment: &Path) -> Result<bool, InitError> {
    let registry_path = claude_dir.join("plugins/installed_plugins.json");
    let registry = native_object(&registry_path)?;
    let Some(plugins) = registry.get("plugins") else {
        return Ok(false);
    };
    let plugins = plugins
        .as_object()
        .ok_or_else(|| native_error(&registry_path, "plugins must be an object"))?;
    let Some(entries) = plugins.get(PLUGIN) else {
        return Ok(false);
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| native_error(&registry_path, "APPA registrations must be an array"))?;
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or_else(|| native_error(&registry_path, "APPA registration must be an object"))?;
        for field in ["scope", "installPath", "projectPath"] {
            if entry.get(field).is_some_and(|value| !value.is_string()) {
                return Err(native_error(&registry_path, &format!("{field} must be a string")));
            }
        }
    }
    if entries.len() != 1 || entries[0].get("scope").and_then(Value::as_str) != Some("user") {
        return Ok(false);
    }
    let Some(installed) = entries[0].get("installPath").and_then(Value::as_str) else {
        return Ok(false);
    };
    let installed = Path::new(installed);
    match fs::metadata(installed) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(InitError::WriteFile {
                path: installed.to_owned(),
                source,
            });
        }
    }
    if appa_package::tree::canonical_tree_digest(installed).map_err(crate::plugin_bundle::PluginBundleError::from)?
        != appa_package::tree::canonical_tree_digest(&deployment.join("plugin"))
            .map_err(crate::plugin_bundle::PluginBundleError::from)?
    {
        return Ok(false);
    }
    let marketplace_path = claude_dir.join("plugins/known_marketplaces.json");
    let marketplaces = native_object(&marketplace_path)?;
    let Some(entry) = marketplaces.get(MARKETPLACE) else {
        return Ok(false);
    };
    if !marketplace_matches(entry, deployment, &marketplace_path)? {
        return Ok(false);
    }
    let settings_path = claude_dir.join("settings.json");
    let settings = native_object(&settings_path)?;
    if let Some(extra) = settings.get("extraKnownMarketplaces") {
        let extra = extra
            .as_object()
            .ok_or_else(|| native_error(&settings_path, "extraKnownMarketplaces must be an object"))?;
        if let Some(entry) = extra.get(MARKETPLACE)
            && !marketplace_matches(entry, deployment, &settings_path)?
        {
            return Ok(false);
        }
    }
    let Some(enabled) = settings.get("enabledPlugins") else {
        return Ok(false);
    };
    let enabled = enabled
        .as_object()
        .ok_or_else(|| native_error(&settings_path, "enabledPlugins must be an object"))?;
    match enabled.get(PLUGIN) {
        Some(Value::Bool(enabled)) => Ok(*enabled),
        None => Ok(false),
        Some(_) => Err(native_error(
            &settings_path,
            "APPA enabledPlugins value must be a boolean",
        )),
    }
}

fn native_error(path: &Path, message: &str) -> InitError {
    InitError::NativeState {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn native_bytes(path: &Path) -> Result<Option<Vec<u8>>, InitError> {
    crate::installation::optional_bytes(path).map_err(|error| native_error(path, &error.to_string()))
}

fn native_object(path: &Path) -> Result<Map<String, Value>, InitError> {
    let Some(bytes) = native_bytes(path)? else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| native_error(path, &error.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| native_error(path, "expected a JSON object"))
}

fn marketplace_matches(entry: &Value, deployment: &Path, registry: &Path) -> Result<bool, InitError> {
    let entry = entry
        .as_object()
        .ok_or_else(|| native_error(registry, "APPA marketplace must be an object"))?;
    let Some(source) = entry.get("source") else {
        return Ok(false);
    };
    let source = source
        .as_object()
        .ok_or_else(|| native_error(registry, "APPA marketplace source must be an object"))?;
    for field in ["source", "path"] {
        if source.get(field).is_some_and(|value| !value.is_string()) {
            return Err(native_error(
                registry,
                &format!("APPA marketplace source {field} must be a string"),
            ));
        }
    }
    if source.get("source").and_then(Value::as_str) != Some("directory") {
        return Ok(false);
    }
    let Some(path) = source.get("path").and_then(Value::as_str) else {
        return Ok(false);
    };
    let path = Path::new(path);
    let actual = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(InitError::WriteFile {
                path: path.to_owned(),
                source,
            });
        }
    };
    let expected = fs::canonicalize(deployment).map_err(|source| InitError::WriteFile {
        path: deployment.to_owned(),
        source,
    })?;
    Ok(actual == expected)
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
        "description": "Temporary rollback source created by appa plugin install.",
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
    let mut settings = Value::Object(native_object(&settings_path)?);
    let existing = settings
        .get("statusLine")
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str);
    if existing.is_some_and(|command| !command.contains("appa-statusline")) {
        return Ok(());
    }
    #[cfg(windows)]
    let statusline_command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        target.display()
    );
    #[cfg(not(windows))]
    let statusline_command = target.to_string_lossy().into_owned();
    let source_bytes = native_bytes(&source)?.ok_or_else(|| InitError::MissingPluginFile(source.clone()))?;
    if existing == Some(statusline_command.as_str())
        && settings["statusLine"]["type"] == "command"
        && native_bytes(&target)?.as_deref() == Some(source_bytes.as_slice())
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::metadata(&target)
                .map_err(|source| InitError::WriteFile {
                    path: target.clone(),
                    source,
                })?
                .permissions();
            if permissions.mode() & 0o111 == 0o111 {
                return Ok(());
            }
        }
        #[cfg(not(unix))]
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

    fn write_json(path: &Path, value: Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn registered_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let claude = root.join("claude");
        let deployment = root.join("deployment");
        let installed = root.join("installed");
        for directory in [deployment.join("plugin"), installed.clone()] {
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("hook.sh"), "verified hook").unwrap();
        }
        write_json(
            &claude.join("plugins/installed_plugins.json"),
            serde_json::json!({
                "version": 2, "plugins": { (PLUGIN): [{"scope": "user", "installPath": installed}] }
            }),
        );
        write_json(
            &claude.join("plugins/known_marketplaces.json"),
            serde_json::json!({
                (MARKETPLACE): {"source": {"source": "directory", "path": deployment}}
            }),
        );
        write_json(
            &claude.join("settings.json"),
            serde_json::json!({"enabledPlugins": {(PLUGIN): true}}),
        );
        (claude, deployment, installed)
    }

    #[test]
    fn registration_reuse_requires_the_complete_verified_tree_and_enabled_plugin() {
        let root = tempfile::tempdir().unwrap();
        let (claude, deployment, installed) = registered_fixture(root.path());
        assert!(registered_deployment_matches(&claude, &deployment).unwrap());
        fs::write(installed.join("extra"), "unexpected").unwrap();
        assert!(!registered_deployment_matches(&claude, &deployment).unwrap());
        fs::remove_file(installed.join("extra")).unwrap();
        write_json(
            &claude.join("settings.json"),
            serde_json::json!({"enabledPlugins": {(PLUGIN): false}}),
        );
        assert!(!registered_deployment_matches(&claude, &deployment).unwrap());
        write_json(
            &claude.join("settings.json"),
            serde_json::json!({
                "enabledPlugins": {(PLUGIN): true},
                "extraKnownMarketplaces": {(MARKETPLACE): {"source": {"source": "directory", "path": installed}}}
            }),
        );
        assert!(!registered_deployment_matches(&claude, &deployment).unwrap());
    }

    #[test]
    fn registration_reuse_distinguishes_missing_state_from_malformed_state() {
        let root = tempfile::tempdir().unwrap();
        let (claude, deployment, _) = registered_fixture(root.path());
        let registry = claude.join("plugins/installed_plugins.json");
        for value in [
            serde_json::json!([]),
            serde_json::json!({"plugins": []}),
            serde_json::json!({"plugins": {(PLUGIN): {}}}),
            serde_json::json!({"plugins": {(PLUGIN): [{"scope": false}]}}),
        ] {
            write_json(&registry, value);
            assert!(registered_deployment_matches(&claude, &deployment).is_err());
        }
        fs::write(&registry, "{").unwrap();
        assert!(registered_deployment_matches(&claude, &deployment).is_err());
        fs::remove_file(&registry).unwrap();
        assert!(!registered_deployment_matches(&claude, &deployment).unwrap());
        let (claude, deployment, _) = registered_fixture(root.path());
        write_json(
            &claude.join("settings.json"),
            serde_json::json!({"enabledPlugins": {(PLUGIN): "true"}}),
        );
        assert!(registered_deployment_matches(&claude, &deployment).is_err());
        fs::create_dir_all(claude.join("invalid")).unwrap();
        fs::remove_file(&registry).unwrap();
        fs::create_dir(&registry).unwrap();
        assert!(registered_deployment_matches(&claude, &deployment).is_err());
    }

    #[test]
    fn registration_reuse_requires_one_user_scope_and_an_existing_plugin_tree() {
        let root = tempfile::tempdir().unwrap();
        let (claude, deployment, installed) = registered_fixture(root.path());
        let registry = claude.join("plugins/installed_plugins.json");
        for entries in [
            serde_json::json!([]),
            serde_json::json!([{"scope": "project", "installPath": installed}]),
            serde_json::json!([{"scope": "user", "installPath": installed}, {"scope": "user", "installPath": installed}]),
            serde_json::json!([{"scope": "user"}]),
            serde_json::json!([{"scope": "user", "installPath": root.path().join("absent")}]),
            serde_json::json!([{"scope": "user", "installPath": installed.join("hook.sh")}]),
        ] {
            write_json(&registry, serde_json::json!({"plugins": {(PLUGIN): entries}}));
            assert!(!registered_deployment_matches(&claude, &deployment).unwrap());
        }
        write_json(
            &registry,
            serde_json::json!({"plugins": {(PLUGIN): [{"scope": "user", "installPath": installed}]}}),
        );
        assert!(registered_deployment_matches(&claude, &deployment).unwrap());
    }

    #[test]
    fn registration_reuse_accepts_filesystem_alias_and_checks_extra_source() {
        let root = tempfile::tempdir().unwrap();
        let (claude, deployment, _) = registered_fixture(root.path());
        let alias = deployment.join("plugin/..");
        let source = serde_json::json!({"source": {"source": "directory", "path": alias}});
        write_json(
            &claude.join("plugins/known_marketplaces.json"),
            serde_json::json!({(MARKETPLACE): source}),
        );
        write_json(
            &claude.join("settings.json"),
            serde_json::json!({
                "enabledPlugins": {(PLUGIN): true}, "extraKnownMarketplaces": {(MARKETPLACE): source}
            }),
        );
        assert!(registered_deployment_matches(&claude, &deployment).unwrap());
        write_json(
            &claude.join("plugins/known_marketplaces.json"),
            serde_json::json!({(MARKETPLACE): {"source": []}}),
        );
        assert!(registered_deployment_matches(&claude, &deployment).is_err());
    }

    #[test]
    fn unchanged_statusline_does_not_write_or_record_compensation() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugin");
        let paths = DeploymentPaths {
            install_dir: root.path().join("bin"),
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            claude_dir: root.path().join("claude"),
        };
        fs::create_dir_all(&plugin).unwrap();
        fs::create_dir_all(&paths.install_dir).unwrap();
        #[cfg(windows)]
        let (source, target) = ("statusline.ps1", "appa-statusline.ps1");
        #[cfg(not(windows))]
        let (source, target) = ("statusline.sh", "appa-statusline.sh");
        fs::write(plugin.join(source), "verified statusline").unwrap();
        install_statusline(&plugin, &paths, &mut Compensation::default()).unwrap();
        let settings_path = paths.claude_dir.join("settings.json");
        let bytes = fs::read(&settings_path).unwrap();
        let target_path = paths.install_dir.join(target);
        let sentinel = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        for path in [&settings_path, &target_path] {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(sentinel))
                .unwrap();
        }
        let settings_modified = fs::metadata(&settings_path).unwrap().modified().unwrap();
        let target_modified = fs::metadata(&target_path).unwrap().modified().unwrap();
        let mut compensation = Compensation::default();
        install_statusline(&plugin, &paths, &mut compensation).unwrap();
        assert!(compensation.done.is_empty());
        assert_eq!(fs::read(&settings_path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&settings_path).unwrap().modified().unwrap(),
            settings_modified
        );
        assert_eq!(fs::metadata(&target_path).unwrap().modified().unwrap(), target_modified);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target_path, fs::Permissions::from_mode(0o644)).unwrap();
            let mut repair = Compensation::default();
            install_statusline(&plugin, &paths, &mut repair).unwrap();
            assert_eq!(repair.done.len(), 2);
            assert_eq!(fs::metadata(&target_path).unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(fs::read(&target_path).unwrap(), b"verified statusline");
        }
        fs::write(paths.install_dir.join(target), "changed").unwrap();
        install_statusline(&plugin, &paths, &mut compensation).unwrap();
        assert_eq!(compensation.done.len(), 2);
        assert_eq!(
            fs::read(paths.install_dir.join(target)).unwrap(),
            b"verified statusline"
        );
        write_json(
            &settings_path,
            serde_json::json!({"statusLine": {"type": "command", "command": "my-custom-statusline"}}),
        );
        let bytes = fs::read(&settings_path).unwrap();
        let mut compensation = Compensation::default();
        install_statusline(&plugin, &paths, &mut compensation).unwrap();
        assert!(compensation.done.is_empty());
        assert_eq!(fs::read(&settings_path).unwrap(), bytes);
    }

    #[test]
    fn marketplace_line_matches_only_the_named_marketplace() {
        assert!(is_appa_marketplace_line("  ❯ appa"));
        assert!(!is_appa_marketplace_line("  ❯ appa-other"));
        assert!(!is_appa_marketplace_line("Source: GitHub (appa)"));
    }
}
