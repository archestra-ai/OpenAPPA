//! The root config's `include` list and `server_aliases` table, as the
//! installer reads them. One spelling names a battery,
//! `batteries/<name>/appa.toml`, and it resolves beside the config, where the
//! install keeps the store. The edits themselves are [`crate::config::edit`]'s;
//! what the installer adds here is the battery spelling, the reading back, and
//! the uninstall, which takes the aliases of a battery that goes.

use std::collections::BTreeSet;
use std::path::Path;

use appa_package::{Namespace, PackageName};
use toml_edit::{DocumentMut, Item};

use super::InstallError;
use crate::config::edit::bound_servers;

pub(crate) fn battery_include(name: &PackageName) -> String {
    format!("batteries/{name}/appa.toml")
}

fn document(text: &str) -> Result<DocumentMut, InstallError> {
    text.parse()
        .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))
}

/// The text without the aliases of `namespaces`: a battery's aliases mean
/// nothing without the battery.
pub(crate) fn unbind_servers(text: &str, namespaces: &[Namespace]) -> Result<String, InstallError> {
    let mut document = document(text)?;
    let Some(aliases) = document.get_mut("server_aliases").and_then(Item::as_table_like_mut) else {
        return Ok(text.to_owned());
    };
    let mut changed = false;
    for namespace in namespaces {
        changed |= aliases.remove(namespace.as_str()).is_some();
    }
    if !changed {
        return Ok(text.to_owned());
    }
    if aliases.is_empty() {
        document.remove("server_aliases");
    }
    Ok(document.to_string())
}

/// The batteries the include list names: the entries spelled
/// `batteries/<name>/appa.toml`.
pub(crate) fn included(text: &str) -> Result<BTreeSet<String>, InstallError> {
    Ok(included_in(&document(text)?))
}

/// The batteries the config includes, and its `server_aliases` table. A
/// binding that is not an array of strings is refused, as the loader refuses it.
pub(crate) fn batteries(text: &str) -> Result<(BTreeSet<String>, crate::config::ServerBindings), InstallError> {
    let document = document(text)?;
    let bindings = document
        .get("server_aliases")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|aliases| aliases.iter())
        .map(|(namespace, bound)| {
            bound_servers(bound)
                .map(|servers| (namespace.to_owned(), servers))
                .ok_or_else(|| InstallError::Invalid(format!("server_aliases.{namespace} must be an array of strings")))
        })
        .collect::<Result<_, _>>()?;
    Ok((included_in(&document), bindings))
}

fn included_in(document: &DocumentMut) -> BTreeSet<String> {
    document
        .get("include")
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml_edit::Value::as_str)
        .filter_map(|entry| crate::batteries::name_from_include(Path::new(entry)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::edit::{add_include, bind_servers};

    const AUTHORED: &str = "# my policy\n[policy]\nversion = 2\n# keep this order\n[[policy.tool]]\nname = 'Read'\n";

    #[test]
    fn an_added_include_is_the_battery_the_config_includes() {
        let github = PackageName::parse("github").unwrap();
        let added = add_include(AUTHORED, &battery_include(&github)).unwrap();
        assert_eq!(included(&added).unwrap(), BTreeSet::from(["github".to_owned()]));
    }

    #[test]
    fn only_the_store_spelling_names_a_battery() {
        let text = "include = ['.appa/appa.toml/generations/abc/marketplace/batteries/slack/appa.toml', 'shared/policy.toml', './batteries/github/appa.toml']\n";
        assert_eq!(included(text).unwrap(), BTreeSet::from(["github".to_owned()]));
    }

    #[test]
    fn unbinding_takes_only_the_named_namespaces_and_the_last_one_takes_the_table() {
        let servers = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        let github = Namespace::parse("github").unwrap();
        let slack = Namespace::parse("slack").unwrap();
        let bound = bind_servers(AUTHORED, github.as_str(), &servers(&["home-github", "lab-github"])).unwrap();
        assert_eq!(
            batteries(&bound).unwrap().1["github"],
            servers(&["home-github", "lab-github"])
        );
        assert!(batteries("[server_aliases]\ngithub = 'work-github'\n").is_err());
        let with_slack = bind_servers(&bound, slack.as_str(), &servers(&["team-slack"])).unwrap();
        let unbound = unbind_servers(&with_slack, std::slice::from_ref(&github)).unwrap();
        assert!(!unbound.contains("home-github") && unbound.contains("team-slack"));
        assert_eq!(
            unbind_servers(&unbound, std::slice::from_ref(&github)).unwrap(),
            unbound
        );
        assert_eq!(
            unbind_servers(&unbound, std::slice::from_ref(&slack)).unwrap(),
            AUTHORED
        );
    }
}
