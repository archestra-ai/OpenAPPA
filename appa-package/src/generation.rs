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
#[serde(try_from = "RawGeneration")]
pub struct Generation {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGeneration {
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

impl TryFrom<RawGeneration> for Generation {
    type Error = GenerationError;

    fn try_from(raw: RawGeneration) -> Result<Self, Self::Error> {
        if raw.schema != 1 {
            return Err(invalid("schema", "only schema 1 is supported"));
        }
        if raw.repository != REPOSITORY {
            return Err(invalid("repository", "only the official marketplace is supported"));
        }
        if raw.protocol != crate::PROTOCOL {
            return Err(invalid("protocol", "this runtime does not support that protocol"));
        }
        let version = raw.release.strip_prefix('v').unwrap_or_default();
        if version.is_empty()
            || version.len() > 96
            || !version.starts_with(|c: char| c.is_ascii_digit())
            || !version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
        {
            return Err(invalid("release", "expected a version tag such as v0.14.1"));
        }
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
            schema: raw.schema,
            repository: raw.repository,
            commit: raw.commit,
            release: raw.release,
            protocol: raw.protocol,
            catalog: raw.catalog,
            marketplace: raw.marketplace,
            claude_plugin: raw.claude_plugin,
            runtime_chart: raw.runtime_chart,
            binaries: raw.binaries,
            images: raw.images,
        })
    }
}

impl Generation {
    pub fn parse(bytes: &[u8]) -> Result<Self, GenerationError> {
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(GenerationError::TooLarge);
        }
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn commit(&self) -> &Commit {
        &self.commit
    }
    pub fn release(&self) -> &str {
        &self.release
    }
    pub fn catalog(&self) -> &ArtifactDigest {
        &self.catalog
    }
    pub fn marketplace(&self) -> &ArtifactDigest {
        &self.marketplace
    }
    pub fn binaries(&self) -> &BTreeMap<Platform, ArtifactDigest> {
        &self.binaries
    }
    pub fn images(&self) -> &BTreeMap<Image, ImageDigests> {
        &self.images
    }

    /// Official asset names, never a URL/path supplied by a package manifest.
    pub fn archives(&self) -> BTreeMap<String, ArtifactDigest> {
        let version = &self.release[1..];
        let mut files: BTreeMap<_, _> = self
            .binaries
            .iter()
            .map(|(platform, digest)| (platform.archive().to_owned(), digest.clone()))
            .collect();
        files.insert(format!("appa-marketplace-{version}.tar.gz"), self.marketplace.clone());
        files.insert(format!("appa-plugin-{version}.tar.gz"), self.claude_plugin.clone());
        files.insert(format!("appa-runtime-{version}.tgz"), self.runtime_chart.clone());
        files
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
