//! Explicit custom files and manual includes, preserved as an immutable layout.

use super::*;
use std::collections::BTreeMap;

const MANIFEST: &str = "files.json";
const SOURCE: &str = "source.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    size: u64,
    digest: ArtifactDigest,
    executable: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    files: BTreeMap<String, Member>,
    declared: BTreeMap<String, String>,
    includes: BTreeMap<String, String>,
    commands: BTreeMap<String, String>,
}

pub(super) struct Snapshot {
    pub root: PathBuf,
    pub digest: ArtifactDigest,
    manifest: Manifest,
    checks: Vec<(PathBuf, Member)>,
    _stage: Option<tempfile::TempDir>,
}

fn invalid(message: impl ToString) -> InstallError {
    InstallError::Invalid(message.to_string())
}

fn document(text: &str) -> Result<toml_edit::DocumentMut, InstallError> {
    text.parse().map_err(invalid)
}

fn strings(document: &toml_edit::DocumentMut, bundle: bool) -> Result<Vec<String>, InstallError> {
    let item = if bundle {
        document.get("bundle").and_then(|table| table.get("files"))
    } else {
        document.get("include")
    };
    let Some(item) = item else { return Ok(Vec::new()) };
    item.as_array()
        .ok_or_else(|| invalid("bundle files and includes must be arrays"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("file paths must be strings"))
        })
        .collect()
}

fn manual_includes(text: &str, selection: &Selection) -> Result<Vec<String>, InstallError> {
    Ok(strings(&document(text)?, false)?
        .into_iter()
        .filter(|path| !selection.includes.iter().any(|owned| owned.path == *path))
        .collect())
}

pub(super) fn requires_snapshot(text: &str, selection: &Selection) -> Result<bool, InstallError> {
    Ok(!strings(&document(text)?, true)?.is_empty() || !manual_includes(text, selection)?.is_empty())
}

/// Source references may contain parent components; snapshot member paths may not.
fn source_path(base: &Path, authored: &Path) -> Result<PathBuf, InstallError> {
    use std::path::Component;
    let mut path = if authored.is_absolute() {
        PathBuf::new()
    } else {
        base.to_owned()
    };
    let components: Vec<_> = authored.components().collect();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                if !path.pop() {
                    return Err(invalid("source path traverses above its filesystem root"));
                }
            }
            _ => path.push(component.as_os_str()),
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| io("read declared file; check bundle.files", &path, error))?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(invalid(format!(
                "{} must not be a symlink or special file",
                path.display()
            )));
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(invalid(format!(
                "{} is not a directory in the declared source path",
                path.display()
            )));
        }
    }
    Ok(path)
}

fn member(path: &Path) -> Result<Member, InstallError> {
    let file = open_regular(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| io("inspect custom file", path, error))?;
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Ok(Member {
        size: metadata.len(),
        digest: ArtifactDigest::of_reader(file, appa_package::tree::MAX_UNCOMPRESSED_BYTES)
            .map_err(|error| io("hash custom file", path, error))?,
        executable,
    })
}

fn same_member(path: &Path, expected: &Member, modes: bool) -> Result<(), InstallError> {
    let actual = member(path)?;
    if actual.size != expected.size
        || actual.digest != expected.digest
        || (modes && actual.executable != expected.executable)
    {
        return Err(InstallError::Changed(path.to_owned()));
    }
    Ok(())
}

fn digest(root: &Path) -> Result<ArtifactDigest, InstallError> {
    ArtifactDigest::parse(&TreeDigest::of_tree(root).map_err(invalid)?.to_string()).map_err(invalid)
}

fn portable(path: &Path) -> Result<String, InstallError> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("custom file paths must be UTF-8"))?
        .replace(std::path::MAIN_SEPARATOR, "/");
    appa_package::RelativePath::parse(&text).map_err(invalid)?;
    Ok(text)
}

impl Snapshot {
    pub(super) fn executable_paths(&self) -> impl Iterator<Item = &str> {
        self.manifest
            .files
            .iter()
            .filter(|(_, member)| member.executable)
            .map(|(name, _)| name.as_str())
    }

