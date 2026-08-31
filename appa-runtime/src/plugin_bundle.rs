//! The Claude plugin bundle belonging to this binary's own release.
//!
//! A binary is built knowing the SHA-256 of its release's plugin artifact and
//! accepts no other bytes. That digest is the whole compatibility rule: there is
//! no version field inside the artifact and no commit to compare, because the
//! digest already pins the exact bytes. A development build has no digest, so it
//! refuses to download and requires `--plugin-source`.

use std::env;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Files and directories every plugin source must carry, in the marketplace-root
/// shape `scripts/appa-stage-plugin-bundle.sh` produces. One validator serves
/// both source resolution and the reuse check on an existing deployment.
const REQUIRED_FILES: [&str; 4] = [
    ".claude-plugin/marketplace.json",
    "plugin/.claude-plugin/plugin.json",
    "plugin/hooks/hooks.json",
    "website/content/docs/contracts.md",
];
const REQUIRED_DIRS: [&str; 2] = ["plugin", "batteries"];

/// A source carries both hook maps; materialization keeps the one for this
/// platform and removes the other, so a deployment carries exactly one. The same
/// validator serves both, and this is the only thing it varies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeShape {
    Source,
    Deployment,
}

#[derive(Debug, Error)]
pub enum PluginBundleError {
    #[error(
        "appa {version} is a development build with no plugin artifact digest; pass --plugin-source <checkout>"
    )]
    NoBakedDigest { version: &'static str },
    #[error("{value} is not a SHA-256 digest")]
    MalformedDigest { value: String },
    #[error("the plugin source at {path} is not a marketplace root: {reason}")]
    InvalidSource { path: PathBuf, reason: String },
    #[error("cannot read the plugin source at {path}: {source}")]
    ReadSource {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid UTF-8 with `/` separators; rename it and retry")]
    UnportablePath { path: PathBuf },
    #[error("{path} is neither a regular file nor a directory")]
    UnsupportedEntry { path: PathBuf },
    #[error("{value} is not a usable runtime endpoint: {reason}")]
    MalformedEndpoint { value: String, reason: String },
    #[error("cannot write the deployment at {path}: {source}")]
    WriteDeployment {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the plugin archive at {path} is unusable: {reason}")]
    MalformedArchive { path: PathBuf, reason: String },
    #[error("cannot reserve a working directory under {path}")]
    NoReservation { path: PathBuf },
}

/// The SHA-256 of a plugin artifact: what a binary was built with, and what a
/// fetched or cached archive must hash to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PluginDigest([u8; 32]);

impl PluginDigest {
    pub fn parse(value: &str) -> Result<Self, PluginBundleError> {
        let trimmed = value.trim();
        let malformed = || PluginBundleError::MalformedDigest {
            value: trimmed.to_owned(),
        };
        if trimmed.len() != 64 {
            return Err(malformed());
        }
        let mut bytes = [0u8; 32];
        for (slot, pair) in bytes.iter_mut().zip(trimmed.as_bytes().chunks_exact(2)) {
            let hex = std::str::from_utf8(pair).map_err(|_| malformed())?;
            *slot = u8::from_str_radix(hex, 16).map_err(|_| malformed())?;
        }
        Ok(Self(bytes))
    }

    pub fn of(bytes: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes);
        Self(digest.finalize().into())
    }

    pub fn from_hasher(hasher: Sha256) -> Self {
        Self(hasher.finalize().into())
    }
}

impl fmt::Display for PluginDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for PluginDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PluginDigest({self})")
    }
}

/// The digest of the plugin artifact this build belongs to.
///
/// Release builds carry it as a compile-time constant, so a shipped binary
/// cannot be pointed at other bytes by its environment. `[profile.release]` pins
/// `debug-assertions = false`, which is what closes the debug branch there.
pub fn expected_plugin_digest() -> Option<PluginDigest> {
    let raw = if cfg!(debug_assertions) {
        env::var("APPA_PLUGIN_SHA256").ok()
    } else {
        option_env!("APPA_PLUGIN_SHA256").map(str::to_owned)
    };
    raw.as_deref().and_then(|value| PluginDigest::parse(value).ok())
}

/// Where a deployment's bytes come from. `Explicit` is the development override;
/// everything else resolves to this binary's own release artifact.
#[derive(Debug, Clone)]
pub enum PluginSource {
    Explicit(PathBuf),
    Release(PluginDigest),
}

