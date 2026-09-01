//! The Claude plugin bundle belonging to this binary's own release.
//!
//! A release binary is built knowing its release tag and the SHA-256 of that
//! tag's plugin artifact. A clean source build knows its Git commit and the
//! canonical SHA-256 of the plugin tree built beside it. In both cases init
//! resolves immutable remote bytes automatically and verifies them before any
//! installed state changes. A dirty source build uses the exact local checkout
//! only while it still matches the tree digest baked at compile time.

use std::env;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Files and directories every plugin source must carry, in the marketplace-root
/// shape `scripts/appa-stage-plugin-bundle.sh` produces. One validator serves
/// both source resolution and the reuse check on an existing deployment.
const REQUIRED_FILES: [&str; 8] = [
    ".claude-plugin/marketplace.json",
    "plugin/.claude-plugin/plugin.json",
    "plugin/hooks/hooks.json",
    // Both hook maps register a wrapper script rather than a command line, so a
    // tree carrying the map without its script registers hooks that cannot run.
    // Materialization keeps both scripts; only the inactive map is removed.
    "plugin/hooks/hook.sh",
    "plugin/hooks/hook.ps1",
    // Both statuslines, for the same reason and one more: init copies the one
    // for its platform after the plugin has already been replaced, and Windows
    // reaches that step without having touched the file at all. Missing here is
    // a refusal before anything is mutated; missing there is a half-upgrade.
    "plugin/statusline.sh",
    "plugin/statusline.ps1",
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
    #[error("this appa build has no usable plugin source identity; rebuild it from a Git checkout or a release tag")]
    MissingBuildIdentity,
    #[error("this appa build carries a release plugin digest but no release tag")]
    MissingReleaseRef,
    #[error("this appa build carries an invalid plugin tree digest: {value}")]
    MalformedBuildDigest { value: String },
    #[error("{value} is not a SHA-256 digest")]
    MalformedDigest { value: String },
    #[error("the plugin source at {path} is not a marketplace root: {reason}")]
    InvalidSource { path: PathBuf, reason: String },
    #[error("cannot read the plugin source at {path}: {source}")]
    ReadSource { path: PathBuf, source: std::io::Error },
    #[error("{path} is not valid UTF-8 with `/` separators; rename it and retry")]
    UnportablePath { path: PathBuf },
    #[error("{path} is neither a regular file nor a directory")]
    UnsupportedEntry { path: PathBuf },
    #[error("the plugin source at {path} is too large to deploy: {reason}")]
    OversizedSource { path: PathBuf, reason: String },
    #[error("{value} is not a usable runtime endpoint: {reason}")]
    MalformedEndpoint { value: String, reason: String },
    #[error("cannot write the deployment at {path}: {source}")]
    WriteDeployment { path: PathBuf, source: std::io::Error },
    #[error("the plugin archive at {path} is unusable: {reason}")]
    MalformedArchive { path: PathBuf, reason: String },
    #[error("cannot reserve a working directory under {path}")]
    NoReservation { path: PathBuf },
    #[error("cannot fetch the plugin artifact from {url}: {reason}")]
    Fetch { url: String, reason: String },
    #[error("the plugin artifact at {url} is not the one this build accepts: expected {expected}, got {actual}")]
    DigestMismatch {
        url: String,
        expected: PluginDigest,
        actual: PluginDigest,
    },
    #[error("the plugin at {origin} is not the twin of this appa build: expected tree {expected}, got {actual}")]
    SourceDigestMismatch {
        origin: String,
        expected: PluginDigest,
        actual: PluginDigest,
    },
    #[error("cannot stage the plugin from the OpenAPPA source at {path}: {reason}")]
    StageRepository { path: PathBuf, reason: String },
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
        // The length is already 64, so the remainder is empty by construction.
        let (pairs, _) = trimmed.as_bytes().as_chunks::<2>();
        for (slot, pair) in bytes.iter_mut().zip(pairs) {
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

#[derive(Debug, Clone, Copy)]
struct BuildIdentity<'a> {
    pub release_digest: Option<PluginDigest>,
    pub release_ref: Option<&'a str>,
    pub commit: Option<&'a str>,
    pub tree_digest: Option<PluginDigest>,
    pub local_root: Option<&'a str>,
    pub source_kind: Option<&'a str>,
}

impl BuildIdentity<'static> {
    fn compiled() -> Result<Self, PluginBundleError> {
        // Both digests are compile-time constants. Runtime environment changes
        // cannot redirect a shipped binary to different plugin bytes.
        let release_digest = option_env!("APPA_PLUGIN_SHA256")
            .map(PluginDigest::parse)
            .transpose()
            .map_err(|_| PluginBundleError::MalformedBuildDigest {
                value: option_env!("APPA_PLUGIN_SHA256").unwrap_or_default().to_owned(),
            })?;
        let raw_tree = option_env!("APPA_PLUGIN_TREE_SHA256").ok_or(PluginBundleError::MissingBuildIdentity)?;
        let tree_digest = PluginDigest::parse(raw_tree).map_err(|_| PluginBundleError::MalformedBuildDigest {
            value: raw_tree.to_owned(),
        })?;
        Ok(Self {
            release_digest,
            release_ref: option_env!("APPA_RELEASE_REF"),
            commit: option_env!("APPA_BUILD_COMMIT"),
            tree_digest: Some(tree_digest),
            local_root: option_env!("APPA_PLUGIN_SOURCE_ROOT"),
            source_kind: option_env!("APPA_PLUGIN_SOURCE_KIND"),
        })
    }
}

