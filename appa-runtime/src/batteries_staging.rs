//! Staging the batteries archive from an OpenAPPA repository checkout.
//!
//! Separate from [`crate::batteries_layout`] because `build.rs` stages the
//! archive in every build, while the library itself needs it only where the
//! installer does.

use std::fs;
use std::io;
use std::path::Path;

/// The host-side code of a protected session is the deployed binary, so the
/// archive carries only the batteries a policy may include: the repository's
/// batteries tree, at the archive's `batteries/`.
pub const SOURCE: &str = "marketplace/batteries";
const TARGET: &str = "batteries";

/// Stage the batteries archive's tree from an OpenAPPA repository checkout.
pub fn stage_repository(repository: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    crate::batteries_layout::copy_entry(&repository.join(SOURCE), &destination.join(TARGET))
}
