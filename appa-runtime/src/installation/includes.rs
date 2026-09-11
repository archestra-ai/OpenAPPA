//! The root config's `include` list and `server_aliases` table, as the
//! installer edits them. One spelling names a battery,
//! `batteries/<name>/appa.toml`, and it resolves beside the config, where the
//! install keeps the store. The installer owns nothing in the config: a line
//! it wrote and a line the person wrote read the same, and an edit changes
//! exactly the entry it names.

use std::collections::BTreeSet;
use std::path::Path;

use appa_package::{Namespace, PackageName};
use toml_edit::{DocumentMut, Item};

use super::InstallError;

pub(crate) fn battery_include(name: &PackageName) -> String {
    format!("batteries/{name}/appa.toml")
}

fn document(text: &str) -> Result<DocumentMut, InstallError> {
    text.parse()
        .map_err(|error: toml_edit::TomlError| InstallError::Invalid(error.to_string()))
}

fn include_list(document: &mut DocumentMut) -> Result<&mut toml_edit::Array, InstallError> {
    if document.get("include").is_none() {
        // Root keys precede tables; toml_edit places the new key before the
        // authored tables without reserializing them.
        document["include"] = toml_edit::value(toml_edit::Array::new());
    }
    let includes = document["include"]
        .as_array_mut()
        .ok_or_else(|| InstallError::Invalid("include must be an array".into()))?;
    if includes.iter().any(|value| value.as_str().is_none()) {
        return Err(InstallError::Invalid("include entries must be strings".into()));
    }
    Ok(includes)
}

/// The text with `include` in its include list once: unchanged when it is there.
pub(crate) fn add(text: &str, include: &str) -> Result<String, InstallError> {
    let mut document = document(text)?;
    let includes = include_list(&mut document)?;
    if includes.iter().any(|value| value.as_str() == Some(include)) {
        return Ok(text.to_owned());
    }
    includes.push(include);
    Ok(document.to_string())
}

/// The text without `include`: unchanged when it is not there. Every entry
/// spelled exactly so goes; another spelling of the same file is the person's
/// and stays.
pub(crate) fn remove(text: &str, include: &str) -> Result<String, InstallError> {
    let mut document = document(text)?;
    let Some(includes) = document.get_mut("include").and_then(Item::as_array_mut) else {
        return Ok(text.to_owned());
    };
    let before = includes.len();
    includes.retain(|value| value.as_str() != Some(include));
    if includes.len() == before {
        return Ok(text.to_owned());
    }
    if includes.is_empty() {
        document.remove("include");
    }
    Ok(document.to_string())
}

/// The text with `server_aliases.<namespace>` naming `server`.
pub(crate) fn bind_server(text: &str, namespace: &str, server: &str) -> Result<String, InstallError> {
    let mut document = document(text)?;
    if document.get("server_aliases").is_none() {
        document["server_aliases"] = toml_edit::table();
    }
    let aliases = document["server_aliases"]
        .as_table_like_mut()
        .ok_or_else(|| InstallError::Invalid("server_aliases must be a table".into()))?;
    if aliases.get(namespace).and_then(Item::as_str) == Some(server) {
        return Ok(text.to_owned());
    }
    aliases.insert(namespace, toml_edit::value(server));
    Ok(document.to_string())
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
    let document = document(text)?;
    Ok(document
        .get("include")
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml_edit::Value::as_str)
        .filter_map(|entry| crate::batteries::name_from_include(Path::new(entry)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTHORED: &str = "# my policy\n[policy]\nversion = 2\n# keep this order\n[[policy.tool]]\nname = 'Read'\n";

    #[test]
    fn an_include_is_added_once_and_the_authored_text_stays() {
        let github = PackageName::parse("github").unwrap();
        let include = battery_include(&github);
        let added = add(AUTHORED, &include).unwrap();
        assert!(added.contains(AUTHORED));
        assert_eq!(add(&added, &include).unwrap(), added);
        assert_eq!(included(&added).unwrap(), BTreeSet::from(["github".to_owned()]));
        assert_eq!(remove(&added, &include).unwrap(), AUTHORED);
    }

    #[test]
    fn removal_takes_the_spelled_entry_and_leaves_another_spelling_of_it() {
        let text = "include = ['./batteries/github/appa.toml', 'batteries/github/appa.toml', 'other.toml']\n";
        let without = remove(text, "batteries/github/appa.toml").unwrap();
        assert_eq!(without, "include = ['./batteries/github/appa.toml', 'other.toml']\n");
        assert_eq!(remove(&without, "batteries/github/appa.toml").unwrap(), without);
        assert_eq!(
            remove("[policy]\nversion = 2\n", "batteries/github/appa.toml").unwrap(),
            "[policy]\nversion = 2\n"
        );
    }

    #[test]
    fn only_the_store_spelling_names_a_battery() {
        let text = "include = ['.appa/appa.toml/generations/abc/marketplace/batteries/slack/appa.toml', 'shared/policy.toml', './batteries/github/appa.toml']\n";
        assert_eq!(included(text).unwrap(), BTreeSet::from(["github".to_owned()]));
    }

    #[test]
    fn a_server_binding_replaces_the_namespaces_alias_and_unbinding_takes_only_the_named_ones() {
        let bound = bind_server(AUTHORED, "github", "work-github").unwrap();
        assert!(bound.contains(AUTHORED));
        assert_eq!(bind_server(&bound, "github", "work-github").unwrap(), bound);
        let rebound = bind_server(&bound, "github", "home-github").unwrap();
        assert!(rebound.contains("home-github") && !rebound.contains("work-github"));
        let with_slack = bind_server(&rebound, "slack", "team-slack").unwrap();
        let github = Namespace::parse("github").unwrap();
        let unbound = unbind_servers(&with_slack, std::slice::from_ref(&github)).unwrap();
        assert!(!unbound.contains("home-github") && unbound.contains("team-slack"));
        assert_eq!(
            unbind_servers(&unbound, std::slice::from_ref(&github)).unwrap(),
            unbound
        );
        let slack = Namespace::parse("slack").unwrap();
        assert_eq!(
            unbind_servers(&unbound, std::slice::from_ref(&slack)).unwrap(),
            AUTHORED
        );
    }
}
