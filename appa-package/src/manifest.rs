//! What both manifest parsers refuse, and the schema they read.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::digest::TreeDigestParseError;
use crate::names::{Host, NameError, NamespaceError, RelativePathError};

/// The only manifest schema this build reads.
pub const SCHEMA: u32 = 1;

/// Why a manifest is not a manifest. Every variant names the file it read.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    Namespace {
        path: PathBuf,
        #[source]
        source: NamespaceError,
    },
    #[error("{path} declares the namespace `{namespace}` twice")]
    RepeatedNamespace { path: PathBuf, namespace: String },
    #[error("{path} is not valid TOML: {source}")]
    Syntax {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{path} declares schema {found}; this build reads schema {SCHEMA}")]
    Schema { path: PathBuf, found: u32 },
    #[error("{path}: `{field}`: {source}")]
    Name {
        path: PathBuf,
        field: String,
        #[source]
        source: NameError,
    },
    #[error("{path}: `{field}`: {source}")]
    Path {
        path: PathBuf,
        field: String,
        #[source]
        source: RelativePathError,
    },
    #[error("{path}: `{field}`: {source}")]
    Digest {
        path: PathBuf,
        field: String,
        #[source]
        source: TreeDigestParseError,
    },
    #[error("{path}: `{kind}` is not a package kind: a package is an adapter or a battery")]
    Kind { path: PathBuf, kind: String },
    #[error("{path}: `{host}` is not a host: this build serves claude-code and kagent")]
    Host { path: PathBuf, host: String },
    #[error("{path}: packages `{first}` and `{second}` share the path `{shared}`")]
    DuplicatePath {
        path: PathBuf,
        first: String,
        second: String,
        shared: String,
    },
    #[error("{path} declares neither `[battery]` nor `[adapter]`")]
    NoRole { path: PathBuf },
    #[error("{path} declares both `[battery]` and `[adapter]`")]
    BothRoles { path: PathBuf },
    #[error("{path} speaks protocol {found}; this build serves protocol {}", crate::PROTOCOL)]
    Protocol { path: PathBuf, found: u32 },
    #[error("{path}: `{field}` is not a field of a {host} adapter")]
    FieldNotForHost {
        path: PathBuf,
        host: Host,
        field: &'static str,
    },
    #[error("{path}: a {host} adapter must declare `{field}`, and this one does not")]
    MissingField {
        path: PathBuf,
        host: Host,
        field: &'static str,
    },
    #[error("{path}: image `{name}` has no reference")]
    ImageReference { path: PathBuf, name: String },
}
