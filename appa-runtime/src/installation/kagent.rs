//! Local, content-addressed deployment files. No Kubernetes or registry calls.

use super::*;
use appa_package::generation::Image;
use base64::Engine;
use std::collections::BTreeMap;

const MAX_PREPARED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PREPARED_ENTRIES: usize = 20000;
const CONFIG_MAP_BUDGET: usize = 700 * 1024;

fn invalid(error: impl ToString) -> InstallError {
    InstallError::Invalid(error.to_string())
}

fn json(value: &impl Serialize) -> Result<Vec<u8>, InstallError> {
    serde_json::to_vec_pretty(value).map_err(invalid)
}

#[derive(Serialize)]
struct Member {
    digest: Option<ArtifactDigest>,
    executable: bool,
}

/// Hash the complete prepared tree, including empty working directories and
/// executable intent. Unlike a single package it can contain multiple trees.
fn inventory(root: &Path) -> Result<BTreeMap<String, Member>, InstallError> {
    require_directory_or_absent(root)?;
    let mut pending = vec![root.to_owned()];
    let mut members = BTreeMap::new();
    let mut remaining = MAX_PREPARED_BYTES;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| io("read prepared directory", &directory, error))? {
            let path = entry
                .map_err(|error| io("read prepared entry", &directory, error))?
                .path();
            if members.len() >= MAX_PREPARED_ENTRIES {
                return Err(invalid("prepared deployment has too many files"));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(invalid)?
                .to_str()
                .ok_or_else(|| invalid("prepared paths must be UTF-8"))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            appa_package::RelativePath::parse(&relative).map_err(invalid)?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| io("inspect prepared entry", &path, error))?;
            let executable = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                }
                #[cfg(not(unix))]
                {
                    false
                }
            };
            let digest = if metadata.is_dir() {
                pending.push(path.clone());
                None
            } else if metadata.is_file() {
                remaining = remaining
                    .checked_sub(metadata.len())
                    .ok_or_else(|| invalid("prepared deployment exceeds 1 GiB"))?;
                Some(
                    ArtifactDigest::of_reader(open_regular(&path)?, metadata.len())
                        .map_err(|error| io("hash prepared file", &path, error))?,
                )
            } else {
                return Err(invalid("prepared deployment contains a symlink or special file"));
            };
            members.insert(relative, Member { digest, executable });
        }
    }
    Ok(members)
}

fn identity(root: &Path) -> Result<ArtifactDigest, InstallError> {
    Ok(ArtifactDigest::of_bytes(&json(&inventory(root)?)?))
}

fn directory(installation: &Installation, digest: &ArtifactDigest) -> PathBuf {
    installation.state.join("kagent").join(digest.hex())
}

pub(super) fn verify(installation: &Installation, selection: &Selection) -> Result<(), InstallError> {
    if let Some(digest) = &selection.kagent_assets {
        require_directory_or_absent(&installation.state.join("kagent"))?;
        let root = directory(installation, digest);
        require_directory_or_absent(&root)?;
        if !root.exists() {
            return Err(InstallError::Changed(root));
        }
        if identity(&root)? != *digest {
            return Err(InstallError::Changed(root));
        }
    }
    Ok(())
}

pub(super) fn remove_previous(
    installation: &Installation,
    previous: &Selection,
    selected: &Selection,
) -> Result<(), InstallError> {
    if previous.kagent_assets == selected.kagent_assets {
        return Ok(());
    }
    if let Some(digest) = &previous.kagent_assets {
        let root = directory(installation, digest);
        require_directory_or_absent(&installation.state.join("kagent"))?;
        require_directory_or_absent(&root)?;
        if root.exists() {
            verify(installation, previous).map_err(|error| InstallError::Recovery {
                path: root.clone(),
                reason: format!("new selection is active, but superseded prepared files were preserved: {error}. Restore their original contents, or move this exact directory outside the installation store, then retry the command"),
            })?;
            fs::remove_dir_all(&root).map_err(|error| io("remove superseded prepared deployment", &root, error))?;
            sync_directory(root.parent().expect("prepared directory has parent"))?;
        }
    }
    Ok(())
}

