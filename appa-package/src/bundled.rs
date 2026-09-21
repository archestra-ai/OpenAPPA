//! The batteries this build of the repository ships, embedded at compile
//! time by `build.rs` from `marketplace/batteries`. A host that links the
//! crate from a pinned checkout installs from here: the same trees the
//! marketplace lists, at the version the host built against, with no
//! download and no directory to keep.

use std::path::Path;

use crate::{ManifestError, Package};

/// One file of a bundled battery: its path relative to the battery root, `/`
/// separated, and its text.
#[derive(Debug, Clone, Copy)]
pub struct BundledFile {
    pub path: &'static str,
    pub text: &'static str,
}

/// One battery of the repository's tree: its directory name, which is also the
/// package name its manifest declares, and every file under it.
#[derive(Debug, Clone, Copy)]
pub struct BundledBattery {
    pub name: &'static str,
    pub files: &'static [BundledFile],
}

impl BundledBattery {
    /// The text of one file of this battery, by its path under the battery root.
    pub fn file(&self, path: &str) -> Option<&'static str> {
        self.files.iter().find(|file| file.path == path).map(|file| file.text)
    }

    /// The battery's manifest, read the way the marketplace reads it.
    pub fn manifest(&self) -> Result<Package, ManifestError> {
        let path = Path::new(self.name).join(crate::MANIFEST_FILE);
        let text = self.file(crate::MANIFEST_FILE).ok_or_else(|| ManifestError::Read {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no manifest was bundled"),
        })?;
        Package::parse(text, &path)
    }
}

/// Every battery of the repository's tree, in name order.
pub fn bundled_batteries() -> &'static [BundledBattery] {
    BUNDLED
}

const BUNDLED: &[BundledBattery] = include!(concat!(env!("OUT_DIR"), "/bundled_batteries.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;

    #[test]
    fn every_bundled_battery_carries_its_manifest_policy_and_helpers() {
        assert!(!bundled_batteries().is_empty());
        for battery in bundled_batteries() {
            let package = battery
                .manifest()
                .unwrap_or_else(|error| panic!("{}: {error}", battery.name));
            assert_eq!(package.name.as_str(), battery.name);
            let Role::Battery(declared) = &package.role else {
                panic!("{} is not a battery", battery.name);
            };
            assert!(
                battery.file(declared.policy.as_str()).is_some(),
                "{}: policy",
                battery.name
            );
            for helper in &declared.helpers {
                assert!(
                    battery.file(helper.as_str()).is_some(),
                    "{}: {}",
                    battery.name,
                    helper.as_str()
                );
            }
        }
    }

    #[test]
    fn a_bundled_battery_is_read_the_way_the_marketplace_reads_it() {
        let github = bundled_batteries()
            .iter()
            .find(|battery| battery.name == "github")
            .expect("the github battery ships");
        let package = github.manifest().expect("its manifest parses");
        assert!(!package.battery().expect("a battery").hosts.is_empty());
        assert!(github.file("appa.toml").expect("its policy").contains("[policy]"));
    }
}
