//! The root marketplace manifest: what the marketplace holds and what each
//! package tree must digest to.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::digest::TreeDigest;
use crate::manifest::{ManifestError, SCHEMA};
use crate::names::{PackageKind, PackageName, RelativePath};

/// One listed package: where it lives and what its tree must digest to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub kind: PackageKind,
    pub name: PackageName,
    pub path: RelativePath,
    pub digest: TreeDigest,
}

/// The root manifest, in kind then name order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marketplace {
    pub name: PackageName,
    pub packages: Vec<PackageEntry>,
}

impl Marketplace {
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let text = fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    /// `path` names the file the text came from; it appears in every refusal.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ManifestError> {
        let raw: RawMarketplace = toml::from_str(text).map_err(|source| ManifestError::Syntax {
            path: path.to_path_buf(),
            source,
        })?;
        if raw.schema != SCHEMA {
            return Err(ManifestError::Schema {
                path: path.to_path_buf(),
                found: raw.schema,
            });
        }
        let name = PackageName::parse(&raw.name).map_err(|source| ManifestError::Name {
            path: path.to_path_buf(),
            field: "name".to_owned(),
            source,
        })?;

        let mut packages = Vec::new();
        for (kind, listed) in raw.packages {
            let kind = PackageKind::parse(&kind).ok_or(ManifestError::Kind {
                path: path.to_path_buf(),
                kind: kind.clone(),
            })?;
            for (name, entry) in listed {
                let field = format!("packages.{kind}.{name}");
                let name = PackageName::parse(&name).map_err(|source| ManifestError::Name {
                    path: path.to_path_buf(),
                    field: field.clone(),
                    source,
                })?;
                let package_path = RelativePath::parse(&entry.path).map_err(|source| ManifestError::Path {
                    path: path.to_path_buf(),
                    field: format!("{field}.path"),
                    source,
                })?;
                let digest = TreeDigest::parse(&entry.digest).map_err(|source| ManifestError::Digest {
                    path: path.to_path_buf(),
                    field: format!("{field}.digest"),
                    source,
                })?;
                packages.push(PackageEntry {
                    kind,
                    name,
                    path: package_path,
                    digest,
                });
            }
        }

        // One directory is one package: two entries over the same tree would
        // give that tree two identities.
        let mut claimed: BTreeMap<&str, &PackageName> = BTreeMap::new();
        for entry in &packages {
            if let Some(first) = claimed.insert(entry.path.as_str(), &entry.name) {
                return Err(ManifestError::DuplicatePath {
                    path: path.to_path_buf(),
                    first: first.to_string(),
                    second: entry.name.to_string(),
                    shared: entry.path.to_string(),
                });
            }
        }

        Ok(Self { name, packages })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMarketplace {
    schema: u32,
    name: String,
    #[serde(default)]
    packages: BTreeMap<String, BTreeMap<String, RawEntry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    path: String,
    digest: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:2619322b5feb343f951e2ed55ab86948a93d9460eea0fc70d46a20fdb9f87dec";

    fn manifest(body: &str) -> Result<Marketplace, ManifestError> {
        Marketplace::parse(body, Path::new("marketplace/marketplace.toml"))
    }

    fn listing() -> String {
        format!(
            "schema = 1\nname = \"appa\"\n\n\
             [packages.adapter.claude-code]\npath = \"adapters/claude-code\"\ndigest = \"{DIGEST}\"\n\n\
             [packages.battery.github]\npath = \"batteries/github\"\ndigest = \"{DIGEST}\"\n"
        )
    }

    #[test]
    fn a_listing_parses_into_kinds_paths_and_digests() {
        let marketplace = manifest(&listing()).unwrap();

        assert_eq!(marketplace.name.as_str(), "appa");
        assert_eq!(
            marketplace
                .packages
                .iter()
                .map(|entry| (entry.kind, entry.name.as_str(), entry.path.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (PackageKind::Adapter, "claude-code", "adapters/claude-code"),
                (PackageKind::Battery, "github", "batteries/github"),
            ]
        );
        assert_eq!(marketplace.packages[0].digest, TreeDigest::parse(DIGEST).unwrap());
    }

    #[test]
    fn another_schema_is_refused() {
        assert!(matches!(
            manifest(&listing().replace("schema = 1", "schema = 2")),
            Err(ManifestError::Schema { found: 2, .. })
        ));
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        assert!(matches!(
            manifest(&listing().replace("packages.battery.github", "packages.skill.github")),
            Err(ManifestError::Kind { .. })
        ));
    }

    #[test]
    fn a_malformed_name_is_refused() {
        assert!(matches!(
            manifest(&listing().replace("name = \"appa\"", "name = \"APPA\"")),
            Err(ManifestError::Name { .. })
        ));
        assert!(matches!(
            manifest(&listing().replace("packages.battery.github", "packages.battery.GitHub")),
            Err(ManifestError::Name { .. })
        ));
    }

    #[test]
    fn an_escaping_path_is_refused() {
        assert!(matches!(
            manifest(&listing().replace("\"batteries/github\"", "\"../batteries/github\"")),
            Err(ManifestError::Path { .. })
        ));
        assert!(matches!(
            manifest(&listing().replace("\"batteries/github\"", "\"/batteries/github\"")),
            Err(ManifestError::Path { .. })
        ));
    }

    #[test]
    fn a_malformed_digest_is_refused() {
        assert!(matches!(
            manifest(&listing().replace(DIGEST, "sha256:deadbeef")),
            Err(ManifestError::Digest { .. })
        ));
    }

    #[test]
    fn two_packages_may_not_share_a_path() {
        assert!(matches!(
            manifest(&listing().replace("\"batteries/github\"", "\"adapters/claude-code\"")),
            Err(ManifestError::DuplicatePath { .. })
        ));
    }

    #[test]
    fn a_stray_key_is_refused() {
        assert!(matches!(
            manifest(&format!("{}owner = \"someone\"\n", listing())),
            Err(ManifestError::Syntax { .. })
        ));
    }
}
