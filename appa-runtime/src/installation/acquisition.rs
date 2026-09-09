//! Explicit acquisition only. Callers validate their config before entering
//! here; listing installed state and runtime startup do not enter this module.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use appa_package::generation::{
    ArtifactDigest, BUILD_PLUGIN_ARCHIVE, Commit, DESCRIPTOR_FILE, Generation, Platform, REPOSITORY,
};
use serde::Deserialize;

use super::{InstallError, Selection, io, verify_packages};

const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TAG_PAGES: usize = 5;

#[derive(Debug, Clone, Copy)]
pub enum Requirements {
    Packages,
    Claude(Platform),
    Kagent,
    Both(Platform),
}

/// Temporary, verified inputs. Installed state has not changed when this
/// returns; the destination installer owns publication and host activation.
pub struct Acquired {
    _stage: Option<tempfile::TempDir>,
    generation: Generation,
    marketplace: PathBuf,
    archives: BTreeMap<String, PathBuf>,
    imported: Option<Imported>,
}

pub struct Imported {
    selection: Selection,
    config: String,
    snapshot: Option<super::files::Snapshot>,
}

impl Imported {
    pub fn selection(&self) -> &Selection {
        &self.selection
    }
    pub fn config(&self) -> &str {
        &self.config
    }

    pub fn configuration(&self, installation: &super::Installation) -> Result<(Selection, String), InstallError> {
        let config = if let Some(snapshot) = &self.snapshot {
            // Check all authored mappings before publishing even immutable files.
            let config = snapshot.rebase(&self.config, &self.selection, installation, false)?;
            snapshot.publish(installation)?;
            config
        } else {
            self.config.clone()
        };
        Ok((self.selection.clone(), config))
    }
}

impl Acquired {
    /// Reuse the selected immutable generation without resolving a remote tag.
    pub fn retained(
        installation: &super::Installation,
        selection: &Selection,
        requirements: Requirements,
    ) -> Result<Self, InstallError> {
        let generation = selection.generation().clone();
        let root = installation
            .state
            .join("generations")
            .join(generation.commit().as_str());
        let cached = Generation::parse(&super::required_bytes(&root.join(DESCRIPTOR_FILE))?)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        if cached != generation {
            return Err(InstallError::Invalid(
                "retained descriptor disagrees with the selected generation".into(),
            ));
        }
        let marketplace = root.join("marketplace");
        selection.validate_packages(&marketplace)?;
        let mut archives = BTreeMap::new();
        for name in required_archives(&generation, requirements)? {
            let digest = &generation.archives()[&name];
            let path = installation.state.join("artifacts").join(digest.hex());
            verify_artifact(&path, digest)?;
            archives.insert(name, path);
        }
        Ok(Self {
            _stage: None,
            generation,
            marketplace,
            archives,
            imported: None,
        })
    }

    pub fn generation(&self) -> &Generation {
        &self.generation
    }
    pub fn marketplace(&self) -> &Path {
        &self.marketplace
    }
    pub fn archives(&self) -> &BTreeMap<String, PathBuf> {
        &self.archives
    }
    pub fn imported(&self) -> Option<&Imported> {
        self.imported.as_ref()
    }