    pub(super) fn copy_for_deployment(&self, target: &Path) -> Result<(), InstallError> {
        copy_package_tree(&self.root, target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for (name, member) in &self.manifest.files {
                let path = target.join(name);
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(if member.executable { 0o755 } else { 0o644 }),
                )
                .map_err(|error| io("preserve helper executable mode", &path, error))?;
            }
        }
        Ok(())
    }

    pub fn capture(
        installation: &Installation,
        selection: &Selection,
        text: &str,
    ) -> Result<Option<Self>, InstallError> {
        let parsed = document(text)?;
        let declared = strings(&parsed, true)?;
        let includes = manual_includes(text, selection)?;
        if declared.is_empty() && includes.is_empty() {
            return Ok(None);
        }
        let base = installation.config.parent().expect("installation config has a parent");
        let declared_sources = declared
            .iter()
            .map(|name| Ok((name.clone(), source_path(base, Path::new(name))?)))
            .collect::<Result<BTreeMap<_, _>, InstallError>>()?;
        let include_sources = includes
            .iter()
            .map(|name| Ok((name.clone(), source_path(base, Path::new(name))?)))
            .collect::<Result<BTreeMap<_, _>, InstallError>>()?;
        if declared_sources.len() != declared.len() || include_sources.len() != includes.len() {
            return Err(invalid("duplicate custom file declarations"));
        }
        let commands = crate::config::root_command_directories(text, base).map_err(invalid)?;
        let commands = commands
            .into_iter()
            .map(|(key, path)| Ok((key, source_path(base, &path)?)))
            .collect::<Result<BTreeMap<_, _>, InstallError>>()?;
        let mut ancestor = commands.values().next().cloned().unwrap_or_else(|| base.to_owned());
        for path in declared_sources
            .values()
            .chain(include_sources.values())
            .chain(commands.values())
        {
            while !path.starts_with(&ancestor) {
                if !ancestor.pop() {
                    return Err(invalid("custom files must share a filesystem root"));
                }
            }
        }
        let offset = |path: &Path| {
            let relative = path.strip_prefix(&ancestor).map_err(invalid)?;
            if relative.as_os_str().is_empty() {
                Ok("tree".to_owned())
            } else {
                portable(&Path::new("tree").join(relative))
            }
        };
        let mut manifest = Manifest {
            schema: 1,
            files: BTreeMap::new(),
            declared: BTreeMap::new(),
            includes: BTreeMap::new(),
            commands: BTreeMap::new(),
        };
        let stage = tempfile::tempdir().map_err(|error| io("stage custom files", base, error))?;
        let root = stage.path().join("snapshot");
        fs::create_dir(&root).map_err(|error| io("create custom snapshot", &root, error))?;
        let mut remaining = appa_package::tree::MAX_UNCOMPRESSED_BYTES;
        let mut checks = Vec::new();
        for (sources, mappings) in [
            (&declared_sources, &mut manifest.declared),
            (&include_sources, &mut manifest.includes),
        ] {
            for (name, path) in sources {
                let target = offset(path)?;
                if manifest.files.contains_key(&target) {
                    return Err(invalid("a custom file and include resolve to the same snapshot member"));
                }
                if manifest.files.len() >= appa_package::tree::MAX_ENTRIES - 2 {
                    return Err(invalid("too many custom files"));
                }
                let expected = member(path)?;
                remaining = remaining
                    .checked_sub(expected.size)
                    .ok_or_else(|| invalid("custom files exceed 64 MiB"))?;
                let destination = root.join(&target);
                fs::create_dir_all(destination.parent().expect("tree files have a parent"))
                    .map_err(|error| io("create custom directory", &destination, error))?;
                let mut output =
                    File::create(&destination).map_err(|error| io("create custom file", &destination, error))?;
                let copied = std::io::copy(&mut open_regular(path)?.take(expected.size + 1), &mut output)
                    .map_err(|error| io("snapshot custom file", path, error))?;
                if copied != expected.size {
                    return Err(InstallError::Changed(path.clone()));
                }
                output
                    .sync_all()
                    .map_err(|error| io("sync custom file", &destination, error))?;
                same_member(&destination, &expected, false)?;
                mappings.insert(name.clone(), target.clone());
                manifest.files.insert(target, expected.clone());
                checks.push((path.clone(), expected));
            }
        }
        for (key, path) in commands {
            if !path.is_dir() {
                return Err(invalid("custom command cwd must be a directory"));
            }
            let target = offset(&path)?;
            fs::create_dir_all(root.join(&target))
                .map_err(|error| io("create command working directory", &root, error))?;
            manifest.commands.insert(key, target);
        }
        write_synced(&root.join(SOURCE), text.as_bytes())?;
        write_synced(&root.join(MANIFEST), &serde_json::to_vec(&manifest).map_err(invalid)?)?;
        let digest = digest(&root)?;
        let snapshot = Self {
            root,
            digest,
            manifest,
            checks,
            _stage: Some(stage),
        };
        snapshot.verify_sources()?;
        Ok(Some(snapshot))
    }

    pub fn read(root: PathBuf, expected: &ArtifactDigest) -> Result<Self, InstallError> {
        require_directory_or_absent(&root)?;
        if &digest(&root)? != expected {
            return Err(InstallError::Changed(root));
        }
        let manifest: Manifest = serde_json::from_slice(&required_bytes(&root.join(MANIFEST))?).map_err(invalid)?;
        if manifest.schema != 1 {
            return Err(invalid("unsupported custom file manifest"));
        }
        let snapshot = Self {
            root,
            digest: expected.clone(),
            manifest,
            checks: Vec::new(),
            _stage: None,
        };
        snapshot.verify_members()?;
        Ok(snapshot)
    }

    fn verify_members(&self) -> Result<(), InstallError> {
        let source = String::from_utf8(required_bytes(&self.root.join(SOURCE))?).map_err(invalid)?;
        let source_document = document(&source)?;
        let declarations: BTreeSet<_> = strings(&source_document, true)?.into_iter().collect();
        if declarations != self.manifest.declared.keys().cloned().collect() {
            return Err(invalid("snapshot source declarations do not match its manifest"));
        }
        let includes: BTreeSet<_> = strings(&source_document, false)?.into_iter().collect();
        if self.manifest.includes.keys().any(|path| !includes.contains(path)) {
            return Err(invalid("snapshot source includes do not match its manifest"));
        }
        let origins = crate::config::root_command_directories(&source, &self.root).map_err(invalid)?;
        if origins.keys().ne(self.manifest.commands.keys()) {
            return Err(invalid("snapshot source command origins do not match its manifest"));
        }
        let mut expected = BTreeSet::from([SOURCE.to_owned(), MANIFEST.to_owned()]);
        let mut directories = BTreeSet::new();
        for (path, file) in &self.manifest.files {
            appa_package::RelativePath::parse(path).map_err(invalid)?;
            if !path.starts_with("tree/") {
                return Err(invalid("custom members must live under tree/"));
            }
            same_member(&self.root.join(path), file, false)?;
            expected.insert(path.clone());
        }
        for path in self.manifest.declared.values().chain(self.manifest.includes.values()) {
            if !self.manifest.files.contains_key(path) {
                return Err(invalid("custom mapping refers to an absent file"));
            }
        }
        let mapped: BTreeSet<_> = self
            .manifest
            .declared
            .values()
            .chain(self.manifest.includes.values())
            .collect();
        if mapped.len() != self.manifest.files.len()
            || mapped.len() != self.manifest.declared.len() + self.manifest.includes.len()
        {
            return Err(invalid("duplicate or unreferenced custom files"));
        }
        for path in expected
            .iter()
            .map(Path::new)
            .chain(self.manifest.commands.values().map(Path::new))
        {
            if path.starts_with("tree") {
                for ancestor in path.ancestors().skip(usize::from(expected.contains(&portable(path)?))) {
                    if ancestor.as_os_str().is_empty() {
                        break;
                    }
                    directories.insert(portable(ancestor)?);
                }
            }
        }
        for cwd in self.manifest.commands.values() {
            appa_package::RelativePath::parse(cwd).map_err(invalid)?;
            if !Path::new(cwd).starts_with("tree") || !self.root.join(cwd).is_dir() {
                return Err(invalid("invalid snapshot command directory"));
            }
        }
        for entry in appa_package::tree::walk(&self.root).map_err(invalid)? {
            let found = match entry.kind {
                appa_package::tree::EntryKind::File => expected.remove(&entry.portable),
                appa_package::tree::EntryKind::Directory => directories.remove(&entry.portable),
            };
            if !found {
                return Err(invalid("snapshot contains an unlisted member"));
            }
        }
        if !expected.is_empty() || !directories.is_empty() {
            return Err(invalid("snapshot is missing declared members"));
        }
        Ok(())
    }

    pub fn verify_sources(&self) -> Result<(), InstallError> {
        for (path, member) in &self.checks {
            same_member(path, member, true)?;
        }
        if digest(&self.root)? != self.digest {
            return Err(InstallError::Changed(self.root.clone()));
        }
        if self._stage.is_none() {
            self.verify_modes()?;
        }
        Ok(())
    }

    fn prefix(&self, config: &Path) -> String {
        format!(
            ".appa/{}/files/{}",
            config.file_name().expect("config filename").to_string_lossy(),
            self.digest.hex()
        )
    }

    pub fn publish(&self, installation: &Installation) -> Result<PathBuf, InstallError> {
        let directory = installation.state.join("files");
        require_directory_or_absent(&directory)?;
        fs::create_dir_all(&directory).map_err(|error| io("create file store", &directory, error))?;
        let target = directory.join(self.digest.hex());
        require_directory_or_absent(&target)?;
        if target.exists() {
            let existing = Self::read(target.clone(), &self.digest)?;
            existing.verify_modes()?;
            return Ok(target);
        }
        let stage =
            tempfile::tempdir_in(&directory).map_err(|error| io("stage file publication", &directory, error))?;
        let tree = stage.path().join("snapshot");
        copy_package_tree(&self.root, &tree)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tree, fs::Permissions::from_mode(0o700))
                .map_err(|error| io("protect custom snapshot", &tree, error))?;
            for (path, member) in &self.manifest.files {
                let path = tree.join(path);
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(if member.executable { 0o700 } else { 0o600 }),
                )
                .map_err(|error| io("set custom executable mode", &path, error))?;
                File::open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(|error| io("sync custom file mode", &path, error))?;
            }
        }
        Self::read(tree.clone(), &self.digest)?.verify_modes()?;
        sync_directory(&tree)?;
        fs::rename(&tree, &target).map_err(|error| io("publish custom files", &target, error))?;
        sync_directory(&directory)?;
        Ok(target)
    }

    fn verify_modes(&self) -> Result<(), InstallError> {
        #[cfg(unix)]
        for (path, member) in &self.manifest.files {
            same_member(&self.root.join(path), member, true)?;
        }
        Ok(())
    }

    /// Rebase only declared references. Policy and argv are not interpreted.
    pub fn rebase(
        &self,
        text: &str,
        selection: &Selection,
        installation: &Installation,
        exporting: bool,
    ) -> Result<String, InstallError> {
        self.rebase_at(text, selection, &installation.config, exporting)
    }

    /// Render declared references for a destination without opening that path.
    /// This also supports Linux container paths when preparation runs elsewhere.
    pub(super) fn rebase_at(
        &self,
        text: &str,
        selection: &Selection,
        config: &Path,
        exporting: bool,
    ) -> Result<String, InstallError> {
        let mut document = document(text)?;
        for path in manual_includes(text, selection)? {
            let windows_prefix = path.as_bytes().get(1) == Some(&b':');
            if Path::new(&path).is_absolute() || path.contains(['\\', '\0']) || windows_prefix {
                return Err(invalid(
                    "manual include source paths must be relative and use forward slashes",
                ));
            }
        }
        let prefix = self.prefix(config);
        for (bundle, mappings) in [(true, &self.manifest.declared), (false, &self.manifest.includes)] {
            let expected: BTreeSet<_> = mappings
                .iter()
                .map(|(source, target)| {
                    if exporting {
                        format!("{prefix}/{target}")
                    } else {
                        source.clone()
                    }
                })
                .collect();
            let actual: BTreeSet<_> = if bundle {
                strings(&document, true)?
            } else {
                manual_includes(text, selection)?
            }
            .into_iter()
            .collect();
            if actual != expected {
                return Err(invalid(
                    "custom file declarations changed; create an updated bundle from the authored source config",
                ));
            }
            let array = if bundle {
                document.get_mut("bundle").and_then(|item| item.get_mut("files"))
            } else {
                document.get_mut("include")
            }
            .and_then(toml_edit::Item::as_array_mut);
            if let Some(array) = array {
                for (source, target) in mappings {
                    let installed = format!("{prefix}/{target}");
                    let (from, to) = if exporting {
                        (installed.as_str(), source.as_str())
                    } else {
                        (source.as_str(), installed.as_str())
                    };
                    let index = array
                        .iter()
                        .position(|entry| entry.as_str() == Some(from))
                        .ok_or_else(|| invalid("custom reference is missing"))?;
                    array.replace(index, to);
                }
            }
        }
        let base = config.parent().ok_or_else(|| invalid("config has no parent"))?;
        let commands = crate::config::root_command_directories(text, base).map_err(invalid)?;
        if commands.keys().ne(self.manifest.commands.keys()) {
            return Err(invalid(
                "snapshot command origins do not match root command declarations",
            ));
        }
        if exporting {
            for (key, path) in &commands {
                if *path != base.join(&prefix).join(&self.manifest.commands[key]) {
                    return Err(invalid("snapshot command working directory was changed"));
                }
            }
            let original = String::from_utf8(required_bytes(&self.root.join(SOURCE))?).map_err(invalid)?;
            let original = self::document(&original)?;
            match original.get("appa_composed") {
                Some(value) => {
                    document["appa_composed"] = value.clone();
                }
                None => {
                    document.remove("appa_composed");
                }
            }
        } else if !commands.is_empty() {
            let mut origins = toml_edit::Table::new();
            for (key, offset) in &self.manifest.commands {
                let cwd = base.join(&prefix).join(offset);
                origins.insert(
                    key,
                    toml_edit::value(
                        cwd.to_str()
                            .ok_or_else(|| invalid("snapshot cwd is not UTF-8"))?
                            .replace(std::path::MAIN_SEPARATOR, "/"),
                    ),
                );
            }
            let mut metadata = toml_edit::Table::new();
            metadata.insert("command_cwd", toml_edit::Item::Table(origins));
            document["appa_composed"] = toml_edit::Item::Table(metadata);
        }
        Ok(document.to_string())
    }
}