impl PluginSource {
    /// `--plugin-source` when given, otherwise this build's release artifact.
    /// A development build with neither refuses rather than guessing.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, PluginBundleError> {
        match explicit {
            Some(path) => Ok(Self::Explicit(canonical_source(Path::new(path))?)),
            None => expected_plugin_digest()
                .map(Self::Release)
                .ok_or(PluginBundleError::NoBakedDigest {
                    version: env!("CARGO_PKG_VERSION"),
                }),
        }
    }
}

/// Resolve the user's `--plugin-source` argument like any other path argument.
///
/// Windows keeps the existing carve-out: Claude rejects a `\\?\` marketplace
/// path, so the extended-length prefix is stripped after canonicalization.
fn canonical_source(path: &Path) -> Result<PathBuf, PluginBundleError> {
    let canonical = fs::canonicalize(path).map_err(|source| PluginBundleError::ReadSource {
        path: path.to_path_buf(),
        source,
    })?;
    let canonical = if cfg!(windows) {
        strip_extended_prefix(&canonical)
    } else {
        canonical
    };
    validate_tree(&canonical, TreeShape::Source)?;
    Ok(canonical)
}

fn strip_extended_prefix(path: &Path) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}

/// Structural validation, applied identically to a `--plugin-source` checkout, a
/// freshly extracted archive, and an existing deployment considered for reuse.
///
/// This checks shape, not content: a tree whose `batteries/` files were edited in
/// place passes. Reuse pairs it with a byte comparison of the one generated file.
pub fn validate_tree(root: &Path, shape: TreeShape) -> Result<(), PluginBundleError> {
    let invalid = |reason: String| PluginBundleError::InvalidSource {
        path: root.to_path_buf(),
        reason,
    };
    if !root.is_dir() {
        return Err(invalid("it is not a directory".to_owned()));
    }
    for relative in REQUIRED_DIRS {
        if !root.join(relative).is_dir() {
            return Err(invalid(format!("{relative}/ is missing")));
        }
    }
    for relative in REQUIRED_FILES {
        if !root.join(relative).is_file() {
            return Err(invalid(format!("{relative} is missing")));
        }
    }
    if shape == TreeShape::Source && !root.join(WINDOWS_HOOKS).is_file() {
        return Err(invalid(format!("{WINDOWS_HOOKS} is missing")));
    }
    Ok(())
}

/// A staged relative path as the canonical digest encodes it: UTF-8 with `/`
/// separators on every platform. The bundle is ASCII filenames, so a path that
/// cannot be spelled this way is refused rather than normalized.
pub fn portable_relative_path(relative: &Path) -> Result<String, PluginBundleError> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => match part.to_str() {
                Some(text) => parts.push(text),
                None => {
                    return Err(PluginBundleError::UnportablePath {
                        path: relative.to_path_buf(),
                    });
                }
            },
            _ => {
                return Err(PluginBundleError::UnportablePath {
                    path: relative.to_path_buf(),
                });
            }
        }
    }
    Ok(parts.join("/"))
}

// ---------------------------------------------------------------------------
// Endpoint
// ---------------------------------------------------------------------------

/// The address the deployment's runtime listens on and every consumer talks to.
///
/// One value, validated once before any mutation, delivered explicitly to each
/// consumer rather than left to per-file default constants. The production
/// address is fixed; `APPA_ENDPOINT` overrides it in debug builds only, which is
/// the seam the endpoint tests need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    url: String,
    listen: SocketAddr,
}

/// The spelling every consumer carries today, and the one the regression scan
/// looks for in a materialized tree.
pub const DEFAULT_ENDPOINT_URL: &str = "http://127.0.0.1:8787";

impl Endpoint {
    pub fn resolve() -> Result<Self, PluginBundleError> {
        let configured = if cfg!(debug_assertions) {
            env::var("APPA_ENDPOINT").ok()
        } else {
            None
        };
        Self::parse(configured.as_deref().unwrap_or(DEFAULT_ENDPOINT_URL))
    }

