//! Remove only the native registration belonging to the selected deployment.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::claude::{installed_plugin_root, plugin_registry, run_claude};
use super::{CLAPPA, InitError, MARKETPLACE, PLUGIN, appa_filename, deployment_paths};
use crate::plugin_bundle::{self, Endpoint, VerifiedArchive};

#[cfg(unix)]
const REMOVING: &str = "#!/bin/sh\nprintf 'APPA plugin removal is incomplete; rerun appa plugin remove claude-code with the same config.\\n' >&2\nexit 1\n";
#[cfg(windows)]
const REMOVING: &str = "@echo off\r\necho APPA plugin removal is incomplete; rerun appa plugin remove claude-code with the same config. 1>&2\r\nexit /b 1\r\n";

/// The caller holds a durable removal journal. A partial failure is replayable:
/// absent registrations/files are accepted, changed user state is not removed.
/// The runtime, configuration, retained artifacts and trajectory data stay put.
pub fn claude_code_remove(config: &Path, archive: &Path) -> Result<(), InitError> {
    let paths = deployment_paths()?;
    let _profile_lock = super::lock_claude_profile(&paths.claude_dir)?;
    let source = VerifiedArchive::of(archive)?;
    let scratch = tempfile::tempdir().map_err(|source| InitError::WriteFile {
        path: std::env::temp_dir(),
        source,
    })?;
    let deployment = plugin_bundle::materialize(
        source.population(),
        scratch.path(),
        &paths.data_dir.join("bin").join(appa_filename()),
        config,
        &paths.data_dir,
        &Endpoint::resolve()?,
    )?;
    let registered_root = paths.data_dir.join("deployments").join(
        deployment
            .root
            .file_name()
            .expect("materialize names a deployment by its digest"),
    );
    let registry_path = paths.claude_dir.join("plugins/installed_plugins.json");
    let registry = plugin_registry(&paths.claude_dir)?;
    let installed = selected_registration(&registry, &registry_path)?;
    if installed {
        let root = installed_plugin_root(&paths.claude_dir)?;
        verify_plugin(&root, &deployment.root.join("plugin"), &registry_path)?;
    }
    let marketplace_path = paths.claude_dir.join("plugins/known_marketplaces.json");
    let mut marketplace = read_json(&marketplace_path)?;
    let registered_marketplace = match marketplace.get(MARKETPLACE) {
        None => false,
        Some(entry) => {
            verify_marketplace(entry, &registered_root, &marketplace_path)?;
            true
        }
    };
    let launcher = paths.install_dir.join(CLAPPA.0);
    let launcher_before = file_before(&launcher)?;
    verify_launcher(launcher_before.as_deref(), &launcher)?;

    // Disable the protected entrypoint before unregistering its hooks. A crash
    // must not leave a working-looking clappa that starts unprotected Claude.
    if launcher_before.is_some() {
        write_state(&launcher, REMOVING.as_bytes())?;
    }
    if installed {
        run_claude(["plugin", "uninstall", PLUGIN, "--scope", "user", "--yes"], None)?;
    }
    if registered_marketplace {
        run_claude(["plugin", "marketplace", "remove", MARKETPLACE], None)?;
    }
    if selected_registration(&plugin_registry(&paths.claude_dir)?, &registry_path)? {
        return Err(conflict(&registry_path, "Claude still reports the removed APPA plugin"));
    }
    marketplace = read_json(&marketplace_path)?;
    if marketplace.get(MARKETPLACE).is_some() {
        return Err(conflict(
            &marketplace_path,
            "Claude still reports the removed APPA marketplace",
        ));
    }
    remove_statusline(&paths, &deployment.root.join("plugin"))?;
    if launcher_before.is_some() {
        if file_before(&launcher)?.as_deref() != Some(REMOVING.as_bytes()) {
            return Err(conflict(
                &launcher,
                "launcher changed during removal; leaving it unchanged",
            ));
        }
        fs::remove_file(&launcher).map_err(|source| InitError::WriteFile { path: launcher, source })?;
    }
    Ok(())
}

fn verify_plugin(root: &Path, expected: &Path, registry: &Path) -> Result<(), InitError> {
    if appa_package::tree::canonical_tree_digest(root).map_err(plugin_bundle::PluginBundleError::from)?
        != appa_package::tree::canonical_tree_digest(expected).map_err(plugin_bundle::PluginBundleError::from)?
    {
        return Err(conflict(
            registry,
            "registered APPA plugin differs from this deployment; leaving it unchanged",
        ));
    }
    Ok(())
}