impl Installation {
    pub(super) fn selected_files(&self, selection: &Selection) -> Result<Option<Snapshot>, InstallError> {
        require_directory_or_absent(&self.state.join("files"))?;
        selection
            .files
            .as_ref()
            .map(|digest| Snapshot::read(self.state.join("files").join(digest.hex()), digest))
            .transpose()
    }

    pub(super) fn verify_selected_files(&self, selection: &Selection, text: &str) -> Result<(), InstallError> {
        if let Some(snapshot) = self.selected_files(selection)? {
            snapshot.verify_modes()?;
            snapshot.rebase(text, selection, self, true)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "[policy]\nversion=2\n[externals]\ntimeout_ms=1000\nmax_body_bytes=65536\n";

    fn installed(root: &Path, text: &str) -> Installation {
        let installation = Installation::open(&root.join("config/appa.toml")).unwrap();
        let selection = super::super::tests::selection();
        let source = root.join("marketplace");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("marketplace.toml"), "schema = 1\nname = 'appa'\n").unwrap();
        installation.publish_packages(&source, selection.generation()).unwrap();
        let artifacts = installation.state.join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        fs::write(artifacts.join(ArtifactDigest::of_bytes(b"artifact").hex()), b"artifact").unwrap();
        installation.commit_config(None, text.as_bytes(), &selection).unwrap();
        installation
    }

    #[cfg(unix)]
    fn execute_helpers(config: &Path) {
        let config = crate::config::Config::load(config).unwrap();
        for name in ["root", "shared"] {
            let crate::config::Implementation::Command(command) = &config.externals.authorities[name] else {
                panic!("expected command")
            };
            let result = std::process::Command::new(&command.argv[0])
                .args(&command.argv[1..])
                .current_dir(&command.cwd)
                .output()
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            assert_eq!(result.stdout, b"declared data");
        }
    }

    #[cfg(unix)]
    #[test]
    fn offline_roundtrip_preserves_root_and_parent_include_helpers_without_source_machine() {
        use std::os::unix::fs::PermissionsExt;
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("config")).unwrap();
        fs::create_dir(source.path().join("shared")).unwrap();
        fs::write(source.path().join("config/root.sh"), "#!/bin/sh\ncat ../shared/data\n").unwrap();
        fs::write(source.path().join("shared/helper.sh"), "#!/bin/sh\ncat data\n").unwrap();
        for path in ["config/root.sh", "shared/helper.sh"] {
            fs::set_permissions(source.path().join(path), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(source.path().join("shared/data"), "declared data").unwrap();
        fs::write(source.path().join("shared/secret"), "must not be exported").unwrap();
        fs::write(
            source.path().join("shared/policy.toml"),
            "[policy]\nversion=2\n[externals.authorities.shared]\ncommand=['./helper.sh']\n",
        )
        .unwrap();
        let text = format!(
            "# authored comment\ninclude=['../shared/policy.toml']\n[bundle]\nfiles=['root.sh','../shared/helper.sh','../shared/data']\n{BASE}[externals.authorities.root]\ncommand=['./root.sh']\n"
        );
        let installation = installed(source.path(), &text);
        execute_helpers(&installation.config);
        let bundle = source.path().join("export.tar.gz");
        let checksum = installation.export_bundle(&bundle).unwrap();
        let imported = Acquired::import(&bundle, &checksum).unwrap();
        let snapshot = imported.imported().unwrap().selection().files.clone().unwrap();
        let replica = tempfile::tempdir().unwrap();
        let replica_install = Installation::open(&replica.path().join("moved.toml")).unwrap();
        replica_install.retain(&imported).unwrap();
        let (mut selected, imported_text) = imported.imported().unwrap().configuration(&replica_install).unwrap();
        let imported_text = selected
            .relocate(
                &imported_text,
                imported.generation().clone(),
                replica_install.config_path(),
                imported.marketplace(),
            )
            .unwrap();
        replica_install
            .commit_config(None, imported_text.as_bytes(), &selected)
            .unwrap();
        assert!(imported_text.contains("# authored comment"));
        let retained = replica_install.selected_files(&selected).unwrap().unwrap();
        assert_eq!(fs::read(retained.root.join(SOURCE)).unwrap(), text.as_bytes());
        assert!(
            !appa_package::tree::walk(&retained.root)
                .unwrap()
                .iter()
                .any(|entry| entry.portable.ends_with("secret"))
        );
        // Removing the author's copies demonstrates there is no source-host dependency.
        fs::remove_file(source.path().join("config/root.sh")).unwrap();
        fs::remove_file(source.path().join("shared/helper.sh")).unwrap();
        fs::remove_file(source.path().join("shared/data")).unwrap();
        fs::remove_file(source.path().join("shared/policy.toml")).unwrap();
        execute_helpers(replica_install.config_path());
        let rebundle = replica.path().join("again.tar.gz");
        let checksum = replica_install.export_bundle(&rebundle).unwrap();
        let imported_again = Acquired::import(&rebundle, &checksum).unwrap();
        assert_eq!(imported_again.imported().unwrap().selection().files, Some(snapshot));
        let third = tempfile::tempdir().unwrap();
        let third_install = Installation::open(&third.path().join("third.toml")).unwrap();
        third_install.retain(&imported_again).unwrap();
        let (mut selected, text) = imported_again
            .imported()
            .unwrap()
            .configuration(&third_install)
            .unwrap();
        let text = selected
            .relocate(
                &text,
                imported_again.generation().clone(),
                third_install.config_path(),
                imported_again.marketplace(),
            )
            .unwrap();
        third_install.commit_config(None, text.as_bytes(), &selected).unwrap();
        execute_helpers(third_install.config_path());
        let member = retained.manifest.declared["root.sh"].clone();
        fs::write(retained.root.join(member), "changed").unwrap();
        let refused = replica.path().join("refused.tar.gz");
        assert!(replica_install.export_bundle(&refused).is_err());
        assert!(!refused.exists());
        assert!(!replica_install.state.join("transaction.json").exists());
    }

    #[test]
    fn missing_and_changed_sources_are_not_silently_omitted() {
        let root = tempfile::tempdir().unwrap();
        let installation = Installation::open(&root.path().join("appa.toml")).unwrap();
        let selection = super::super::tests::selection();
        let text = format!("[bundle]\nfiles=['data']\n{BASE}");
        assert!(Snapshot::capture(&installation, &selection, &text).is_err());
        fs::write(root.path().join("data"), "original").unwrap();
        let snapshot = Snapshot::capture(&installation, &selection, &text).unwrap().unwrap();
        fs::write(root.path().join("data"), "changed").unwrap();
        assert!(matches!(snapshot.verify_sources(), Err(InstallError::Changed(_))));
        fs::write(snapshot.root.join("unlisted"), "unlisted").unwrap();
        assert!(Snapshot::read(snapshot.root.clone(), &digest(&snapshot.root).unwrap()).is_err());
    }

    #[test]
    fn oversized_and_colliding_files_and_forged_targets_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let installation = Installation::open(&root.path().join("appa.toml")).unwrap();
        let selection = super::super::tests::selection();
        let path = root.path().join("data");
        let file = File::create(&path).unwrap();
        file.set_len(appa_package::tree::MAX_UNCOMPRESSED_BYTES + 1).unwrap();
        let text = format!("[bundle]\nfiles=['data']\n{BASE}");
        assert!(Snapshot::capture(&installation, &selection, &text).is_err());
        fs::write(&path, "data").unwrap();
        assert!(Snapshot::capture(&installation, &selection, &format!("include=['data']\n{text}")).is_err());
        let mut snapshot = Snapshot::capture(&installation, &selection, &text).unwrap().unwrap();
        let member = snapshot.manifest.files.remove("tree/data").unwrap();
        snapshot.manifest.files.insert("../escape".into(), member);
        snapshot.manifest.declared.insert("data".into(), "../escape".into());
        fs::write(
            snapshot.root.join(MANIFEST),
            serde_json::to_vec(&snapshot.manifest).unwrap(),
        )
        .unwrap();
        assert!(Snapshot::read(snapshot.root.clone(), &digest(&snapshot.root).unwrap()).is_err());
        assert!(!root.path().join("escape").exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_and_parent_symlinks_and_devices_are_refused() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let installation = Installation::open(&root.path().join("appa.toml")).unwrap();
        let selection = super::super::tests::selection();
        fs::create_dir(root.path().join("real")).unwrap();
        fs::write(root.path().join("real/data"), "data").unwrap();
        symlink("real", root.path().join("alias")).unwrap();
        symlink("real/data", root.path().join("link")).unwrap();
        for path in ["alias/data", "link"] {
            assert!(
                Snapshot::capture(
                    &installation,
                    &selection,
                    &format!("[bundle]\nfiles=['{path}']\n{BASE}")
                )
                .is_err()
            );
        }
        assert!(source_path(root.path(), Path::new("/dev/null")).is_err());
    }
}