    /// `http://` plus a loopback literal and a port. No path, no trailing slash,
    /// no hostname: anything else is refused up front rather than half-applied.
    pub fn parse(text: &str) -> Result<Self, PluginBundleError> {
        let malformed = |reason: &str| PluginBundleError::MalformedEndpoint {
            value: text.to_owned(),
            reason: reason.to_owned(),
        };
        let authority = text
            .strip_prefix("http://")
            .ok_or_else(|| malformed("it must begin with http://"))?;
        if authority.contains('/') {
            return Err(malformed("it must carry no path and no trailing slash"));
        }
        let listen: SocketAddr = authority.parse().map_err(|_| {
            malformed("it must be a literal address and port, such as 127.0.0.1:8787")
        })?;
        if !listen.ip().is_loopback() {
            return Err(malformed("it must be a loopback address"));
        }
        if listen.port() == 0 {
            return Err(malformed("it must name a fixed port"));
        }
        Ok(Self {
            url: format!("http://{authority}"),
            listen,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    /// The URL of one runtime path, for probes such as `/binary-fingerprint`.
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }
}

// ---------------------------------------------------------------------------
// Canonical digests
// ---------------------------------------------------------------------------

/// Every length prefix in both digests is an unsigned 64-bit big-endian integer
/// followed immediately by exactly that many bytes. Naive concatenation would be
/// ambiguous, and both digests name deployment directories, so the encoding is
/// observable identity and is pinned rather than left to the implementation.
fn absorb_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

const KIND_FILE: u8 = b'f';
const KIND_DIRECTORY: u8 = b'd';

type StagedEntry = (String, u8, PathBuf);

/// The identity of a plugin source that has no release digest of its own.
///
/// Computed over the staged tree after staging and **before** rendering, so it
/// never depends on the paths being rendered into it. Without this, editing a
/// file in a `--plugin-source` checkout and re-running init would reuse the
/// existing deployment and never reach Claude.
pub fn canonical_source_digest(root: &Path) -> Result<PluginDigest, PluginBundleError> {
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries)?;
    // Bytewise on the UTF-8 path bytes, never locale collation.
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    let mut hasher = Sha256::new();
    for (relative, kind, absolute) in entries {
        absorb_field(&mut hasher, relative.as_bytes());
        hasher.update([kind]);
        if kind == KIND_DIRECTORY {
            absorb_field(&mut hasher, &[]);
        } else {
            let contents = fs::read(&absolute).map_err(|source| PluginBundleError::ReadSource {
                path: absolute.clone(),
                source,
            })?;
            absorb_field(&mut hasher, &contents);
        }
    }
    Ok(PluginDigest::from_hasher(hasher))
}

fn collect_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<StagedEntry>,
) -> Result<(), PluginBundleError> {
    let read = fs::read_dir(directory).map_err(|source| PluginBundleError::ReadSource {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in read {
        let entry = entry.map_err(|source| PluginBundleError::ReadSource {
            path: directory.to_path_buf(),
            source,
        })?;
        let absolute = entry.path();
        let relative =
            absolute
                .strip_prefix(root)
                .map_err(|_| PluginBundleError::UnportablePath {
                    path: absolute.clone(),
                })?;
        let portable = portable_relative_path(relative)?;
        // Symlinks and special files are refused here exactly as in extraction:
        // the bundle is regular files and directories.
        let kind = entry
            .file_type()
            .map_err(|source| PluginBundleError::ReadSource {
                path: absolute.clone(),
                source,
            })?;
        if kind.is_dir() {
            entries.push((portable, KIND_DIRECTORY, absolute.clone()));
            collect_entries(root, &absolute, entries)?;
        } else if kind.is_file() {
            entries.push((portable, KIND_FILE, absolute));
        } else {
            return Err(PluginBundleError::UnsupportedEntry { path: absolute });
        }
    }
    Ok(())
}

/// Everything a deployment's identity depends on beyond the source bytes.
#[derive(Debug, Clone)]
pub struct DeploymentPlan {
    pub source_digest: PluginDigest,
    pub binary_path: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub endpoint: Endpoint,
}

/// The name of the deployment directory: the source identity plus every path and
/// platform detail rendered into it, so a deployment can never be reused for a
/// different binary, config, data directory, endpoint or platform.
pub fn deployment_digest(plan: &DeploymentPlan) -> Result<PluginDigest, PluginBundleError> {
    let mut hasher = Sha256::new();
    absorb_field(&mut hasher, plan.source_digest.to_string().as_bytes());
    absorb_field(&mut hasher, path_identity(&plan.binary_path)?.as_bytes());
    absorb_field(&mut hasher, path_identity(&plan.config_path)?.as_bytes());
    absorb_field(&mut hasher, path_identity(&plan.data_dir)?.as_bytes());
    absorb_field(&mut hasher, plan.endpoint.url().as_bytes());
    absorb_field(&mut hasher, platform_token().as_bytes());
    Ok(PluginDigest::from_hasher(hasher))
}

fn platform_token() -> &'static str {
    if cfg!(windows) { "windows" } else { "unix" }
}

/// The lexical absolute path init constructed: no `canonicalize`, no case
/// folding, consistent with refusing rather than normalizing elsewhere.
fn path_identity(path: &Path) -> Result<&str, PluginBundleError> {
    path.to_str()
        .ok_or_else(|| PluginBundleError::UnportablePath {
            path: path.to_path_buf(),
        })
}

// ---------------------------------------------------------------------------
// Materialization
// ---------------------------------------------------------------------------

/// Caps on what an archive may unpack to. The bundle is a few hundred small
/// text files; these bound a hostile or corrupt archive without being tight
/// enough to constrain the real one.
const MAX_ENTRIES: usize = 4096;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// The generated file every deployed shell surface sources.
const PATHS_SH: &str = "plugin/hooks/appa-paths.sh";
const WINDOWS_HOOKS: &str = "plugin/hooks/hooks.windows.json";
const PATHS_PS1: &str = "plugin/hooks/appa-paths.ps1";

/// Where a deployment's bytes come from at materialization time.
pub enum Population<'a> {
    /// A `--plugin-source` checkout, copied.
    Tree(&'a Path),
    /// A verified release archive, extracted.
    Archive(&'a Path),
}

/// A published, immutable deployment directory: what Claude registers.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub root: PathBuf,
    pub digest: PluginDigest,
    pub source_digest: PluginDigest,
    pub reused: bool,
}

/// Materialize the deployment for this source and these paths, or reuse the
/// existing one after validating it.
///
/// Publication is a rename, so two inits racing on the same source converge on
/// one immutable directory rather than interleaving writes into it.
pub fn materialize(
    population: Population<'_>,
    deployments_dir: &Path,
    binary_path: &Path,
    config_path: &Path,
    data_dir: &Path,
    endpoint: &Endpoint,
) -> Result<Deployment, PluginBundleError> {
    fs::create_dir_all(deployments_dir).map_err(|source| PluginBundleError::WriteDeployment {
        path: deployments_dir.to_path_buf(),
        source,
    })?;

    let incoming = reserve_directory(deployments_dir, ".incoming-")?;
    let staged = || -> Result<(PluginDigest, DeploymentPlan), PluginBundleError> {
        match population {
            Population::Tree(source) => copy_tree(source, &incoming)?,
            Population::Archive(archive) => extract_archive(archive, &incoming)?,
        }
        validate_tree(&incoming, TreeShape::Source)?;
        // After staging, before rendering: the source identity must not depend
        // on the paths about to be rendered into it.
        let source_digest = canonical_source_digest(&incoming)?;
        let plan = DeploymentPlan {
            source_digest,
            binary_path: binary_path.to_path_buf(),
            config_path: config_path.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            endpoint: endpoint.clone(),
        };
        let digest = deployment_digest(&plan)?;
        Ok((digest, plan))
    }();

    let (digest, plan) = match staged {
        Ok(staged) => staged,
        Err(error) => {
            // Our own unpublished reservation: removing it deletes no
            // registered state.
            let _ = fs::remove_dir_all(&incoming);
            return Err(error);
        }
    };

    let published = deployments_dir.join(digest.to_string());
    if published.is_dir() {
        match reusable(&published, &plan) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&incoming);
                return Ok(Deployment {
                    root: published,
                    digest,
                    source_digest: plan.source_digest,
                    reused: true,
                });
            }
            Err(reason) => {
                tracing::debug!(path = %published.display(), %reason, "quarantining a damaged deployment");
                quarantine(deployments_dir, &published)?;
            }
        }
    }

    if let Err(error) = render(&incoming, &plan) {
        let _ = fs::remove_dir_all(&incoming);
        return Err(error);
    }

    match fs::rename(&incoming, &published) {
        Ok(()) => {}
        // Another init published the same immutable bytes first. Both are
        // correct and identical, so the race is benign.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            let _ = fs::remove_dir_all(&incoming);
        }
        Err(source) => {
            let _ = fs::remove_dir_all(&incoming);
            return Err(PluginBundleError::WriteDeployment {
                path: published,
                source,
            });
        }
    }

    Ok(Deployment {
        root: deployments_dir.join(digest.to_string()),
        digest,
        source_digest: plan.source_digest,
        reused: false,
    })
}