fn verify_marketplace(entry: &Value, expected: &Path, registry: &Path) -> Result<(), InitError> {
    let matches = entry["source"]["path"].as_str().is_some_and(|path| {
        let path = Path::new(path);
        // The exact recorded location is also valid after its directory was
        // removed. Aliases must resolve to the same existing directory.
        path == expected || super::paths::same_file(path, expected)
    });
    if entry["source"]["source"] != "directory" || !matches {
        return Err(conflict(
            registry,
            "APPA marketplace belongs to a different deployment; leaving it unchanged",
        ));
    }
    Ok(())
}

fn verify_launcher(bytes: Option<&[u8]>, path: &Path) -> Result<(), InitError> {
    if bytes.is_some_and(|bytes| bytes != CLAPPA.1.as_bytes() && bytes != REMOVING.as_bytes()) {
        return Err(conflict(
            path,
            "launcher was edited; resolve it before removing the plugin",
        ));
    }
    Ok(())
}

fn selected_registration(registry: &Value, path: &Path) -> Result<bool, InitError> {
    let root = registry
        .as_object()
        .ok_or_else(|| conflict(path, "registry must be an object"))?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(false);
    };
    let plugins = plugins
        .as_object()
        .ok_or_else(|| conflict(path, "plugins must be an object"))?;
    let Some(entries) = plugins.get(PLUGIN) else {
        return Ok(false);
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| conflict(path, "APPA registrations must be an array"))?;
    if entries.is_empty() {
        return Ok(false);
    }
    if entries.len() != 1 || entries[0]["scope"] != "user" {
        return Err(conflict(
            path,
            "removal requires exactly this deployment's user-scoped APPA plugin",
        ));
    }
    Ok(true)
}

fn read_json(path: &Path) -> Result<Value, InitError> {
    let Some(bytes) = file_before(path)? else {
        return Ok(serde_json::json!({}));
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| conflict(path, &error.to_string()))?;
    if !value.is_object() {
        return Err(conflict(path, "expected a JSON object"));
    }
    Ok(value)
}

