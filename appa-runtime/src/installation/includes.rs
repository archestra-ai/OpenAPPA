//! The root config's `include` list and `server_aliases` table, as the
//! installer reads them. One spelling names a battery,
//! `batteries/<name>/appa.toml`, and it resolves beside the config, where the
//! install keeps the store. The edits themselves are [`crate::config::edit`]'s;
//! what the installer adds here is the battery spelling and the reading back.

use std::collections::BTreeSet;
use std::path::Path;

use appa_package::PackageName;
use toml_edit::{DocumentMut, Item};

use super::InstallError;
use crate::config::edit::{bound_servers, document};

pub(crate) fn battery_include(name: &PackageName) -> String {
    format!("batteries/{name}/appa.toml")
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

    /// The installer reads back what the editor wrote, and refuses a binding the
    /// loader would refuse.
    #[test]
    fn the_bindings_read_back_are_the_ones_the_editor_wrote() {
        let servers = ["home-github".to_owned(), "lab-github".to_owned()];
        let bound = bind_servers(AUTHORED, "github", &servers).unwrap();
        assert_eq!(batteries(&bound).unwrap().1["github"], servers);
        assert!(batteries("[server_aliases]\ngithub = 'work-github'\n").is_err());
    }
}