/// Whether an existing deployment can be reused as-is.
///
/// Structural validation plus a byte comparison of the one generated file. This
/// is deliberately not a content hash of every file: a tree whose `batteries/`
/// contents were edited in place is not detected, and init's convergence claim
/// is scoped to match.
fn reusable(published: &Path, plan: &DeploymentPlan) -> Result<(), String> {
    validate_tree(published, TreeShape::Deployment).map_err(|error| error.to_string())?;
    let rendered = published.join(PATHS_SH);
    let current = fs::read(&rendered).map_err(|error| format!("{PATHS_SH} is unreadable: {error}"))?;
    if current != paths_sh(plan).into_bytes() {
        return Err(format!("{PATHS_SH} is stale"));
    }
    Ok(())
}

/// Move a damaged deployment aside. Nothing is ever deleted, and the
/// destination never pre-exists, so `rename` cannot silently replace a
/// directory. The namespace is deliberately distinct from the
/// `.appa-init-recovery-` prefix that init sweeps globally.
fn quarantine(deployments_dir: &Path, published: &Path) -> Result<(), PluginBundleError> {
    let name = published
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("deployment");
    let container = reserve_directory(deployments_dir, &format!("{name}.quarantine-"))?;
    fs::rename(published, container.join("tree")).map_err(|source| {
        PluginBundleError::WriteDeployment {
            path: container.join("tree"),
            source,
        }
    })
}