/// Where a deployment's bytes come from. `Explicit` is the semi-hidden
/// development override; normal init resolves the identity baked into this
/// binary without consulting PATH, the working directory, or mutable refs.
#[derive(Debug, Clone)]
pub enum PluginSource {
    Explicit(PathBuf),
    Release { reference: String, digest: PluginDigest },
    Commit { commit: String, digest: PluginDigest },
    Local { root: PathBuf, digest: PluginDigest },
}

impl PluginSource {
    /// `--plugin-source` when given, otherwise this build's immutable twin.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, PluginBundleError> {
        Self::decide(explicit, BuildIdentity::compiled()?)
    }

    /// The decision itself with metadata supplied explicitly for unit tests.
    fn decide(explicit: Option<&str>, build: BuildIdentity<'_>) -> Result<Self, PluginBundleError> {
        if let Some(path) = explicit {
            return Ok(Self::Explicit(canonical_source(Path::new(path))?));
        }
        if let Some(digest) = build.release_digest {
            let reference = build.release_ref.ok_or(PluginBundleError::MissingReleaseRef)?;
            return Ok(Self::Release {
                reference: reference.to_owned(),
                digest,
            });
        }
        let digest = build.tree_digest.ok_or(PluginBundleError::MissingBuildIdentity)?;
        match build.source_kind {
            Some("commit") => {
                let commit = build.commit.ok_or(PluginBundleError::MissingBuildIdentity)?;
                Ok(Self::Commit {
                    commit: commit.to_owned(),
                    digest,
                })
            }
            Some("local") => {
                let root = build.local_root.ok_or(PluginBundleError::MissingBuildIdentity)?;
                Ok(Self::Local {
                    root: PathBuf::from(root),
                    digest,
                })
            }
            _ => Err(PluginBundleError::MissingBuildIdentity),
        }
    }
}

/// Resolve the developer's `--plugin-source` override like any other path argument.
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

/// Structural validation, applied identically to a `--plugin-source` tree, a
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
        let listen: SocketAddr = authority
            .parse()
            .map_err(|_| malformed("it must be a literal address and port, such as 127.0.0.1:8787"))?;
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
/// file in a `--plugin-source` tree and re-running init would reuse the
/// existing deployment and never reach Claude.
pub fn canonical_source_digest(root: &Path) -> Result<PluginDigest, PluginBundleError> {
    let entries = walk(root)?;

    let mut hasher = Sha256::new();
    for (relative, kind, absolute) in entries {
        absorb_field(&mut hasher, relative.as_bytes());
        hasher.update([kind]);
        if kind == KIND_DIRECTORY {
            absorb_field(&mut hasher, &[]);
        } else {
            absorb_file(&mut hasher, &absolute)?;
        }
    }
    Ok(PluginDigest::from_hasher(hasher))
}

/// The tree in canonical order, refused if it exceeds the source bounds.
fn walk(root: &Path) -> Result<Vec<StagedEntry>, PluginBundleError> {
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries)?;
    if entries.len() > MAX_ENTRIES {
        return Err(PluginBundleError::OversizedSource {
            path: root.to_path_buf(),
            reason: format!("it holds more than {MAX_ENTRIES} entries"),
        });
    }
    let mut total = 0u64;
    for (_, kind, absolute) in &entries {
        if *kind != KIND_FILE {
            continue;
        }
        let length = fs::metadata(absolute)
            .map_err(|source| PluginBundleError::ReadSource {
                path: absolute.clone(),
                source,
            })?
            .len();
        total = total.saturating_add(length);
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(PluginBundleError::OversizedSource {
                path: root.to_path_buf(),
                reason: format!("it holds more than {MAX_UNCOMPRESSED_BYTES} bytes"),
            });
        }
    }
    // Bytewise on the UTF-8 path bytes, never locale collation.
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    Ok(entries)
}

/// A file's length prefix and then its bytes, streamed.
///
/// The length comes from the file's own metadata and exactly that many bytes are
/// absorbed, so the encoding stays the length-prefixed one the digest is defined
/// as, without holding a whole file in memory.
fn absorb_file(hasher: &mut Sha256, path: &Path) -> Result<(), PluginBundleError> {
    use std::io::Read;

    let read = |source: std::io::Error| PluginBundleError::ReadSource {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::File::open(path).map_err(read)?;
    let length = file.metadata().map_err(read)?.len();
    hasher.update(length.to_be_bytes());

    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let filled = file.read(&mut buffer[..wanted]).map_err(read)?;
        if filled == 0 {
            return Err(PluginBundleError::OversizedSource {
                path: path.to_path_buf(),
                reason: "it changed size while being read".to_owned(),
            });
        }
        hasher.update(&buffer[..filled]);
        remaining -= filled as u64;
    }
    Ok(())
}