    /// No network path is reachable from import. The outer digest is required
    /// from the caller, not trusted from a neighboring checksum file.
    pub fn import(bundle: &Path, digest: &ArtifactDigest) -> Result<Self, InstallError> {
        let stage = tempfile::tempdir().map_err(|error| io("stage bundle import", bundle, error))?;
        // Hash and unpack a private snapshot, not two separate opens of a
        // caller-owned path that could change after its digest was checked.
        super::require_file_or_absent(bundle)?;
        let input = super::open_regular(bundle)?;
        let mut snapshot =
            tempfile::NamedTempFile::new_in(stage.path()).map_err(|error| io("snapshot bundle", bundle, error))?;
        let copied = std::io::copy(&mut std::io::Read::take(input, MAX_ARTIFACT_BYTES + 1), &mut snapshot)
            .map_err(|error| io("copy bundle snapshot", bundle, error))?;
        if copied > MAX_ARTIFACT_BYTES {
            return Err(InstallError::Invalid("bundle exceeds its byte limit".into()));
        }
        verify_artifact(snapshot.path(), digest)?;
        let unpacked = stage.path().join("unpacked");
        fs::create_dir(&unpacked).map_err(|error| io("stage bundle contents", &unpacked, error))?;
        crate::plugin_bundle::extract_bundle_archive(snapshot.path(), &unpacked)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let generation = Generation::parse(&super::required_bytes(&unpacked.join(DESCRIPTOR_FILE))?)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        let selection: Selection = serde_json::from_slice(&super::required_bytes(&unpacked.join("selection.json"))?)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        selection.validate()?;
        if selection.generation() != &generation {
            return Err(InstallError::Invalid("bundle mixes generation identities".into()));
        }
        let config = String::from_utf8(super::required_bytes(&unpacked.join("config.toml"))?)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        selection.validate_owned_config(&config)?;
        let snapshot = selection
            .files
            .as_ref()
            .map(|digest| super::files::Snapshot::read(unpacked.join("snapshot"), digest))
            .transpose()?;
        if snapshot.is_none() && unpacked.join("snapshot").exists() {
            return Err(InstallError::Invalid("bundle has an unselected custom snapshot".into()));
        }
        if snapshot.is_none() && super::files::requires_snapshot(&config, &selection)? {
            return Err(InstallError::Invalid(
                "bundle is missing its declared custom files or manual includes".into(),
            ));
        }
        let marketplace = unpacked.join("marketplace");
        selection.validate_packages(&marketplace)?;
        let mut archives = BTreeMap::new();
        for name in required_archives(&generation, selection.requirements())? {
            let path = unpacked.join("artifacts").join(&name);
            verify_artifact(&path, &generation.archives()[&name])?;
            archives.insert(name, path);
        }
        Ok(Self {
            _stage: Some(stage),
            generation,
            marketplace,
            archives,
            imported: Some(Imported {
                selection,
                config,
                snapshot,
            }),
        })
    }

    /// This binary's own generation: the published one for a release build,
    /// itself for a development build. An explicit revision is always published.
    pub fn own(revision: Option<&str>, requirements: Requirements) -> Result<Self, InstallError> {
        let own_commit = revision.is_none_or(|revision| Some(revision) == option_env!("APPA_BUILD_COMMIT"));
        if own_commit && !super::cli::is_published_build() {
            Self::build(requirements)
        } else {
            Self::fetch(revision, requirements)
        }
    }

    /// Whether a generation is the one this development build produces.
    pub fn is_own_build(generation: &Generation) -> bool {
        generation.build_artifacts().is_some_and(|build| {
            Some(generation.commit().as_str()) == option_env!("APPA_BUILD_COMMIT")
                && Some(build.plugin_tree()) == option_env!("APPA_PLUGIN_TREE_SHA256")
        })
    }

