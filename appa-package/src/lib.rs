//! The marketplace's data model: the root manifest that lists packages, the
//! per-package manifest, the canonical identity of a package tree, and the
//! refusals that keep a package distributable.
//!
//! It shares the protocol vocabulary with `appa-runtime-api`, but never
//! depends on the runtime implementation, so a build script can call it.

pub mod generation;
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
pub use package::{Battery, ImageName, ImageReference, MANIFEST_FILE, Package, Plugin, Role};
pub use tree::canonical_tree_digest;
pub use validate::{BINDABLE_KINDS, INCLUDABLE_POLICY_FIELDS, PackageError, validate_package};

pub use appa_runtime_api::PROTOCOL;
