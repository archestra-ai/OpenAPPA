//! Published after the build: the bytes belonging to one marketplace commit.
//! This descriptor is fetched from the official release, not trusted merely
//! because an archive contains it. Offline callers authenticate the outer
//! archive with a digest obtained through a trusted channel.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const REPOSITORY: &str = "archestra-ai/OpenAPPA";
pub const IMAGE_REGISTRY: &str = "europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public";
pub const DESCRIPTOR_FILE: &str = "appa-generation.json";
pub const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("generation descriptor exceeds {MAX_DESCRIPTOR_BYTES} bytes")]
    TooLarge,
    #[error("invalid generation descriptor: {0}")]
    Syntax(#[from] serde_json::Error),
    #[error("invalid generation field `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> GenerationError {
    GenerationError::Invalid {
        field,
        reason: reason.into(),
    }
}

/// Full source identity, never an abbreviated commit or moving branch name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Commit(String);

impl Commit {
    pub fn parse(text: &str) -> Result<Self, GenerationError> {
        if text.len() != 40 || !text.bytes().all(lower_hex) {
            return Err(invalid("commit", "expected 40 lowercase hexadecimal characters"));
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Commit {
    type Error = GenerationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<Commit> for String {
    fn from(value: Commit) -> Self {
        value.0
    }
}
impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// SHA-256 of artifact bytes, distinct from the canonical package-tree digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    pub fn parse(text: &str) -> Result<Self, GenerationError> {
        let Some(hex) = text.strip_prefix("sha256:") else {
            return Err(invalid("sha256", "expected sha256:<64 lowercase hex characters>"));
        };
        if hex.len() != 64 || !hex.bytes().all(lower_hex) {
            return Err(invalid("sha256", "expected sha256:<64 lowercase hex characters>"));
        }
        Ok(Self(text.to_owned()))
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(bytes)))
    }