/// Reserve a fresh directory by creation, incrementing until it succeeds.
///
/// A PID suffix would collide after PID reuse by a crashed init, so the
/// reservation is the creation itself. One helper serves both the incoming
/// directory and the quarantine container.
fn reserve_directory(parent: &Path, prefix: &str) -> Result<PathBuf, PluginBundleError> {
    for attempt in 0..1024 {
        let candidate = parent.join(format!("{prefix}{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(PluginBundleError::WriteDeployment {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(PluginBundleError::NoReservation {
        path: parent.join(prefix),
    })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), PluginBundleError> {
    let write = |path: &Path, source: std::io::Error| PluginBundleError::WriteDeployment {
        path: path.to_path_buf(),
        source,
    };
    fs::create_dir_all(destination).map_err(|error| write(destination, error))?;
    let entries = fs::read_dir(source).map_err(|error| PluginBundleError::ReadSource {
        path: source.to_path_buf(),
        source: error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| PluginBundleError::ReadSource {
            path: source.to_path_buf(),
            source: error,
        })?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| PluginBundleError::ReadSource {
                path: from.clone(),
                source: error,
            })?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).map_err(|error| write(&to, error))?;
        } else {
            return Err(PluginBundleError::UnsupportedEntry { path: from });
        }
    }
    Ok(())
}