    /// A development build installs itself: the plugin tree staged from its
    /// commit, which must digest to what the build stamped, the marketplace
    /// tree at that commit, and this process's own executable. Kagent needs
    /// published images and a chart, which a build cannot supply.
    pub fn build(requirements: Requirements) -> Result<Self, InstallError> {
        let commit = option_env!("APPA_BUILD_COMMIT").ok_or_else(|| {
            InstallError::Invalid(
                "this development build has uncommitted plugin changes; commit them and rebuild, or install a published generation with --revision"
                    .into(),
            )
        })?;
        let commit = Commit::parse(commit).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let plugin_tree = option_env!("APPA_PLUGIN_TREE_SHA256")
            .ok_or_else(|| InstallError::Invalid("this build carries no plugin identity".into()))?;
        let platform = Platform::current()
            .ok_or_else(|| InstallError::Invalid("this platform has no published runtime binary".into()))?;
        if matches!(requirements, Requirements::Kagent | Requirements::Both(_)) {
            return Err(InstallError::Invalid(
                "kagent needs a published generation with its images and chart; a development build has none"
                    .into(),
            ));
        }
        let stage =
            tempfile::tempdir().map_err(|error| io("stage acquisition", Path::new("temporary directory"), error))?;
        let repository = source_at_commit(&commit, stage.path())?;
        let staged = stage.path().join("plugin");
        crate::plugin_bundle::stage_repository(&repository, &staged)
            .map_err(|error| InstallError::Invalid(format!("cannot stage the plugin tree: {error}")))?;
        crate::plugin_bundle::validate_source_tree(&staged).map_err(|error| InstallError::Invalid(error.to_string()))?;
        let actual = appa_package::canonical_tree_digest(&staged)
            .map_err(|error| InstallError::Invalid(error.to_string()))?
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != plugin_tree {
            return Err(InstallError::Invalid(format!(
                "the plugin tree at commit {commit} does not match this build; rebuild from that commit"
            )));
        }
        let marketplace = repository.join("marketplace");
        let catalog = ArtifactDigest::of_bytes(&super::required_bytes(&marketplace.join("marketplace.toml"))?);
        let plugin_archive = stage.path().join(BUILD_PLUGIN_ARCHIVE);
        pack_tree(&staged, &plugin_archive)?;
        let binary_archive = stage.path().join(platform.archive());
        let executable = std::env::current_exe().map_err(|error| io("locate this executable", Path::new("appa"), error))?;
        pack_binary(&executable, platform, &binary_archive)?;
        let generation = Generation::build(
            commit.clone(),
            catalog,
            platform,
            plugin_tree,
            digest_of(&binary_archive)?,
            digest_of(&plugin_archive)?,
        )
        .map_err(|error| InstallError::Invalid(error.to_string()))?;
        verify_packages(&marketplace, &generation).map_err(|error| {
            InstallError::Invalid(format!(
                "{error}; if packages changed at commit {commit}, regenerate the catalog with scripts/appa-marketplace.sh and commit it"
            ))
        })?;
        let mut archives = BTreeMap::new();
        for name in required_archives(&generation, requirements)? {
            archives.insert(name.clone(), stage.path().join(&name));
        }
        Ok(Self {
            _stage: Some(stage),
            generation,
            marketplace,
            archives,
            imported: None,
        })
    }

    /// None means this binary's own published generation, never latest/main.
    pub fn fetch(revision: Option<&str>, requirements: Requirements) -> Result<Self, InstallError> {
        let own_commit = option_env!("APPA_BUILD_COMMIT");
        let requested = match revision {
            Some(revision) => revision,
            None => option_env!("APPA_RELEASE_REF").or(own_commit)
                .ok_or_else(|| InstallError::Invalid("this development build has no published generation; specify --revision with a published tag or commit".into()))?,
        };
        let expected_commit = if revision.is_none() {
            own_commit
                .map(Commit::parse)
                .transpose()
                .map_err(|error| InstallError::Invalid(error.to_string()))?
        } else {
            None
        };
        let api = crate::plugin_bundle::debug_override("APPA_MARKETPLACE_API_URL")
            .unwrap_or_else(|| format!("https://api.github.com/repos/{REPOSITORY}"));
        let releases = crate::plugin_bundle::release_base_url();
        Self::fetch_from(requested, expected_commit.as_ref(), requirements, &api, &releases)
    }

    fn fetch_from(
        revision: &str,
        expected: Option<&Commit>,
        requirements: Requirements,
        api: &str,
        releases: &str,
    ) -> Result<Self, InstallError> {
        validate_revision(revision)?;
        let stage =
            tempfile::tempdir().map_err(|error| io("stage acquisition", Path::new("temporary directory"), error))?;
        let commit = if revision.starts_with('v') {
            None
        } else {
            Some(Commit::parse(revision).map_err(|error| InstallError::Invalid(error.to_string()))?)
        };
        let release = match &commit {
            Some(commit) => release_for_commit(commit, api, stage.path())?,
            None => revision.to_owned(),
        };
        let descriptor = stage.path().join(DESCRIPTOR_FILE);
        fetch(
            &asset_url(releases, &release, DESCRIPTOR_FILE)?,
            &descriptor,
            appa_package::generation::MAX_DESCRIPTOR_BYTES as u64,
        )?;
        let bytes = fs::read(&descriptor).map_err(|error| io("read generation descriptor", &descriptor, error))?;
        let generation = Generation::parse(&bytes).map_err(|error| InstallError::Invalid(error.to_string()))?;
        if generation.published().map(|published| published.release()) != Some(release.as_str())
            || commit
                .as_ref()
                .or(expected)
                .is_some_and(|commit| generation.commit() != commit)
        {
            return Err(InstallError::Invalid(
                "published descriptor does not belong to the requested generation".into(),
            ));
        }
        let available = generation.archives();
        let marketplace_archive = generation
            .marketplace_archive()
            .expect("a published generation names its marketplace archive");
        let names = required_archives(&generation, requirements)?;
        let mut archives = BTreeMap::new();
        for name in names {
            let expected = available
                .get(&name)
                .ok_or_else(|| InstallError::Invalid(format!("generation does not publish {name}")))?;
            let path = stage.path().join(&name);
            fetch(&asset_url(releases, &release, &name)?, &path, MAX_ARTIFACT_BYTES)?;
            verify_artifact(&path, expected)?;
            archives.insert(name, path);
        }
        let marketplace = stage.path().join("marketplace");
        fs::create_dir(&marketplace).map_err(|error| io("stage marketplace", &marketplace, error))?;
        crate::plugin_bundle::extract_archive(&archives[&marketplace_archive], &marketplace)
            .map_err(|error| InstallError::Invalid(error.to_string()))?;
        verify_packages(&marketplace, &generation)?;
        Ok(Self {
            _stage: Some(stage),
            generation,
            marketplace,
            archives,
            imported: None,
        })
    }
}

