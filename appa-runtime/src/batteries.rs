//! Bundled batteries: directories the runtime can look up, and the list
//! it serves. Policy still changes only through an explicit root
//! `include`. These directories are a search path for
//! `batteries/<name>/appa.toml`.
//!
//! The store is a deployment's copy of every battery of the installed
//! version: `batteries/` beside its config, filled by the install. An
//! include spelled `batteries/<name>/appa.toml` resolves there without any
//! directory being named, as a relative include beside the config.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// The store of the deployment whose root config is `config`.
pub fn store_dir(config: &Path) -> PathBuf {
    config.parent().unwrap_or(Path::new("")).join("batteries")
}

/// The directories a deployment lists when the runtime is given none: its
/// store.
pub fn default_search_path(config: &Path) -> Vec<PathBuf> {
    vec![store_dir(config)]
}

/// Fill the store from a version's batteries tree. Every battery there
/// replaces the store's copy of the same name by one rename, so a reader
/// sees the old battery or the new one; batteries the tree does not carry
/// stay. The names stocked, sorted.
pub fn stock(tree: &Path, store: &Path) -> io::Result<Vec<String>> {
    let mut names = match names_in(tree) {
        Ok(names) => names,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    names.sort();
    fs::create_dir_all(store)?;
    for name in &names {
        let stage = tempfile::Builder::new().prefix(".stage-").tempdir_in(store)?;
        let staged = stage.path().join(name);
        crate::batteries_layout::copy_entry(&tree.join(name), &staged)?;
        let target = store.join(name);
        let retired = tempfile::Builder::new().prefix(".retired-").tempdir_in(store)?;
        if target.exists() {
            fs::rename(&target, retired.path().join(name))?;
        }
        fs::rename(&staged, &target)?;
    }
    Ok(names)
}

/// One battery the runtime can name: its directory name and the tool
/// names its `appa.toml` declares. Paths stay off the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundledBattery {
    pub name: String,
    pub tools: Vec<String>,
}

/// The JSON body of `GET /batteries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatteriesResponse {
    pub batteries: Vec<BundledBattery>,
}

/// Create each configured directory and refuse a path that exists and is
/// not a directory, or that cannot be read after creation.
pub fn prepare(dirs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut prepared = Vec::new();
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            return Err("a batteries directory path is empty".to_string());
        }
        if dir.exists() && !dir.is_dir() {
            return Err(format!(
                "batteries directory {} exists and is not a directory",
                dir.display()
            ));
        }
        fs::create_dir_all(dir)
            .map_err(|source| format!("cannot create batteries directory {}: {source}", dir.display()))?;
        let canonical = fs::canonicalize(dir)
            .map_err(|source| format!("cannot read batteries directory {}: {source}", dir.display()))?;
        fs::read_dir(&canonical)
            .map_err(|source| format!("cannot read batteries directory {}: {source}", canonical.display()))?;
        prepared.push(canonical);
    }
    Ok(prepared)
}

/// A snapshot of batteries visible across `dirs`, first directory
/// winning on a name. Names inside one directory are sorted. A battery
/// whose `appa.toml` cannot be parsed still appears, with no tools.
pub fn snapshot(dirs: &[PathBuf]) -> BatteriesResponse {
    let mut seen = BTreeSet::new();
    let mut batteries = Vec::new();
    for dir in dirs {
        let mut found = match names_in(dir) {
            Ok(names) => names,
            // A directory of the default search path that nothing filled yet.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "cannot refresh the batteries catalog");
                continue;
            }
        };
        found.sort();
        for name in found {
            if !seen.insert(name.clone()) {
                continue;
            }
            let tools = tools_in(&dir.join(&name).join("appa.toml"));
            batteries.push(BundledBattery { name, tools });
        }
    }
    BatteriesResponse { batteries }
}