pub(super) fn prepare(
    installation: &Installation,
    selection: &Selection,
    config: &[u8],
) -> Result<Option<ArtifactDigest>, InstallError> {
    let Some(runtime) = selection.kagent_runtime else {
        return Ok(None);
    };
    let published = selection.generation.published().ok_or_else(|| {
        invalid("kagent needs a published version with its images and chart; a development build has none")
    })?;
    selection.validate()?;
    let text = std::str::from_utf8(config).map_err(invalid)?;
    let packages = installation
        .state
        .join("generations")
        .join(selection.commit().as_str())
        .join("marketplace");
    selection.validate_packages(&packages)?;
    installation.verify_selected_files(selection, text)?;
    let store = installation.state.join("kagent");
    require_directory_or_absent(&store)?;
    fs::create_dir_all(&store).map_err(|error| io("create preparation store", &store, error))?;
    let stage = tempfile::tempdir_in(&store).map_err(|error| io("stage kagent deployment", &store, error))?;
    let root = stage.path().join("prepared");
    let assets = root.join("assets");
    fs::create_dir_all(&assets).map_err(|error| io("create prepared assets", &assets, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|error| io("protect prepared configuration", &root, error))?;
    }
    let filename = installation
        .config
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("invalid config filename"))?;
    appa_package::RelativePath::parse(filename).map_err(invalid)?;
    let container_config = Path::new("/etc/appa").join(filename);
    let (snapshot, portable) = if let Some(snapshot) = installation.selected_files(selection)? {
        let portable = snapshot.rebase(text, installation, true)?;
        (Some(snapshot), portable)
    } else {
        (files::Snapshot::capture(installation, text)?, text.to_owned())
    };
    let prepared_text = if let Some(snapshot) = &snapshot {
        let target = assets.join(format!(".appa/{filename}/files/{}", snapshot.digest.hex()));
        fs::create_dir_all(target.parent().expect("snapshot parent"))
            .map_err(|error| io("create prepared snapshot parent", &target, error))?;
        snapshot.copy_for_deployment(&target)?;
        snapshot.rebase_at(&portable, &container_config, false)?
    } else {
        portable
    };
    let catalog = Marketplace::read(&packages.join("marketplace.toml")).map_err(invalid)?;
    for entry in &catalog.packages {
        if entry.kind == PackageKind::Battery && selection.batteries.contains(entry.name.as_str()) {
            let destination = assets.join(format!(
                ".appa/{filename}/generations/{}/marketplace/{}",
                selection.commit(),
                entry.path
            ));
            fs::create_dir_all(destination.parent().expect("package parent"))
                .map_err(|error| io("create prepared package parent", &destination, error))?;
            copy_package_tree(&packages.join(entry.path.as_str()), &destination)?;
        }
    }
    let key = "appa.toml";
    write_synced(&assets.join(key), prepared_text.as_bytes())?;
    let mut members = inventory(&assets)?;
    if let Some(snapshot) = &snapshot {
        for name in snapshot.executable_paths() {
            let path = format!(".appa/{filename}/files/{}/{name}", snapshot.digest.hex());
            members
                .get_mut(&path)
                .ok_or_else(|| invalid("declared helper is absent from prepared assets"))?
                .executable = true;
        }
    }
    let mut projected = BTreeMap::new();
    let mut projection_size = json(&prepared_text)?.len() + 4096;
    for (name, member) in &members {
        let path = assets.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if member.digest.is_none() {
                0o755
            } else if member.executable {
                0o555
            } else {
                0o444
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(|error| io("set asset permissions", &path, error))?;
        }
        if member.digest.is_some() && name != key {
            let size = fs::metadata(&path)
                .map_err(|error| io("size prepared asset", &path, error))?
                .len();
            projection_size =
                projection_size.saturating_add((size as usize).saturating_mul(4).div_ceil(3) + json(name)?.len() + 256);
            if projection_size <= CONFIG_MAP_BUDGET {
                let bytes = fs::read(&path).map_err(|error| io("read projected asset", &path, error))?;
                projected.insert(name, serde_json::json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes),"executable":member.executable}));
            }
        }
    }
    // Empty directories cannot be represented by a ConfigMap projection.
    let empty_directories = members.iter().any(|(name, member)| {
        member.digest.is_none() && !members.keys().any(|other| other.starts_with(&format!("{name}/")))
    });
    let unprojectable_path = members.keys().any(|name| {
        name.len() > 4096
            || name.chars().any(char::is_control)
            || name.split('/').any(|part| part.starts_with("..") || part.len() > 255)
    });
    let volume = projection_size > CONFIG_MAP_BUDGET || empty_directories || unprojectable_path;
    let modes: BTreeMap<_, _> = members
        .iter()
        .map(|(name, member)| {
            (
                name,
                if member.digest.is_none() {
                    0o755
                } else if member.executable {
                    0o555
                } else {
                    0o444
                },
            )
        })
        .collect();
    write_synced(&root.join("asset-modes.json"), &json(&modes)?)?;
    let config_values = if volume {
        serde_json::json!({"key":key,"existingClaim":"appa-config"})
    } else {
        serde_json::json!({"key":key,"contents":prepared_text,"files":projected})
    };
    let values = serde_json::json!({
        "fullnameOverride":"appa-runtime",
        "image":{"repository":Image::Runtime.repository(),"digest":published.images()[&Image::Runtime].digest()},
        "appaGuide":{"enabled":false}, "config":config_values
    });
    write_synced(&root.join("runtime-values.json"), &json(&values)?)?;
    for (name, bytes) in super::kagent_images::artifacts(&selection.generation, runtime)? {
        appa_package::RelativePath::parse(&name).map_err(invalid)?;
        write_synced(&root.join(name), &bytes)?;
    }
    let instructions = if volume {
        "Configuration uses a volume. Copy the complete assets/ tree (including dot directories) into an operator-owned PVC named appa-config in namespace appa, preserving permissions and relative paths. asset-modes.json records decimal POSIX modes; apply these on the destination if your copy crosses a filesystem without POSIX modes, such as Windows. Mounts are read-only. Override config.existingClaim if your PVC has another name. Provision and populate the PVC before deploying. No upload or cluster changes were performed.\n"
    } else {
        "Configuration is embedded in runtime-values.json. The identical assets/ tree is also available for offline sync. Treat both as private configuration; do not put credentials in ConfigMaps.\n"
    };
    let chart = selection
        .generation
        .runtime_chart_archive()
        .expect("a published version names its chart");
    let instructions = format!(
        "{instructions}\nAfter reviewing KAGENT.md and verifying images, deploy from this directory with your explicit context:\n\nhelm upgrade --install appa-runtime ./{chart} --kube-context YOUR_CONTEXT --namespace appa --create-namespace --values runtime-values.json\n\nThis command is shown for the operator; APPA did not execute it.\n"
    );
    write_synced(&root.join("CONFIGURATION.txt"), instructions.as_bytes())?;
    let chart_digest = selection.generation.archives()[&chart].clone();
    let source = installation.state.join("artifacts").join(chart_digest.hex());
    acquisition::verify_artifact(&source, &chart_digest)?;
    let target = root.join(&chart);
    let mut output = File::create(&target).map_err(|error| io("create prepared chart", &target, error))?;
    std::io::copy(&mut open_regular(&source)?.take(512 * 1024 * 1024 + 1), &mut output)
        .map_err(|error| io("copy prepared chart", &target, error))?;
    output
        .sync_all()
        .map_err(|error| io("sync prepared chart", &target, error))?;
    acquisition::verify_artifact(&target, &chart_digest)?;
    if let Some(snapshot) = &snapshot {
        snapshot.verify_sources()?;
    }
    for (name, member) in inventory(&root)?.iter().rev() {
        let path = root.join(name);
        if member.digest.is_none() {
            sync_directory(&path)?;
        } else {
            File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|error| io("sync prepared member", &path, error))?;
        }
    }
    let digest = identity(&root)?;
    let destination = directory(installation, &digest);
    require_directory_or_absent(&destination)?;
    if destination.exists() {
        if identity(&destination)? != digest {
            return Err(InstallError::Changed(destination));
        }
    } else {
        sync_directory(&root)?;
        fs::rename(&root, &destination).map_err(|error| io("publish prepared deployment", &destination, error))?;
        sync_directory(&store)?;
    }
    Ok(Some(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[policy]\nversion=2\n[externals]\ntimeout_ms=1000\nmax_body_bytes=65536\n";

    fn installation(root: &Path) -> (Installation, Selection) {
        let installation = Installation::open(&root.join("config/my deployment.toml")).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace");
        let catalog = fs::read(source.join("marketplace.toml")).unwrap();
        let mut descriptor = serde_json::to_value(super::super::tests::selection().generation()).unwrap();
        descriptor["catalog"] = serde_json::to_value(ArtifactDigest::of_bytes(&catalog)).unwrap();
        let generation = Generation::parse(&serde_json::to_vec(&descriptor).unwrap()).unwrap();
        installation.publish_packages(&source, &generation).unwrap();
        fs::create_dir(installation.state.join("artifacts")).unwrap();
        fs::write(
            installation
                .state
                .join("artifacts")
                .join(ArtifactDigest::of_bytes(b"artifact").hex()),
            b"artifact",
        )
        .unwrap();
        let mut selection = Selection::empty(generation, Platform::MacArm64);
        selection.select(PackageKind::Plugin, &PackageName::parse("kagent").unwrap());
        (installation, selection)
    }

    #[test]
    fn preparation_is_local_idempotent_and_removal_preserves_authored_files() {
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        installation
            .commit_installation(None, CONFIG.as_bytes(), &selection)
            .unwrap();
        let selected = installation.selection().unwrap().unwrap();
        let digest = selected.kagent_assets.clone().unwrap();
        let prepared = directory(&installation, &digest);
        let values: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared.join("runtime-values.json")).unwrap()).unwrap();
        assert_eq!(values["config"]["contents"], CONFIG);
        assert_eq!(values["config"]["key"], "appa.toml");
        assert_eq!(
            values["image"]["digest"],
            selected.generation.published().unwrap().images()[&Image::Runtime]
                .digest()
                .as_str()
        );
        assert_eq!(fs::read(prepared.join("assets/appa.toml")).unwrap(), CONFIG.as_bytes());
        installation
            .commit_installation(Some(CONFIG.as_bytes()), CONFIG.as_bytes(), &selected)
            .unwrap();
        assert_eq!(installation.selection().unwrap().unwrap(), selected);
        assert_eq!(fs::read_dir(installation.state.join("kagent")).unwrap().count(), 1);
        let mut removed = selected.clone();
        removed.deselect(PackageKind::Plugin, &PackageName::parse("kagent").unwrap());
        installation
            .commit_installation(Some(CONFIG.as_bytes()), CONFIG.as_bytes(), &removed)
            .unwrap();
        assert!(!prepared.exists());
        assert_eq!(fs::read(installation.config_path()).unwrap(), CONFIG.as_bytes());
        assert!(installation.state.join("artifacts").exists());
        assert!(installation.state.join("generations").exists());
    }

    #[test]
    fn drift_refuses_replacement_before_config_or_selection_changes() {
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        installation
            .commit_installation(None, CONFIG.as_bytes(), &selection)
            .unwrap();
        let selected = installation.selection().unwrap().unwrap();
        let prepared = directory(&installation, selected.kagent_assets.as_ref().unwrap());
        fs::write(prepared.join("runtime-values.json"), "changed").unwrap();
        assert!(matches!(
            installation.commit_installation(
                Some(CONFIG.as_bytes()),
                format!("{CONFIG}\n# changed\n").as_bytes(),
                &selected
            ),
            Err(InstallError::Changed(_))
        ));
        assert_eq!(installation.selection().unwrap().unwrap(), selected);
        assert_eq!(fs::read(installation.config_path()).unwrap(), CONFIG.as_bytes());
        assert!(!installation.state.join("transaction.json").exists());
    }

    #[test]
    fn recovery_publishes_the_prepared_selection_and_finishes_owned_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        installation
            .commit_installation(None, CONFIG.as_bytes(), &selection)
            .unwrap();
        let previous = installation.selection().unwrap().unwrap();
        let old = directory(&installation, previous.kagent_assets.as_ref().unwrap());
        let mut next = previous.clone();
        next.kagent_runtime = Some(super::super::kagent_images::KagentRuntime::Go);
        let after = format!("{CONFIG}\n# operator update\n").into_bytes();
        next.kagent_assets = prepare(&installation, &next, &after).unwrap();
        let journal = ConfigTransaction {
            before: Some(CONFIG.as_bytes().to_vec()),
            after: after.clone(),
            selection: next.clone(),
            activation: Activation::None,
            previous: Some(previous),
        };
        atomic_write(&installation.state.join("transaction.json"), &json(&journal).unwrap()).unwrap();
        atomic_write(&installation.config, &after).unwrap();
        installation.recover_config().unwrap();
        assert_eq!(installation.selection().unwrap().unwrap(), next);
        assert!(!old.exists());
        assert!(!installation.state.join("transaction.json").exists());
        assert!(
            directory(&installation, next.kagent_assets.as_ref().unwrap())
                .join("agent-go.json")
                .exists()
        );
        // A crash after cleanup but before journal removal must be resumable too.
        atomic_write(&installation.state.join("transaction.json"), &json(&journal).unwrap()).unwrap();
        installation.recover_config().unwrap();
        assert_eq!(installation.selection().unwrap().unwrap(), next);
    }

    #[test]
    fn missing_assets_do_not_block_removing_an_already_absent_preparation() {
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        installation
            .commit_installation(None, CONFIG.as_bytes(), &selection)
            .unwrap();
        let mut selected = installation.selection().unwrap().unwrap();
        let path = directory(&installation, selected.kagent_assets.as_ref().unwrap());
        fs::remove_dir_all(&path).unwrap();
        assert!(matches!(verify(&installation, &selected), Err(InstallError::Changed(found)) if found == path));
        selected.deselect(PackageKind::Plugin, &PackageName::parse("kagent").unwrap());
        installation
            .commit_installation(Some(CONFIG.as_bytes()), CONFIG.as_bytes(), &selected)
            .unwrap();
        assert_eq!(installation.selection().unwrap().unwrap(), selected);
        assert_eq!(fs::read(installation.config_path()).unwrap(), CONFIG.as_bytes());
    }

    #[test]
    fn recovery_preserves_modified_superseded_files_until_operator_moves_them() {
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        installation
            .commit_installation(None, CONFIG.as_bytes(), &selection)
            .unwrap();
        let previous = installation.selection().unwrap().unwrap();
        let old = directory(&installation, previous.kagent_assets.as_ref().unwrap());
        let mut next = previous.clone();
        next.deselect(PackageKind::Plugin, &PackageName::parse("kagent").unwrap());
        let journal = ConfigTransaction {
            before: Some(CONFIG.as_bytes().to_vec()),
            after: CONFIG.as_bytes().to_vec(),
            selection: next.clone(),
            activation: Activation::None,
            previous: Some(previous),
        };
        atomic_write(&installation.state.join("transaction.json"), &json(&journal).unwrap()).unwrap();
        fs::write(old.join("operator-note"), b"preserve this").unwrap();
        assert!(matches!(
            installation.recover_config(),
            Err(InstallError::Recovery { .. })
        ));
        assert_eq!(installation.selection().unwrap().unwrap(), next);
        assert_eq!(fs::read(old.join("operator-note")).unwrap(), b"preserve this");
        let retained = root.path().join("operator-copy");
        fs::rename(&old, &retained).unwrap();
        installation.recover_config().unwrap();
        assert!(!installation.state.join("transaction.json").exists());
        assert_eq!(fs::read(retained.join("operator-note")).unwrap(), b"preserve this");
    }

    #[cfg(unix)]
    #[test]
    fn projected_helpers_match_raw_layout_and_large_files_use_volume() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let (installation, selection) = installation(root.path());
        let base = installation.config.parent().unwrap();
        fs::write(base.join("helper.sh"), "#!/bin/sh\nprintf helper-ok\n").unwrap();
        fs::set_permissions(base.join("helper.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let text =
            format!("[bundle]\nfiles=['helper.sh']\n{CONFIG}[externals.authorities.helper]\ncommand=['./helper.sh']\n");
        installation
            .commit_installation(None, text.as_bytes(), &selection)
            .unwrap();
        let selected = installation.selection().unwrap().unwrap();
        let prepared = directory(&installation, selected.kagent_assets.as_ref().unwrap());
        let values: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared.join("runtime-values.json")).unwrap()).unwrap();
        let projected = values["config"]["files"].as_object().unwrap();
        for (name, file) in projected {
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(file["data"].as_str().unwrap())
                    .unwrap(),
                fs::read(prepared.join("assets").join(name)).unwrap()
            );
        }
        let (helper, value) = projected.iter().find(|(name, _)| name.ends_with("/helper.sh")).unwrap();
        assert_eq!(value["executable"], true);
        let path = prepared.join("assets").join(helper);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o555);
        let parsed: toml::Value = toml::from_str(values["config"]["contents"].as_str().unwrap()).unwrap();
        let cwd = parsed["appa_composed"]["command_cwd"]
            .as_table()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_str()
            .unwrap();
        assert!(cwd.starts_with("/etc/appa/.appa/my deployment.toml/files/"));
        let result = std::process::Command::new("./helper.sh")
            .current_dir(prepared.join("assets").join(cwd.strip_prefix("/etc/appa/").unwrap()))
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"helper-ok");
        File::create(base.join("large.dat"))
            .unwrap()
            .set_len(800 * 1024)
            .unwrap();
        let large_text = format!("[bundle]\nfiles=['large.dat']\n{CONFIG}");
        installation
            .commit_installation(Some(text.as_bytes()), large_text.as_bytes(), &selected)
            .unwrap();
        let selected = installation.selection().unwrap().unwrap();
        let prepared = directory(&installation, selected.kagent_assets.as_ref().unwrap());
        let values: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared.join("runtime-values.json")).unwrap()).unwrap();
        assert_eq!(values["config"]["existingClaim"], "appa-config");
        assert!(values["config"].get("files").is_none());
        assert!(
            inventory(&prepared.join("assets"))
                .unwrap()
                .keys()
                .any(|name| name.ends_with("/large.dat"))
        );
    }
}