impl Selection {
    pub(super) fn requirements(&self) -> Requirements {
        match (self.plugins.contains("claude-code"), self.plugins.contains("kagent")) {
            (true, true) => Requirements::Both(self.platform),
            (true, false) => Requirements::Claude(self.platform),
            (false, true) => Requirements::Kagent,
            (false, false) => Requirements::Packages,
        }
    }
}

pub(super) fn required_archives(
    generation: &Generation,
    requirements: Requirements,
) -> Result<Vec<String>, InstallError> {
    let mut names: Vec<String> = generation.marketplace_archive().into_iter().collect();
    if let Requirements::Claude(platform) | Requirements::Both(platform) = requirements {
        if generation
            .build_artifacts()
            .is_some_and(|build| build.platform() != platform)
        {
            return Err(InstallError::Invalid(
                "the selected development generation was built for another platform".into(),
            ));
        }
        names.push(platform.archive().to_owned());
        names.push(generation.plugin_archive());
    }
    if matches!(requirements, Requirements::Kagent | Requirements::Both(_)) {
        names.push(generation.runtime_chart_archive().ok_or_else(|| {
            InstallError::Invalid(
                "kagent needs a published generation with its images and chart; a development build has none".into(),
            )
        })?);
    }
    Ok(names)
}

/// The repository at this build's commit: exported from the checkout that
/// built it when that checkout still has the commit at HEAD, otherwise the
/// immutable source archive for the commit.
fn source_at_commit(commit: &Commit, stage: &Path) -> Result<PathBuf, InstallError> {
    let repository = stage.join("repository");
    if let Some(root) = option_env!("APPA_BUILD_REPOSITORY").map(Path::new)
        && git_head(root).as_deref() == Some(commit.as_str())
    {
        eprintln!("appa: exporting commit {} from {}...", &commit.as_str()[..12], root.display());
        export_commit(root, &repository)?;
        return Ok(repository);
    }
    let url = format!("{}/{commit}.tar.gz", crate::plugin_bundle::source_archive_base_url());
    eprintln!("appa: fetching the source archive for commit {}...", &commit.as_str()[..12]);
    let archive = stage.join("source.tar.gz");
    crate::plugin_bundle::download_bounded(&url, &archive, MAX_ARTIFACT_BYTES)
        .map_err(|error| InstallError::Invalid(format!("cannot fetch this build's source: {error}")))?;
    let container = stage.join("source");
    fs::create_dir(&container).map_err(|error| io("stage source", &container, error))?;
    crate::plugin_bundle::extract_archive(&archive, &container)
        .map_err(|error| InstallError::Invalid(error.to_string()))?;
    crate::plugin_bundle::single_directory(&container).map_err(|error| InstallError::Invalid(error.to_string()))
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Committed content only, so a dirty checkout exports exactly its HEAD.
fn export_commit(root: &Path, destination: &Path) -> Result<(), InstallError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["archive", "--format=tar", "HEAD", "marketplace"]);
    for (source, _) in crate::plugin_layout::REPOSITORY_MAPPINGS {
        if !source.starts_with("marketplace/") {
            command.arg(source);
        }
    }
    let output = command.output().map_err(|error| io("export the build's commit", root, error))?;
    if !output.status.success() {
        return Err(InstallError::Invalid(format!(
            "git archive failed at {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() as u64 > appa_package::tree::MAX_UNCOMPRESSED_BYTES {
        return Err(InstallError::Invalid("the exported source exceeds its byte limit".into()));
    }
    fs::create_dir(destination).map_err(|error| io("stage exported source", destination, error))?;
    tar::Archive::new(std::io::Cursor::new(output.stdout))
        .unpack(destination)
        .map_err(|error| io("unpack exported source", destination, error))
}

/// One archive for one tree, byte for byte: fixed metadata and canonical order,
/// so the same commit yields the same digest on every install.
fn pack_tree(root: &Path, destination: &Path) -> Result<(), InstallError> {
    let entries = appa_package::tree::walk(root).map_err(|error| InstallError::Invalid(error.to_string()))?;
    let file = File::create(destination).map_err(|error| io("create archive", destination, error))?;
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()));
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        match entry.kind {
            appa_package::tree::EntryKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
                header.set_cksum();
                archive
                    .append_data(&mut header, format!("{}/", entry.portable), std::io::empty())
                    .map_err(|error| io("archive directory", destination, error))?;
            }
            appa_package::tree::EntryKind::File => {
                let metadata = fs::metadata(&entry.absolute).map_err(|error| io("inspect file", &entry.absolute, error))?;
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(metadata.len());
                header.set_mode(if is_executable(&metadata) { 0o755 } else { 0o644 });
                header.set_cksum();
                let input = super::open_regular(&entry.absolute)?;
                archive
                    .append_data(&mut header, &entry.portable, input)
                    .map_err(|error| io("archive file", &entry.absolute, error))?;
            }
        }
    }
    finish_archive(archive, destination)
}

