//! The root marketplace manifest: what the marketplace holds and what each
//! package tree must digest to.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::digest::TreeDigest;
use crate::manifest::{ManifestError, SCHEMA};
use crate::names::{CredentialPrefix, Namespace, PackageKind, PackageName, RelativePath};
use crate::package::{Package, Role};

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

/// Why a set of packages cannot sit in one marketplace together.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OwnershipError {
    #[error("`{first}` and `{second}` both cover the namespace `{namespace}`")]
    SharedNamespace {
        first: PackageName,
        second: PackageName,
        namespace: Namespace,
    },
    #[error("`{first}` reads every credential `{second}` reads, under `{prefix}`")]
    NestedCredentials {
        first: PackageName,
        second: PackageName,
        prefix: CredentialPrefix,
    },
}

/// Every namespace and every credential in a marketplace has one owner.
///
/// One package validates against its own manifest, which is all a single
/// directory can be checked against, and a battery may cover a namespace its
/// own name cannot spell (`claude_ai_Slack`). So nothing inside a package stops
/// a second package from covering the first's namespace, appending permissive
/// contracts to tools the first never declared and routing their arguments
/// through its own Annotator; and nothing stops a second package from naming a
/// credential the first's prefix covers and receiving it at spawn. Both are
/// refused here, where the whole set is visible.
///
/// Adapters carry neither, so this is a rule about batteries.
pub fn check_ownership(packages: &[Package]) -> Result<(), OwnershipError> {
    let batteries: Vec<(&PackageName, &[Namespace])> = packages
        .iter()
        .filter_map(|package| match &package.role {
            Role::Battery(battery) => Some((&package.name, battery.namespaces.as_slice())),
            Role::Adapter(_) => None,
        })
        .collect();

    let mut owner: BTreeMap<&Namespace, &PackageName> = BTreeMap::new();
    for (name, namespaces) in &batteries {
        for namespace in *namespaces {
            if let Some(first) = owner.insert(namespace, name) {
                return Err(OwnershipError::SharedNamespace {
                    first: first.clone(),
                    second: (*name).clone(),
                    namespace: namespace.clone(),
                });
            }
        }
    }

    // A prefix owns its own continuations, so `slack` would read every
    // credential `slack-admin` reads. Names, not prefixes, are compared for
    // identity: two batteries cannot share a name in one marketplace.
    for (first, _) in &batteries {
        let prefix = first.credential_prefix();
        for (second, _) in &batteries {
            if first != second && prefix.owns(second.credential_prefix().as_str()) {
                return Err(OwnershipError::NestedCredentials {
                    first: (*first).clone(),
                    second: (*second).clone(),
                    prefix,
                });
            }
        }
    }
    Ok(())
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

    fn battery(name: &str, namespaces: &[&str]) -> Package {
        let namespaces = namespaces
            .iter()
            .map(|namespace| format!("\"{namespace}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Package::parse(
            &format!(
                "schema = 1\nname = \"{name}\"\ndescription = \"a battery\"\n\n\
                 [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nnamespaces = [{namespaces}]\n"
            ),
            Path::new("appa-package.toml"),
        )
        .expect("the manifest parses")
    }

    fn adapter(name: &str) -> Package {
        Package::parse(
            &format!(
                "schema = 1\nname = \"{name}\"\ndescription = \"an adapter\"\n\n\
                 [adapter]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"d.toml\"\n\
                 plugin_dir = \"plugin\"\nplugin = \"appa-runtime\"\n"
            ),
            Path::new("appa-package.toml"),
        )
        .expect("the manifest parses")
    }

    #[test]
    fn two_batteries_may_not_cover_one_namespace() {
        let together = [battery("github", &["github"]), battery("evil", &["github"])];

        assert!(matches!(
            check_ownership(&together),
            Err(OwnershipError::SharedNamespace { .. })
        ));
        assert!(check_ownership(&[battery("github", &["github"]), battery("evil", &["evil"])]).is_ok());
    }

    /// A battery covers several namespaces, and each of them is its own.
    #[test]
    fn a_second_namespace_is_owned_like_the_first() {
        let slack = battery("slack", &["claude_ai_Slack", "slack"]);

        assert!(check_ownership(&[slack.clone(), battery("grain", &["claude_ai_Grain"])]).is_ok());
        assert!(matches!(
            check_ownership(&[slack, battery("other", &["slack"])]),
            Err(OwnershipError::SharedNamespace { .. })
        ));
    }

    /// One package's credential prefix owns its own continuations, so a
    /// marketplace holding both `slack` and `slack-admin` would let `slack`
    /// read every credential `slack-admin` reads.
    #[test]
    fn no_battery_owns_another_batterys_credentials() {
        assert!(matches!(
            check_ownership(&[battery("slack", &["a"]), battery("slack-admin", &["b"])]),
            Err(OwnershipError::NestedCredentials { .. })
        ));
        assert!(check_ownership(&[battery("slack", &["a"]), battery("slackadmin", &["b"])]).is_ok());
    }

    /// An adapter reads no credentials and covers no namespaces, so it shares
    /// a name with a battery freely — the marketplace ships `claude-code` twice.
    #[test]
    fn an_adapter_owns_nothing_a_battery_could_want() {
        assert!(check_ownership(&[adapter("claude-code"), battery("claude-code", &["claude-code"])]).is_ok());
    }

    #[test]
    fn a_stray_key_is_refused() {
        assert!(matches!(
            manifest(&format!("{}owner = \"someone\"\n", listing())),
            Err(ManifestError::Syntax { .. })
        ));
    }
}
