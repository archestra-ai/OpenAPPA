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

use crate::init::settings::{ps_literal, sh_literal};
pub use crate::plugin_layout::stage_repository;
use appa_package::tree::{
    EntryKind, MAX_ENTRIES, MAX_UNCOMPRESSED_BYTES, TreeDigestError, absorb_field, canonical_tree_digest, walk,
};

// ---------------------------------------------------------------------------
// Debug-only test seams
// ---------------------------------------------------------------------------

/// The one place a debug-only seam reads the environment.
///
/// Test seams for endpoint and acquisition URLs all use this function. `[profile.release]` pins
/// `debug-assertions = false`, so a shipped binary reads no environment here at
/// all. The release workflow proves that on the packaged artifact by feeding it
/// a malformed `APPA_ENDPOINT` and requiring it to be ignored; that single probe
/// stands for these seams only for as long as this is the only gate.
pub(crate) fn debug_override(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        env::var(name).ok()
    } else {
        None
    }
}

/// Files and directories every plugin source must carry, in the marketplace-root
/// shape `plugin_layout::stage_repository` produces. One validator serves
/// both source resolution and the reuse check on an existing deployment.
const REQUIRED_FILES: [&str; 9] = [
    ".claude-plugin/marketplace.json",
    "plugin/.claude-plugin/plugin.json",
    "plugin/hooks/hooks.json",
    // The starter init runs on Unix once the plugin is already in place, and the
    // file `set_executable_modes` marks executable. Missing here is a refusal
    // before anything is mutated; missing there is a plugin Claude points at and
    // a runtime nothing can bring up.
    "plugin/hooks/ensure-runtime.sh",
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
enum TreeShape {
    Source,
    Deployment,
}

#[derive(Debug, Error)]
pub enum PluginBundleError {
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
}

/// The SHA-256 of a plugin artifact: what a binary was built with, and what a
/// fetched or cached archive must hash to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PluginDigest([u8; 32]);

impl PluginDigest {
    fn parse(value: &str) -> Result<Self, PluginBundleError> {
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

    #[cfg(test)]
    fn of(bytes: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes);
        Self(digest.finalize().into())
    }

    fn from_hasher(hasher: Sha256) -> Self {
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

/// What `build.rs` stamped into this binary about the plugin it belongs with.
#[derive(Debug, Clone, Copy)]
struct BuildIdentity<'a> {
    release_digest: Option<PluginDigest>,
    release_ref: Option<&'a str>,
    commit: Option<&'a str>,
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
        Ok(Self {
            release_digest,
            release_ref: option_env!("APPA_RELEASE_REF"),
            commit: option_env!("APPA_BUILD_COMMIT"),
        })
    }
}

/// A local plugin archive this binary accepts as its own.
///
/// Activation keeps the identity check compiled into the binary: a release
/// build accepts only its release archive, by file digest, and a development
/// build accepts an archive of its own staged tree, which materialization
/// verifies by tree digest against the identity stamped at compilation. No
/// environment variable, working directory or mutable ref takes part.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedArchive {
    reference: String,
}

impl VerifiedArchive {
    pub(crate) fn of(path: &Path) -> Result<Self, PluginBundleError> {
        let identity = BuildIdentity::compiled()?;
        let reference = match identity.release_digest {
            Some(expected) => {
                let actual = digest_of_file(path)?;
                if actual != expected {
                    return Err(PluginBundleError::DigestMismatch {
                        url: path.display().to_string(),
                        expected,
                        actual,
                    });
                }
                identity
                    .release_ref
                    .ok_or(PluginBundleError::MissingReleaseRef)?
                    .to_owned()
            }
            None => format!(
                "build {}",
                identity
                    .commit
                    .map(|commit| &commit[..commit.len().min(12)])
                    .unwrap_or("unknown")
            ),
        };
        Ok(Self { reference })
    }

    /// The origin as a receipt names it: the release tag, or the build's commit.
    pub(crate) fn label(&self) -> String {
        format!("appa {} plugin", self.reference)
    }
}

/// Structural validation, applied identically to a freshly extracted archive
/// and to an existing deployment considered for reuse.
///
/// This checks shape, not content. Reuse also compares the complete rendered
/// tree with the freshly verified source.
/// A staged marketplace root has every file a deployment renders from.
pub(crate) fn validate_source_tree(root: &Path) -> Result<(), PluginBundleError> {
    validate_tree(root, TreeShape::Source)
}

