//! The edits a root document takes from a program rather than a person: the
//! include list, the `server_aliases` bindings and the `[credentials]`
//! declaration. The editor owns nothing in the document — a line it wrote and a
//! line the person wrote read the same, comments and unrelated entries stay as
//! authored, and a document that already says what the edit asks for comes back
//! byte for byte. The CLI installer edits the file this way; a host that keeps
//! its document in a store edits it the same way.

use std::path::Path;

use appa_package::Namespace;
use toml_edit::{DocumentMut, Item};

use super::{ConfigError, refuse_foreign_credential, refuse_include_entry};

pub(crate) fn document(text: &str) -> Result<DocumentMut, ConfigError> {
    text.parse()
        .map_err(|error: toml_edit::TomlError| ConfigError::UneditableDocument {
            reason: error.to_string(),
        })
}

fn include_list(document: &mut DocumentMut) -> Result<&mut toml_edit::Array, ConfigError> {
    if document.get("include").is_none() {
        // Root keys precede tables; toml_edit places the new key before the
        // authored tables without reserializing them.
        document["include"] = toml_edit::value(toml_edit::Array::new());
    }
    let includes = document["include"]
        .as_array_mut()
        .ok_or_else(|| ConfigError::UneditableDocument {
            reason: "include must be an array".to_string(),
        })?;
    if includes.iter().any(|value| value.as_str().is_none()) {
        return Err(ConfigError::UneditableDocument {
            reason: "include entries must be strings".to_string(),
        });
    }
    Ok(includes)
}

/// The text with `include` in its include list once: unchanged when it is
/// there, under any spelling that names the same battery. A spelling no loader
/// would resolve is refused here rather than written.
pub fn add_include(text: &str, include: &str) -> Result<String, ConfigError> {
    refuse_include_entry(include)?;
    let mut document = document(text)?;
    let includes = include_list(&mut document)?;
    let battery = crate::batteries::name_from_include(Path::new(include));
    let present = includes.iter().filter_map(toml_edit::Value::as_str).any(|entry| {
        entry == include || (battery.is_some() && crate::batteries::name_from_include(Path::new(entry)) == battery)
    });
    if present {
        return Ok(text.to_owned());
    }
    includes.push(include);
    Ok(document.to_string())
}

/// The text without `include`: unchanged when it is not there. Every entry
/// spelled exactly so goes; another spelling of the same file is the person's
/// and stays.
pub fn remove_include(text: &str, include: &str) -> Result<String, ConfigError> {
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

/// The text with `server_aliases.<namespace>` naming exactly `servers`, in
/// their order; a binding already spelled so is left as written.
pub fn bind_servers(text: &str, namespace: &str, servers: &[String]) -> Result<String, ConfigError> {
    let namespace = Namespace::parse(namespace).map_err(|error| ConfigError::UneditableDocument {
        reason: error.to_string(),
    })?;
    let mut document = document(text)?;
    if document.get("server_aliases").is_none() {
        document["server_aliases"] = toml_edit::table();
    }
    let aliases = document["server_aliases"]
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::UneditableDocument {
            reason: "server_aliases must be a table".to_string(),
        })?;
    if aliases
        .get(namespace.as_str())
        .is_some_and(|bound| bound_servers(bound).as_deref() == Some(servers))
    {
        return Ok(text.to_owned());
    }
    aliases.insert(
        namespace.as_str(),
        toml_edit::value(servers.iter().map(String::as_str).collect::<toml_edit::Array>()),
    );
    Ok(document.to_string())
}

