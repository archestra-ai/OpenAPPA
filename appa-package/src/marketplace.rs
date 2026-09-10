//! The root marketplace manifest: what the marketplace holds and what each
//! package tree must digest to.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::digest::TreeDigest;
use crate::manifest::{ManifestError, SCHEMA};
use crate::names::{CredentialPrefix, Host, Namespace, PackageKind, PackageName, RelativePath};
use crate::package::{Battery, Package, Role};

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
    #[error("two batteries are named `{name}`")]
    DuplicateName { name: PackageName },
    #[error("`{first}` and `{second}` both cover the namespace `{namespace}`")]
    SharedNamespace {
        first: PackageName,
        second: PackageName,
        namespace: Namespace,
    },
    #[error("`{first}` and `{second}` both bind the audience source `{provider}`")]
    SharedAudienceProvider {
        first: PackageName,
        second: PackageName,
        provider: String,
    },
    #[error("`{first}` reads every credential `{second}` reads, under `{prefix}`")]
    NestedCredentials {
        first: PackageName,
        second: PackageName,
        prefix: CredentialPrefix,
    },
    #[error("plugin `{plugin}` includes the battery `{battery}`, which this marketplace does not carry")]
    UnknownBattery { plugin: PackageName, battery: PackageName },
    #[error("plugin `{plugin}` includes the battery `{battery}`, which is not written for the host `{host}`")]
    BatteryHost {
        plugin: PackageName,
        battery: PackageName,
        host: Host,
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
/// A plugin's included batteries are checked here too: each is a battery of
/// this marketplace written for the plugin's host, so a first install never
/// selects a package the catalog cannot supply.
pub fn check_ownership(packages: &[Package]) -> Result<(), OwnershipError> {
    let batteries: Vec<(&PackageName, &Battery)> = packages
        .iter()
        .filter_map(|package| match &package.role {
            Role::Battery(battery) => Some((&package.name, battery)),
            Role::Plugin(_) => None,
        })
        .collect();

    // A name is what the other two rules are stated in terms of: one owns a
    // credential prefix, and the other tells two packages apart. Two batteries
    // sharing a name would defeat both, so it is refused before either runs.
    let mut named: BTreeSet<&PackageName> = BTreeSet::new();
    for (name, _) in &batteries {
        if !named.insert(name) {
            return Err(OwnershipError::DuplicateName { name: (*name).clone() });
        }
    }

    let mut owner: BTreeMap<&Namespace, &PackageName> = BTreeMap::new();
    for (name, battery) in &batteries {
        for namespace in &battery.namespaces {
            if let Some(first) = owner.insert(namespace, name) {
                return Err(OwnershipError::SharedNamespace {
                    first: first.clone(),
                    second: (*name).clone(),
                    namespace: namespace.clone(),
                });
            }
        }
    }

    // An audience source answers who may read what, so one provider name has
    // one battery answering for it — flat and exact, as a namespace is owned.
    let mut source_owner: BTreeMap<&str, &PackageName> = BTreeMap::new();
    for (name, battery) in &batteries {
        for provider in &battery.audiences {
            if let Some(first) = source_owner.insert(provider.as_str(), name) {
                return Err(OwnershipError::SharedAudienceProvider {
                    first: first.clone(),
                    second: (*name).clone(),
                    provider: provider.clone(),
                });
            }
        }
    }

    // A prefix owns its own continuations, so `slack` would read every
    // credential `slack-admin` reads. Names are compared, not prefixes: a
    // prefix owns itself, and the names are already known distinct.
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

    for package in packages {
        let Role::Plugin(plugin) = &package.role else {
            continue;
        };
        for battery in plugin.batteries() {
            let included = packages.iter().find_map(|candidate| match &candidate.role {
                Role::Battery(included) if candidate.name == *battery => Some(included),
                _ => None,
            });
            match included {
                None => {
                    return Err(OwnershipError::UnknownBattery {
                        plugin: package.name.clone(),
                        battery: battery.clone(),
                    });
                }
                Some(included) if !included.hosts.contains(&plugin.host()) => {
                    return Err(OwnershipError::BatteryHost {
                        plugin: package.name.clone(),
                        battery: battery.clone(),
                        host: plugin.host(),
                    });
                }
                Some(_) => {}
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
             [packages.plugin.claude-code]\npath = \"plugins/claude-code\"\ndigest = \"{DIGEST}\"\n\n\
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
                (PackageKind::Battery, "github", "batteries/github"),
                (PackageKind::Plugin, "claude-code", "plugins/claude-code"),
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
            manifest(&listing().replace("\"batteries/github\"", "\"plugins/claude-code\"")),
            Err(ManifestError::DuplicatePath { .. })
        ));
    }

    fn battery(name: &str, namespaces: &[&str]) -> Package {
        battery_with_audiences(name, namespaces, &[])
    }

    fn battery_with_audiences(name: &str, namespaces: &[&str], audiences: &[&str]) -> Package {
        let quoted = |items: &[&str]| {
            items
                .iter()
                .map(|item| format!("\"{item}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (namespaces, audiences) = (quoted(namespaces), quoted(audiences));
        Package::parse(
            &format!(
                "schema = 1\nname = \"{name}\"\ndescription = \"a battery\"\n\n\
                 [battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nnamespaces = [{namespaces}]\n\
                 audiences = [{audiences}]\n"
            ),
            Path::new("appa-package.toml"),
        )
        .expect("the manifest parses")
    }

    fn plugin(name: &str) -> Package {
        plugin_including(name, &[])
    }

    fn plugin_including(name: &str, batteries: &[&str]) -> Package {
        let batteries = batteries
            .iter()
            .map(|battery| format!("\"{battery}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Package::parse(
            &format!(
                "schema = 1\nname = \"{name}\"\ndescription = \"an plugin\"\n\n\
                 [plugin]\nhost = \"claude-code\"\nprotocol = 1\ndefault_policy = \"d.toml\"\n\
                 batteries = [{batteries}]\n"
            ),
            Path::new("appa-package.toml"),
        )
        .expect("the manifest parses")
    }

    /// A first install selects the batteries a plugin includes, so each must be
    /// a battery of this marketplace written for the plugin's host.
    #[test]
    fn a_plugin_includes_only_batteries_this_marketplace_carries_for_its_host() {
        let plugin = plugin_including("claude-code", &["claude-code"]);
        assert!(check_ownership(&[plugin.clone(), battery("claude-code", &["claude-code"])]).is_ok());
        assert!(matches!(
            check_ownership(std::slice::from_ref(&plugin)),
            Err(OwnershipError::UnknownBattery { .. })
        ));
        let kagent_only = Package::parse(
            "schema = 1\nname = \"claude-code\"\ndescription = \"a battery\"\n\n\
             [battery]\npolicy = \"appa.toml\"\nhosts = [\"kagent\"]\n",
            Path::new("appa-package.toml"),
        )
        .expect("the manifest parses");
        assert!(matches!(
            check_ownership(&[plugin, kagent_only]),
            Err(OwnershipError::BatteryHost { .. })
        ));
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

    /// An audience source answers who may read what, so one provider name has
    /// one battery behind it — a flat, exact match, as a namespace is owned.
    #[test]
    fn two_batteries_may_not_bind_one_audience_source() {
        let slack = battery_with_audiences("slack", &["claude_ai_Slack"], &["slack"]);

        assert!(
            check_ownership(&[
                slack.clone(),
                battery_with_audiences("github", &["github"], &["github"])
            ])
            .is_ok()
        );
        assert!(matches!(
            check_ownership(&[slack, battery_with_audiences("other", &["other"], &["slack"])]),
            Err(OwnershipError::SharedAudienceProvider { .. })
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

    /// An plugin reads no credentials and covers no namespaces, so it shares
    /// a name with a battery freely — the marketplace ships `claude-code` twice.
    #[test]
    fn an_adapter_owns_nothing_a_battery_could_want() {
        assert!(check_ownership(&[plugin("claude-code"), battery("claude-code", &["claude-code"])]).is_ok());
    }

    /// Two batteries under one name own each other's credentials, and the two
    /// rules below cannot see it: their namespaces may be disjoint, and a name
    /// is what tells one package from another.
    #[test]
    fn two_batteries_may_not_share_a_name() {
        assert!(matches!(
            check_ownership(&[battery("slack", &["a"]), battery("slack", &["b"])]),
            Err(OwnershipError::DuplicateName { .. })
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