fn validate_tree(root: &Path, shape: TreeShape) -> Result<(), PluginBundleError> {
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
pub const DEFAULT_ENDPOINT_URL: &str = crate::runtime_url::DEFAULT_RUNTIME_URL;

impl Endpoint {
    pub fn resolve() -> Result<Self, PluginBundleError> {
        let configured = debug_override("APPA_ENDPOINT");
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

    fn listen(&self) -> SocketAddr {
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

/// The identity of a plugin source that has no release digest of its own:
/// the canonical tree digest `build.rs` bakes in, over the staged tree and
/// before rendering. It is what tells a rebuilt development plugin from the
/// deployment already installed, so a changed tree is never reused as-is.
fn canonical_source_digest(root: &Path) -> Result<PluginDigest, PluginBundleError> {
    Ok(PluginDigest(canonical_tree_digest(root)?))
}

impl From<TreeDigestError> for PluginBundleError {
    fn from(error: TreeDigestError) -> Self {
        match error {
            TreeDigestError::Read { path, source } => Self::ReadSource { path, source },
            TreeDigestError::UnportablePath { path } => Self::UnportablePath { path },
            TreeDigestError::UnsupportedEntry { path } => Self::UnsupportedEntry { path },
            TreeDigestError::Oversized { path, reason } => Self::OversizedSource { path, reason },
        }
    }
}

/// Everything a deployment's identity depends on beyond the source bytes.
#[derive(Debug, Clone)]
struct DeploymentPlan {
    source_digest: PluginDigest,
    binary_path: PathBuf,
    config_path: PathBuf,
    data_dir: PathBuf,
    endpoint: Endpoint,
}

/// The name of the deployment directory: the source identity plus every path and
/// platform detail rendered into it, so a deployment can never be reused for a
/// different binary, config, data directory, endpoint or platform.
fn deployment_digest(plan: &DeploymentPlan) -> Result<PluginDigest, PluginBundleError> {
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

/// The two generated files every deployed shell surface sources, one per
/// platform. Both are written into every deployment, so a deployment made from
/// a POSIX host still carries a current PowerShell one.
const PATHS_SH: &str = "plugin/hooks/appa-paths.sh";
const PATHS_PS1: &str = "plugin/hooks/appa-paths.ps1";

/// The Windows hook map. Materialization renames it over `hooks.json` on
/// Windows and removes it everywhere else, so a deployment carries exactly one.
const WINDOWS_HOOKS: &str = "plugin/hooks/hooks.windows.json";

/// Where a deployment's bytes come from at materialization time.
#[derive(Clone, Copy)]
pub enum Population<'a> {
    /// A staged marketplace root, copied as it is. Tests deploy from one; an
    /// install never does.
    Tree(&'a Path),
    /// A local archive checked against this binary's compiled source tree.
    VerifiedArchive { path: &'a Path, expected: PluginDigest },
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
                // Bound the tree before copying it: a source that accidentally
                // holds a large generated directory should be refused, not
                // duplicated into the deployment store.
                walk(source)?;
                copy_tree(source, &incoming)?
            }
            Population::VerifiedArchive { path: archive, .. } => extract_archive(archive, &incoming)?,
        }
        validate_tree(&incoming, TreeShape::Source)?;
        // After staging, before rendering: the source identity must not depend
        // on the paths about to be rendered into it.
        let source_digest = canonical_source_digest(&incoming)?;
        if let Population::VerifiedArchive { path: root, expected } = population
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
            discard_reservation(&incoming);
            return Err(error);
        }
    };

    if let Err(error) = render(&incoming, &plan) {
        discard_reservation(&incoming);
        return Err(error);
    }
    let published = deployments_dir.join(digest.to_string());
    if published.is_dir() {
        match reusable(&published, &incoming) {
            Ok(()) => {
                discard_reservation(&incoming);
                return Ok(Deployment { root: published });
            }
            Err(reason) => {
                tracing::debug!(path = %published.display(), %reason, "quarantining a damaged deployment");
                quarantine(deployments_dir, &published)?;
            }
        }
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
            discard_reservation(&incoming);
        }
        Err(source) => {
            discard_reservation(&incoming);
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

/// Remove this init's own unpublished reservation. No registered state is
/// lost with it, so a failure is noted rather than reported over the error
/// or the deployment the caller is already returning.
fn discard_reservation(incoming: &Path) {
    if let Err(error) = fs::remove_dir_all(incoming) {
        tracing::warn!(path = %incoming.display(), %error, "cannot remove an unpublished deployment");
    }
}

/// Whether an existing deployment can be reused as-is.
///
/// The source has already been verified and rendered for these exact paths.
/// Compare every entry so edited hooks or policy cannot survive cache reuse.
fn reusable(published: &Path, rendered: &Path) -> Result<(), String> {
    validate_tree(published, TreeShape::Deployment).map_err(|error| error.to_string())?;
    let actual = canonical_tree_digest(published).map_err(|error| error.to_string())?;
    let expected = canonical_tree_digest(rendered).map_err(|error| error.to_string())?;
    if actual != expected {
        return Err("rendered deployment contents changed".into());
    }
    Ok(())
}

/// Move a damaged deployment aside. Nothing is ever deleted, and the
/// destination never pre-exists, so `rename` cannot silently replace a
/// directory. The namespace is deliberately distinct from the
/// `.appa-init-recovery-` prefix init names its rollback source with.
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
pub(crate) fn extract_archive(archive: &Path, destination: &Path) -> Result<(), PluginBundleError> {
    extract_bounded(archive, destination, MAX_UNCOMPRESSED_BYTES, MAX_ENTRIES)
}

pub(crate) fn extract_bundle_archive(archive: &Path, destination: &Path) -> Result<(), PluginBundleError> {
    // A complete marketplace may already use its entry allowance. The bundle
    // adds a separately bounded custom-file snapshot, descriptor, selection,
    // config, artifact files and directories.
    extract_bounded(archive, destination, 512 * 1024 * 1024, MAX_ENTRIES * 2 + 32)
}

fn extract_bounded(
    archive: &Path,
    destination: &Path,
    max_bytes: u64,
    max_entries: usize,
) -> Result<(), PluginBundleError> {
    let file = fs::File::open(archive).map_err(|source| PluginBundleError::ReadSource {
        path: archive.to_path_buf(),
        source,
    })?;
    // Include headers/padding in a second bound, including PAX records the tar
    // reader consumes internally before yielding a regular entry.
    let decoded = std::io::Read::take(
        flate2::read::GzDecoder::new(file),
        max_bytes + (max_entries as u64 * 1024),
    );
    let mut tar = tar::Archive::new(decoded);
    let malformed = |reason: String| PluginBundleError::MalformedArchive {
        path: archive.to_path_buf(),
        reason,
    };
    let entries = tar
        .entries()
        .map_err(|error| malformed(format!("it is not a tar archive: {error}")))?;

    let mut count = 0usize;
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let mut entry = entry.map_err(|error| malformed(format!("unreadable entry: {error}")))?;
        count += 1;
        if count > max_entries {
            return Err(malformed(format!("it holds more than {max_entries} entries")));
        }
        total = total.saturating_add(entry.size());
        if total > max_bytes {
            return Err(malformed(format!("it unpacks to more than {max_bytes} bytes")));
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
            EntryPath::ArchiveRoot if kind == tar::EntryType::Directory => continue,
            EntryPath::ArchiveRoot => return Err(malformed("only a directory may name the archive root".into())),
            EntryPath::Escaping => {
                return Err(malformed(format!("{} escapes the archive root", path.display())));
            }
        };
        let target = destination.join(&relative);
        if !seen.insert(relative) {
            return Err(malformed(format!("{} occurs more than once", path.display())));
        }
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
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(write)?;
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
    for entry in walk(root)? {
        if entry.kind != EntryKind::File {
            continue;
        }
        let absolute = entry.absolute;
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
    let mut rendered = String::from("# Generated by appa plugin install claude-code. Do not edit.\n");
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
    let mut rendered = String::from("# Generated by appa plugin install claude-code. Do not edit.\n");
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

// ---------------------------------------------------------------------------
// Release artifact fetch and cache
// ---------------------------------------------------------------------------

const RELEASE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/releases/download";
const SOURCE_ARCHIVE_BASE_URL: &str = "https://github.com/archestra-ai/OpenAPPA/archive";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
/// The release download base. `APPA_RELEASE_BASE_URL` overrides it in debug
/// builds only, and the install reads it once at the boundary rather than
/// leaving the fetch to consult the environment underneath its caller.
pub(crate) fn release_base_url() -> String {
    debug_override("APPA_RELEASE_BASE_URL").unwrap_or_else(|| RELEASE_BASE_URL.to_owned())
}

/// The immutable GitHub source-archive base. Like the release test seam, the
/// override exists only in debug builds and cannot redirect a shipped binary.
pub(crate) fn source_archive_base_url() -> String {
    debug_override("APPA_SOURCE_ARCHIVE_BASE_URL").unwrap_or_else(|| SOURCE_ARCHIVE_BASE_URL.to_owned())
}

pub(crate) fn single_directory(container: &Path) -> Result<PathBuf, PluginBundleError> {
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
/// The install is the synchronous CLI path and never runs under an existing
/// reactor, so this owns a current-thread runtime for the duration of the fetch.
pub(crate) fn download_bounded(url: &str, destination: &Path, max_bytes: u64) -> Result<(), PluginBundleError> {
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
        let https_only = url.starts_with("https://");
        let client = reqwest::Client::builder()
            .user_agent(concat!("appa/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many artifact redirects");
                }
                if https_only && attempt.url().scheme() != "https" {
                    return attempt.error("an authenticated artifact request cannot redirect to plaintext HTTP");
                }
                attempt.follow()
            }))
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
            && length > max_bytes
        {
            return Err(failed(format!(
                "it declares {length} bytes, more than the {max_bytes} accepted"
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
            if written > max_bytes {
                return Err(failed(format!("it exceeds the {max_bytes} bytes accepted")));
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

    #[test]
    fn edited_policy_and_hooks_do_not_survive_native_cache_reuse() {
        for name in ["batteries/README.md", "plugin/hooks/hook.sh"] {
            let source = tempfile::tempdir().unwrap();
            let deployments = tempfile::tempdir().unwrap();
            sample_tree(source.path());
            let first = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
            let expected = fs::read(first.root.join(name)).unwrap();
            fs::write(first.root.join(name), b"edited cached content").unwrap();
            let repaired = deploy(source.path(), deployments.path(), DEFAULT_ENDPOINT_URL);
            assert_eq!(first.root, repaired.root);
            assert_eq!(fs::read(repaired.root.join(name)).unwrap(), expected);
            assert_eq!(fs::read_dir(deployments.path()).unwrap().count(), 2);
        }
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

    #[test]
    fn build_time_and_runtime_repository_staging_have_one_tree_identity() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let staged = tempfile::tempdir().unwrap();
        stage_repository(repository, staged.path()).unwrap();

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
        stage_repository(repository, &mapped).unwrap();

        assert_eq!(
            canonical_source_digest(&scripted).unwrap(),
            canonical_source_digest(&mapped).unwrap()
        );
        // Exercise the actual shell archive boundary too. BSD tar otherwise
        // serializes macOS extended attributes as extra AppleDouble files.
        let archive = directory.path().join("plugin.tar.gz");
        let status = std::process::Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-C")
            .arg(&scripted)
            .arg("-czf")
            .arg(&archive)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
        let extracted = directory.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        extract_archive(&archive, &extracted).unwrap();
        assert_eq!(
            canonical_source_digest(&mapped).unwrap(),
            canonical_source_digest(&extracted).unwrap()
        );
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

        let expected = canonical_source_digest(source.path()).unwrap();
        let deployment = materialize(
            Population::VerifiedArchive {
                path: &archive,
                expected,
            },
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

    #[test]
    fn deployment_bundles_have_room_for_metadata_beyond_the_package_entry_limit() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("bundle.tar.gz");
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        ));
        for index in 0..=MAX_ENTRIES {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("entry-{index}"), std::io::empty())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        let packages = root.path().join("packages");
        let bundle = root.path().join("bundle");
        fs::create_dir(&packages).unwrap();
        fs::create_dir(&bundle).unwrap();
        assert!(extract_archive(&archive, &packages).is_err());
        extract_bundle_archive(&archive, &bundle).unwrap();
        assert_eq!(fs::read_dir(bundle).unwrap().count(), MAX_ENTRIES + 1);
    }

    #[test]
    fn extraction_refuses_duplicate_paths_without_overwriting_the_first_file() {
        for second in ["file", "./file"] {
            let source = tempfile::tempdir().unwrap();
            let destination = tempfile::tempdir().unwrap();
            let archive = source.path().join("bundle.tar.gz");
            let packed = fs::File::create(&archive).unwrap();
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(packed, flate2::Compression::fast()));
            for (name, bytes) in [("file", b"first".as_slice()), (second, b"second".as_slice())] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, bytes).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();

            assert!(matches!(
                extract_archive(&archive, destination.path()),
                Err(PluginBundleError::MalformedArchive { .. })
            ));
            assert_eq!(fs::read(destination.path().join("file")).unwrap(), b"first");
        }
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
