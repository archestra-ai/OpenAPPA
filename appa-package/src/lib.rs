//! The marketplace's data model: the root manifest that lists packages, the
//! per-package manifest, the canonical identity of a package tree, and the
//! refusals that keep a package distributable.
//!
//! This crate is a leaf: it reads manifests and directories and nothing else.
//! It never depends on the runtime, so a build script can call it.

pub mod tree;

mod digest;
mod manifest;
mod marketplace;
mod names;
mod package;
mod validate;

pub use digest::{TreeDigest, TreeDigestParseError};
pub use manifest::{ManifestError, SCHEMA};
pub use marketplace::{Marketplace, OwnershipError, PackageEntry, check_ownership};
pub use names::{
    CredentialPrefix, Host, NameError, Namespace, NamespaceError, PackageKind, PackageName, RelativePath,
    RelativePathError,
};
pub use package::{Adapter, Battery, ImageName, ImageReference, MANIFEST_FILE, Package, Role};
pub use tree::canonical_tree_digest;
pub use validate::{BINDABLE_KINDS, INCLUDABLE_POLICY_FIELDS, PackageError, validate_package};

/// The runtime protocol an adapter in this workspace speaks. It must track
/// `appa_runtime_api::wire::PROTOCOL`; this crate is a leaf and cannot name it.
pub const PROTOCOL: u32 = 1;