/// Resolve an authored include against the battery search path, then
/// against the root config directory. Absolute includes stay the
/// caller's refusal.
pub fn resolve_include(authored: &str, source_dir: &Path, battery_dirs: &[PathBuf]) -> PathBuf {
    if let Some(name) = name_from_include(Path::new(authored)) {
        for dir in battery_dirs {
            let candidate = dir.join(&name).join("appa.toml");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    source_dir.join(authored)
}

/// The battery name an include path names when it is shaped
/// `batteries/<name>/appa.toml`.
pub fn name_from_include(path: &Path) -> Option<String> {
    let mut parts = path.components();
    let first = parts.next()?;
    let batteries = match first {
        Component::CurDir => parts.next()?,
        component => component,
    };
    let Component::Normal(batteries) = batteries else {
        return None;
    };
    let Component::Normal(name) = parts.next()? else {
        return None;
    };
    let Component::Normal(file) = parts.next()? else {
        return None;
    };
    if batteries != OsStr::new("batteries") || file != OsStr::new("appa.toml") || parts.next().is_some() {
        return None;
    }
    name.to_str().map(str::to_owned)
}

/// A resolved battery file has the tail `batteries/<name>/appa.toml`,
/// regardless of how the root config spelled the relative include.
pub fn name_from_resolved(path: &Path, battery_dirs: &[PathBuf]) -> Option<String> {
    for directory in battery_dirs {
        // `path` arrives canonical; a search-path entry may not be (a symlink, or
        // `/var` for `/private/var` on macOS), and the prefix test needs both alike.
        let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.clone());
        let Ok(relative) = path.strip_prefix(&directory) else {
            continue;
        };
        let parts: Vec<_> = relative.components().collect();
        if let [Component::Normal(name), Component::Normal(file)] = parts.as_slice()
            && *file == OsStr::new("appa.toml")
        {
            return name.to_str().map(str::to_owned);
        }
    }
    name_from_path(path)
}

/// The battery a path names by its tail `batteries/<name>/appa.toml`,
/// however the root config spelled it: the search-path form, or a path
/// into a retained version.
pub fn name_from_path(path: &Path) -> Option<String> {
    if path.file_name()? != OsStr::new("appa.toml") {
        return None;
    }
    let directory = path.parent()?;
    if directory.parent()?.file_name()? != OsStr::new("batteries") {
        return None;
    }
    directory.file_name()?.to_str().map(str::to_owned)
}

fn names_in(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') || name.is_empty() {
            continue;
        }
        if !entry.path().join("appa.toml").is_file() {
            continue;
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn tools_in(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let mut tools: Vec<String> = value
        .get("policy")
        .and_then(|policy| policy.get("tool"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name"))
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut seen = BTreeSet::new();
    tools.retain(|tool| seen.insert(tool.clone()));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_from_include_reads_the_battery_path_shape() {
        assert_eq!(
            name_from_include(Path::new("batteries/slack/appa.toml")).as_deref(),
            Some("slack")
        );
        assert_eq!(
            name_from_include(Path::new("./batteries/claude-code/appa.toml")).as_deref(),
            Some("claude-code")
        );
        for other in [
            "slack/appa.toml",
            "first.toml",
            "other/batteries/slack/appa.toml",
            "batteries/slack/appa.toml/more",
            "batteries/../other/appa.toml",
        ] {
            assert_eq!(
                name_from_include(Path::new(other)),
                None,
                "{other:?} is not a battery include"
            );
        }
    }

    #[test]
    fn resolved_battery_name_uses_the_path_tail() {
        assert_eq!(
            name_from_resolved(Path::new("/deployment/vendor/batteries/slack/appa.toml"), &[]).as_deref(),
            Some("slack")
        );
        for other in [
            "/deployment/vendor/slack/appa.toml",
            "/deployment/batteries/slack/other.toml",
            "/deployment/batteries/appa.toml",
        ] {
            assert_eq!(name_from_resolved(Path::new(other), &[]), None);
        }
        assert_eq!(
            name_from_resolved(
                Path::new("/deployment/image/slack/appa.toml"),
                &[PathBuf::from("/deployment/image")],
            )
            .as_deref(),
            Some("slack")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_battery_directory_reached_through_a_link_keeps_its_name() {
        let root = tempfile::tempdir().expect("temporary directory");
        let real = root.path().join("real");
        let link = root.path().join("link");
        write_battery(&real, "slack", &["send"]);
        std::os::unix::fs::symlink(&real, &link).expect("link to the batteries directory");

        // The include arrives canonical, as `resolve_include` leaves it.
        let include = fs::canonicalize(real.join("slack/appa.toml")).expect("canonical include");
        assert_eq!(name_from_resolved(&include, &[link]).as_deref(), Some("slack"));
    }

    #[test]
    fn list_walks_directories_in_order_and_first_name_wins() {
        let root = tempfile::tempdir().expect("temporary directory");
        let image = root.path().join("image");
        let overlay = root.path().join("overlay");
        write_battery(&image, "slack", &["send"]);
        write_battery(&image, "github", &["create_issue", "create_issue"]);
        write_battery(&overlay, "slack", &["overlay-send"]);
        write_battery(&overlay, "grain", &["list_recordings"]);
        fs::create_dir_all(overlay.join("empty")).expect("empty directory");
        fs::write(image.join("not-a-battery.toml"), "").expect("stray file");

        let response = snapshot(&[overlay.clone(), image.clone()]);
        assert_eq!(
            response,
            BatteriesResponse {
                batteries: vec![
                    BundledBattery {
                        name: "grain".into(),
                        tools: vec!["list_recordings".into()],
                    },
                    BundledBattery {
                        name: "slack".into(),
                        tools: vec!["overlay-send".into()],
                    },
                    BundledBattery {
                        name: "github".into(),
                        tools: vec!["create_issue".into()],
                    },
                ],
            }
        );
    }

    #[test]
    fn resolve_include_prefers_the_first_directory_that_has_the_battery() {
        let root = tempfile::tempdir().expect("temporary directory");
        let image = root.path().join("image");
        let overlay = root.path().join("overlay");
        let config_dir = root.path().join("config");
        write_battery(&image, "slack", &["image"]);
        write_battery(&overlay, "slack", &["overlay"]);
        write_battery(&config_dir.join("batteries"), "slack", &["config"]);
        fs::write(config_dir.join("appa.toml"), "").expect("root config");

        let overlay_path = resolve_include(
            "batteries/slack/appa.toml",
            &config_dir,
            &[overlay.clone(), image.clone()],
        );
        assert_eq!(overlay_path, overlay.join("slack/appa.toml"));

        let image_path = resolve_include("batteries/slack/appa.toml", &config_dir, std::slice::from_ref(&image));
        assert_eq!(image_path, image.join("slack/appa.toml"));

        let config_path = resolve_include("batteries/slack/appa.toml", &config_dir, &[]);
        assert_eq!(config_path, config_dir.join("batteries/slack/appa.toml"));
    }

    #[test]
    fn prepare_creates_a_missing_directory_and_refuses_a_file() {
        let root = tempfile::tempdir().expect("temporary directory");
        let missing = root.path().join("batteries");
        let prepared = prepare(std::slice::from_ref(&missing)).expect("missing directory is created");
        assert!(missing.is_dir());
        assert_eq!(prepared[0], fs::canonicalize(&missing).expect("canonical"));

        let file = root.path().join("file");
        fs::write(&file, "not a directory").expect("file");
        let refusal = prepare(&[file]).expect_err("a file is not a batteries directory");
        assert!(refusal.contains("not a directory"));
    }

    /// The store takes every battery of a version's tree, replaces its own copy
    /// of a battery the tree carries again, and keeps one it does not.
    #[test]
    fn stocking_replaces_the_versions_batteries_and_keeps_the_rest() {
        let tree = tempfile::tempdir().expect("tree");
        let config_dir = tempfile::tempdir().expect("config dir");
        let store = store_dir(&config_dir.path().join("appa.toml"));
        assert_eq!(store, config_dir.path().join("batteries"));
        assert_eq!(
            default_search_path(&config_dir.path().join("appa.toml")),
            vec![store.clone()]
        );
        write_battery(&store, "slack", &["old"]);
        write_battery(&store, "mine", &["kept"]);
        fs::write(store.join("slack/helper.py"), "gone with the old copy").expect("helper");
        write_battery(tree.path(), "slack", &["new"]);
        write_battery(tree.path(), "github", &["added"]);
        fs::write(tree.path().join("github/appa-package.toml"), "schema = 1\n").expect("manifest");

        assert_eq!(stock(tree.path(), &store).expect("stocked"), vec!["github", "slack"]);

        assert_eq!(tools_in(&store.join("slack/appa.toml")), vec!["new"]);
        assert!(!store.join("slack/helper.py").exists());
        assert_eq!(tools_in(&store.join("github/appa.toml")), vec!["added"]);
        assert!(!store.join("github/appa-package.toml").exists());
        assert_eq!(tools_in(&store.join("mine/appa.toml")), vec!["kept"]);
        assert_eq!(
            snapshot(std::slice::from_ref(&store))
                .batteries
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["github", "mine", "slack"]
        );
        assert_eq!(
            stock(&tree.path().join("absent"), &store).expect("nothing to stock"),
            Vec::<String>::new()
        );
    }

    fn write_battery(root: &Path, name: &str, tools: &[&str]) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("battery directory");
        let mut body = String::from("[policy]\nversion = 2\n");
        for tool in tools {
            body.push_str(&format!("[[policy.tool]]\nname = \"{tool}\"\ndelta = {{}}\n"));
        }
        fs::write(dir.join("appa.toml"), body).expect("battery config");
    }
}