fn collect_entries(root: &Path, directory: &Path, entries: &mut Vec<StagedEntry>) -> Result<(), PluginBundleError> {
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
        let relative = absolute
            .strip_prefix(root)
            .map_err(|_| PluginBundleError::UnportablePath { path: absolute.clone() })?;
        let portable = portable_relative_path(relative)?;
        // Symlinks and special files are refused here exactly as in extraction:
        // the bundle is regular files and directories.
        let kind = entry.file_type().map_err(|source| PluginBundleError::ReadSource {
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
    path.to_str().ok_or_else(|| PluginBundleError::UnportablePath {
        path: path.to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// Materialization
// ---------------------------------------------------------------------------

/// Caps on what a plugin source may be, whichever way it arrives. The bundle is
/// a few hundred small text files; these bound a hostile archive or a
/// development checkout that has accumulated a large generated directory,
/// without being tight enough to constrain the real one.
const MAX_ENTRIES: usize = 4096;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// The generated file every deployed shell surface sources.
const PATHS_SH: &str = "plugin/hooks/appa-paths.sh";
const WINDOWS_HOOKS: &str = "plugin/hooks/hooks.windows.json";
const PATHS_PS1: &str = "plugin/hooks/appa-paths.ps1";

/// Where a deployment's bytes come from at materialization time.
#[derive(Clone, Copy)]
pub enum Population<'a> {
    /// A staged `--plugin-source` marketplace root, copied.
    Tree(&'a Path),
    /// The repository that produced a dirty source build, staged through the
    /// same repository-to-marketplace mapping used at compile time.
    Repository { root: &'a Path, expected: PluginDigest },
    /// A verified release archive, extracted.
    Archive(&'a Path),
}

/// A published, immutable deployment directory: what Claude registers.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub root: PathBuf,
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
            Population::Tree(source) => {
                // Bound the tree before copying it: a development source that
                // accidentally holds a large generated directory should be
                // refused, not duplicated into the deployment store.
                walk(source)?;
                copy_tree(source, &incoming)?
            }
            Population::Repository { root, .. } => {
                crate::plugin_layout::stage_repository(root, &incoming).map_err(|error| {
                    PluginBundleError::StageRepository {
                        path: root.to_path_buf(),
                        reason: error.to_string(),
                    }
                })?;
            }
            Population::Archive(archive) => extract_archive(archive, &incoming)?,
        }
        validate_tree(&incoming, TreeShape::Source)?;
        // After staging, before rendering: the source identity must not depend
        // on the paths about to be rendered into it.
        let source_digest = canonical_source_digest(&incoming)?;
        if let Population::Repository { root, expected } = population
            && source_digest != expected
        {
            return Err(PluginBundleError::SourceDigestMismatch {
                origin: root.display().to_string(),
                expected,
                actual: source_digest,
            });
        }
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
                return Ok(Deployment { root: published });
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
    })
}

/// Whether an existing deployment can be reused as-is.
///
/// Structural validation plus a byte comparison of both generated files. Both
/// are checked on every platform, not just the one whose hooks are active here:
/// a deployment is a single artifact, and a stale PowerShell paths file would
/// otherwise survive every rerun performed from a POSIX host. This is
/// deliberately not a content hash of every file: a tree whose `batteries/`
/// contents were edited in place is not detected, and init's convergence claim
/// is scoped to match.
fn reusable(published: &Path, plan: &DeploymentPlan) -> Result<(), String> {
    validate_tree(published, TreeShape::Deployment).map_err(|error| error.to_string())?;
    for (name, expected) in [(PATHS_SH, paths_sh(plan)), (PATHS_PS1, paths_ps1(plan))] {
        let current = fs::read(published.join(name)).map_err(|error| format!("{name} is unreadable: {error}"))?;
        if current != expected.into_bytes() {
            return Err(format!("{name} is stale"));
        }
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
    fs::rename(published, container.join("tree")).map_err(|source| PluginBundleError::WriteDeployment {
        path: container.join("tree"),
        source,
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
        let kind = entry.file_type().map_err(|error| PluginBundleError::ReadSource {
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

        let kind = entry.header().entry_type();
        // GitHub source archives may contain POSIX PAX metadata records. They
        // describe following entries and do not materialize in the plugin
        // tree; the following entry's resolved path is still checked below.
        if matches!(kind, tar::EntryType::XHeader | tar::EntryType::XGlobalHeader) {
            continue;
        }

        let path = entry
            .path()
            .map_err(|error| malformed(format!("unreadable entry path: {error}")))?
            .into_owned();
        let relative = match safe_relative(&path) {
            EntryPath::Relative(relative) => relative,
            // A `./` root entry carries no content of its own.
            EntryPath::ArchiveRoot => continue,
            EntryPath::Escaping => {
                return Err(malformed(format!("{} escapes the archive root", path.display())));
            }
        };
        let target = destination.join(&relative);
        let write = |source: std::io::Error| PluginBundleError::WriteDeployment {
            path: target.clone(),
            source,
        };

        match kind {
            tar::EntryType::Directory => fs::create_dir_all(&target).map_err(write)?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|source| PluginBundleError::WriteDeployment {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                let mut out = fs::File::create(&target).map_err(write)?;
                std::io::copy(&mut entry, &mut out).map_err(|source| PluginBundleError::WriteDeployment {
                    path: target.clone(),
                    source,
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

/// What an archive entry's path denotes.
#[derive(Debug, PartialEq, Eq)]
enum EntryPath {
    Relative(PathBuf),
    /// `.` or `./`: the archive root itself, which carries no content.
    ArchiveRoot,
    /// Absolute, or climbing out with `..`.
    Escaping,
}

fn safe_relative(path: &Path) -> EntryPath {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            _ => return EntryPath::Escaping,
        }
    }
    if relative.as_os_str().is_empty() {
        EntryPath::ArchiveRoot
    } else {
        EntryPath::Relative(relative)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Turn the staged tree into a deployment for these exact paths: select the
/// platform hook map, write the generated paths files, and replace the default
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
        fs::rename(&windows, &hooks).map_err(|source| PluginBundleError::WriteDeployment { path: hooks, source })
    } else {
        fs::remove_file(&windows).map_err(|source| PluginBundleError::WriteDeployment { path: windows, source })
    }
}

/// Replace the default endpoint literal in every text file of the deployment.
///
/// The substitution is total over the tree rather than driven by a list of
/// files, so no consumer of the literal can be missed. A file that does not
/// contain the literal is left untouched, and binary files cannot contain it.
fn render_endpoint(root: &Path, endpoint_url: &str) -> Result<(), PluginBundleError> {
    if endpoint_url == DEFAULT_ENDPOINT_URL {
        return Ok(());
    }
    for (_, kind, absolute) in walk(root)? {
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
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|source| PluginBundleError::WriteDeployment { path, source })?;
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
    rendered.push_str(&format!("APPA_ENDPOINT={}\n", sh_literal(plan.endpoint.url())));
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
        rendered.push_str(&format!("${name} = {}\n", ps_literal(&value.to_string_lossy())));
    }
    rendered.push_str(&format!("$AppaEndpoint = {}\n", ps_literal(plan.endpoint.url())));
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

// ---------------------------------------------------------------------------
// Release artifact fetch and cache
// ---------------------------------------------------------------------------

const RELEASE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/releases/download";
const SOURCE_ARCHIVE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/archive";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
/// The archive is a few hundred KB; this bounds a hostile or wrong response.
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;

fn artifact_name(version: &str) -> String {
    format!("appa-plugin-{version}.tar.gz")
}

/// The cache is content-addressed by name, so an entry can never be the wrong
/// bytes under the right name without the re-hash catching it.
fn cached_archive_path(cache_dir: &Path, version: &str, digest: PluginDigest) -> PathBuf {
    cache_dir.join(format!("appa-plugin-{version}-{digest}.tar.gz"))
}

/// The release download base. `APPA_RELEASE_BASE_URL` overrides it in debug
/// builds only, and init reads it once at the boundary rather than leaving the
/// fetch to consult the environment underneath its caller.
pub fn release_base_url() -> String {
    let configured = if cfg!(debug_assertions) {
        env::var("APPA_RELEASE_BASE_URL").ok()
    } else {
        None
    };
    configured.unwrap_or_else(|| RELEASE_BASE_URL.to_owned())
}

/// The immutable GitHub source-archive base. Like the release test seam, the
/// override exists only in debug builds and cannot redirect a shipped binary.
pub fn source_archive_base_url() -> String {
    let configured = if cfg!(debug_assertions) {
        env::var("APPA_SOURCE_ARCHIVE_BASE_URL").ok()
    } else {
        None
    };
    configured.unwrap_or_else(|| SOURCE_ARCHIVE_BASE_URL.to_owned())
}

/// The verified archive for this build's release, from the cache when it is
/// already there and from the release otherwise.
///
/// Every path ends in the same check against the digest this binary was built
/// with, so the cache cannot be used to bypass a refusal.
pub fn ensure_archive(
    digest: PluginDigest,
    reference: &str,
    version: &str,
    cache_dir: &Path,
    base_url: &str,
) -> Result<PathBuf, PluginBundleError> {
    let cached = cached_archive_path(cache_dir, version, digest);
    if cached.is_file() {
        match digest_of_file(&cached) {
            Ok(actual) if actual == digest => return Ok(cached),
            // A corrupt or truncated cache entry is replaced by a fresh
            // download rather than trusted or reported as fatal.
            Ok(_) | Err(_) => {
                tracing::debug!(path = %cached.display(), "re-fetching a cache entry that no longer matches its digest");
            }
        }
    }

    fs::create_dir_all(cache_dir).map_err(|source| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source,
    })?;

    let url = format!("{base_url}/{reference}/{}", artifact_name(version));
    let incoming = tempfile::NamedTempFile::new_in(cache_dir).map_err(|source| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source,
    })?;
    download(&url, incoming.path())?;

    // The check runs against the temp file. Nothing enters the cache before it
    // passes, so a failed check leaves the cache empty.
    let actual = digest_of_file(incoming.path())?;
    if actual != digest {
        return Err(PluginBundleError::DigestMismatch {
            url,
            expected: digest,
            actual,
        });
    }

    incoming
        .persist(&cached)
        .map_err(|error| PluginBundleError::WriteDeployment {
            path: cached.clone(),
            source: error.error,
        })?;
    Ok(cached)
}

fn commit_archive_name(commit: &str, digest: PluginDigest) -> String {
    format!("appa-plugin-{commit}-{digest}.tar.gz")
}

/// Resolve a clean source build's plugin from its immutable Git commit.
///
/// GitHub's repository archive is transport only. It is staged into the same
/// marketplace shape as a release and checked against the canonical tree
/// digest baked into the binary before entering the cache.
pub fn ensure_commit_archive(
    commit: &str,
    expected: PluginDigest,
    cache_dir: &Path,
    base_url: &str,
) -> Result<PathBuf, PluginBundleError> {
    fs::create_dir_all(cache_dir).map_err(|source| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source,
    })?;
    let cached = cache_dir.join(commit_archive_name(commit, expected));
    if cached.is_file() {
        match bundle_archive_digest(&cached) {
            Ok(actual) if actual == expected => return Ok(cached),
            Ok(_) | Err(_) => {
                tracing::debug!(path = %cached.display(), "re-fetching a commit bundle that no longer matches its tree digest");
            }
        }
    }

    let source = tempfile::NamedTempFile::new_in(cache_dir).map_err(|error| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source: error,
    })?;
    let url = format!("{base_url}/{commit}.tar.gz");
    download(&url, source.path())?;

    let workspace = tempfile::tempdir_in(cache_dir).map_err(|error| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source: error,
    })?;
    let repository_container = workspace.path().join("repository");
    fs::create_dir(&repository_container).map_err(|source| PluginBundleError::WriteDeployment {
        path: repository_container.clone(),
        source,
    })?;
    extract_archive(source.path(), &repository_container)?;
    let repository = single_directory(&repository_container)?;
    let bundle = workspace.path().join("bundle");
    crate::plugin_layout::stage_repository(&repository, &bundle).map_err(|error| {
        PluginBundleError::StageRepository {
            path: repository.clone(),
            reason: error.to_string(),
        }
    })?;
    validate_tree(&bundle, TreeShape::Source)?;
    let actual = canonical_source_digest(&bundle)?;
    if actual != expected {
        return Err(PluginBundleError::SourceDigestMismatch {
            origin: format!("commit {commit} fetched from {url}"),
            expected,
            actual,
        });
    }

    let incoming = tempfile::NamedTempFile::new_in(cache_dir).map_err(|error| PluginBundleError::WriteDeployment {
        path: cache_dir.to_path_buf(),
        source: error,
    })?;
    pack_bundle(&bundle, incoming.path())?;
    incoming
        .persist(&cached)
        .map_err(|error| PluginBundleError::WriteDeployment {
            path: cached.clone(),
            source: error.error,
        })?;
    Ok(cached)
}

fn single_directory(container: &Path) -> Result<PathBuf, PluginBundleError> {
    let mut entries = fs::read_dir(container).map_err(|source| PluginBundleError::ReadSource {
        path: container.to_path_buf(),
        source,
    })?;
    let first = entries
        .next()
        .transpose()
        .map_err(|source| PluginBundleError::ReadSource {
            path: container.to_path_buf(),
            source,
        })?
        .ok_or_else(|| PluginBundleError::MalformedArchive {
            path: container.to_path_buf(),
            reason: "it contains no repository root".to_owned(),
        })?;
    if entries.next().is_some() || !first.path().is_dir() {
        return Err(PluginBundleError::MalformedArchive {
            path: container.to_path_buf(),
            reason: "it does not contain exactly one repository root".to_owned(),
        });
    }
    Ok(first.path())
}

fn pack_bundle(source: &Path, destination: &Path) -> Result<(), PluginBundleError> {
    let file = fs::File::create(destination).map_err(|source| PluginBundleError::WriteDeployment {
        path: destination.to_path_buf(),
        source,
    })?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    archive
        .append_dir_all(".", source)
        .map_err(|source| PluginBundleError::WriteDeployment {
            path: destination.to_path_buf(),
            source,
        })?;
    archive
        .into_inner()
        .and_then(flate2::write::GzEncoder::finish)
        .map_err(|source| PluginBundleError::WriteDeployment {
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn bundle_archive_digest(archive: &Path) -> Result<PluginDigest, PluginBundleError> {
    let staged = tempfile::tempdir().map_err(|source| PluginBundleError::WriteDeployment {
        path: archive.to_path_buf(),
        source,
    })?;
    extract_archive(archive, staged.path())?;
    validate_tree(staged.path(), TreeShape::Source)?;
    canonical_source_digest(staged.path())
}

fn digest_of_file(path: &Path) -> Result<PluginDigest, PluginBundleError> {
    let mut file = fs::File::open(path).map_err(|source| PluginBundleError::ReadSource {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer).map_err(|source| PluginBundleError::ReadSource {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(PluginDigest::from_hasher(hasher))
}

/// Stream the artifact to `destination`, enforcing the size cap as it goes.
///
/// Init is the synchronous CLI path and never runs under an existing reactor, so
/// this owns a current-thread runtime for the duration of the fetch.
fn download(url: &str, destination: &Path) -> Result<(), PluginBundleError> {
    crate::tls::install_crypto_provider();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| PluginBundleError::Fetch {
            url: url.to_owned(),
            reason: format!("cannot start a runtime for the download: {source}"),
        })?;

    runtime.block_on(async {
        let failed = |reason: String| PluginBundleError::Fetch {
            url: url.to_owned(),
            reason,
        };
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(|error| failed(error.to_string()))?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| failed(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(failed(format!("the release responded {status}")));
        }
        if let Some(length) = response.content_length()
            && length > MAX_ARCHIVE_BYTES
        {
            return Err(failed(format!(
                "it declares {length} bytes, more than the {MAX_ARCHIVE_BYTES} accepted"
            )));
        }

        let mut file = fs::File::create(destination).map_err(|source| PluginBundleError::WriteDeployment {
            path: destination.to_path_buf(),
            source,
        })?;
        let mut written = 0u64;
        let mut stream = response;
        while let Some(chunk) = stream.chunk().await.map_err(|error| failed(error.to_string()))? {
            written = written.saturating_add(chunk.len() as u64);
            if written > MAX_ARCHIVE_BYTES {
                return Err(failed(format!("it exceeds the {MAX_ARCHIVE_BYTES} bytes accepted")));
            }
            std::io::Write::write_all(&mut file, &chunk).map_err(|source| PluginBundleError::WriteDeployment {
                path: destination.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    })
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
        assert_eq!(portable_relative_path(&relative).unwrap(), "plugin/hooks/hooks.json");
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

        let paths = fs::read_to_string(first.root.join(PATHS_SH)).unwrap();
        assert!(paths.contains("APPA_BIN='/data/bin/appa'"));
        assert!(paths.contains("APPA_ENDPOINT='http://127.0.0.1:9999'"));

        // Every consumer carrying the default literal is rendered.
        let statusline = fs::read_to_string(first.root.join("plugin/statusline.sh")).unwrap();
        assert!(statusline.contains("http://127.0.0.1:9999"));
        assert!(!statusline.contains(DEFAULT_ENDPOINT_URL));

        // A rerun that changes nothing converges on the same directory and moves
        // nothing aside.
        let second = deploy(source.path(), deployments.path(), "http://127.0.0.1:9999");
        assert_eq!(second.root, first.root);
        assert_eq!(fs::read_dir(deployments.path()).unwrap().count(), 1);
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

    /// Either generated paths file, whichever platform's hooks are active on the
    /// host running init. A deployment is one artifact: a PowerShell paths file
    /// left stale would otherwise survive every rerun performed from POSIX.
    #[test]
    fn a_stale_paths_file_is_repaired_on_rerun() {
        for (name, damaged, restored) in [
            (PATHS_SH, "APPA_BIN='/tmp/hostile'\n", "APPA_BIN='/data/bin/appa'"),
            (PATHS_PS1, "$AppaBin = '/tmp/hostile'\n", "$AppaBin = '/data/bin/appa'"),
        ] {
            let source = tempfile::tempdir().unwrap();
            let deployments = tempfile::tempdir().unwrap();
            sample_tree(source.path());

            let first = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
            fs::write(first.root.join(name), damaged).unwrap();

            let repaired = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
            assert!(
                fs::read_to_string(repaired.root.join(name)).unwrap().contains(restored),
                "{name} was not restored",
            );
        }
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

    /// A one-shot artifact server on an ephemeral loopback port. Serves `body`
    /// to every request until dropped.
    fn serve(body: Vec<u8>) -> (String, std::sync::mpsc::Sender<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for connection in listener.incoming() {
                if stopped.try_recv().is_ok() {
                    return;
                }
                let Ok(mut connection) = connection else {
                    return;
                };
                let mut scratch = [0u8; 2048];
                let _ = connection.read(&mut scratch);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = connection.write_all(header.as_bytes());
                let _ = connection.write_all(&body);
                let _ = connection.flush();
            }
        });
        (base, stop)
    }

    fn repository_archive() -> Vec<u8> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut archive = tar::Builder::new(encoder);

            // GitHub source archives can begin with this POSIX PAX metadata
            // entry. It is transport metadata, not part of the source tree.
            let metadata = b"27 comment=GitHub archive\n";
            let mut header = tar::Header::new_ustar();
            header.set_path("pax_global_header").unwrap();
            header.set_size(metadata.len() as u64);
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_cksum();
            archive.append(&header, metadata.as_slice()).unwrap();

            for (source, _) in crate::plugin_layout::REPOSITORY_MAPPINGS {
                let path = repository.join(source);
                let archived = Path::new("OpenAPPA-test").join(source);
                if path.is_dir() {
                    archive.append_dir_all(archived, path).unwrap();
                } else {
                    archive.append_path_with_name(path, archived).unwrap();
                }
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        bytes
    }

    #[test]
    fn build_time_and_runtime_repository_staging_have_one_tree_identity() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let staged = tempfile::tempdir().unwrap();
        crate::plugin_layout::stage_repository(repository, staged.path()).unwrap();

        let runtime = canonical_source_digest(staged.path()).unwrap();
        let compiled = PluginDigest::parse(env!("APPA_PLUGIN_TREE_SHA256")).unwrap();
        assert_eq!(runtime, compiled);
    }

    #[cfg(unix)]
    #[test]
    fn the_release_script_and_runtime_mapping_stage_identical_trees() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let scripted = directory.path().join("scripted");
        let mapped = directory.path().join("mapped");
        let status = std::process::Command::new("sh")
            .arg(repository.join("scripts/appa-stage-plugin-bundle.sh"))
            .arg(&scripted)
            .status()
            .unwrap();
        assert!(status.success());
        crate::plugin_layout::stage_repository(repository, &mapped).unwrap();

        assert_eq!(
            canonical_source_digest(&scripted).unwrap(),
            canonical_source_digest(&mapped).unwrap()
        );
    }

    #[test]
    fn a_commit_source_archive_is_staged_verified_and_cached() {
        let expected = PluginDigest::parse(env!("APPA_PLUGIN_TREE_SHA256")).unwrap();
        let (base, _stop) = serve(repository_archive());
        let cache = tempfile::tempdir().unwrap();

        let archive = ensure_commit_archive("71b5080", expected, cache.path(), &base).unwrap();

        assert_eq!(bundle_archive_digest(&archive).unwrap(), expected);
        assert!(archive.file_name().unwrap().to_string_lossy().contains("71b5080"));
    }

    #[test]
    fn a_commit_whose_plugin_tree_is_not_the_build_twin_is_refused() {
        let (base, _stop) = serve(repository_archive());
        let cache = tempfile::tempdir().unwrap();
        let expected = PluginDigest::of(b"not this repository tree");

        let refused = ensure_commit_archive("71b5080", expected, cache.path(), &base);

        assert!(matches!(refused, Err(PluginBundleError::SourceDigestMismatch { .. })));
        assert_eq!(fs::read_dir(cache.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_verified_download_lands_in_the_cache() {
        let cache = tempfile::tempdir().unwrap();
        let body = b"an archive".to_vec();
        let digest = PluginDigest::of(&body);
        let (base, _stop) = serve(body);

        let archive = ensure_archive(digest, "v9.9.9", "9.9.9", cache.path(), &base).unwrap();

        assert_eq!(digest_of_file(&archive).unwrap(), digest);
        assert!(
            archive
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&digest.to_string()),
            "the cache is content-addressed by name"
        );
    }

    #[test]
    fn a_wrong_artifact_is_refused_and_leaves_the_cache_empty() {
        let cache = tempfile::tempdir().unwrap();
        let (base, _stop) = serve(b"someone else's bytes".to_vec());
        let expected = PluginDigest::of(b"what this build accepts");

        let refused = ensure_archive(expected, "v9.9.9", "9.9.9", cache.path(), &base);

        assert!(matches!(refused, Err(PluginBundleError::DigestMismatch { .. })));
        let leftovers: Vec<_> = fs::read_dir(cache.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert!(leftovers.is_empty(), "a failed check left {leftovers:?}");
    }

    #[test]
    fn a_cached_artifact_is_used_without_a_release_to_reach() {
        let cache = tempfile::tempdir().unwrap();
        let body = b"an archive".to_vec();
        let digest = PluginDigest::of(&body);
        fs::write(cached_archive_path(cache.path(), "9.9.9", digest), &body).unwrap();

        // Nothing is listening here, so any request would fail.
        let archive = ensure_archive(digest, "v9.9.9", "9.9.9", cache.path(), "http://127.0.0.1:1").unwrap();

        assert_eq!(fs::read(archive).unwrap(), body);
    }

    #[test]
    fn a_corrupt_cache_entry_is_replaced_rather_than_trusted() {
        let cache = tempfile::tempdir().unwrap();
        let body = b"an archive".to_vec();
        let digest = PluginDigest::of(&body);
        let path = cached_archive_path(cache.path(), "9.9.9", digest);
        fs::write(&path, b"corrupted").unwrap();

        let (base, _stop) = serve(body.clone());
        let archive = ensure_archive(digest, "v9.9.9", "9.9.9", cache.path(), &base).unwrap();

        assert_eq!(fs::read(archive).unwrap(), body);
    }

    #[test]
    fn a_clean_source_build_resolves_its_commit_twin() {
        let digest = PluginDigest::of(b"tree");
        let source = PluginSource::decide(
            None,
            BuildIdentity {
                release_digest: None,
                release_ref: None,
                commit: Some("71b5080ad2a49e21493887c5bf71a45c620e924f"),
                tree_digest: Some(digest),
                local_root: None,
                source_kind: Some("commit"),
            },
        )
        .unwrap();
        assert!(
            matches!(source, PluginSource::Commit { commit, digest: actual } if commit == "71b5080ad2a49e21493887c5bf71a45c620e924f" && actual == digest)
        );
    }

    #[test]
    fn the_compiled_binary_always_has_an_automatic_plugin_source() {
        assert!(matches!(
            PluginSource::resolve(None).unwrap(),
            PluginSource::Release { .. } | PluginSource::Commit { .. } | PluginSource::Local { .. }
        ));
    }

    #[test]
    fn a_release_build_resolves_the_stamped_tag_not_the_cargo_version() {
        let release = PluginDigest::of(b"release archive");
        let source = PluginSource::decide(
            None,
            BuildIdentity {
                release_digest: Some(release),
                release_ref: Some("v7.8.9"),
                commit: Some("ignored"),
                tree_digest: Some(PluginDigest::of(b"tree")),
                local_root: None,
                source_kind: Some("release"),
            },
        )
        .unwrap();
        assert!(
            matches!(source, PluginSource::Release { reference, digest } if reference == "v7.8.9" && digest == release)
        );
    }

    #[test]
    fn the_explicit_development_source_wins_over_build_metadata() {
        let source = tempfile::tempdir().unwrap();
        sample_tree(source.path());
        let selected = PluginSource::decide(
            source.path().to_str(),
            BuildIdentity {
                release_digest: None,
                release_ref: None,
                commit: None,
                tree_digest: None,
                local_root: None,
                source_kind: None,
            },
        )
        .unwrap();
        assert!(matches!(selected, PluginSource::Explicit(path) if path == source.path().canonicalize().unwrap()));
    }

    #[test]
    fn an_extracted_archive_materializes() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        let archive = source.path().parent().unwrap().join("bundle.tar.gz");
        let packed = fs::File::create(&archive).unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(packed, flate2::Compression::fast()));
        builder.append_dir_all(".", source.path()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let deployment = materialize(
            Population::Archive(&archive),
            deployments.path(),
            Path::new("/data/bin/appa"),
            Path::new("/config/appa.toml"),
            Path::new("/data"),
            &Endpoint::parse(DEFAULT_ENDPOINT_URL).unwrap(),
        )
        .unwrap();

        assert!(deployment.root.join("plugin/hooks/hooks.json").is_file());
        assert!(deployment.root.join(PATHS_SH).is_file());
        // The platform selection removes the map this platform does not use.
        assert!(!deployment.root.join(WINDOWS_HOOKS).is_file());
        fs::remove_file(&archive).unwrap();
    }

    /// Every required file, refused at validation rather than partway through an
    /// install. The ones init reaches only after the Claude plugin has already
    /// been replaced -- the platform statuslines -- are the reason this covers
    /// the whole list instead of the file some earlier failure happens to reach
    /// first: on Windows nothing touches `statusline.ps1` until that point, and
    /// a refusal there would leave a half-upgraded installation.
    #[test]
    fn a_source_missing_any_required_file_is_refused() {
        for missing in REQUIRED_FILES {
            let source = tempfile::tempdir().unwrap();
            sample_tree(source.path());
            fs::remove_file(source.path().join(missing)).unwrap();

            let refused = validate_tree(source.path(), TreeShape::Source);

            assert!(
                matches!(refused, Err(PluginBundleError::InvalidSource { .. })),
                "a source without {missing} was accepted",
            );
        }
    }

    #[test]
    fn an_oversized_development_source_is_refused_before_it_is_copied() {
        let source = tempfile::tempdir().unwrap();
        let deployments = tempfile::tempdir().unwrap();
        sample_tree(source.path());

        // A generated directory that wandered into the checkout.
        let generated = source.path().join("plugin/generated");
        fs::create_dir_all(&generated).unwrap();
        for index in 0..=MAX_ENTRIES {
            fs::write(generated.join(format!("{index}")), b"x").unwrap();
        }

        let refused = materialize(
            Population::Tree(source.path()),
            deployments.path(),
            Path::new("/data/bin/appa"),
            Path::new("/config/appa.toml"),
            Path::new("/data"),
            &Endpoint::parse(DEFAULT_ENDPOINT_URL).unwrap(),
        );

        assert!(matches!(refused, Err(PluginBundleError::OversizedSource { .. })));
        // Nothing was published, and no half-copied tree was left behind.
        let published: Vec<_> = fs::read_dir(deployments.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert!(published.is_empty(), "a refused source left {published:?}");
    }

    #[test]
    fn extraction_refuses_traversal_and_specials() {
        for escaping in ["../escape", "/absolute", "plugin/../../escape"] {
            assert_eq!(safe_relative(Path::new(escaping)), EntryPath::Escaping);
        }
        assert_eq!(safe_relative(Path::new(".")), EntryPath::ArchiveRoot);
        assert_eq!(
            safe_relative(Path::new("./plugin/hooks.json")),
            EntryPath::Relative(PathBuf::from("plugin/hooks.json"))
        );
    }
}