fn conflict(path: &Path, message: &str) -> InitError {
    InitError::NativeState {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn file_before(path: &Path) -> Result<Option<Vec<u8>>, InitError> {
    crate::installation::optional_bytes(path).map_err(|error| conflict(path, &error.to_string()))
}

fn write_state(path: &Path, bytes: &[u8]) -> Result<(), InitError> {
    crate::installation::atomic_write(path, bytes).map_err(|error| conflict(path, &error.to_string()))
}

fn remove_statusline(paths: &super::paths::DeploymentPaths, plugin: &Path) -> Result<(), InitError> {
    let settings_path = paths.claude_dir.join("settings.json");
    let mut settings = read_json(&settings_path)?;
    #[cfg(windows)]
    let filename = "appa-statusline.ps1";
    #[cfg(not(windows))]
    let filename = "appa-statusline.sh";
    let target = paths.install_dir.join(filename);
    #[cfg(windows)]
    let command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        target.display()
    );
    #[cfg(not(windows))]
    let command = target.to_string_lossy().into_owned();
    #[cfg(windows)]
    let source = plugin.join("statusline.ps1");
    #[cfg(not(windows))]
    let source = plugin.join("statusline.sh");
    let expected = file_before(&source)?;
    let owned_file = file_before(&target)?.is_some_and(|bytes| expected.as_ref() == Some(&bytes));
    // A customized statusline is user state, even when it retains an APPA name.
    if settings["statusLine"]["command"].as_str() == Some(&command) && owned_file {
        settings
            .as_object_mut()
            .expect("read_json requires an object")
            .remove("statusLine");
        let bytes = serde_json::to_vec_pretty(&settings).expect("JSON values encode");
        write_state(&settings_path, &bytes)?;
    }
    if owned_file && settings.get("statusLine").is_none() {
        fs::remove_file(&target).map_err(|source| InitError::WriteFile { path: target, source })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_checks_full_plugin_contents_without_modifying_them() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("installed");
        let expected = root.path().join("expected");
        for path in [&installed, &expected] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("hook.sh"), "original").unwrap();
        }
        let registry = root.path().join("registry.json");
        verify_plugin(&installed, &expected, &registry).unwrap();
        fs::write(installed.join("hook.sh"), "customized").unwrap();
        assert!(verify_plugin(&installed, &expected, &registry).is_err());
        assert_eq!(fs::read(installed.join("hook.sh")).unwrap(), b"customized");
        fs::write(installed.join("hook.sh"), "original").unwrap();
        fs::write(installed.join("extra.sh"), "extra executable").unwrap();
        assert!(verify_plugin(&installed, &expected, &registry).is_err());
    }

    #[test]
    fn removal_accepts_only_owned_marketplace_and_launcher_states() {
        let root = tempfile::tempdir().unwrap();
        let expected = root.path().join("deployment");
        let registry = root.path().join("registry.json");
        let entry = serde_json::json!({"source":{"source":"directory","path":expected}});
        // A missing deployment directory must not be recreated to remove its
        // exact registration, including on replay after interruption.
        verify_marketplace(&entry, &expected, &registry).unwrap();
        assert!(!expected.exists());
        let foreign = serde_json::json!({"source":{"source":"directory","path":root.path()}});
        assert!(verify_marketplace(&foreign, &expected, &registry).is_err());
        let remote = serde_json::json!({"source":{"source":"github","path":expected}});
        assert!(verify_marketplace(&remote, &expected, &registry).is_err());
        let launcher = root.path().join(CLAPPA.0);
        for bytes in [None, Some(CLAPPA.1.as_bytes()), Some(REMOVING.as_bytes())] {
            verify_launcher(bytes, &launcher).unwrap();
        }
        assert!(verify_launcher(Some(b"custom launcher"), &launcher).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removal_accepts_marketplace_alias_only_for_same_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let expected = root.path().join("deployment");
        let alias = root.path().join("alias");
        let registry = root.path().join("registry.json");
        fs::create_dir(&expected).unwrap();
        std::os::unix::fs::symlink(&expected, &alias).unwrap();
        let entry = serde_json::json!({"source":{"source":"directory","path":alias}});
        verify_marketplace(&entry, &expected, &registry).unwrap();
        fs::remove_dir(&expected).unwrap();
        assert!(verify_marketplace(&entry, &expected, &registry).is_err());
    }

    #[test]
    fn removal_refuses_ambiguous_or_non_user_registrations() {
        let path = Path::new("registry.json");
        for entries in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([{"scope":"project"}]),
            serde_json::json!([{}]),
            serde_json::json!([{"scope":"user"},{"scope":"user"}]),
        ] {
            assert!(selected_registration(&serde_json::json!({"plugins":{PLUGIN:entries}}), path).is_err());
        }
        assert!(selected_registration(&serde_json::json!({"plugins":{PLUGIN:[{"scope":"user"}]}}), path).unwrap());
        assert!(!selected_registration(&serde_json::json!({"plugins":{PLUGIN:[]}}), path).unwrap());
        assert!(
            !selected_registration(
                &serde_json::json!({"plugins":{"other@market":[{"scope":"user"}]}}),
                path
            )
            .unwrap()
        );
    }

    #[test]
    fn statusline_removal_preserves_custom_settings_and_changed_files() {
        for customized in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let paths = super::super::paths::DeploymentPaths {
                install_dir: root.path().join("bin"),
                config_dir: root.path().join("config"),
                data_dir: root.path().join("data"),
                claude_dir: root.path().join("claude"),
            };
            let plugin = root.path().join("plugin");
            for directory in [&plugin, &paths.install_dir, &paths.claude_dir] {
                fs::create_dir(directory).unwrap();
            }
            #[cfg(windows)]
            let (source_name, target_name) = ("statusline.ps1", "appa-statusline.ps1");
            #[cfg(not(windows))]
            let (source_name, target_name) = ("statusline.sh", "appa-statusline.sh");
            fs::write(plugin.join(source_name), "owned contents").unwrap();
            let target = paths.install_dir.join(target_name);
            fs::write(
                &target,
                if customized {
                    "custom contents"
                } else {
                    "owned contents"
                },
            )
            .unwrap();
            #[cfg(windows)]
            let command = format!(
                "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
                target.display()
            );
            #[cfg(not(windows))]
            let command = target.to_string_lossy().into_owned();
            let settings = paths.claude_dir.join("settings.json");
            let original = serde_json::json!({"statusLine":{"type":"command","command":command},"custom":"preserved"});
            fs::write(&settings, serde_json::to_vec(&original).unwrap()).unwrap();
            remove_statusline(&paths, &plugin).unwrap();
            let after = read_json(&settings).unwrap();
            assert_eq!(after["custom"], "preserved");
            assert_eq!(target.exists(), customized);
            if customized {
                assert_eq!(after, original);
            } else {
                assert!(after.get("statusLine").is_none());
            }
            remove_statusline(&paths, &plugin).unwrap();
        }
    }
}