    pub fn of_reader(mut reader: impl Read, limit: u64) -> io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut count = 0u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count.saturating_add(read as u64);
            if count > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact exceeds its byte limit",
                ));
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Self(format!("sha256:{:x}", hasher.finalize())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn hex(&self) -> &str {
        &self.0[7..]
    }
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

impl TryFrom<String> for ArtifactDigest {
    type Error = GenerationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<ArtifactDigest> for String {
    fn from(value: ArtifactDigest) -> Self {
        value.0
    }
}
impl fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The platforms for which the release workflow builds an APPA executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    LinuxAmd64,
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    LinuxArm64,
    #[serde(rename = "x86_64-apple-darwin")]
    MacAmd64,
    #[serde(rename = "aarch64-apple-darwin")]
    MacArm64,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    WindowsAmd64,
    #[serde(rename = "aarch64-pc-windows-msvc")]
    WindowsArm64,
}

impl Platform {
    pub const ALL: [Self; 6] = [
        Self::LinuxAmd64,
        Self::LinuxArm64,
        Self::MacAmd64,
        Self::MacArm64,
        Self::WindowsAmd64,
        Self::WindowsArm64,
    ];

    pub fn archive(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "appa-x86_64-unknown-linux-gnu.tar.gz",
            Self::LinuxArm64 => "appa-aarch64-unknown-linux-gnu.tar.gz",
            Self::MacAmd64 => "appa-x86_64-apple-darwin.tar.gz",
            Self::MacArm64 => "appa-aarch64-apple-darwin.tar.gz",
            Self::WindowsAmd64 => "appa-x86_64-pc-windows-msvc.zip",
            Self::WindowsArm64 => "appa-aarch64-pc-windows-msvc.zip",
        }
    }

    pub fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Some(Self::LinuxAmd64),
            ("linux", "aarch64") => Some(Self::LinuxArm64),
            ("macos", "x86_64") => Some(Self::MacAmd64),
            ("macos", "aarch64") => Some(Self::MacArm64),
            ("windows", "x86_64") => Some(Self::WindowsAmd64),
            ("windows", "aarch64") => Some(Self::WindowsArm64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ImagePlatform {
    #[serde(rename = "linux/amd64")]
    Amd64,
    #[serde(rename = "linux/arm64")]
    Arm64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Image {
    Runtime,
    Python,
    Go,
}

impl Image {
    pub const ALL: [Self; 3] = [Self::Runtime, Self::Python, Self::Go];

    pub fn repository(self) -> String {
        let name = match self {
            Self::Runtime => "appa-runtime",
            Self::Python => "appa-kagent-adk",
            // The ordinary kagent controller derives this repository name.
            Self::Go => "golang-adk",
        };
        format!("{IMAGE_REGISTRY}/{name}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDigests {
    digest: ArtifactDigest,
    platforms: BTreeMap<ImagePlatform, ArtifactDigest>,
}

impl ImageDigests {
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
    pub fn platforms(&self) -> &BTreeMap<ImagePlatform, ArtifactDigest> {
        &self.platforms
    }
}

/// Validation happens once at acquisition. No fields can be changed after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawGeneration", into = "RawGeneration")]
pub struct Generation {
    commit: Commit,
    catalog: ArtifactDigest,
    artifacts: Artifacts,
}

/// Where a generation's runtime and plugin bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifacts {
    /// The release workflow's assets for one version tag: every platform's
    /// executable, the plugin and marketplace archives, the chart, the images.
    Published(Published),
    /// One development build installing itself on the machine that built it:
    /// its own executable and the plugin tree stamped into it at compilation.
    Build(Build),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    release: String,
    marketplace: ArtifactDigest,
    claude_plugin: ArtifactDigest,
    runtime_chart: ArtifactDigest,
    binaries: BTreeMap<Platform, ArtifactDigest>,
    images: BTreeMap<Image, ImageDigests>,
}

impl Published {
    pub fn release(&self) -> &str {
        &self.release
    }
    pub fn binaries(&self) -> &BTreeMap<Platform, ArtifactDigest> {
        &self.binaries
    }
    pub fn images(&self) -> &BTreeMap<Image, ImageDigests> {
        &self.images
    }
    fn version(&self) -> &str {
        &self.release[1..]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    platform: Platform,
    plugin_tree: String,
    binary: ArtifactDigest,
    claude_plugin: ArtifactDigest,
}

impl Build {
    pub fn platform(&self) -> Platform {
        self.platform
    }
    /// The canonical digest of the staged plugin tree, as the build stamps it.
    pub fn plugin_tree(&self) -> &str {
        &self.plugin_tree
    }
}

pub const BUILD_PLUGIN_ARCHIVE: &str = "appa-plugin-build.tar.gz";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPublished {
    schema: u32,
    repository: String,
    commit: Commit,
    release: String,
    protocol: u32,
    catalog: ArtifactDigest,
    marketplace: ArtifactDigest,
    claude_plugin: ArtifactDigest,
    runtime_chart: ArtifactDigest,
    binaries: BTreeMap<Platform, ArtifactDigest>,
    images: BTreeMap<Image, ImageDigests>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BuildMarker {
    Build,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    schema: u32,
    repository: String,
    commit: Commit,
    protocol: u32,
    catalog: ArtifactDigest,
    source: BuildMarker,
    platform: Platform,
    plugin_tree: String,
    binary: ArtifactDigest,
    claude_plugin: ArtifactDigest,
}

/// The published shape is the release workflow's, unchanged. A build
/// descriptor is told apart by its `source` field before either shape is
/// parsed, so a malformed published document still reports its own field.
#[derive(Serialize)]
#[serde(untagged)]
enum RawGeneration {
    Published(RawPublished),
    Build(RawBuild),
}

impl<'de> Deserialize<'de> for RawGeneration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = serde_json::Value::deserialize(deserializer)?;
        let build = document.get("source").is_some_and(|source| source == "build");
        if build {
            serde_json::from_value(document).map(Self::Build)
        } else {
            serde_json::from_value(document).map(Self::Published)
        }
        .map_err(serde::de::Error::custom)
    }
}

fn validate_identity(schema: u32, repository: &str, protocol: u32) -> Result<(), GenerationError> {
    if schema != 1 {
        return Err(invalid("schema", "only schema 1 is supported"));
    }
    if repository != REPOSITORY {
        return Err(invalid("repository", "only the official marketplace is supported"));
    }
    if protocol != crate::PROTOCOL {
        return Err(invalid("protocol", "this runtime does not support that protocol"));
    }
    Ok(())
}

fn validate_release(release: &str) -> Result<(), GenerationError> {
    let version = release.strip_prefix('v').unwrap_or_default();
    if version.is_empty()
        || version.len() > 96
        || !version.starts_with(|c: char| c.is_ascii_digit())
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
    {
        return Err(invalid("release", "expected a version tag such as v0.14.1"));
    }
    Ok(())
}

fn validate_plugin_tree(digest: &str) -> Result<(), GenerationError> {
    if digest.len() != 64 || !digest.bytes().all(lower_hex) {
        return Err(invalid("plugin_tree", "expected 64 lowercase hexadecimal characters"));
    }
    Ok(())
}

impl TryFrom<RawGeneration> for Generation {
    type Error = GenerationError;

    fn try_from(raw: RawGeneration) -> Result<Self, Self::Error> {
        match raw {
            RawGeneration::Published(raw) => {
                validate_identity(raw.schema, &raw.repository, raw.protocol)?;
                validate_release(&raw.release)?;
                if Platform::ALL.iter().any(|p| !raw.binaries.contains_key(p)) {
                    return Err(invalid("binaries", "all six release platforms must be present"));
                }
                if Image::ALL.iter().any(|image| !raw.images.contains_key(image))
                    || raw.images.values().any(|image| image.platforms.is_empty())
                {
                    return Err(invalid(
                        "images",
                        "runtime, Python and Go must each name at least one platform digest",
                    ));
                }
                Ok(Self {
                    commit: raw.commit,
                    catalog: raw.catalog,
                    artifacts: Artifacts::Published(Published {
                        release: raw.release,
                        marketplace: raw.marketplace,
                        claude_plugin: raw.claude_plugin,
                        runtime_chart: raw.runtime_chart,
                        binaries: raw.binaries,
                        images: raw.images,
                    }),
                })
            }
            RawGeneration::Build(raw) => {
                validate_identity(raw.schema, &raw.repository, raw.protocol)?;
                validate_plugin_tree(&raw.plugin_tree)?;
                Ok(Self {
                    commit: raw.commit,
                    catalog: raw.catalog,
                    artifacts: Artifacts::Build(Build {
                        platform: raw.platform,
                        plugin_tree: raw.plugin_tree,
                        binary: raw.binary,
                        claude_plugin: raw.claude_plugin,
                    }),
                })
            }
        }
    }
}

impl From<Generation> for RawGeneration {
    fn from(generation: Generation) -> Self {
        match generation.artifacts {
            Artifacts::Published(published) => Self::Published(RawPublished {
                schema: 1,
                repository: REPOSITORY.to_owned(),
                commit: generation.commit,
                release: published.release,
                protocol: crate::PROTOCOL,
                catalog: generation.catalog,
                marketplace: published.marketplace,
                claude_plugin: published.claude_plugin,
                runtime_chart: published.runtime_chart,
                binaries: published.binaries,
                images: published.images,
            }),
            Artifacts::Build(build) => Self::Build(RawBuild {
                schema: 1,
                repository: REPOSITORY.to_owned(),
                commit: generation.commit,
                protocol: crate::PROTOCOL,
                catalog: generation.catalog,
                source: BuildMarker::Build,
                platform: build.platform,
                plugin_tree: build.plugin_tree,
                binary: build.binary,
                claude_plugin: build.claude_plugin,
            }),
        }
    }
}

impl Generation {
    pub fn parse(bytes: &[u8]) -> Result<Self, GenerationError> {
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(GenerationError::TooLarge);
        }
        Ok(serde_json::from_slice(bytes)?)
    }

    /// A development build's own generation, validated like a parsed one.
    pub fn build(
        commit: Commit,
        catalog: ArtifactDigest,
        platform: Platform,
        plugin_tree: &str,
        binary: ArtifactDigest,
        claude_plugin: ArtifactDigest,
    ) -> Result<Self, GenerationError> {
        validate_plugin_tree(plugin_tree)?;
        Ok(Self {
            commit,
            catalog,
            artifacts: Artifacts::Build(Build {
                platform,
                plugin_tree: plugin_tree.to_owned(),
                binary,
                claude_plugin,
            }),
        })
    }

    pub fn commit(&self) -> &Commit {
        &self.commit
    }
    pub fn catalog(&self) -> &ArtifactDigest {
        &self.catalog
    }
    pub fn artifacts(&self) -> &Artifacts {
        &self.artifacts
    }
    pub fn published(&self) -> Option<&Published> {
        match &self.artifacts {
            Artifacts::Published(published) => Some(published),
            Artifacts::Build(_) => None,
        }
    }
    pub fn build_artifacts(&self) -> Option<&Build> {
        match &self.artifacts {
            Artifacts::Build(build) => Some(build),
            Artifacts::Published(_) => None,
        }
    }

    /// How a person names this generation: its version tag, or the build's commit.
    pub fn label(&self) -> String {
        match &self.artifacts {
            Artifacts::Published(published) => published.release.clone(),
            Artifacts::Build(_) => format!("build {}", &self.commit.as_str()[..12]),
        }
    }

    /// The archive that carries the Claude Code plugin tree.
    pub fn plugin_archive(&self) -> String {
        match &self.artifacts {
            Artifacts::Published(published) => format!("appa-plugin-{}.tar.gz", published.version()),
            Artifacts::Build(_) => BUILD_PLUGIN_ARCHIVE.to_owned(),
        }
    }

    /// The archive that carries the marketplace tree; a build stages its own.
    pub fn marketplace_archive(&self) -> Option<String> {
        self.published()
            .map(|published| format!("appa-marketplace-{}.tar.gz", published.version()))
    }

    /// The kagent runtime chart; a build publishes none.
    pub fn runtime_chart_archive(&self) -> Option<String> {
        self.published()
            .map(|published| format!("appa-runtime-{}.tgz", published.version()))
    }

    /// Official asset names, never a URL/path supplied by a package manifest.
    pub fn archives(&self) -> BTreeMap<String, ArtifactDigest> {
        match &self.artifacts {
            Artifacts::Published(published) => {
                let mut files: BTreeMap<_, _> = published
                    .binaries
                    .iter()
                    .map(|(platform, digest)| (platform.archive().to_owned(), digest.clone()))
                    .collect();
                files.insert(
                    format!("appa-marketplace-{}.tar.gz", published.version()),
                    published.marketplace.clone(),
                );
                files.insert(
                    format!("appa-plugin-{}.tar.gz", published.version()),
                    published.claude_plugin.clone(),
                );
                files.insert(
                    format!("appa-runtime-{}.tgz", published.version()),
                    published.runtime_chart.clone(),
                );
                files
            }
            Artifacts::Build(build) => BTreeMap::from([
                (build.platform.archive().to_owned(), build.binary.clone()),
                (BUILD_PLUGIN_ARCHIVE.to_owned(), build.claude_plugin.clone()),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> Value {
        let digest = ArtifactDigest::of_bytes(b"fixture");
        json!({"schema": 1, "repository": REPOSITORY, "commit": "a".repeat(40),
            "release": "v0.14.1", "protocol": crate::PROTOCOL, "catalog": digest,
            "marketplace": digest, "claude_plugin": digest, "runtime_chart": digest,
            "binaries": Platform::ALL.into_iter().map(|p| (p, digest.clone())).collect::<BTreeMap<_, _>>(),
            "images": Image::ALL.into_iter().map(|i| (i, json!({"digest": digest,
                "platforms": {"linux/amd64": digest}}))).collect::<BTreeMap<_, _>>()})
    }

    #[test]
    fn published_generation_round_trips_with_closed_artifact_names() {
        let bytes = serde_json::to_vec(&fixture()).unwrap();
        let generation = Generation::parse(&bytes).unwrap();
        assert_eq!(
            Generation::parse(&serde_json::to_vec(&generation).unwrap()).unwrap(),
            generation
        );
        assert_eq!(generation.archives().len(), 9);
        assert!(generation.archives().contains_key("appa-plugin-0.14.1.tar.gz"));
        assert_eq!(generation.commit().as_str(), "a".repeat(40));
        assert_eq!(generation.published().unwrap().release(), "v0.14.1");
        assert!(generation.build_artifacts().is_none());
        // Serialization keeps the release workflow's document shape.
        let serialized: Value = serde_json::to_value(&generation).unwrap();
        assert_eq!(serialized, fixture());
    }

    #[test]
    fn build_generation_round_trips_and_names_its_two_archives() {
        let digest = ArtifactDigest::of_bytes(b"fixture");
        let generation = Generation::build(
            Commit::parse(&"b".repeat(40)).unwrap(),
            digest.clone(),
            Platform::MacArm64,
            &"c".repeat(64),
            ArtifactDigest::of_bytes(b"binary"),
            ArtifactDigest::of_bytes(b"plugin"),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&generation).unwrap();
        let parsed = Generation::parse(&bytes).unwrap();
        assert_eq!(parsed, generation);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["source"], "build");
        assert!(parsed.published().is_none());
        assert_eq!(parsed.build_artifacts().unwrap().platform(), Platform::MacArm64);
        assert_eq!(parsed.archives().len(), 2);
        assert_eq!(parsed.plugin_archive(), BUILD_PLUGIN_ARCHIVE);
        assert!(parsed.marketplace_archive().is_none());
        assert!(parsed.runtime_chart_archive().is_none());
        assert!(Generation::build(
            Commit::parse(&"b".repeat(40)).unwrap(),
            digest,
            Platform::MacArm64,
            "short",
            ArtifactDigest::of_bytes(b"binary"),
            ArtifactDigest::of_bytes(b"plugin"),
        )
        .is_err());
    }

    #[test]
    fn malformed_identity_and_unknown_fields_are_rejected() {
        for (field, value) in [
            ("schema", json!(2)),
            ("protocol", json!(99)),
            ("repository", json!("untrusted/marketplace")),
            ("commit", json!("main")),
            ("release", json!("v1/../../other")),
            ("catalog", json!("sha256:00")),
            ("surprise", json!(true)),
        ] {
            let mut document = fixture();
            document[field] = value;
            assert!(
                Generation::parse(&serde_json::to_vec(&document).unwrap()).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn missing_or_unusable_platforms_and_images_are_rejected() {
        for path in ["binaries", "images"] {
            let mut document = fixture();
            document[path] = json!({});
            assert!(Generation::parse(&serde_json::to_vec(&document).unwrap()).is_err());
        }
        let mut document = fixture();
        document["images"]["go"]["platforms"] = json!({});
        assert!(Generation::parse(&serde_json::to_vec(&document).unwrap()).is_err());
        document["images"]["go"]["platforms"] = json!({"unknown/os": ArtifactDigest::of_bytes(b"x")});
        assert!(Generation::parse(&serde_json::to_vec(&document).unwrap()).is_err());
    }

    #[test]
    fn descriptor_size_is_bounded_before_parsing() {
        assert!(matches!(
            Generation::parse(&vec![b' '; MAX_DESCRIPTOR_BYTES + 1]),
            Err(GenerationError::TooLarge)
        ));
    }

    #[test]
    fn digests_are_of_bytes_and_have_one_portable_spelling() {
        assert_eq!(
            ArtifactDigest::of_bytes(b"abc").hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for value in ["abc", "sha256:ABC", &format!("sha256:{}", "A".repeat(64))] {
            assert!(ArtifactDigest::parse(value).is_err());
        }
        assert_eq!(
            ArtifactDigest::of_reader(b"abc".as_slice(), 3).unwrap(),
            ArtifactDigest::of_bytes(b"abc")
        );
        assert!(ArtifactDigest::of_reader(b"abc".as_slice(), 2).is_err());
    }
}