/// The text without the aliases of `namespaces`: a battery's aliases mean
/// nothing without the battery, and the last one takes the table with it.
pub fn unbind_servers(text: &str, namespaces: &[&str]) -> Result<String, ConfigError> {
    let namespaces = namespaces
        .iter()
        .map(|namespace| {
            Namespace::parse(namespace).map_err(|error| ConfigError::UneditableDocument {
                reason: error.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut document = document(text)?;
    let Some(aliases) = document.get_mut("server_aliases").and_then(Item::as_table_like_mut) else {
        return Ok(text.to_owned());
    };
    let mut changed = false;
    for namespace in &namespaces {
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

/// The servers one binding names: an array of strings, else nothing.
pub(crate) fn bound_servers(item: &Item) -> Option<Vec<String>> {
    item.as_array()?
        .iter()
        .map(|server| server.as_str().map(str::to_owned))
        .collect()
}

/// The text with `credentials.<var>` naming `key`, or without the variable when
/// `key` is nothing; the last entry takes the table with it, so a document that
/// declares no credential carries no empty declaration. A binding the hosted
/// loader would refuse is refused here; removing one is always allowed, because
/// that is how a document a loader refuses gets fixed.
pub fn set_credential(text: &str, var: &str, key: Option<&str>) -> Result<String, ConfigError> {
    if key.is_some() {
        refuse_foreign_credential(var, key)?;
    }
    let mut document = document(text)?;
    let Some(key) = key else {
        let Some(credentials) = document.get_mut("credentials").and_then(Item::as_table_like_mut) else {
            return Ok(text.to_owned());
        };
        if credentials.remove(var).is_none() {
            return Ok(text.to_owned());
        }
        if credentials.is_empty() {
            document.remove("credentials");
        }
        return Ok(document.to_string());
    };
    if document.get("credentials").is_none() {
        document["credentials"] = toml_edit::table();
    }
    let credentials = document["credentials"]
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::UneditableDocument {
            reason: "credentials must be a table".to_string(),
        })?;
    if credentials.get(var).and_then(Item::as_str) == Some(key) {
        return Ok(text.to_owned());
    }
    credentials.insert(var, toml_edit::value(key));
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, HostDefaults};

    const AUTHORED: &str = "# my policy\n[policy]\nversion = 2\n# keep this order\n[[policy.tool]]\nname = 'Read'\n";

    const HOST_DEFAULTS: HostDefaults = HostDefaults {
        consult_timeout: std::time::Duration::from_millis(5000),
        max_body_bytes: 65_536,
    };

    #[test]
    fn an_include_is_added_once_and_the_authored_text_stays() {
        let include = "batteries/github/appa.toml";
        let added = add_include(AUTHORED, include).unwrap();
        assert!(added.contains(AUTHORED));
        assert_eq!(add_include(&added, include).unwrap(), added);
        let spelled = "include = ['./batteries/github/appa.toml']\n";
        assert_eq!(add_include(spelled, include).unwrap(), spelled);
        assert_eq!(remove_include(&added, include).unwrap(), AUTHORED);
    }

    /// A spelling no loader resolves is refused before it reaches the document, with
    /// the refusal the loader itself would answer.
    #[test]
    fn an_include_the_loader_would_refuse_is_never_written() {
        assert!(matches!(
            add_include(AUTHORED, "/srv/batteries/github/appa.toml"),
            Err(ConfigError::AbsoluteInclude { path }) if path == "/srv/batteries/github/appa.toml"
        ));
        assert!(matches!(
            add_include(AUTHORED, "../github/appa.toml"),
            Err(ConfigError::TraversingInclude { path }) if path == "../github/appa.toml"
        ));
    }

    #[test]
    fn removal_takes_the_spelled_entry_and_leaves_another_spelling_of_it() {
        let text = "include = ['./batteries/github/appa.toml', 'batteries/github/appa.toml', 'other.toml']\n";
        let without = remove_include(text, "batteries/github/appa.toml").unwrap();
        assert_eq!(without, "include = ['./batteries/github/appa.toml', 'other.toml']\n");
        assert_eq!(remove_include(&without, "batteries/github/appa.toml").unwrap(), without);
        assert_eq!(
            remove_include("[policy]\nversion = 2\n", "batteries/github/appa.toml").unwrap(),
            "[policy]\nversion = 2\n"
        );
    }

    #[test]
    fn a_server_binding_replaces_the_namespaces_alias_and_keeps_the_authored_text() {
        let servers = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        let bound = bind_servers(AUTHORED, "github", &servers(&["work-github"])).unwrap();
        assert!(bound.contains(AUTHORED));
        assert_eq!(
            bind_servers(&bound, "github", &servers(&["work-github"])).unwrap(),
            bound
        );
        let rebound = bind_servers(&bound, "github", &servers(&["home-github", "lab-github"])).unwrap();
        assert!(rebound.contains("home-github") && !rebound.contains("work-github"));
        assert!(matches!(
            bind_servers(AUTHORED, "not a namespace", &servers(&["github"])),
            Err(ConfigError::UneditableDocument { .. })
        ));
    }

    #[test]
    fn unbinding_takes_only_the_named_namespaces_and_the_last_one_takes_the_table() {
        let servers = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        let bound = bind_servers(AUTHORED, "github", &servers(&["home-github"])).unwrap();
        let with_slack = bind_servers(&bound, "slack", &servers(&["team-slack"])).unwrap();
        let unbound = unbind_servers(&with_slack, &["github"]).unwrap();
        assert!(!unbound.contains("home-github") && unbound.contains("team-slack"));
        assert_eq!(unbind_servers(&unbound, &["github"]).unwrap(), unbound);
        assert_eq!(unbind_servers(&unbound, &["slack"]).unwrap(), AUTHORED);
        assert!(matches!(
            unbind_servers(&unbound, &["not a namespace"]),
            Err(ConfigError::UneditableDocument { .. })
        ));
    }

    /// The table the editor writes is the one the hosted loader admits, and the
    /// entry it drops leaves no declaration behind.
    #[test]
    fn a_credential_binding_round_trips_through_the_hosted_loader() {
        let bound = set_credential(AUTHORED, "APPA_PROVIDER_GITHUB_TOKEN", Some("github_prod")).unwrap();
        assert!(bound.contains(AUTHORED));
        assert_eq!(
            set_credential(&bound, "APPA_PROVIDER_GITHUB_TOKEN", Some("github_prod")).unwrap(),
            bound
        );
        let config = Config::hosted(&bound, HOST_DEFAULTS, |_| None).expect("the editor writes a hosted document");
        assert_eq!(
            config.credentials()["APPA_PROVIDER_GITHUB_TOKEN"],
            "github_prod".to_string()
        );

        let rebound = set_credential(&bound, "APPA_PROVIDER_GITHUB_TOKEN", Some("github_dev")).unwrap();
        assert_eq!(
            Config::hosted(&rebound, HOST_DEFAULTS, |_| None).unwrap().credentials()["APPA_PROVIDER_GITHUB_TOKEN"],
            "github_dev".to_string()
        );

        let dropped = set_credential(&rebound, "APPA_PROVIDER_GITHUB_TOKEN", None).unwrap();
        assert_eq!(dropped, AUTHORED);
        assert_eq!(
            set_credential(&dropped, "APPA_PROVIDER_GITHUB_TOKEN", None).unwrap(),
            dropped
        );
        assert!(
            Config::hosted(&dropped, HOST_DEFAULTS, |_| None)
                .unwrap()
                .credentials()
                .is_empty()
        );
    }

    /// A binding the hosted loader would refuse is refused where it is written, with the
    /// loader's own answer; removing one stays possible, so a refused document is fixable.
    #[test]
    fn a_credential_the_loader_would_refuse_is_never_written() {
        assert!(matches!(
            set_credential(AUTHORED, "APPA_BRIDGE_TOKEN", Some("bridge")),
            Err(ConfigError::CredentialVariable { var }) if var == "APPA_BRIDGE_TOKEN"
        ));
        assert!(matches!(
            set_credential(AUTHORED, "APPA_PROVIDER_GITHUB_TOKEN", Some("")),
            Err(ConfigError::CredentialValue { var }) if var == "APPA_PROVIDER_GITHUB_TOKEN"
        ));
        let foreign = format!("{AUTHORED}[credentials]\nAPPA_BRIDGE_TOKEN = \"bridge\"\n");
        assert!(Config::hosted(&foreign, HOST_DEFAULTS, |_| None).is_err());
        assert_eq!(set_credential(&foreign, "APPA_BRIDGE_TOKEN", None).unwrap(), AUTHORED);
    }

    /// A document that declares two credentials keeps the other one when one goes.
    #[test]
    fn dropping_one_credential_leaves_the_others_declared() {
        let both = set_credential(
            &set_credential(AUTHORED, "APPA_PROVIDER_GITHUB_TOKEN", Some("github_prod")).unwrap(),
            "APPA_PROVIDER_SLACK_TOKEN",
            Some("slack_prod"),
        )
        .unwrap();
        let one = set_credential(&both, "APPA_PROVIDER_GITHUB_TOKEN", None).unwrap();
        let config = Config::hosted(&one, HOST_DEFAULTS, |_| None).unwrap();
        assert_eq!(
            config.credentials(),
            &std::collections::BTreeMap::from([("APPA_PROVIDER_SLACK_TOKEN".to_string(), "slack_prod".to_string())])
        );
    }

    #[test]
    fn a_document_that_cannot_be_parsed_or_shaped_is_refused() {
        assert!(matches!(
            add_include("include = 'one'\n", "batteries/github/appa.toml"),
            Err(ConfigError::UneditableDocument { .. })
        ));
        assert!(matches!(
            set_credential("credentials = 'one'\n", "APPA_PROVIDER_GITHUB_TOKEN", Some("key")),
            Err(ConfigError::UneditableDocument { .. })
        ));
        assert!(matches!(
            remove_include("[policy\n", "batteries/github/appa.toml"),
            Err(ConfigError::UneditableDocument { .. })
        ));
    }
}