/// Unpack a verified archive. Absolute paths, `..` components and anything that
/// is not a regular file or directory are refused; entry count and total
/// uncompressed bytes are capped. Modes come from init, not from the archive.
fn extract_archive(archive: &Path, destination: &Path) -> Result<(), PluginBundleError> {
    let file = fs::File::open(archive).map_err(|source| PluginBundleError::ReadSource {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let malformed = |reason: String| PluginBundleError::MalformedArchive {
        path: archive.to_path_buf(),
        reason,
    };
    let entries = tar
        .entries()
        .map_err(|error| malformed(format!("it is not a tar archive: {error}")))?;

    let mut count = 0usize;
    let mut total = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|error| malformed(format!("unreadable entry: {error}")))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(malformed(format!("it holds more than {MAX_ENTRIES} entries")));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(malformed(format!(
                "it unpacks to more than {MAX_UNCOMPRESSED_BYTES} bytes"
            )));
        }

        let path = entry
            .path()
            .map_err(|error| malformed(format!("unreadable entry path: {error}")))?
            .into_owned();
        let relative = safe_relative(&path).ok_or_else(|| {
            malformed(format!("{} escapes the archive root", path.display()))
        })?;
        let target = destination.join(&relative);
        let write = |source: std::io::Error| PluginBundleError::WriteDeployment {
            path: target.clone(),
            source,
        };

        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&target).map_err(write)?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        PluginBundleError::WriteDeployment {
                            path: parent.to_path_buf(),
                            source,
                        }
                    })?;
                }
                let mut out = fs::File::create(&target).map_err(write)?;
                std::io::copy(&mut entry, &mut out).map_err(|source| {
                    PluginBundleError::WriteDeployment {
                        path: target.clone(),
                        source,
                    }
                })?;
            }
            other => {
                return Err(malformed(format!(
                    "{} is a {other:?} entry; only regular files and directories are accepted",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// A relative, traversal-free path, or nothing.
fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Turn the staged tree into a deployment for these exact paths: select the
/// platform hook map, write the generated paths files, and replace the legacy
/// endpoint literal everywhere it appears.
fn render(root: &Path, plan: &DeploymentPlan) -> Result<(), PluginBundleError> {
    select_platform_hooks(root)?;
    write_file(&root.join(PATHS_SH), paths_sh(plan).as_bytes())?;
    write_file(&root.join(PATHS_PS1), paths_ps1(plan).as_bytes())?;
    render_endpoint(root, plan.endpoint.url())?;
    set_executable_modes(root)
}

/// One hook map ships per platform. The other is removed rather than left for a
/// reader to wonder about.
fn select_platform_hooks(root: &Path) -> Result<(), PluginBundleError> {
    let hooks = root.join("plugin/hooks/hooks.json");
    let windows = root.join(WINDOWS_HOOKS);
    if cfg!(windows) {
        fs::rename(&windows, &hooks).map_err(|source| PluginBundleError::WriteDeployment {
            path: hooks,
            source,
        })
    } else {
        fs::remove_file(&windows).map_err(|source| PluginBundleError::WriteDeployment {
            path: windows,
            source,
        })
    }
}

/// Replace the legacy endpoint literal in every text file of the deployment.
///
/// Three successive drafts of this change each missed a consumer, so the
/// substitution is total over the tree rather than driven by a hand-written list
/// of files. A file that does not contain the literal is left untouched, and
/// binary files cannot contain it.
fn render_endpoint(root: &Path, endpoint_url: &str) -> Result<(), PluginBundleError> {
    if endpoint_url == DEFAULT_ENDPOINT_URL {
        return Ok(());
    }
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries)?;
    for (_, kind, absolute) in entries {
        if kind != KIND_FILE {
            continue;
        }
        let bytes = fs::read(&absolute).map_err(|source| PluginBundleError::ReadSource {
            path: absolute.clone(),
            source,
        })?;
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if !text.contains(DEFAULT_ENDPOINT_URL) {
            continue;
        }
        let rendered = text.replace(DEFAULT_ENDPOINT_URL, endpoint_url);
        write_file(&absolute, rendered.as_bytes())?;
    }
    Ok(())
}

/// Substitute the endpoint into one file outside the deployment tree, such as an
/// installed statusline copy.
pub fn render_endpoint_in_file(path: &Path, endpoint: &Endpoint) -> Result<(), PluginBundleError> {
    if endpoint.url() == DEFAULT_ENDPOINT_URL {
        return Ok(());
    }
    let bytes = fs::read(path).map_err(|source| PluginBundleError::ReadSource {
        path: path.to_path_buf(),
        source,
    })?;
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(());
    };
    if !text.contains(DEFAULT_ENDPOINT_URL) {
        return Ok(());
    }
    write_file(path, text.replace(DEFAULT_ENDPOINT_URL, endpoint.url()).as_bytes())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), PluginBundleError> {
    fs::write(path, bytes).map_err(|source| PluginBundleError::WriteDeployment {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn set_executable_modes(root: &Path) -> Result<(), PluginBundleError> {
    use std::os::unix::fs::PermissionsExt;

    for relative in ["plugin/statusline.sh", "plugin/hooks/ensure-runtime.sh"] {
        let path = root.join(relative);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|source| {
            PluginBundleError::WriteDeployment { path, source }
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_modes(_root: &Path) -> Result<(), PluginBundleError> {
    Ok(())
}

/// The rendered paths file assigns unconditionally from what init resolved: a
/// deployed tree performs no PATH resolution and honours no pre-existing
/// `APPA_BIN`. Only the checkout's committed development copy falls back to the
/// environment, so `claude --plugin-dir <checkout>` keeps working.
fn paths_sh(plan: &DeploymentPlan) -> String {
    let mut rendered = String::from("# Generated by appa init claude-code. Do not edit.\n");
    for (name, value) in [
        ("APPA_BIN", plan.binary_path.as_path()),
        ("APPA_CONFIG", plan.config_path.as_path()),
        ("APPA_DATA_DIR", plan.data_dir.as_path()),
    ] {
        rendered.push_str(&format!("{name}={}\n", sh_literal(&value.to_string_lossy())));
    }
    rendered.push_str(&format!(
        "APPA_ENDPOINT={}\n",
        sh_literal(plan.endpoint.url())
    ));
    rendered.push_str(&format!(
        "APPA_LISTEN={}\n",
        sh_literal(&plan.endpoint.listen().to_string())
    ));
    rendered.push_str("export APPA_BIN APPA_CONFIG APPA_DATA_DIR APPA_ENDPOINT APPA_LISTEN\n");
    rendered
}

fn paths_ps1(plan: &DeploymentPlan) -> String {
    let mut rendered = String::from("# Generated by appa init claude-code. Do not edit.\n");
    for (name, value) in [
        ("AppaBin", plan.binary_path.as_path()),
        ("AppaConfig", plan.config_path.as_path()),
        ("AppaDataDir", plan.data_dir.as_path()),
    ] {
        rendered.push_str(&format!(
            "${name} = {}\n",
            ps_literal(&value.to_string_lossy())
        ));
    }
    rendered.push_str(&format!(
        "$AppaEndpoint = {}\n",
        ps_literal(plan.endpoint.url())
    ));
    rendered.push_str(&format!(
        "$AppaListen = {}\n",
        ps_literal(&plan.endpoint.listen().to_string())
    ));
    rendered
}

/// Total for any UTF-8 path: single-quoted, with embedded `'` closed, escaped
/// and reopened. Spaces, `$`, backticks, quotes and newlines are all
/// representable, so rendering never refuses a path that got this far.
fn sh_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Total for any UTF-8 path: single-quoted, with embedded `'` doubled.
fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_round_trips_through_hex() {
        let digest = PluginDigest::of(b"appa");
        let text = digest.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(PluginDigest::parse(&text).unwrap(), digest);
    }

    #[test]
    fn digest_rejects_malformed_hex() {
        for value in ["", "abc", &"z".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert!(PluginDigest::parse(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn portable_path_uses_forward_slashes() {
        let relative: PathBuf = ["plugin", "hooks", "hooks.json"].iter().collect();
        assert_eq!(
            portable_relative_path(&relative).unwrap(),
            "plugin/hooks/hooks.json"
        );
    }

    #[test]
    fn portable_path_refuses_traversal_components() {
        assert!(portable_relative_path(Path::new("../escape")).is_err());
    }

    fn sample_tree(root: &Path) {
        for relative in REQUIRED_FILES {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{relative}\n")).unwrap();
        }
        fs::create_dir_all(root.join("batteries")).unwrap();
        fs::write(root.join("batteries/README.md"), "batteries\n").unwrap();
        fs::create_dir_all(root.join("plugin/hooks")).unwrap();
        fs::write(root.join(WINDOWS_HOOKS), "{}\n").unwrap();
        fs::write(
            root.join("plugin/statusline.sh"),
            format!("curl \"${{APPA_RUNTIME_URL:-{DEFAULT_ENDPOINT_URL}}}/status\"\n"),
        )
        .unwrap();
        fs::write(
            root.join("plugin/hooks/ensure-runtime.sh"),
            format!("probe {DEFAULT_ENDPOINT_URL}/health\n"),
        )
        .unwrap();
    }

    fn sample_plan(root: &Path, endpoint: &str) -> DeploymentPlan {
        DeploymentPlan {
            source_digest: canonical_source_digest(root).unwrap(),
            binary_path: PathBuf::from("/data/bin/appa"),
            config_path: PathBuf::from("/config/appa.toml"),
            data_dir: PathBuf::from("/data"),
            endpoint: Endpoint::parse(endpoint).unwrap(),
        }
    }

    fn deploy(source: &Path, deployments: &Path, endpoint: &str) -> Deployment {
        materialize(
            Population::Tree(source),
            deployments,
            Path::new("/data/bin/appa"),
            Path::new("/config/appa.toml"),
            Path::new("/data"),
            &Endpoint::parse(endpoint).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn endpoint_accepts_loopback_and_refuses_everything_else() {
        assert_eq!(
            Endpoint::parse("http://127.0.0.1:8787").unwrap().url(),
            "http://127.0.0.1:8787"
        );
        assert!(Endpoint::parse("http://[::1]:9000").is_ok());
        for rejected in [
            "https://127.0.0.1:8787",
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:8787/mcp",
            "http://localhost:8787",
            "http://10.0.0.1:8787",
            "http://127.0.0.1:0",
            "127.0.0.1:8787",
        ] {
            assert!(Endpoint::parse(rejected).is_err(), "accepted {rejected}");
        }
    }

    #[test]
    fn source_digest_changes_when_a_file_is_edited() {
        let source = tempfile::tempdir().unwrap();
        sample_tree(source.path());
        let before = canonical_source_digest(source.path()).unwrap();
        fs::write(source.path().join("batteries/README.md"), "edited\n").unwrap();
        let after = canonical_source_digest(source.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn deployment_digest_separates_paths_and_endpoints() {
        let source = tempfile::tempdir().unwrap();
        sample_tree(source.path());
        let base = sample_plan(source.path(), DEFAULT_ENDPOINT_URL);
        let baseline = deployment_digest(&base).unwrap();

        let mut other_binary = base.clone();
        other_binary.binary_path = PathBuf::from("/elsewhere/bin/appa");
        assert_ne!(deployment_digest(&other_binary).unwrap(), baseline);

        let mut other_endpoint = base.clone();
        other_endpoint.endpoint = Endpoint::parse("http://127.0.0.1:9999").unwrap();
        assert_ne!(deployment_digest(&other_endpoint).unwrap(), baseline);
    }

    #[test]
    fn materialize_renders_paths_and_endpoint_then_reuses() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        let first = deploy(source.path(), deployments.path(), "http://127.0.0.1:9999");
        assert!(!first.reused);

        let paths = fs::read_to_string(first.root.join(PATHS_SH)).unwrap();
        assert!(paths.contains("APPA_BIN='/data/bin/appa'"));
        assert!(paths.contains("APPA_ENDPOINT='http://127.0.0.1:9999'"));

        // Every consumer carrying the legacy literal is rendered, without a
        // hand-written list of files.
        let statusline = fs::read_to_string(first.root.join("plugin/statusline.sh")).unwrap();
        assert!(statusline.contains("http://127.0.0.1:9999"));
        assert!(!statusline.contains(DEFAULT_ENDPOINT_URL));

        let second = deploy(source.path(), deployments.path(), "http://127.0.0.1:9999");
        assert!(second.reused);
        assert_eq!(second.root, first.root);
    }

    #[test]
    fn editing_the_source_reaches_a_new_deployment() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        let first = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
        fs::write(source.path().join("batteries/README.md"), "edited\n").unwrap();
        let second = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);

        assert_ne!(second.root, first.root);
        assert_eq!(
            fs::read_to_string(second.root.join("batteries/README.md")).unwrap(),
            "edited\n"
        );
    }

    #[test]
    fn a_damaged_deployment_is_quarantined_and_rebuilt() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        let first = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
        fs::remove_file(first.root.join("plugin/.claude-plugin/plugin.json")).unwrap();

        let repaired = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
        assert_eq!(repaired.root, first.root);
        assert!(repaired.root.join("plugin/.claude-plugin/plugin.json").is_file());

        // Nothing was deleted: the damaged tree is still on disk beside it.
        let quarantined: Vec<_> = fs::read_dir(deployments.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".quarantine-"))
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert!(quarantined[0].path().join("tree").is_dir());
    }

    #[test]
    fn a_stale_paths_file_is_repaired_on_rerun() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        let first = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
        fs::write(first.root.join(PATHS_SH), "APPA_BIN='/tmp/hostile'\n").unwrap();

        let repaired = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
        assert!(!repaired.reused);
        assert!(
            fs::read_to_string(repaired.root.join(PATHS_SH))
                .unwrap()
                .contains("APPA_BIN='/data/bin/appa'")
        );
    }

    #[test]
    fn paths_carrying_hostile_characters_render_as_literals() {
        let awkward = "/tmp/a b/it's \"quoted\"/$(touch pwned)/`x`/tab\tnewline\n/appa";
        let quoted = sh_literal(awkward);
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));

        // The shell must read back exactly the bytes init rendered.
        let script = format!("printf '%s' {quoted}");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), awkward);

        assert_eq!(ps_literal("it's"), "'it''s'");
    }

    #[test]
    fn extraction_refuses_traversal_and_specials() {
        assert!(safe_relative(Path::new("../escape")).is_none());
        assert!(safe_relative(Path::new("/absolute")).is_none());
        assert!(safe_relative(Path::new("plugin/../../escape")).is_none());
        assert_eq!(
            safe_relative(Path::new("./plugin/hooks.json")).unwrap(),
            PathBuf::from("plugin/hooks.json")
        );
    }
}
