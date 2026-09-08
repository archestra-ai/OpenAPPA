//! Local installation state. Acquisition and host activation are separate from
//! immutable package publication; ordinary runtime startup never calls these.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use appa_package::generation::{ArtifactDigest, Commit, DESCRIPTOR_FILE, Generation, Platform};
use appa_package::{Marketplace, Package, PackageKind, PackageName, Role, TreeDigest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

mod acquisition;
mod battery;
pub mod cli;
pub mod native;
pub use acquisition::{Acquired, Requirements};

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("{operation} at {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("another installer owns {}; retry after it finishes", .0.display())]
    Busy(PathBuf),
    #[error("invalid installation input: {0}")]
    Invalid(String),
    #[error("{} changed during installation; no replacement was made", .0.display())]
    Changed(PathBuf),
    #[error("installation recovery is required at {}: {reason}", path.display())]
    Recovery { path: PathBuf, reason: String },
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> InstallError {
    InstallError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

/// An installer-owned include. A pre-existing include is never adopted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedInclude {
    battery: String,
    path: String,
}

/// Portable selection evidence. Package paths are derived from the catalog,
/// never accepted as arbitrary paths from this mutable deployment record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    schema: u32,
    generation: Generation,
    platform: Platform,
    plugins: BTreeSet<String>,
    batteries: BTreeSet<String>,
    includes: Vec<OwnedInclude>,
    aliases: Vec<battery::OwnedAlias>,
}

impl Selection {
    /// Rebase only installer-owned include paths when a generation or config
    /// filename changes. Authored policy and manual includes retain their order.
    pub fn relocate(
        &mut self,
        text: &str,
        generation: Generation,
        config: &Path,
        marketplace: &Path,
    ) -> Result<String, InstallError> {
        self.validate_owned_config(text)?;
        let filename = config
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| InstallError::Invalid("config filename must be UTF-8".into()))?;
        let catalog = Marketplace::read(&marketplace.join("marketplace.toml"))
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let mut document: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))?;
        let mut proposed = self.clone();
        proposed.generation = generation;
        for owned in &mut proposed.includes {
            let entry = catalog
                .packages
                .iter()
                .find(|entry| entry.kind == PackageKind::Battery && entry.name.as_str() == owned.battery)
                .ok_or_else(|| {
                    InstallError::Invalid(format!("battery {} is absent from the new generation", owned.battery))
                })?;
            let package = Package::read(&marketplace.join(entry.path.as_str()).join(appa_package::MANIFEST_FILE))
                .map_err(|error| InstallError::Invalid(error.to_string()))?;
            let Role::Battery(battery) = package.role else {
                return Err(InstallError::Invalid("catalog battery has another role".into()));
            };
            let replacement = format!(
                ".appa/{filename}/generations/{}/marketplace/{}/{}",
                proposed.generation.commit(),
                entry.path,
                battery.policy
            );
            let includes = document
                .get_mut("include")
                .and_then(toml_edit::Item::as_array_mut)
                .expect("owned config validation checked includes");
            let index = includes
                .iter()
                .position(|value| value.as_str() == Some(&owned.path))
                .expect("owned config validation found each entry");
            includes.replace(index, replacement.clone());
            owned.path = replacement;
        }
        proposed.validate_packages(marketplace)?;
        *self = proposed;
        Ok(document.to_string())
    }

    pub fn empty(generation: Generation, platform: Platform) -> Self {
        Self {
            schema: 1,
            generation,
            platform,
            plugins: BTreeSet::new(),
            batteries: BTreeSet::new(),
            includes: Vec::new(),
            aliases: Vec::new(),
        }
    }

    pub fn commit(&self) -> &Commit {
        self.generation.commit()
    }

    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn names(&self, kind: PackageKind) -> &BTreeSet<String> {
        match kind {
            PackageKind::Plugin => &self.plugins,
            PackageKind::Battery => &self.batteries,
        }
    }

    pub fn select(&mut self, kind: PackageKind, name: &PackageName) {
        match kind {
            PackageKind::Plugin => &mut self.plugins,
            PackageKind::Battery => &mut self.batteries,
        }
        .insert(name.to_string());
    }

    pub fn deselect(&mut self, kind: PackageKind, name: &PackageName) -> Result<(), InstallError> {
        if kind == PackageKind::Battery
            && (self.includes.iter().any(|include| include.battery == name.as_str()) || self.owns_battery_aliases(name))
        {
            return Err(InstallError::Invalid(
                "remove the battery's owned config entries before deselecting it".into(),
            ));
        }
        match kind {
            PackageKind::Plugin => &mut self.plugins,
            PackageKind::Battery => &mut self.batteries,
        }
        .remove(name.as_str());
        Ok(())
    }

    fn validate(&self) -> Result<(), InstallError> {
        if self.schema != 1 {
            return Err(InstallError::Invalid("unsupported selection schema".into()));
        }
        for name in self.plugins.iter().chain(&self.batteries) {
            PackageName::parse(name).map_err(|error| InstallError::Invalid(error.to_string()))?;
        }
        let mut paths = BTreeSet::new();
        let mut owners = BTreeSet::new();
        for include in &self.includes {
            if !self.batteries.contains(&include.battery)
                || !paths.insert(&include.path)
                || !owners.insert(&include.battery)
            {
                return Err(InstallError::Invalid("inconsistent owned include ledger".into()));
            }
            appa_package::RelativePath::parse(&include.path)
                .map_err(|error| InstallError::Invalid(error.to_string()))?;
        }
        self.validate_aliases()?;
        Ok(())
    }

    /// A lock is not evidence that a named package exists or supports the
    /// selected hosts. Recheck it against the verified generation's catalog.
    fn validate_packages(&self, marketplace: &Path) -> Result<(), InstallError> {
        self.validate()?;
        let packages = verify_packages(marketplace, &self.generation)?;
        let catalog = Marketplace::read(&marketplace.join("marketplace.toml"))
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let mut hosts = Vec::new();
        for name in &self.plugins {
            let plugin = packages
                .iter()
                .find_map(|package| match &package.role {
                    Role::Plugin(plugin) if package.name.as_str() == name => Some(plugin),
                    _ => None,
                })
                .ok_or_else(|| {
                    InstallError::Invalid(format!("plugin {name} is absent from the selected generation"))
                })?;
            if hosts.contains(&plugin.host()) {
                return Err(InstallError::Invalid(
                    "two selected plugins target the same host".into(),
                ));
            }
            hosts.push(plugin.host());
        }
        for name in &self.batteries {
            let battery = packages
                .iter()
                .find_map(|package| match &package.role {
                    Role::Battery(battery) if package.name.as_str() == name => Some(battery),
                    _ => None,
                })
                .ok_or_else(|| {
                    InstallError::Invalid(format!("battery {name} is absent from the selected generation"))
                })?;
            if hosts.iter().any(|host| !battery.hosts.contains(host)) {
                return Err(InstallError::Invalid(format!(
                    "battery {name} does not support every selected host"
                )));
            }
            self.validate_battery_aliases(name, &battery.namespaces)?;
            if let Some(include) = self.includes.iter().find(|include| &include.battery == name) {
                let entry = catalog
                    .packages
                    .iter()
                    .find(|entry| entry.kind == PackageKind::Battery && entry.name.as_str() == name)
                    .expect("verified packages and catalog have identical identities");
                let segments: Vec<_> = include.path.split('/').collect();
                if segments.len() < 6
                    || segments[0] != ".appa"
                    || segments[2] != "generations"
                    || segments[3] != self.commit().as_str()
                    || segments[4] != "marketplace"
                    || segments[5..].join("/") != format!("{}/{}", entry.path, battery.policy)
                {
                    return Err(InstallError::Invalid(format!(
                        "owned include for {name} does not name its selected package policy"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_owned_config(&self, text: &str) -> Result<(), InstallError> {
        let document: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))?;
        for owned in &self.includes {
            let count = document
                .get("include")
                .and_then(toml_edit::Item::as_array)
                .map(|includes| {
                    includes
                        .iter()
                        .filter(|value| value.as_str() == Some(&owned.path))
                        .count()
                })
                .unwrap_or(0);
            if count != 1 {
                return Err(InstallError::Invalid(format!(
                    "owned include for {} was changed or duplicated",
                    owned.battery
                )));
            }
        }
        self.validate_owned_aliases(&document)
    }

    /// Add only the include; package acquisition never creates an MCP server,
    /// binds an authority, or obtains credentials.
    pub fn include_battery(&mut self, text: &str, name: &PackageName, include: &str) -> Result<String, InstallError> {
        appa_package::RelativePath::parse(include).map_err(|error| InstallError::Invalid(error.to_string()))?;
        if !self.batteries.contains(name.as_str()) {
            return Err(InstallError::Invalid(
                "select the battery before adding its include".into(),
            ));
        }
        let mut document: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))?;
        if document.get("include").is_none() {
            // Root keys must precede tables; toml_edit handles that without
            // serializing the authored tables or changing their order.
            document["include"] = toml_edit::value(toml_edit::Array::new());
        }
        let includes = document["include"]
            .as_array_mut()
            .ok_or_else(|| InstallError::Invalid("include must be an array".into()))?;
        if includes.iter().any(|value| value.as_str().is_none()) {
            return Err(InstallError::Invalid("include entries must be strings".into()));
        }
        if let Some(owned) = self.includes.iter().find(|entry| entry.battery == name.as_str())
            && (owned.path != include
                || includes
                    .iter()
                    .filter(|value| value.as_str() == Some(&owned.path))
                    .count()
                    != 1)
        {
            return Err(InstallError::Invalid(
                "the owned battery include was changed; resolve it before installing".into(),
            ));
        }
        if includes.iter().any(|value| value.as_str() == Some(include)) {
            return Ok(text.to_owned());
        }
        includes.push(include);
        self.includes.push(OwnedInclude {
            battery: name.to_string(),
            path: include.to_owned(),
        });
        Ok(document.to_string())
    }

    pub fn remove_battery_include(&mut self, text: &str, name: &PackageName) -> Result<String, InstallError> {
        let Some(index) = self.includes.iter().position(|entry| entry.battery == name.as_str()) else {
            return Ok(text.to_owned());
        };
        let mut document: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))?;
        let includes = document
            .get_mut("include")
            .and_then(toml_edit::Item::as_array_mut)
            .ok_or_else(|| InstallError::Invalid("the owned include was changed; resolve it before removing".into()))?;
        let matches: Vec<_> = includes
            .iter()
            .enumerate()
            .filter_map(|(i, value)| (value.as_str() == Some(&self.includes[index].path)).then_some(i))
            .collect();
        if matches.len() != 1 {
            return Err(InstallError::Invalid(
                "the owned include was changed or duplicated".into(),
            ));
        }
        includes.remove(matches[0]);
        self.includes.remove(index);
        Ok(document.to_string())
    }
}