/// The executable alone, in the archive shape the release publishes for its
/// platform, so activation extracts a build's binary exactly like a release's.
fn pack_binary(executable: &Path, platform: Platform, destination: &Path) -> Result<(), InstallError> {
    let input = super::open_regular(executable)?;
    let size = input
        .metadata()
        .map_err(|error| io("inspect executable", executable, error))?
        .len();
    if matches!(platform, Platform::WindowsAmd64 | Platform::WindowsArm64) {
        let file = File::create(destination).map_err(|error| io("create archive", destination, error))?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(0o755);
        zip.start_file("appa.exe", options)
            .and_then(|()| std::io::copy(&mut input.take(size), &mut zip).map_err(zip::result::ZipError::Io))
            .and_then(|_| zip.finish())
            .map_err(|error| InstallError::Invalid(format!("cannot archive the executable: {error}")))?;
        return Ok(());
    }
    let file = File::create(destination).map_err(|error| io("create archive", destination, error))?;
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()));
    let mut header = tar::Header::new_gnu();
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(size);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "appa", input.take(size))
        .map_err(|error| io("archive executable", executable, error))?;
    finish_archive(archive, destination)
}

fn finish_archive(
    archive: tar::Builder<flate2::write::GzEncoder<File>>,
    destination: &Path,
) -> Result<(), InstallError> {
    archive
        .into_inner()
        .and_then(flate2::write::GzEncoder::finish)
        .and_then(|file| file.sync_all())
        .map_err(|error| io("finish archive", destination, error))
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn digest_of(path: &Path) -> Result<ArtifactDigest, InstallError> {
    ArtifactDigest::of_reader(super::open_regular(path)?, MAX_ARTIFACT_BYTES).map_err(|error| io("hash archive", path, error))
}

pub(super) fn verify_artifact(path: &Path, expected: &ArtifactDigest) -> Result<(), InstallError> {
    super::require_file_or_absent(path)?;
    let file = super::open_regular(path)?;
    let actual =
        ArtifactDigest::of_reader(file, MAX_ARTIFACT_BYTES).map_err(|error| io("hash artifact", path, error))?;
    if &actual != expected {
        return Err(InstallError::Invalid(format!(
            "artifact digest mismatch at {}: expected {expected}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn validate_revision(revision: &str) -> Result<(), InstallError> {
    if let Some(version) = revision.strip_prefix('v') {
        if !version.is_empty()
            && version.len() <= 96
            && version.starts_with(|c: char| c.is_ascii_digit())
            && version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
        {
            return Ok(());
        }
    } else if Commit::parse(revision).is_ok() {
        return Ok(());
    }
    Err(InstallError::Invalid(
        "revision must be a full commit or a version tag such as v0.14.1; moving branches are not accepted".into(),
    ))
}

fn asset_url(base: &str, release: &str, file: &str) -> Result<String, InstallError> {
    let mut url = url::Url::parse(base).map_err(|error| InstallError::Invalid(error.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| InstallError::Invalid("invalid release endpoint".into()))?
        .pop_if_empty()
        .push(release)
        .push(file);
    Ok(url.to_string())
}

fn fetch(url: &str, path: &Path, limit: u64) -> Result<(), InstallError> {
    crate::plugin_bundle::download_bounded(url, path, limit).map_err(|error| {
        InstallError::Invalid(format!(
            "cannot acquire a published generation: {error}; source-only commits are not installable releases"
        ))
    })
}

#[derive(Deserialize)]
struct Tag {
    name: String,
    commit: TagCommit,
}
#[derive(Deserialize)]
struct TagCommit {
    sha: String,
}

fn release_for_commit(commit: &Commit, api: &str, directory: &Path) -> Result<String, InstallError> {
    let mut found = Vec::new();
    for page in 1..=MAX_TAG_PAGES {
        let path = directory.join("tags.json");
        fetch(
            &format!("{}/tags?per_page=100&page={page}", api.trim_end_matches('/')),
            &path,
            1024 * 1024,
        )?;
        let bytes = fs::read(&path).map_err(|error| io("read release tags", &path, error))?;
        let tags: Vec<Tag> =
            serde_json::from_slice(&bytes).map_err(|error| InstallError::Invalid(error.to_string()))?;
        for tag in &tags {
            if tag.commit.sha == commit.as_str() && tag.name.starts_with('v') {
                validate_revision(&tag.name)?;
                found.push(tag.name.clone());
            }
        }
        if tags.len() < 100 {
            return match found.as_slice() {
                [release] => Ok(release.clone()),
                [] => Err(InstallError::Invalid(format!(
                    "commit {commit} has no published version tag; choose a published generation"
                ))),
                _ => Err(InstallError::Invalid(
                    "several release tags name this commit; specify the intended version tag".into(),
                )),
            };
        }
    }
    Err(InstallError::Invalid(format!(
        "release lookup reached its {MAX_TAG_PAGES}-page limit; specify a version tag instead of a commit"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_package::generation::Image;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn a_build_generation_needs_no_marketplace_archive_and_refuses_kagent() {
        let generation = Generation::build(
            Commit::parse(&"a".repeat(40)).unwrap(),
            ArtifactDigest::of_bytes(b"catalog"),
            Platform::MacArm64,
            &"b".repeat(64),
            ArtifactDigest::of_bytes(b"binary"),
            ArtifactDigest::of_bytes(b"plugin"),
        )
        .unwrap();
        assert!(required_archives(&generation, Requirements::Packages).unwrap().is_empty());
        assert_eq!(
            required_archives(&generation, Requirements::Claude(Platform::MacArm64)).unwrap(),
            vec![Platform::MacArm64.archive().to_owned(), BUILD_PLUGIN_ARCHIVE.to_owned()]
        );
        assert!(required_archives(&generation, Requirements::Claude(Platform::LinuxAmd64)).is_err());
        assert!(required_archives(&generation, Requirements::Kagent).is_err());
        assert!(required_archives(&generation, Requirements::Both(Platform::MacArm64)).is_err());
    }

    #[test]
    fn packed_trees_are_byte_identical_across_runs() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/file"), b"bytes").unwrap();
        fs::write(source.path().join("top"), b"more").unwrap();
        let output = tempfile::tempdir().unwrap();
        let first = output.path().join("first.tar.gz");
        let second = output.path().join("second.tar.gz");
        pack_tree(source.path(), &first).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        pack_tree(source.path(), &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        let unpacked = tempfile::tempdir().unwrap();
        crate::plugin_bundle::extract_archive(&first, unpacked.path()).unwrap();
        assert_eq!(fs::read(unpacked.path().join("nested/file")).unwrap(), b"bytes");
    }

    #[test]
    fn invalid_revision_is_rejected_without_network_access() {
        for revision in ["main", "latest", "abc123", "v1/../../other"] {
            assert!(
                Acquired::fetch_from(
                    revision,
                    None,
                    Requirements::Packages,
                    "http://127.0.0.1:1",
                    "http://127.0.0.1:1"
                )
                .is_err()
            );
        }
    }

    #[test]
    fn published_packages_are_downloaded_hashed_and_validated_before_return() {
        let source = tempfile::tempdir().unwrap();
        let catalog = b"schema = 1\nname = 'appa'\n";
        fs::write(source.path().join("marketplace.toml"), catalog).unwrap();
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        tar.append_dir_all(".", source.path()).unwrap();
        let archive = tar.into_inner().unwrap().finish().unwrap();
        let digest = ArtifactDigest::of_bytes(b"artifact");
        let descriptor = serde_json::to_vec(&serde_json::json!({"schema": 1, "repository": REPOSITORY,
            "commit": "a".repeat(40), "release": "v1.0.0", "protocol": appa_package::PROTOCOL,
            "catalog": ArtifactDigest::of_bytes(catalog), "marketplace": ArtifactDigest::of_bytes(&archive),
            "claude_plugin": digest, "runtime_chart": digest,
            "binaries": Platform::ALL.into_iter().map(|p| (p, digest.clone())).collect::<BTreeMap<_, _>>(),
            "images": Image::ALL.into_iter().map(|i| (i, serde_json::json!({"digest": digest,
                "platforms": {"linux/amd64": digest}}))).collect::<BTreeMap<_, _>>() }))
        .unwrap();
        for corrupt in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let responses = [
                ("/v1.0.0/appa-generation.json", descriptor.clone()),
                (
                    "/v1.0.0/appa-marketplace-1.0.0.tar.gz",
                    if corrupt { b"corrupt".to_vec() } else { archive.clone() },
                ),
            ];
            let server = thread::spawn(move || {
                for (path, bytes) in responses {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                        .unwrap();
                    let mut request = [0; 8192];
                    let read = socket.read(&mut request).unwrap();
                    assert!(String::from_utf8_lossy(&request[..read]).starts_with(&format!("GET {path} ")));
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    socket.write_all(&bytes).unwrap();
                }
            });
            let result = Acquired::fetch_from("v1.0.0", None, Requirements::Packages, &base, &base);
            server.join().unwrap();
            if corrupt {
                assert!(result.is_err());
            } else {
                let acquired = result.unwrap();
                assert_eq!(
                    fs::read(acquired.marketplace().join("marketplace.toml")).unwrap(),
                    catalog
                );
                assert_eq!(acquired.archives().len(), 1);
                let deployment = tempfile::tempdir().unwrap();
                let installed = super::super::Installation::open(&deployment.path().join("appa.toml")).unwrap();
                installed.retain(&acquired).unwrap();
                let selection = Selection::empty(acquired.generation().clone(), Platform::MacArm64);
                let config = b"[policy]\nversion = 2\n[externals]\ntimeout_ms = 100\nmax_body_bytes = 1024\n";
                installed.commit_config(None, config, &selection).unwrap();
                let bundle = deployment.path().join("bundle.tar.gz");
                let checksum = installed.export_bundle(&bundle).unwrap();
                // The HTTP fixture has shut down: import is entirely local.
                let imported = Acquired::import(&bundle, &checksum).unwrap();
                assert_eq!(imported.imported().unwrap().selection(), &selection);
                assert_eq!(imported.imported().unwrap().config().as_bytes(), config);
                assert_eq!(imported.generation(), acquired.generation());
                assert!(Acquired::import(&bundle, &ArtifactDigest::of_bytes(b"wrong checksum")).is_err());
                assert!(installed.export_bundle(&bundle).is_err(), "export must not overwrite");
                verify_artifact(&bundle, &checksum).unwrap();
            }
        }
    }
}
