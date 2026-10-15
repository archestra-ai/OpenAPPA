//! The root's `[aliases]` table: for each tool family, the namespaces a policy's rules
//! name, bound to the identities this deployment reports in that family. An `mcp` alias
//! binds a server namespace to configured connections; a `host` alias binds an included
//! battery's host namespace to the host that serves its tools here.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::ConfigError;

/// Each namespace a rule names, bound to the identities it stands for.
pub(crate) type NamespaceBindings = BTreeMap<String, Vec<String>>;

/// The tool families an alias can rebind. A canonical id `<family>/<namespace>/<tool>`
/// is rebound only by its own family's aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasFamily {
    Mcp,
    Host,
}

impl AliasFamily {
    pub const ALL: [AliasFamily; 2] = [AliasFamily::Mcp, AliasFamily::Host];

    pub fn kind(self) -> &'static str {
        match self {
            AliasFamily::Mcp => "mcp",
            AliasFamily::Host => "host",
        }
    }

    pub fn of_kind(kind: &str) -> Option<AliasFamily> {
        AliasFamily::ALL.into_iter().find(|family| family.kind() == kind)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Aliases {
    pub(crate) mcp: NamespaceBindings,
    pub(crate) host: NamespaceBindings,
}

impl Aliases {
    pub(crate) fn of(&self, family: AliasFamily) -> &NamespaceBindings {
        match family {
            AliasFamily::Mcp => &self.mcp,
            AliasFamily::Host => &self.host,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAliases {
    #[serde(default)]
    mcp: NamespaceBindings,
    #[serde(default)]
    host: NamespaceBindings,
    tool: Option<toml::Value>,
}

/// The aliases a document declares under either spelling: `[aliases]`, or a top-level
/// `[server_aliases]` table that stands for `aliases.mcp`. A document with both is refused.
pub(super) fn admit(aliases: Option<RawAliases>, legacy: Option<NamespaceBindings>) -> Result<Aliases, ConfigError> {
    match (aliases, legacy) {
        (Some(_), Some(_)) => Err(ConfigError::MixedAliasSpellings),
        (Some(RawAliases { tool: Some(_), .. }), None) => Err(ConfigError::UnsupportedToolAliases),
        (Some(RawAliases { mcp, host, tool: None }), None) => Ok(Aliases { mcp, host }),
        (None, Some(mcp)) => Ok(Aliases {
            mcp,
            host: NamespaceBindings::new(),
        }),
        (None, None) => Ok(Aliases::default()),
    }
}

/// The aliases of a stored or composed document, read the way the loader reads them.
pub(crate) fn of_document(document: &toml::Value) -> Result<Aliases, ConfigError> {
    #[derive(Deserialize)]
    struct Spellings {
        aliases: Option<RawAliases>,
        server_aliases: Option<NamespaceBindings>,
    }
    let tables: toml::Table = ["aliases", "server_aliases"]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), document.get(key)?.clone())))
        .collect();
    let spellings: Spellings = toml::Value::Table(tables)
        .try_into()
        .map_err(|source| ConfigError::UnparsablePolicy { source })?;
    admit(spellings.aliases, spellings.server_aliases)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Aliases, ConfigError> {
        of_document(&toml::from_str(text).unwrap())
    }

    #[test]
    fn either_spelling_binds_mcp_namespaces_and_both_together_are_refused() {
        let github = Aliases {
            mcp: BTreeMap::from([("github".to_owned(), vec!["work".to_owned()])]),
            host: NamespaceBindings::new(),
        };
        assert_eq!(read("[aliases]\nmcp = { github = ['work'] }\n").unwrap(), github);
        assert_eq!(read("[server_aliases]\ngithub = ['work']\n").unwrap(), github);
        assert_eq!(read("[policy]\nversion = 2\n").unwrap(), Aliases::default());
        for both in [
            "[aliases]\n[server_aliases]\n",
            "[aliases]\nmcp = { github = ['work'] }\n[server_aliases]\n",
            "[aliases]\n[server_aliases]\ngithub = ['work']\n",
        ] {
            assert!(matches!(read(both), Err(ConfigError::MixedAliasSpellings)), "{both}");
        }
    }

    #[test]
    fn host_aliases_are_their_own_family_and_tool_aliases_are_refused() {
        let aliases = read("[aliases]\nhost = { claude-code = ['archestra'] }\n").unwrap();
        assert!(aliases.of(AliasFamily::Mcp).is_empty());
        assert_eq!(aliases.of(AliasFamily::Host)["claude-code"], vec!["archestra"]);
        assert!(matches!(
            read("[aliases]\ntool = { Read = ['read_file'] }\n"),
            Err(ConfigError::UnsupportedToolAliases)
        ));
        assert!(matches!(
            read("[aliases]\nagent = {}\n"),
            Err(ConfigError::UnparsablePolicy { .. })
        ));
    }
}