/// Holding this value holds the advisory installation lock. The lock inode
/// remains on disk on every exit, including process death. Local filesystems
/// are required; this is not a distributed lock for shared network mounts.
pub struct Installation {
    config: PathBuf,
    state: PathBuf,
    _lock: File,
}

impl Installation {
    fn validate_removal(&self, text: &str, removed: &Path) -> Result<(), InstallError> {
        let parent = self.config.parent().expect("installation config has a parent");
        let removed = fs::canonicalize(removed).map_err(|error| io("resolve removed battery", removed, error))?;
        let document: toml::Value = toml::from_str(text).map_err(|error| InstallError::Invalid(error.to_string()))?;
        for include in document
            .get("include")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(path) = include.as_str() {
                let path = parent.join(path);
                if fs::canonicalize(&path)
                    .map_err(|error| io("resolve remaining include", &path, error))?
                    .starts_with(&removed)
                {
                    return Err(InstallError::Invalid(
                        "a manual include still references this battery; edit it explicitly before removal".into(),
                    ));
                }
            }
        }
        let mut candidate =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| io("stage removal", parent, error))?;
        candidate
            .write_all(text.as_bytes())
            .map_err(|error| io("write removal candidate", candidate.path(), error))?;
        let config =
            crate::config::Config::load(candidate.path()).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let externals = &config.externals;
        let commands = externals
            .authorities
            .values()
            .chain(externals.sanitizers.values())
            .chain(externals.audience.values())
            .chain(externals.identity.values())
            .filter_map(|implementation| match implementation {
                crate::config::Implementation::Command(command) => Some(command),
                _ => None,
            })
            .chain(
                externals
                    .annotators
                    .values()
                    .filter_map(|implementation| match implementation {
                        crate::config::AnnotatorImplementation::Command(command) => Some(command),
                        _ => None,
                    }),
            );
        for command in commands {
            if command.cwd.starts_with(&removed)
                || command.argv.iter().any(|argument| {
                    fs::canonicalize(command.cwd.join(argument)).is_ok_and(|path| path.starts_with(&removed))
                })
            {
                return Err(InstallError::Invalid(
                    "an external command still references this battery; edit its binding explicitly before removal"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// Read-only inspection does not create directories or acquire a mutation
    /// lock. Atomic selection publication gives readers a complete record.
    pub fn inspect(config: &Path) -> Result<Option<Selection>, InstallError> {
        let path = std::path::absolute(config).map_err(|error| io("resolve config", config, error))?;
        let parent = path
            .parent()
            .ok_or_else(|| InstallError::Invalid("config has no parent".into()))?;
        let name = path
            .file_name()
            .ok_or_else(|| InstallError::Invalid("config has no filename".into()))?;
        let state = parent.join(".appa").join(name);
        for directory in [parent.join(".appa"), state.clone()] {
            require_directory_or_absent(&directory)?;
        }
        if optional_bytes(&state.join("transaction.json"))?.is_some() {
            return Err(InstallError::Recovery {
                path: state.join("transaction.json"),
                reason: "an installation transaction is pending; rerun the interrupted install".into(),
            });
        }
        read_selection(&state.join("active.json"))
    }

    pub fn open(config: &Path) -> Result<Self, InstallError> {
        let absolute = std::path::absolute(config).map_err(|error| io("resolve config", config, error))?;
        let parent = absolute
            .parent()
            .ok_or_else(|| InstallError::Invalid("config has no parent".into()))?;
        fs::create_dir_all(parent).map_err(|error| io("create config directory", parent, error))?;
        let parent = fs::canonicalize(parent).map_err(|error| io("resolve config directory", parent, error))?;
        let name = absolute
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| InstallError::Invalid("config filename must be UTF-8".into()))?;
        let config = parent.join(name);
        require_file_or_absent(&config)?;
        let state = parent.join(".appa").join(name);
        for path in [parent.join(".appa"), state.clone()] {
            require_directory_or_absent(&path)?;
            fs::create_dir_all(&path).map_err(|error| io("create installation state", &path, error))?;
        }
        let lock_path = state.join("install.lock");
        require_file_or_absent(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io("open installation lock", &lock_path, error))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(InstallError::Busy(lock_path)),
            Err(std::fs::TryLockError::Error(error)) => return Err(io("lock installation", &lock_path, error)),
        }
        Ok(Self {
            config,
            state,
            _lock: lock,
        })
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }
    pub fn state_path(&self) -> &Path {
        &self.state
    }

    /// Cache acquired archives by digest. A failed copy cannot replace an
    /// existing artifact or make a partially written one visible.
    pub fn retain(&self, acquired: &Acquired) -> Result<PathBuf, InstallError> {
        let tree = self.publish_packages(acquired.marketplace(), acquired.generation())?;
        let directory = self.state.join("artifacts");
        require_directory_or_absent(&directory)?;
        fs::create_dir_all(&directory).map_err(|error| io("create artifact cache", &directory, error))?;
        let declared = acquired.generation().archives();
        for (name, source) in acquired.archives() {
            let digest = declared
                .get(name)
                .ok_or_else(|| InstallError::Invalid("undeclared acquired artifact".into()))?;
            acquisition::verify_artifact(source, digest)?;
            let target = directory.join(digest.hex());
            if target.exists() {
                acquisition::verify_artifact(&target, digest)?;
                continue;
            }
            require_file_or_absent(&target)?;
            let mut stage =
                tempfile::NamedTempFile::new_in(&directory).map_err(|error| io("stage artifact", &directory, error))?;
            let input = open_regular(source)?;
            let copied = std::io::copy(&mut input.take(512 * 1024 * 1024 + 1), &mut stage)
                .map_err(|error| io("copy artifact", stage.path(), error))?;
            if copied > 512 * 1024 * 1024 {
                return Err(InstallError::Invalid("artifact exceeds its byte limit".into()));
            }
            stage
                .as_file()
                .sync_all()
                .map_err(|error| io("sync artifact", stage.path(), error))?;
            acquisition::verify_artifact(stage.path(), digest)?;
            stage
                .persist_noclobber(&target)
                .map_err(|error| io("publish artifact", &target, error.error))?;
        }
        sync_directory(&directory)?;
        Ok(tree)
    }

    /// An offline export contains policy configuration and must be handled as
    /// operator configuration, not posted as a public release artifact.
    pub fn export_bundle(&self, output: &Path) -> Result<ArtifactDigest, InstallError> {
        if self.state.join("transaction.json").exists() {
            return Err(InstallError::Recovery {
                path: self.state.join("transaction.json"),
                reason: "finish the pending installation before exporting".into(),
            });
        }
        let selection = self
            .selection()?
            .ok_or_else(|| InstallError::Invalid("no installed selection to export".into()))?;
        let config = required_bytes(&self.config)?;
        selection.validate_owned_config(
            std::str::from_utf8(&config).map_err(|error| InstallError::Invalid(error.to_string()))?,
        )?;
        crate::config::Config::load(&self.config).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let generation_root = self.state.join("generations").join(selection.commit().as_str());
        let marketplace = generation_root.join("marketplace");
        selection.validate_packages(&marketplace)?;
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut stage = tempfile::NamedTempFile::new_in(parent).map_err(|error| io("stage export", output, error))?;
        {
            let encoder = flate2::write::GzEncoder::new(stage.as_file_mut(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            append_bytes(
                &mut archive,
                "selection.json",
                &serde_json::to_vec(&selection).map_err(|error| InstallError::Invalid(error.to_string()))?,
            )?;
            append_bytes(
                &mut archive,
                DESCRIPTOR_FILE,
                &serde_json::to_vec(selection.generation())
                    .map_err(|error| InstallError::Invalid(error.to_string()))?,
            )?;
            append_bytes(&mut archive, "config.toml", &config)?;
            archive
                .append_dir_all("marketplace", &marketplace)
                .map_err(|error| io("archive packages", &marketplace, error))?;
            for name in acquisition::required_archives(selection.generation(), selection.requirements()) {
                let digest = selection.generation().archives()[&name].clone();
                let path = self.state.join("artifacts").join(digest.hex());
                acquisition::verify_artifact(&path, &digest)?;
                archive
                    .append_path_with_name(&path, format!("artifacts/{name}"))
                    .map_err(|error| io("archive artifact", &path, error))?;
            }
            archive
                .into_inner()
                .map_err(|error| io("finish bundle", output, error))?
                .finish()
                .map_err(|error| io("finish compressed bundle", output, error))?;
        }
        if optional_bytes(&self.config)?.as_deref() != Some(config.as_slice()) {
            return Err(InstallError::Changed(self.config.clone()));
        }
        stage
            .as_file()
            .sync_all()
            .map_err(|error| io("sync bundle", output, error))?;
        let digest = ArtifactDigest::of_reader(
            File::open(stage.path()).map_err(|error| io("read bundle", stage.path(), error))?,
            512 * 1024 * 1024,
        )
        .map_err(|error| io("hash bundle", stage.path(), error))?;
        stage
            .persist_noclobber(output)
            .map_err(|error| io("publish bundle without overwriting", output, error.error))?;
        sync_directory(parent)?;
        Ok(digest)
    }

    pub fn selection(&self) -> Result<Option<Selection>, InstallError> {
        read_selection(&self.state.join("active.json"))
    }

    /// Publish a package-only snapshot after validating the catalog, every
    /// package, and ownership. A matching cached tree is rehashed before reuse.
    pub fn publish_packages(&self, source: &Path, generation: &Generation) -> Result<PathBuf, InstallError> {
        verify_packages(source, generation)?;
        let generations = self.state.join("generations");
        require_directory_or_absent(&generations)?;
        fs::create_dir_all(&generations).map_err(|error| io("create generations", &generations, error))?;
        let destination = generations.join(generation.commit().as_str());
        require_directory_or_absent(&destination)?;
        if destination.exists() {
            let descriptor = required_bytes(&destination.join(DESCRIPTOR_FILE))?;
            let cached = Generation::parse(&descriptor).map_err(|error| InstallError::Invalid(error.to_string()))?;
            if &cached != generation {
                return Err(InstallError::Invalid(
                    "one commit has conflicting generation descriptors".into(),
                ));
            }
            verify_packages(&destination.join("marketplace"), generation)?;
            return Ok(destination);
        }
        let stage = tempfile::tempdir_in(&generations).map_err(|error| io("stage generation", &generations, error))?;
        let packages = stage.path().join("marketplace");
        copy_package_tree(source, &packages)?;
        verify_packages(&packages, generation)?;
        write_synced(
            &stage.path().join(DESCRIPTOR_FILE),
            &serde_json::to_vec(generation).map_err(|error| InstallError::Invalid(error.to_string()))?,
        )?;
        sync_directory(stage.path())?;
        fs::rename(stage.path(), &destination).map_err(|error| io("publish generation", &destination, error))?;
        sync_directory(&generations)?;
        Ok(destination)
    }

    /// A journal covers only the non-atomic config/selection switch. Immutable
    /// tree publication needs no journal. Host activation extends this same
    /// record before it performs any external mutation.
    pub fn commit_config(
        &self,
        before: Option<&[u8]>,
        after: &[u8],
        selection: &Selection,
    ) -> Result<(), InstallError> {
        self.recover_config()?;
        if selection.plugins.contains("claude-code")
            || self
                .selection()?
                .is_some_and(|selected| selected.plugins.contains("claude-code"))
        {
            return Err(InstallError::Invalid(
                "Claude selection requires native activation through commit_installation".into(),
            ));
        }
        self.commit_with_activation(before, after, selection, Activation::None)
    }

    /// Activate host support before publishing the selected generation. The
    /// durable journal remains until both native activation and config agree.
    pub fn commit_installation(
        &self,
        before: Option<&[u8]>,
        after: &[u8],
        selection: &Selection,
    ) -> Result<(), InstallError> {
        self.recover_config()?;
        let activation = if selection.plugins.contains("claude-code") {
            Activation::Claude
        } else if self
            .selection()?
            .is_some_and(|selected| selected.plugins.contains("claude-code"))
        {
            Activation::RemoveClaude
        } else {
            Activation::None
        };
        self.commit_with_activation(before, after, selection, activation)
    }

    fn commit_with_activation(
        &self,
        before: Option<&[u8]>,
        after: &[u8],
        selection: &Selection,
        activation: Activation,
    ) -> Result<(), InstallError> {
        self.recover_config()?;
        selection.validate()?;
        selection.validate_owned_config(
            std::str::from_utf8(after).map_err(|error| InstallError::Invalid(error.to_string()))?,
        )?;
        if !selection.plugins.is_empty() || !selection.batteries.is_empty() {
            selection.validate_packages(
                &self
                    .state
                    .join("generations")
                    .join(selection.commit().as_str())
                    .join("marketplace"),
            )?;
        }
        if optional_bytes(&self.config)?.as_deref() != before {
            return Err(InstallError::Changed(self.config.clone()));
        }
        // Validate beside the real config, so relative user includes and
        // package helper origins have exactly the activation-time meaning.
        let parent = self.config.parent().expect("open resolves a config parent");
        let mut candidate =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| io("stage config", parent, error))?;
        candidate
            .write_all(after)
            .map_err(|error| io("write candidate config", candidate.path(), error))?;
        crate::config::Config::load(candidate.path()).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let previous = self.selection()?;
        if activation == Activation::Claude {
            // Missing or mismatched executables fail before the config changes.
            native::ClaudeArtifacts::prepare(self, selection.generation(), selection.platform)?;
        } else if activation == Activation::RemoveClaude {
            let previous = previous
                .as_ref()
                .ok_or_else(|| InstallError::Invalid("native removal has no prior selection".into()))?;
            native::ClaudeArtifacts::prepare(self, previous.generation(), previous.platform)?;
        }
        let journal_path = self.state.join("transaction.json");
        let journal = ConfigTransaction {
            before: before.map(Vec::from),
            after: after.to_vec(),
            selection: selection.clone(),
            activation,
            previous: if activation != Activation::None { previous } else { None },
        };
        atomic_write(
            &journal_path,
            &serde_json::to_vec(&journal).map_err(|error| InstallError::Invalid(error.to_string()))?,
        )?;
        self.replace_journalled_config(before, after)?;
        self.recover_config()
    }

    fn replace_journalled_config(&self, before: Option<&[u8]>, after: &[u8]) -> Result<(), InstallError> {
        if optional_bytes(&self.config)?.as_deref() != before {
            // No activation or config mutation has happened yet. This journal
            // belongs to the aborted attempt, not to the editor's new bytes.
            let journal = self.state.join("transaction.json");
            fs::remove_file(&journal).and_then(|()| {
                #[cfg(unix)]
                File::open(&self.state)?.sync_all()?;
                Ok(())
            }).map_err(|error| InstallError::Recovery {
                path: journal, reason: format!("config was edited and remains untouched, but its uncommitted journal could not be removed: {error}"),
            })?;
            return Err(InstallError::Changed(self.config.clone()));
        }
        atomic_write(&self.config, after)
    }

    /// If the new config is present, finish its selection publication. If only
    /// the old config is present, abandon the uncommitted switch. Any third
    /// value is a user edit, not permission to overwrite it during recovery.
    pub fn recover_config(&self) -> Result<(), InstallError> {
        let path = self.state.join("transaction.json");
        let Some(bytes) = optional_bytes(&path)? else {
            return Ok(());
        };
        let transaction: ConfigTransaction =
            serde_json::from_slice(&bytes).map_err(|error| InstallError::Recovery {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        transaction.selection.validate()?;
        let selected_claude = transaction.selection.plugins.contains("claude-code");
        let activation_matches = match transaction.activation {
            Activation::None => !selected_claude,
            Activation::Claude => selected_claude,
            Activation::RemoveClaude => {
                !selected_claude
                    && transaction.previous.as_ref().is_some_and(|previous| {
                        previous.plugins.contains("claude-code")
                            && previous.generation() == transaction.selection.generation()
                    })
            }
        };
        if !activation_matches {
            return Err(InstallError::Recovery {
                path: path.clone(),
                reason: "journal activation does not match its selected host".into(),
            });
        }
        let current = optional_bytes(&self.config)?;
        if current.as_deref() == Some(&transaction.after) {
            let validate = || {
                transaction.selection.validate_owned_config(
                    std::str::from_utf8(&transaction.after)
                        .map_err(|error| InstallError::Invalid(error.to_string()))?,
                )?;
                if !transaction.selection.plugins.is_empty() || !transaction.selection.batteries.is_empty() {
                    transaction.selection.validate_packages(
                        &self
                            .state
                            .join("generations")
                            .join(transaction.selection.commit().as_str())
                            .join("marketplace"),
                    )?;
                }
                crate::config::Config::load(&self.config).map_err(|error| InstallError::Invalid(error.to_string()))?;
                Ok::<_, InstallError>(())
            };
            validate().map_err(|error| InstallError::Recovery {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            if transaction.activation != Activation::None {
                let activate = || {
                    let previous = transaction
                        .previous
                        .as_ref()
                        .filter(|selection| selection.plugins.contains("claude-code"))
                        .map(|selection| {
                            native::ClaudeArtifacts::prepare(self, selection.generation(), selection.platform)
                        })
                        .transpose()?;
                    match transaction.activation {
                        Activation::Claude => native::ClaudeArtifacts::prepare(
                            self,
                            transaction.selection.generation(),
                            transaction.selection.platform,
                        )?
                        .activate(&self.config, previous.as_ref())?,
                        Activation::RemoveClaude => previous
                            .as_ref()
                            .ok_or_else(|| InstallError::Invalid("removal requires its prior native artifact".into()))?
                            .remove(&self.config)?,
                        Activation::None => unreachable!("native activation branch excludes None"),
                    }
                    Ok::<_, InstallError>(())
                };
                activate().map_err(|error| InstallError::Recovery {
                    path: path.clone(),
                    reason: error.to_string(),
                })?;
            }
            let selected =
                serde_json::to_vec(&transaction.selection).map_err(|error| InstallError::Invalid(error.to_string()))?;
            let history = self.state.join("history");
            require_directory_or_absent(&history)?;
            fs::create_dir_all(&history).map_err(|error| io("create selection history", &history, error))?;
            atomic_write(
                &history.join(format!("{}.json", ArtifactDigest::of_bytes(&selected).hex())),
                &selected,
            )?;
            atomic_write(&self.state.join("active.json"), &selected)?;
        } else if current != transaction.before {
            return Err(InstallError::Recovery {
                path,
                reason: "config differs from both recorded states; resolve the manual edit before retrying".into(),
            });
        }
        fs::remove_file(&path).map_err(|error| io("finish config transaction", &path, error))?;
        sync_directory(&self.state)
    }
}

fn read_selection(path: &Path) -> Result<Option<Selection>, InstallError> {
    let Some(bytes) = optional_bytes(path)? else {
        return Ok(None);
    };
    let selection: Selection =
        serde_json::from_slice(&bytes).map_err(|error| InstallError::Invalid(error.to_string()))?;
    selection.validate()?;
    Ok(Some(selection))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigTransaction {
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    selection: Selection,
    activation: Activation,
    previous: Option<Selection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Activation {
    None,
    Claude,
    RemoveClaude,
}

fn require_file_or_absent(path: &Path) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(InstallError::Invalid(format!(
            "{} must be a regular file, not a link or special file",
            path.display()
        ))),
        Err(error) => Err(io("inspect file", path, error)),
    }
}

fn require_directory_or_absent(path: &Path) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(InstallError::Invalid(format!(
            "{} must be a directory, not a link",
            path.display()
        ))),
        Err(error) => Err(io("inspect directory", path, error)),
    }
}

pub(crate) fn optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    require_file_or_absent(path)?;
    let file = match open_regular(path) {
        Ok(file) => file,
        Err(InstallError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io("read installation file", path, error))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(InstallError::Invalid(format!(
            "{} exceeds the state byte limit",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

pub(crate) fn open_regular(path: &Path) -> Result<File, InstallError> {
    require_file_or_absent(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not hang if an untrusted import path is exchanged for a FIFO
        // between inspection and open. Inspect the opened handle as well.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| io("open regular file", path, error))?;
    if !file
        .metadata()
        .map_err(|error| io("inspect opened file", path, error))?
        .is_file()
    {
        return Err(InstallError::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

fn required_bytes(path: &Path) -> Result<Vec<u8>, InstallError> {
    optional_bytes(path)?.ok_or_else(|| InstallError::Invalid(format!("{} is missing", path.display())))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| io("create file", path, error))?;
    file.write_all(bytes).map_err(|error| io("write file", path, error))?;
    file.sync_all().map_err(|error| io("sync file", path, error))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(InstallError::Invalid(format!(
            "{} exceeds the state byte limit",
            path.display()
        )));
    }
    require_file_or_absent(path)?;
    let parent = path.parent().expect("installation paths have parents");
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| io("stage file", path, error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| io("write staged file", path, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io("sync staged file", path, error))?;
    temporary
        .persist(path)
        .map_err(|error| io("replace file", path, error.error))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), InstallError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io("sync directory", path, error))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn append_bytes(
    writer: &mut tar::Builder<flate2::write::GzEncoder<&mut File>>,
    name: &str,
    bytes: &[u8],
) -> Result<(), InstallError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    writer
        .append_data(&mut header, name, bytes)
        .map_err(|error| io("archive metadata", Path::new(name), error))
}

fn copy_package_tree(source: &Path, destination: &Path) -> Result<(), InstallError> {
    let entries = appa_package::tree::walk(source).map_err(|error| InstallError::Invalid(error.to_string()))?;
    fs::create_dir(destination).map_err(|error| io("create package snapshot", destination, error))?;
    let mut remaining = appa_package::tree::MAX_UNCOMPRESSED_BYTES;
    for entry in entries {
        let target = destination.join(entry.portable);
        match entry.kind {
            appa_package::tree::EntryKind::Directory => {
                fs::create_dir_all(&target).map_err(|error| io("create package directory", &target, error))?
            }
            appa_package::tree::EntryKind::File => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| io("create package directory", parent, error))?;
                }
                // Package contents use the tree budget, not the smaller JSON
                // state-record budget. Stream them and bound growth after walk.
                let input = open_regular(&entry.absolute)?;
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)
                    .map_err(|error| io("create package file", &target, error))?;
                let copied = std::io::copy(&mut input.take(remaining + 1), &mut output)
                    .map_err(|error| io("copy package file", &target, error))?;
                remaining = remaining
                    .checked_sub(copied)
                    .ok_or_else(|| InstallError::Invalid("package snapshot exceeds its tree byte limit".into()))?;
                output
                    .sync_all()
                    .map_err(|error| io("sync package file", &target, error))?;
            }
        }
    }
    // Flush children before publishing their parent entry.
    for entry in appa_package::tree::walk(destination)
        .map_err(|error| InstallError::Invalid(error.to_string()))?
        .into_iter()
        .rev()
    {
        if entry.kind == appa_package::tree::EntryKind::Directory {
            sync_directory(&entry.absolute)?;
        }
    }
    sync_directory(destination)
}

fn verify_packages(root: &Path, generation: &Generation) -> Result<Vec<Package>, InstallError> {
    require_directory_or_absent(root)?;
    // Inspect the entire snapshot first: unlisted links are rejected too.
    appa_package::tree::walk(root).map_err(|error| InstallError::Invalid(error.to_string()))?;
    let path = root.join("marketplace.toml");
    let bytes = required_bytes(&path)?;
    if &ArtifactDigest::of_bytes(&bytes) != generation.catalog() {
        return Err(InstallError::Invalid("catalog digest mismatch".into()));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| InstallError::Invalid(error.to_string()))?;
    let catalog = Marketplace::parse(text, &path).map_err(|error| InstallError::Invalid(error.to_string()))?;
    let mut packages = Vec::new();
    for entry in catalog.packages {
        let path = root.join(entry.path.as_path());
        let package =
            appa_package::validate_package(&path).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let kind = match package.role {
            Role::Plugin(_) => PackageKind::Plugin,
            Role::Battery(_) => PackageKind::Battery,
        };
        if package.name != entry.name
            || kind != entry.kind
            || TreeDigest::of_tree(&path).map_err(|error| InstallError::Invalid(error.to_string()))? != entry.digest
        {
            return Err(InstallError::Invalid(format!(
                "package {} does not match its catalog identity",
                entry.name
            )));
        }
        packages.push(package);
    }
    appa_package::check_ownership(&packages).map_err(|error| InstallError::Invalid(error.to_string()))?;
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_files_use_the_tree_budget_not_the_state_record_budget() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("snapshot");
        fs::create_dir(&source).unwrap();
        let input = File::create(source.join("asset.bin")).unwrap();
        input.set_len(MAX_STATE_BYTES + 1).unwrap();
        copy_package_tree(&source, &destination).unwrap();
        assert_eq!(
            TreeDigest::of_tree(&source).unwrap(),
            TreeDigest::of_tree(&destination).unwrap()
        );
        assert!(
            required_bytes(&destination.join("asset.bin")).is_err(),
            "state records retain their smaller bound"
        );
    }

    pub(super) fn selection() -> Selection {
        Selection::empty(generation(b"schema = 1\nname = 'appa'\n"), Platform::MacArm64)
    }

    fn generation(catalog: &[u8]) -> Generation {
        use appa_package::generation::{Image, Platform, REPOSITORY};
        use std::collections::BTreeMap;
        let digest = ArtifactDigest::of_bytes(b"artifact");
        let document = serde_json::json!({"schema": 1, "repository": REPOSITORY,
            "commit": "a".repeat(40), "release": "v1.0.0", "protocol": appa_package::PROTOCOL,
            "catalog": ArtifactDigest::of_bytes(catalog), "marketplace": digest,
            "claude_plugin": digest, "runtime_chart": digest,
            "binaries": Platform::ALL.into_iter().map(|p| (p, digest.clone())).collect::<BTreeMap<_, _>>(),
            "images": Image::ALL.into_iter().map(|i| (i, serde_json::json!({"digest": digest,
                "platforms": {"linux/amd64": digest}}))).collect::<BTreeMap<_, _>>()});
        Generation::parse(&serde_json::to_vec(&document).unwrap()).unwrap()
    }

    #[test]
    fn package_publication_rechecks_catalog_and_retained_trees() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let battery = source.path().join("batteries/github");
        fs::create_dir_all(&battery).unwrap();
        fs::write(battery.join("appa-package.toml"), "schema = 1\nname = 'github'\ndescription = 'test'\n[battery]\npolicy = 'appa.toml'\nhosts = ['claude-code', 'kagent']\n").unwrap();
        fs::write(
            battery.join("appa.toml"),
            "[policy]\nversion = 2\n[[policy.tool]]\nname = 'mcp/github/read'\n",
        )
        .unwrap();
        let catalog = format!(
            "schema = 1\nname = 'appa'\n[packages.battery.github]\npath = 'batteries/github'\ndigest = '{}'\n",
            TreeDigest::of_tree(&battery).unwrap()
        );
        fs::write(source.path().join("marketplace.toml"), &catalog).unwrap();
        let generation = generation(catalog.as_bytes());
        let install = Installation::open(&root.path().join("appa.toml")).unwrap();
        let published = install.publish_packages(source.path(), &generation).unwrap();
        let mut selected = Selection::empty(generation.clone(), Platform::MacArm64);
        selected.select(PackageKind::Battery, &PackageName::parse("github").unwrap());
        assert!(selected.validate_packages(&published.join("marketplace")).is_ok());
        let mut forged = selected.clone();
        forged.select(PackageKind::Battery, &PackageName::parse("missing").unwrap());
        assert!(forged.validate_packages(&published.join("marketplace")).is_err());
        assert!(
            install
                .commit_config(
                    None,
                    b"[policy]\nversion=2\n[externals]\ntimeout_ms=100\nmax_body_bytes=1024\n",
                    &forged
                )
                .is_err()
        );
        assert!(!install.config_path().exists());
        assert!(install.selection().unwrap().is_none());
        let github = PackageName::parse("github").unwrap();
        let mut forged_alias = selected.clone();
        forged_alias
            .associate_battery("", &github, "other-provider", "work")
            .unwrap();
        assert!(forged_alias.validate_packages(&published.join("marketplace")).is_err());
        let base = "# authored deployment\n[policy]\nversion=2\n[externals]\ntimeout_ms=100\nmax_body_bytes=1024\n";
        let include = format!(
            ".appa/appa.toml/generations/{}/marketplace/batteries/github/appa.toml",
            generation.commit()
        );
        let with_include = selected.include_battery(base, &github, &include).unwrap();
        let with_alias = selected
            .associate_battery(&with_include, &github, "github", "work-github")
            .unwrap();
        install.commit_config(None, with_alias.as_bytes(), &selected).unwrap();
        let effective = crate::config::Config::load(install.config_path()).unwrap();
        assert_eq!(effective.server_aliases["github"], "work-github");
        assert_eq!(
            effective.policy_file().value()["tool"][0]["name"].as_str(),
            Some("mcp/github/read")
        );
        let mut removed = selected.clone();
        let without_include = removed.remove_battery_include(&with_alias, &github).unwrap();
        let without_alias = removed.remove_battery_aliases(&without_include, &github).unwrap();
        removed.deselect(PackageKind::Battery, &github).unwrap();
        install
            .commit_config(Some(with_alias.as_bytes()), without_alias.as_bytes(), &removed)
            .unwrap();
        assert!(without_alias.contains(base));
        assert!(
            crate::config::Config::load(install.config_path())
                .unwrap()
                .policy_file()
                .value()
                .get("tool")
                .is_none()
        );
        assert_eq!(install.publish_packages(source.path(), &generation).unwrap(), published);
        let retained = published.join("marketplace/batteries/github/appa.toml");
        fs::write(&retained, "[policy]\nversion = 2\n").unwrap();
        assert!(install.publish_packages(source.path(), &generation).is_err());
        assert_eq!(install.selection().unwrap(), Some(removed));
        fs::write(source.path().join("marketplace.toml"), "wrong catalog").unwrap();
        assert!(install.publish_packages(source.path(), &generation).is_err());
    }

    #[test]
    fn successful_switch_records_the_same_generation_as_the_active_config() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let install = Installation::open(&path).unwrap();
        let after = b"[policy]\nversion = 2\n[externals]\ntimeout_ms = 100\nmax_body_bytes = 1024\n";
        install.commit_config(None, after, &selection()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), after);
        assert_eq!(install.selection().unwrap(), Some(selection()));
        install.commit_config(Some(after), after, &selection()).unwrap();
        assert_eq!(fs::read_dir(install.state.join("history")).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_install_is_refused_and_lock_recovers_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let first = Installation::open(&path).unwrap();
        assert!(matches!(Installation::open(&path), Err(InstallError::Busy(_))));
        let lock_path = first.state.join("install.lock");
        drop(first);
        assert!(lock_path.is_file());
        assert!(Installation::open(&path).is_ok());
    }

    #[test]
    fn include_edit_preserves_authored_content_and_never_adopts_manual_entries() {
        let battery = PackageName::parse("github").unwrap();
        let path = ".appa/appa.toml/generations/abc/marketplace/batteries/github/appa.toml";
        let original = "# my policy\n[policy]\nversion = 2\n# keep this order\n[[policy.tool]]\nname = 'Read'\n";
        let mut selected = selection();
        selected.select(PackageKind::Battery, &battery);
        let added = selected.include_battery(original, &battery, path).unwrap();
        assert!(added.contains(original));
        assert_eq!(selected.include_battery(&added, &battery, path).unwrap(), added);
        assert_eq!(selected.includes.len(), 1);
        let removed = selected.remove_battery_include(&added, &battery).unwrap();
        assert!(removed.contains(original));
        assert!(selected.includes.is_empty());
        let mut manual = selection();
        manual.select(PackageKind::Battery, &battery);
        assert_eq!(manual.include_battery(&added, &battery, path).unwrap(), added);
        assert!(manual.includes.is_empty());
        assert_eq!(manual.remove_battery_include(&added, &battery).unwrap(), added);
    }

    #[test]
    fn changed_owned_include_is_not_removed() {
        let mut selected = selection();
        let battery = PackageName::parse("github").unwrap();
        selected.select(PackageKind::Battery, &battery);
        let added = selected
            .include_battery("[policy]\nversion = 2\n", &battery, "owned.toml")
            .unwrap();
        assert!(
            selected
                .remove_battery_include(&added.replace("owned.toml", "custom.toml"), &battery)
                .is_err()
        );
        assert_eq!(selected.includes.len(), 1);
    }

    #[test]
    fn invalid_policy_and_concurrent_edit_leave_config_and_selection_untouched() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let before = b"[policy]\nversion = 2\n";
        fs::write(&path, before).unwrap();
        let install = Installation::open(&path).unwrap();
        assert!(
            install
                .commit_config(Some(before), b"invalid = [", &selection())
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(install.selection().unwrap().is_none());
        assert!(matches!(
            install.commit_config(Some(b"stale"), before, &selection()),
            Err(InstallError::Changed(_))
        ));
    }

    #[test]
    fn crash_after_config_replace_completes_selection_on_recovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let install = Installation::open(&path).unwrap();
        let after = b"[policy]\nversion = 2\n[externals]\ntimeout_ms=100\nmax_body_bytes=1024\n";
        let transaction = ConfigTransaction {
            before: None,
            after: after.to_vec(),
            selection: selection(),
            activation: Activation::None,
            previous: None,
        };
        atomic_write(
            &install.state.join("transaction.json"),
            &serde_json::to_vec(&transaction).unwrap(),
        )
        .unwrap();
        fs::write(&path, after).unwrap();
        drop(install);
        let install = Installation::open(&path).unwrap();
        install.recover_config().unwrap();
        assert_eq!(install.selection().unwrap(), Some(selection()));
        assert!(!install.state.join("transaction.json").exists());
    }

    #[test]
    fn manual_edit_after_journal_creation_abandons_only_the_uncommitted_journal() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let install = Installation::open(&path).unwrap();
        let journal = install.state.join("transaction.json");
        let transaction = ConfigTransaction {
            before: None,
            after: b"proposed".to_vec(),
            selection: selection(),
            activation: Activation::None,
            previous: None,
        };
        atomic_write(&journal, &serde_json::to_vec(&transaction).unwrap()).unwrap();
        fs::write(&path, b"manual edit").unwrap();
        assert!(matches!(
            install.replace_journalled_config(None, b"proposed"),
            Err(InstallError::Changed(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"manual edit");
        assert!(!journal.exists());
        assert!(install.recover_config().is_ok());
        assert!(install.selection().unwrap().is_none());
    }

    #[test]
    fn deselection_cannot_discard_ownership_without_removing_the_config_entries() {
        let mut selection = selection();
        let name = PackageName::parse("github").unwrap();
        selection.select(PackageKind::Battery, &name);
        let included = selection.include_battery("", &name, "owned.toml").unwrap();
        let aliased = selection.associate_battery(&included, &name, "github", "work").unwrap();
        let before = selection.clone();
        assert!(selection.deselect(PackageKind::Battery, &name).is_err());
        assert_eq!(selection, before);
        let no_include = selection.remove_battery_include(&aliased, &name).unwrap();
        assert!(selection.deselect(PackageKind::Battery, &name).is_err());
        selection.remove_battery_aliases(&no_include, &name).unwrap();
        selection.deselect(PackageKind::Battery, &name).unwrap();
        assert!(selection.validate().is_ok());
    }

    #[test]
    fn crash_before_config_replace_abandons_switch_but_preserves_manual_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appa.toml");
        let install = Installation::open(&path).unwrap();
        let journal = install.state.join("transaction.json");
        let transaction = ConfigTransaction {
            before: None,
            after: b"new".to_vec(),
            selection: selection(),
            activation: Activation::None,
            previous: None,
        };
        atomic_write(&journal, &serde_json::to_vec(&transaction).unwrap()).unwrap();
        install.recover_config().unwrap();
        assert!(install.selection().unwrap().is_none());
        assert!(!path.exists());
        atomic_write(&journal, &serde_json::to_vec(&transaction).unwrap()).unwrap();
        fs::write(&path, b"manual").unwrap();
        assert!(matches!(install.recover_config(), Err(InstallError::Recovery { .. })));
        assert_eq!(fs::read(&path).unwrap(), b"manual");
        assert!(journal.exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_links_are_rejected_without_touching_the_target() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".appa")).unwrap();
        assert!(Installation::open(&root.path().join("appa.toml")).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
