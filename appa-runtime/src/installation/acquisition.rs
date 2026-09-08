//! Explicit acquisition only. Callers validate their config before entering
//! here; listing installed state and runtime startup do not enter this module.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use appa_package::generation::{ArtifactDigest, Commit, DESCRIPTOR_FILE, Generation, Platform, REPOSITORY};
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
}

impl Imported {
    pub fn selection(&self) -> &Selection {
        &self.selection
    }
    pub fn config(&self) -> &str {
        &self.config
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
        for name in required_archives(&generation, requirements) {
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
        let marketplace = unpacked.join("marketplace");
        selection.validate_packages(&marketplace)?;
        let mut archives = BTreeMap::new();
        for name in required_archives(&generation, selection.requirements()) {
            let path = unpacked.join("artifacts").join(&name);
            verify_artifact(&path, &generation.archives()[&name])?;
            archives.insert(name, path);
        }
        Ok(Self {
            _stage: Some(stage),
            generation,
            marketplace,
            archives,
            imported: Some(Imported { selection, config }),
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
        if generation.release() != release
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
        let marketplace_archive = format!("appa-marketplace-{}.tar.gz", &generation.release()[1..]);
        let names = required_archives(&generation, requirements);
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

pub(super) fn required_archives(generation: &Generation, requirements: Requirements) -> Vec<String> {
    let version = &generation.release()[1..];
    let mut names = vec![format!("appa-marketplace-{version}.tar.gz")];
    if let Requirements::Claude(platform) | Requirements::Both(platform) = requirements {
        names.push(platform.archive().to_owned());
        names.push(format!("appa-plugin-{version}.tar.gz"));
    }
    if matches!(requirements, Requirements::Kagent | Requirements::Both(_)) {
        names.push(format!("appa-runtime-{version}.tgz"));
    }
    names
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
