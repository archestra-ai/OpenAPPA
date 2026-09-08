//! Ownership of deployment edits is separate from the battery's policy.

use appa_package::{Namespace, PackageName};
use serde::{Deserialize, Serialize};

use super::{InstallError, Selection};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedAlias {
    battery: String,
    namespace: String,
    server: String,
}

impl Selection {
    pub(super) fn owns_battery_aliases(&self, battery: &PackageName) -> bool {
        self.aliases.iter().any(|alias| alias.battery == battery.as_str())
    }

    pub(super) fn validate_owned_aliases(&self, document: &toml_edit::DocumentMut) -> Result<(), InstallError> {
        for alias in &self.aliases {
            if document
                .get("server_aliases")
                .and_then(|item| item.get(&alias.namespace))
                .and_then(toml_edit::Item::as_str)
                != Some(alias.server.as_str())
            {
                return Err(InstallError::Invalid(format!(
                    "owned server alias {} was changed",
                    alias.namespace
                )));
            }
        }
        Ok(())
    }

    pub(super) fn validate_battery_aliases(&self, battery: &str, namespaces: &[Namespace]) -> Result<(), InstallError> {
        for alias in self.aliases.iter().filter(|alias| alias.battery == battery) {
            if !namespaces.iter().any(|namespace| namespace.as_str() == alias.namespace) {
                return Err(InstallError::Invalid(format!(
                    "battery {battery} does not own namespace {}",
                    alias.namespace
                )));
            }
        }
        Ok(())
    }

    pub(super) fn validate_aliases(&self) -> Result<(), InstallError> {
        let mut namespaces = std::collections::BTreeSet::new();
        for alias in &self.aliases {
            if !self.batteries.contains(&alias.battery) || !namespaces.insert(&alias.namespace) {
                return Err(InstallError::Invalid("inconsistent owned server alias ledger".into()));
            }
            namespace(&alias.namespace)?;
            namespace(&alias.server)?;
        }
        Ok(())
    }

    /// Associate a battery namespace with an already configured server. This
    /// neither creates a connection nor adopts a matching user-authored alias.
    pub fn associate_battery(
        &mut self,
        text: &str,
        battery: &PackageName,
        source_namespace: &str,
        server: &str,
    ) -> Result<String, InstallError> {
        namespace(source_namespace)?;
        namespace(server)?;
        if !self.batteries.contains(battery.as_str()) {
            return Err(InstallError::Invalid(
                "select the battery before associating its server".into(),
            ));
        }
        let mut document = parse(text)?;
        let owned = self
            .aliases
            .iter()
            .position(|alias| alias.namespace == source_namespace);
        if let Some(index) = owned {
            let alias = &self.aliases[index];
            if alias.battery != battery.as_str()
                || document
                    .get("server_aliases")
                    .and_then(|item| item.get(source_namespace))
                    .and_then(toml_edit::Item::as_str)
                    != Some(alias.server.as_str())
            {
                return Err(InstallError::Invalid(
                    "the owned server alias was changed; resolve it before installing".into(),
                ));
            }
        }
        if document.get("server_aliases").is_none() {
            document["server_aliases"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let aliases = document["server_aliases"]
            .as_table_like_mut()
            .ok_or_else(|| InstallError::Invalid("server_aliases must be a table".into()))?;
        if let Some(existing) = aliases.get(source_namespace) {
            if existing.as_str() == Some(server) {
                return Ok(text.to_owned());
            }
            if owned.is_none() {
                return Err(InstallError::Invalid(format!(
                    "server_aliases.{source_namespace} is user-owned; edit it explicitly"
                )));
            }
        }
        aliases.insert(source_namespace, toml_edit::value(server));
        match owned {
            Some(index) => self.aliases[index].server = server.to_owned(),
            None => self.aliases.push(OwnedAlias {
                battery: battery.to_string(),
                namespace: source_namespace.to_owned(),
                server: server.to_owned(),
            }),
        }
        Ok(document.to_string())
    }

    /// Validate every entry before editing so a conflict cannot partly mutate
    /// the ledger. User-authored aliases are never removed.
    pub fn remove_battery_aliases(&mut self, text: &str, battery: &PackageName) -> Result<String, InstallError> {
        let owned: Vec<_> = self
            .aliases
            .iter()
            .filter(|alias| alias.battery == battery.as_str())
            .collect();
        if owned.is_empty() {
            return Ok(text.to_owned());
        }
        let mut document = parse(text)?;
        let aliases = document
            .get_mut("server_aliases")
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| InstallError::Invalid("the owned server aliases were changed".into()))?;
        for alias in &owned {
            if aliases.get(&alias.namespace).and_then(toml_edit::Item::as_str) != Some(alias.server.as_str()) {
                return Err(InstallError::Invalid(format!(
                    "server_aliases.{} was changed; resolve it before removing",
                    alias.namespace
                )));
            }
        }
        for alias in owned {
            aliases.remove(&alias.namespace);
        }
        self.aliases.retain(|alias| alias.battery != battery.as_str());
        Ok(document.to_string())
    }
}

fn namespace(text: &str) -> Result<(), InstallError> {
    Namespace::parse(text)
        .map(|_| ())
        .map_err(|error| InstallError::Invalid(error.to_string()))
}

fn parse(text: &str) -> Result<toml_edit::DocumentMut, InstallError> {
    text.parse()
        .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_package::PackageKind;

    #[test]
    fn association_is_idempotent_and_removes_only_owned_aliases() {
        let mut selection = super::super::tests::selection();
        let battery = PackageName::parse("github").unwrap();
        selection.select(PackageKind::Battery, &battery);
        let original = "# keep me\n[server_aliases]\nmanual = 'custom' # and me\n[policy]\nversion = 2\n";
        let added = selection
            .associate_battery(original, &battery, "github", "work-github")
            .unwrap();
        assert_eq!(
            selection
                .associate_battery(&added, &battery, "github", "work-github")
                .unwrap(),
            added
        );
        let changed = selection
            .associate_battery(&added, &battery, "github", "other-github")
            .unwrap();
        assert_eq!(selection.aliases.len(), 1);
        let removed = selection.remove_battery_aliases(&changed, &battery).unwrap();
        assert_eq!(removed, original);
        assert!(selection.aliases.is_empty());
    }

    #[test]
    fn manual_aliases_are_neither_adopted_nor_overwritten() {
        let mut selection = super::super::tests::selection();
        let battery = PackageName::parse("github").unwrap();
        selection.select(PackageKind::Battery, &battery);
        let original = "server_aliases = { github = 'work-github' }\n";
        assert_eq!(
            selection
                .associate_battery(original, &battery, "github", "work-github")
                .unwrap(),
            original
        );
        assert!(selection.aliases.is_empty());
        assert!(
            selection
                .associate_battery(original, &battery, "github", "other")
                .is_err()
        );
        assert_eq!(selection.remove_battery_aliases(original, &battery).unwrap(), original);
    }

    #[test]
    fn changed_owned_alias_refuses_install_and_remove_without_ledger_changes() {
        let mut selection = super::super::tests::selection();
        let battery = PackageName::parse("github").unwrap();
        selection.select(PackageKind::Battery, &battery);
        let added = selection
            .associate_battery("", &battery, "github", "work-github")
            .unwrap();
        let before = selection.clone();
        let edited = added.replace("work-github", "manual");
        assert!(
            selection
                .associate_battery(&edited, &battery, "github", "manual")
                .is_err()
        );
        assert!(selection.remove_battery_aliases(&edited, &battery).is_err());
        assert_eq!(selection, before);
    }
}
